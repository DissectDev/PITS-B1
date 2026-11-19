//! Archetype 8: `shared://` — Cross-Sandbox Concurrent Shared Drive with Path-Level Lease Locks.

use anyhow::{anyhow, Result};
use bytes::Bytes;

use crate::core::registry::{join_prefix, op_write, BackendRegistry};
use crate::index::sqlite::LeaseRecord;
use crate::index::IndexCoordinator;
use crate::profile::SharedDriveArchetypeConfig;

#[derive(Clone)]
pub struct SharedDriveEngine {
    cfg: SharedDriveArchetypeConfig,
    registry: BackendRegistry,
    index: IndexCoordinator,
}

impl SharedDriveEngine {
    pub fn new(
        cfg: SharedDriveArchetypeConfig,
        registry: BackendRegistry,
        index: IndexCoordinator,
    ) -> Self {
        Self { cfg, registry, index }
    }

    fn lock_key(rel_path: &str) -> String {
        format!("shared:{}", rel_path.trim_matches('/'))
    }

    pub async fn lock(&self, rel_path: &str, owner: &str, ttl_secs: i64) -> Result<LeaseRecord> {
        if owner.trim().is_empty() {
            return Err(anyhow!("shared_lock requires non-empty owner"));
        }
        self.index
            .acquire_lease(&Self::lock_key(rel_path), owner.trim(), ttl_secs)
            .await
    }

    pub async fn unlock(&self, rel_path: &str, owner: &str) -> Result<bool> {
        if owner.trim().is_empty() {
            return Err(anyhow!("shared_unlock requires non-empty owner"));
        }
        self.index
            .release_lease(&Self::lock_key(rel_path), owner.trim())
            .await
    }

    pub async fn guarded_write(&self, rel_path: &str, data: Bytes, owner: Option<&str>) -> Result<()> {
        let lock_key = Self::lock_key(rel_path);
        if let Some(holder) = self.index.check_lease_holder(&lock_key).await? {
            let caller = owner.unwrap_or("").trim();
            if holder.owner != caller {
                return Err(anyhow!(
                    "shared://{} is locked by '{}' (fencing_token={}); caller '{}' rejected",
                    rel_path.trim_matches('/'),
                    holder.owner,
                    holder.fencing_token,
                    if caller.is_empty() { "<anonymous>" } else { caller }
                ));
            }
        }
        let op = &self.registry.get(&self.cfg.backend)?.operator;
        let obj_key = join_prefix(self.cfg.prefix.as_deref(), rel_path);
        op_write(op, &obj_key, data).await
    }
}
