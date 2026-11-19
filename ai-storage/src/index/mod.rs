//! Index & Coordination Facade: combines local SQLite WAL with optional CNPG Postgres & Valkey/Redis.

pub mod postgres;
pub mod redis_coord;
pub mod sqlite;

use anyhow::Result;
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::warn;

use crate::index::postgres::PostgresIndex;
use crate::index::redis_coord::RedisCoordinator;
use crate::index::sqlite::{
    ArtifactLineageRecord, KbDocRecord, LeaseRecord, MemoryRecord, SnapshotEntry, SnapshotMeta,
    SqliteStore, StateCasRecord,
};
use crate::profile::{default_state_root, resolve_secret_reference, ProjectProfile};

#[derive(Clone)]
pub struct IndexCoordinator {
    pub sqlite: Arc<SqliteStore>,
    pub postgres: Option<PostgresIndex>,
    pub redis: Option<RedisCoordinator>,
}

impl IndexCoordinator {
    pub async fn from_profile(profile: &ProjectProfile, profile_path: &Path) -> Result<Self> {
        let sqlite_path = resolve_sqlite_path(profile, profile_path);
        let in_memory = sqlite_path.to_string_lossy() == ":memory:";
        let sqlite = Arc::new(SqliteStore::open(&sqlite_path, in_memory)?);

        let pg_dsn = [
            profile.spec.archetypes.memory.as_ref().and_then(|m| m.index.as_deref()),
            profile.spec.archetypes.knowledge.as_ref().and_then(|k| k.index.as_deref()),
            profile.spec.archetypes.artifacts.as_ref().and_then(|a| a.index.as_deref()),
        ]
        .into_iter()
        .flatten()
        .filter_map(resolve_secret_reference)
        .find(|dsn| dsn.starts_with("postgres://") || dsn.starts_with("postgresql://"));

        let postgres = match pg_dsn {
            Some(dsn) => match PostgresIndex::connect(&dsn).await {
                Ok(pg) => Some(pg),
                Err(e) => {
                    warn!("Postgres index unavailable (using SQLite WAL fallback): {}", e);
                    None
                }
            },
            None => None,
        };

        let redis_url = [
            profile.spec.archetypes.state.as_ref().and_then(|s| s.coordinator.as_deref()),
            profile
                .spec
                .archetypes
                .shared_drive
                .as_ref()
                .and_then(|s| s.lock_backend.as_deref()),
        ]
        .into_iter()
        .flatten()
        .filter_map(resolve_secret_reference)
        .find(|u| u.starts_with("redis://") || u.starts_with("rediss://"));

        let redis = match redis_url {
            Some(url) => {
                let prefix = format!("ai-store:{}", profile.metadata.name);
                match RedisCoordinator::connect(&url, &prefix).await {
                    Ok(r) => Some(r),
                    Err(e) => {
                        warn!("Redis/Valkey coordinator unavailable (using SQLite WAL fallback): {}", e);
                        None
                    }
                }
            }
            None => None,
        };

        Ok(Self {
            sqlite,
            postgres,
            redis,
        })
    }

    pub fn save_snapshot(&self, meta: &SnapshotMeta, entries: &[SnapshotEntry]) -> Result<()> {
        self.sqlite.save_snapshot(meta, entries)
    }

    pub fn list_snapshots(&self, prefix: Option<&str>) -> Result<Vec<SnapshotMeta>> {
        self.sqlite.list_snapshots(prefix)
    }

    pub fn get_snapshot_entries(&self, snapshot_id: &str) -> Result<Vec<SnapshotEntry>> {
        self.sqlite.get_snapshot_entries(snapshot_id)
    }

    pub async fn upsert_memory(&self, rec: &MemoryRecord) -> Result<()> {
        self.sqlite.upsert_memory(rec)?;
        if let Some(pg) = &self.postgres {
            if let Err(e) = pg.upsert_memory(rec).await {
                warn!("Postgres memory mirror failed: {}", e);
            }
        }
        Ok(())
    }

