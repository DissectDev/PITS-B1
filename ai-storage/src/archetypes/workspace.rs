//! Archetype 1: `ws://` Workspace — Mutable Sandbox with Snapshot / Diff / Rollback / Fork.

use anyhow::{anyhow, Result};
use bytes::Bytes;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

use crate::core::registry::{
    join_prefix, op_delete, op_list_recursive, op_read_all, op_write, BackendRegistry,
};
use crate::index::sqlite::{SnapshotEntry, SnapshotMeta};
use crate::index::IndexCoordinator;
use crate::profile::{sha256_hex, WorkspaceArchetypeConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSummary {
    pub from: String,
    pub to: String,
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
    pub unchanged_count: usize,
}

#[derive(Clone)]
pub struct WorkspaceEngine {
    cfg: WorkspaceArchetypeConfig,
    registry: BackendRegistry,
    index: IndexCoordinator,
}

impl WorkspaceEngine {
    pub fn new(
        cfg: WorkspaceArchetypeConfig,
        registry: BackendRegistry,
        index: IndexCoordinator,
    ) -> Self {
        Self { cfg, registry, index }
    }

    fn ws_op(&self) -> Result<&opendal::Operator> {
        Ok(&self.registry.get(&self.cfg.backend)?.operator)
    }

    fn archive_op(&self) -> Result<&opendal::Operator> {
        let name = self.cfg.archive_backend.as_deref().unwrap_or(&self.cfg.backend);
        Ok(&self.registry.get(name)?.operator)
    }

    fn full_key(&self, rel: &str) -> String {
        join_prefix(self.cfg.prefix.as_deref(), rel)
    }

    fn archive_key(&self, snap_id: &str, rel: &str) -> String {
        let base = self.cfg.archive_prefix.as_deref().unwrap_or("snapshots");
        let combined = format!("{}/{}", snap_id, rel.trim_start_matches('/'));
        join_prefix(Some(base), &combined)
    }

    pub async fn snapshot(&self, workspace_prefix: &str, label: &str) -> Result<SnapshotMeta> {
        let op = self.ws_op()?;
        let arch = self.archive_op()?;
        let clean_prefix = workspace_prefix.trim_matches('/');
        let list_base = self.full_key(clean_prefix);
        let raw_entries = op_list_recursive(op, &list_base).await?;

        let snap_id = format!("snap-{}", &Uuid::new_v4().to_string()[..8]);
        let parent_id = self
            .index
            .list_snapshots(Some(clean_prefix))?
            .into_iter()
            .next()
            .map(|s| s.snapshot_id);

        let ws_root_prefix = self
            .cfg
            .prefix
            .as_deref()
            .map(|p| p.trim_matches('/').to_string())
            .filter(|p| !p.is_empty());

        let mut snap_entries = Vec::new();
        let mut total_bytes = 0i64;
        for entry in raw_entries {
            if entry.is_dir {
                continue;
            }
            let bytes = op_read_all(op, &entry.path).await?;
            let digest = sha256_hex(&bytes);
            let rel_path = match &ws_root_prefix {
                Some(p) => entry
                    .path
                    .strip_prefix(&format!("{}/", p))
                    .unwrap_or(&entry.path)
                    .to_string(),
                None => entry.path.clone(),
            };
            let obj_key = self.archive_key(&snap_id, &rel_path);
            op_write(arch, &obj_key, bytes.clone()).await?;
            total_bytes += bytes.len() as i64;
            snap_entries.push(SnapshotEntry {
                rel_path,
                sha256: digest,
                size_bytes: bytes.len() as i64,
                object_key: obj_key,
            });
        }

        let meta = SnapshotMeta {
            snapshot_id: snap_id.clone(),
            workspace_prefix: clean_prefix.to_string(),
            label: if label.trim().is_empty() {
                "manual".to_string()
            } else {
                label.trim().to_string()
            },
            parent_snapshot_id: parent_id,
            archive_uri: Some(format!("{}://{}", self.cfg.archive_backend.as_deref().unwrap_or(&self.cfg.backend), self.cfg.archive_prefix.as_deref().unwrap_or("snapshots"))),
            file_count: snap_entries.len() as i64,
            total_bytes,
            created_at: Utc::now().to_rfc3339(),
        };
        self.index.save_snapshot(&meta, &snap_entries)?;
        Ok(meta)
    }

    pub async fn diff(&self, from_snapshot: &str, to_target: Option<&str>) -> Result<DiffSummary> {
        let from_entries = self.index.get_snapshot_entries(from_snapshot)?;
        if from_entries.is_empty()
            && !self
                .index
                .list_snapshots(None)?
                .iter()
                .any(|s| s.snapshot_id == from_snapshot)
        {
            return Err(anyhow!("unknown snapshot '{}'", from_snapshot));
        }
        let from_map: BTreeMap<String, String> = from_entries
            .into_iter()
            .map(|e| (e.rel_path, e.sha256))
            .collect();

        let (to_label, to_map) = match to_target {
            Some(target_snap) if target_snap != "live" => {
                let entries = self.index.get_snapshot_entries(target_snap)?;
                (
                    target_snap.to_string(),
                    entries.into_iter().map(|e| (e.rel_path, e.sha256)).collect(),
                )
            }
            _ => {
                let live_map = self.live_hashes("").await?;
                ("live".to_string(), live_map)
            }
        };

        let all_keys: BTreeSet<String> = from_map.keys().chain(to_map.keys()).cloned().collect();
        let mut added = Vec::new();
        let mut modified = Vec::new();
        let mut deleted = Vec::new();
        let mut unchanged_count = 0usize;

        for k in all_keys {
            match (from_map.get(&k), to_map.get(&k)) {
                (None, Some(_)) => added.push(k),
                (Some(_), None) => deleted.push(k),
                (Some(a), Some(b)) if a != b => modified.push(k),
                (Some(_), Some(_)) => unchanged_count += 1,
                (None, None) => {}
            }
        }

        Ok(DiffSummary {
            from: from_snapshot.to_string(),
            to: to_label,
            added,
            modified,
            deleted,
            unchanged_count,
        })
    }

    pub async fn rollback(&self, snapshot_id: &str, workspace_prefix: &str) -> Result<usize> {
        let snaps = self.index.list_snapshots(None)?;
        if !snaps.iter().any(|s| s.snapshot_id == snapshot_id) {
            return Err(anyhow!("snapshot '{}' not found", snapshot_id));
        }
        let entries = self.index.get_snapshot_entries(snapshot_id)?;
        let ws = self.ws_op()?;
        let arch = self.archive_op()?;

        let clean_prefix = workspace_prefix.trim_matches('/');
        let live = self.live_hashes(clean_prefix).await?;
        let target_set: BTreeSet<String> = entries.iter().map(|e| e.rel_path.clone()).collect();

        for rel in live.keys() {
            if !target_set.contains(rel) {
                let k = self.full_key(rel);
                let _ = op_delete(ws, &k).await;
            }
        }

        let mut restored = 0usize;
        for entry in entries {
            let bytes = op_read_all(arch, &entry.object_key).await?;
            let dest_key = self.full_key(&entry.rel_path);
            op_write(ws, &dest_key, bytes).await?;
            restored += 1;
        }
        Ok(restored)
    }

    pub async fn fork(&self, snapshot_id: &str, target_prefix: &str) -> Result<usize> {
        let clean_target = target_prefix.trim_matches('/');
        if clean_target.is_empty() {
            return Err(anyhow!("fork requires non-empty target_prefix"));
        }
        let entries = self.index.get_snapshot_entries(snapshot_id)?;
        if entries.is_empty()
            && !self
                .index
                .list_snapshots(None)?
                .iter()
                .any(|s| s.snapshot_id == snapshot_id)
        {
            return Err(anyhow!("snapshot '{}' not found", snapshot_id));
        }
        let ws = self.ws_op()?;
        let arch = self.archive_op()?;
        let mut copied = 0usize;
        for entry in entries {
            let bytes: Bytes = op_read_all(arch, &entry.object_key).await?;
            let rel_tail = entry
                .rel_path
                .split_once('/')
                .map(|(_, r)| r)
                .unwrap_or(&entry.rel_path);
            let dest_rel = format!("{}/{}", clean_target, rel_tail);
            op_write(ws, &self.full_key(&dest_rel), bytes).await?;
            copied += 1;
        }
        Ok(copied)
    }

    async fn live_hashes(&self, workspace_prefix: &str) -> Result<BTreeMap<String, String>> {
        let op = self.ws_op()?;
        let list_base = self.full_key(workspace_prefix.trim_matches('/'));
        let entries = op_list_recursive(op, &list_base).await?;
        let ws_root_prefix = self
            .cfg
            .prefix
            .as_deref()
            .map(|p| p.trim_matches('/').to_string())
            .filter(|p| !p.is_empty());

        let mut out = BTreeMap::new();
        for e in entries {
            if e.is_dir {
                continue;
            }
            let bytes = op_read_all(op, &e.path).await?;
            let rel_path = match &ws_root_prefix {
                Some(p) => e
                    .path
                    .strip_prefix(&format!("{}/", p))
                    .unwrap_or(&e.path)
                    .to_string(),
                None => e.path.clone(),
            };
            out.insert(rel_path, sha256_hex(&bytes));
        }
        Ok(out)
    }
}
