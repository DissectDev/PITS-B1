//! Archetype 7: `state://` — Distributed Coordination, Atomic CAS & Fenced Leases.

use anyhow::Result;
use bytes::Bytes;
use chrono::Utc;
use uuid::Uuid;

use crate::core::registry::{join_prefix, op_write, BackendRegistry};
use crate::index::sqlite::{LeaseRecord, StateCasRecord};
use crate::index::IndexCoordinator;
use crate::profile::StateArchetypeConfig;

#[derive(Clone)]
pub struct StateEngine {
    cfg: StateArchetypeConfig,
    registry: BackendRegistry,
    index: IndexCoordinator,
}

impl StateEngine {
    pub fn new(cfg: StateArchetypeConfig, registry: BackendRegistry, index: IndexCoordinator) -> Self {
        Self { cfg, registry, index }
    }

    pub async fn get(&self, key: &str) -> Result<Option<StateCasRecord>> {
        self.index.state_get(key.trim_matches('/')).await
    }

    pub async fn cas(
        &self,
        key: &str,
        value: &serde_json::Value,
        expected_version: Option<i64>,
        expected_etag: Option<&str>,
    ) -> Result<StateCasRecord> {
        let clean_key = key.trim_matches('/');
        let new_etag = format!("\"{}\"", &Uuid::new_v4().to_string()[..12]);
        let now_iso = Utc::now().to_rfc3339();
        let rec = self
            .index
            .state_cas(clean_key, value, expected_version, expected_etag, &new_etag, &now_iso)
            .await?;

        // Mirror checkpoint JSON to OpenDAL `state://` backend for durability & inspection
        let op = &self.registry.get(&self.cfg.backend)?.operator;
        let obj_key = join_prefix(self.cfg.prefix.as_deref(), clean_key);
        let payload = serde_json::to_vec_pretty(&rec)?;
        op_write(op, &obj_key, Bytes::from(payload)).await?;
        Ok(rec)
    }

    pub async fn acquire_lease(&self, lock_key: &str, owner: &str, ttl_secs: i64) -> Result<LeaseRecord> {
        let full = format!("state:{}", lock_key.trim_matches('/'));
        self.index.acquire_lease(&full, owner, ttl_secs).await
    }

    pub async fn release_lease(&self, lock_key: &str, owner: &str) -> Result<bool> {
        let full = format!("state:{}", lock_key.trim_matches('/'));
        self.index.release_lease(&full, owner).await
    }
}
