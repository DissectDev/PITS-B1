//! Archetype 5: `artifacts://` — Content-Addressed Immutable Storage & Lineage Metadata.

use anyhow::{anyhow, Result};
use bytes::Bytes;
use chrono::Utc;
use uuid::Uuid;

use crate::core::registry::{join_prefix, op_exists, op_read_all, op_write, BackendRegistry};
use crate::index::sqlite::ArtifactLineageRecord;
use crate::index::IndexCoordinator;
use crate::profile::{sha256_hex, ArtifactsArchetypeConfig};

#[derive(Clone)]
pub struct ArtifactsEngine {
    cfg: ArtifactsArchetypeConfig,
    registry: BackendRegistry,
    index: IndexCoordinator,
}

impl ArtifactsEngine {
    pub fn new(
        cfg: ArtifactsArchetypeConfig,
        registry: BackendRegistry,
        index: IndexCoordinator,
    ) -> Self {
        Self { cfg, registry, index }
    }

    pub async fn publish(
        &self,
        logical_path: &str,
        data: Bytes,
        run_id: &str,
        agent_id: &str,
        source_ws: Option<String>,
        prompt_hash: Option<String>,
        metadata: serde_json::Value,
    ) -> Result<ArtifactLineageRecord> {
        let clean_rel = logical_path.trim_start_matches('/');
        if clean_rel.is_empty() {
            return Err(anyhow!("artifact publish requires a non-empty logical path"));
        }
        let digest = sha256_hex(&data);
        let op = &self.registry.get(&self.cfg.backend)?.operator;

        // Content-addressed immutable blob: <prefix>/sha256/<digest>
        let cas_rel = format!("sha256/{}", digest);
        let cas_key = join_prefix(self.cfg.prefix.as_deref(), &cas_rel);
        if !op_exists(op, &cas_key).await.unwrap_or(false) {
            op_write(op, &cas_key, data.clone()).await?;
        }

        // Logical path write (append-only: refuse conflicting overwrite with different sha256)
        let logical_key = join_prefix(self.cfg.prefix.as_deref(), clean_rel);
        if op_exists(op, &logical_key).await.unwrap_or(false) {
            let existing = op_read_all(op, &logical_key).await?;
            let existing_digest = sha256_hex(&existing);
            if existing_digest != digest {
                return Err(anyhow!(
                    "append-only conflict on 'artifacts://{}': existing sha256 {} != new {}",
                    clean_rel,
                    existing_digest,
                    digest
                ));
            }
        } else {
            op_write(op, &logical_key, data.clone()).await?;
        }

        let uri = if self.cfg.content_addressed {
            format!("artifacts://sha256/{}", digest)
        } else {
            format!("artifacts://{}", clean_rel)
        };

        let rec = ArtifactLineageRecord {
            artifact_id: Uuid::new_v4().to_string(),
            uri,
            logical_path: format!("artifacts://{}", clean_rel),
            sha256: digest,
            size_bytes: data.len() as i64,
            run_id: if run_id.trim().is_empty() {
                "manual".to_string()
            } else {
                run_id.to_string()
            },
            agent_id: if agent_id.trim().is_empty() {
                "unknown-agent".to_string()
            } else {
                agent_id.to_string()
            },
            source_ws,
            prompt_hash,
            metadata,
            created_at: Utc::now().to_rfc3339(),
        };
        self.index.insert_artifact_lineage(&rec).await?;
        Ok(rec)
    }

    pub fn lineage(&self, filter: Option<&str>) -> Result<Vec<ArtifactLineageRecord>> {
        self.index.query_artifact_lineage(filter)
    }
}
