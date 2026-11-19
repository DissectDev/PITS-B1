//! Optional CloudNativePG PostgreSQL Index Adapter (`pgvector` + `pg_trgm` + Lineage + Memory + KB).
//!
//! Connects asynchronously when `postgres://` DSN is configured in `ProjectProfile`.
//! Automatically attempts `CREATE EXTENSION IF NOT EXISTS pg_trgm` and `vector` (if available on server).

use anyhow::{anyhow, Result};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_postgres::{Client, NoTls};
use tracing::{info, warn};

use crate::index::sqlite::{ArtifactLineageRecord, KbDocRecord, MemoryRecord};

#[derive(Clone)]
pub struct PostgresIndex {
    client: Arc<Client>,
    pub has_pgvector: bool,
    pub has_pg_trgm: bool,
    _guard: Arc<Mutex<()>>,
}

impl PostgresIndex {
    pub async fn connect(dsn: &str) -> Result<Self> {
        let (client, connection) = tokio_postgres::connect(dsn, NoTls).await?;
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                warn!("postgres index background connection ended: {}", e);
            }
        });
        let client = Arc::new(client);

        let has_pg_trgm = client
            .batch_execute("CREATE EXTENSION IF NOT EXISTS pg_trgm;")
            .await
            .is_ok();
        let has_pgvector = client
            .batch_execute("CREATE EXTENSION IF NOT EXISTS vector;")
            .await
            .is_ok();

        client
            .batch_execute(
                r#"
                CREATE TABLE IF NOT EXISTS ai_mem_entries (
                    mem_id TEXT PRIMARY KEY,
                    scope TEXT NOT NULL,
                    uri TEXT NOT NULL,
                    title TEXT NOT NULL,
                    content TEXT NOT NULL,
                    tags_json JSONB NOT NULL DEFAULT '[]'::jsonb,
                    embedding_json JSONB,
                    importance DOUBLE PRECISION NOT NULL DEFAULT 1.0,
                    created_at TEXT NOT NULL,
                    expires_at TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_ai_mem_scope ON ai_mem_entries(scope);

                CREATE TABLE IF NOT EXISTS ai_kb_documents (
                    uri TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    content TEXT NOT NULL,
                    sha256 TEXT NOT NULL,
                    embedding_json JSONB,
                    metadata_json JSONB NOT NULL DEFAULT '{}'::jsonb,
                    indexed_at TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS ai_artifact_lineage (
                    artifact_id TEXT PRIMARY KEY,
                    uri TEXT NOT NULL,
                    logical_path TEXT NOT NULL,
                    sha256 TEXT NOT NULL,
                    size_bytes BIGINT NOT NULL,
                    run_id TEXT NOT NULL,
                    agent_id TEXT NOT NULL,
                    source_ws TEXT,
                    prompt_hash TEXT,
                    metadata_json JSONB NOT NULL DEFAULT '{}'::jsonb,
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_ai_art_sha256 ON ai_artifact_lineage(sha256);
                "#,
            )
            .await?;

        if has_pg_trgm {
            let _ = client
                .batch_execute(
                    "CREATE INDEX IF NOT EXISTS idx_ai_kb_trgm ON ai_kb_documents USING gin (content gin_trgm_ops);",
                )
                .await;
        }

        info!(
            "Connected CNPG Postgres index (pg_trgm={}, pgvector={})",
            has_pg_trgm, has_pgvector
        );
        Ok(Self {
            client,
            has_pgvector,
            has_pg_trgm,
            _guard: Arc::new(Mutex::new(())),
        })
    }

    pub async fn upsert_memory(&self, rec: &MemoryRecord) -> Result<()> {
        let tags_val = serde_json::to_value(&rec.tags)?;
        let emb_val = rec
            .embedding
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?;
        self.client
            .execute(
                "INSERT INTO ai_mem_entries (mem_id, scope, uri, title, content, tags_json, embedding_json, importance, created_at, expires_at)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
                 ON CONFLICT (mem_id) DO UPDATE SET
                   scope=EXCLUDED.scope, uri=EXCLUDED.uri, title=EXCLUDED.title, content=EXCLUDED.content,
                   tags_json=EXCLUDED.tags_json, embedding_json=EXCLUDED.embedding_json,
                   importance=EXCLUDED.importance, expires_at=EXCLUDED.expires_at",
                &[
                    &rec.mem_id,
                    &rec.scope,
                    &rec.uri,
                    &rec.title,
                    &rec.content,
                    &tags_val,
                    &emb_val,
                    &rec.importance,
                    &rec.created_at,
                    &rec.expires_at,
                ],
            )
            .await
            .map_err(|e| anyhow!("pg upsert_memory failed: {}", e))?;
        Ok(())
    }

    pub async fn upsert_kb_doc(&self, doc: &KbDocRecord) -> Result<()> {
        let emb_val = doc.embedding.as_ref().map(serde_json::to_value).transpose()?;
        self.client
            .execute(
                "INSERT INTO ai_kb_documents (uri, title, content, sha256, embedding_json, metadata_json, indexed_at)
                 VALUES ($1,$2,$3,$4,$5,$6,$7)
                 ON CONFLICT (uri) DO UPDATE SET
                   title=EXCLUDED.title, content=EXCLUDED.content, sha256=EXCLUDED.sha256,
                   embedding_json=EXCLUDED.embedding_json, metadata_json=EXCLUDED.metadata_json, indexed_at=EXCLUDED.indexed_at",
                &[
                    &doc.uri,
                    &doc.title,
                    &doc.content,
                    &doc.sha256,
                    &emb_val,
                    &doc.metadata,
                    &doc.indexed_at,
                ],
            )
            .await
            .map_err(|e| anyhow!("pg upsert_kb_doc failed: {}", e))?;
        Ok(())
    }

    pub async fn insert_artifact_lineage(&self, rec: &ArtifactLineageRecord) -> Result<()> {
        self.client
            .execute(
                "INSERT INTO ai_artifact_lineage (artifact_id, uri, logical_path, sha256, size_bytes, run_id, agent_id, source_ws, prompt_hash, metadata_json, created_at)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
                 ON CONFLICT (artifact_id) DO NOTHING",
                &[
                    &rec.artifact_id,
                    &rec.uri,
                    &rec.logical_path,
                    &rec.sha256,
                    &rec.size_bytes,
                    &rec.run_id,
                    &rec.agent_id,
                    &rec.source_ws,
                    &rec.prompt_hash,
                    &rec.metadata,
                    &rec.created_at,
                ],
            )
            .await
            .map_err(|e| anyhow!("pg insert_artifact_lineage failed: {}", e))?;
        Ok(())
    }
}