    pub fn list_memories(&self, scope_prefix: Option<&str>) -> Result<Vec<MemoryRecord>> {
        self.sqlite.list_memories(scope_prefix)
    }

    pub fn delete_memory(&self, mem_id: &str) -> Result<()> {
        self.sqlite.delete_memory(mem_id)
    }

    pub async fn upsert_kb_doc(&self, doc: &KbDocRecord) -> Result<()> {
        self.sqlite.upsert_kb_doc(doc)?;
        if let Some(pg) = &self.postgres {
            if let Err(e) = pg.upsert_kb_doc(doc).await {
                warn!("Postgres kb mirror failed: {}", e);
            }
        }
        Ok(())
    }

    pub fn list_kb_docs(&self) -> Result<Vec<KbDocRecord>> {
        self.sqlite.list_kb_docs()
    }

    pub async fn insert_artifact_lineage(&self, rec: &ArtifactLineageRecord) -> Result<()> {
        self.sqlite.insert_artifact_lineage(rec)?;
        if let Some(pg) = &self.postgres {
            if let Err(e) = pg.insert_artifact_lineage(rec).await {
                warn!("Postgres artifact lineage mirror failed: {}", e);
            }
        }
        Ok(())
    }

    pub fn query_artifact_lineage(&self, filter: Option<&str>) -> Result<Vec<ArtifactLineageRecord>> {
        self.sqlite.query_artifact_lineage(filter)
    }

    pub async fn state_get(&self, key: &str) -> Result<Option<StateCasRecord>> {
        if let Some(r) = &self.redis {
            if let Ok(res) = r.state_get(key).await {
                return Ok(res);
            }
        }
        self.sqlite.state_get(key)
    }

    pub async fn state_cas(
        &self,
        key: &str,
        value: &serde_json::Value,
        expected_version: Option<i64>,
        expected_etag: Option<&str>,
        new_etag: &str,
        now_iso: &str,
    ) -> Result<StateCasRecord> {
        if let Some(r) = &self.redis {
            return r
                .state_cas(key, value, expected_version, expected_etag, new_etag, now_iso)
                .await;
        }
        self.sqlite
            .state_cas(key, value, expected_version, expected_etag, new_etag, now_iso)
    }

    pub async fn acquire_lease(&self, lock_key: &str, owner: &str, ttl_secs: i64) -> Result<LeaseRecord> {
        if let Some(r) = &self.redis {
            return r.acquire_lease(lock_key, owner, ttl_secs).await;
        }
        self.sqlite
            .acquire_lease(lock_key, owner, ttl_secs, Utc::now().timestamp())
    }

    pub async fn check_lease_holder(&self, lock_key: &str) -> Result<Option<LeaseRecord>> {
        if let Some(r) = &self.redis {
            if let Ok(res) = r.check_lease_holder(lock_key).await {
                return Ok(res);
            }
        }
        self.sqlite.check_lease_holder(lock_key, Utc::now().timestamp())
    }

    pub async fn release_lease(&self, lock_key: &str, owner: &str) -> Result<bool> {
        if let Some(r) = &self.redis {
            return r.release_lease(lock_key, owner).await;
        }
        self.sqlite.release_lease(lock_key, owner)
    }
}

fn resolve_sqlite_path(profile: &ProjectProfile, profile_path: &Path) -> PathBuf {
    if let Some(ws) = &profile.spec.archetypes.workspace {
        if let Some(idx) = &ws.index {
            if let Some(p) = idx.strip_prefix("sqlite://") {
                if p == ":memory:" {
                    return PathBuf::from(":memory:");
                }
                return PathBuf::from(p);
            }
        }
    }
    let base = profile_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(default_state_root);
    base.join(".ai-store").join("ai-store-meta.sqlite3")
}
