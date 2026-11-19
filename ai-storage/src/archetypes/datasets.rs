//! Archetype 4: `data://` & `models://` — Byte-Range Streaming & NVMe Read-Through LRU Cache.

use anyhow::Result;
use bytes::Bytes;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::core::registry::{join_prefix, op_read_all, op_read_range, op_stat, BackendRegistry};
use crate::index::IndexCoordinator;
use crate::profile::{sha256_hex, DatasetsArchetypeConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStatusReport {
    pub scheme: String,
    pub cache_dir: Option<String>,
    pub cache_max_bytes: u64,
    pub current_bytes: u64,
    pub entry_count: usize,
    pub hits: u64,
    pub misses: u64,
}

#[derive(Clone)]
pub struct DatasetsEngine {
    scheme: String,
    cfg: DatasetsArchetypeConfig,
    registry: BackendRegistry,
    index: IndexCoordinator,
    hits: Arc<AtomicU64>,
    misses: Arc<AtomicU64>,
}

impl DatasetsEngine {
    pub fn new(
        scheme: impl Into<String>,
        cfg: DatasetsArchetypeConfig,
        registry: BackendRegistry,
        index: IndexCoordinator,
    ) -> Self {
        Self {
            scheme: scheme.into(),
            cfg,
            registry,
            index,
            hits: Arc::new(AtomicU64::new(0)),
            misses: Arc::new(AtomicU64::new(0)),
        }
    }

    pub async fn read_range(
        &self,
        rel_path: &str,
        offset: u64,
        limit: Option<u64>,
    ) -> Result<(Bytes, bool)> {
        let op = &self.registry.get(&self.cfg.backend)?.operator;
        let obj_key = join_prefix(self.cfg.prefix.as_deref(), rel_path);
        let uri = format!("{}://{}", self.scheme, rel_path.trim_start_matches('/'));
        let cache_key = sha256_hex(uri.as_bytes());

        if let Some(cache_dir) = &self.cfg.cache_dir {
            let local_file = Path::new(cache_dir).join(&cache_key);
            if local_file.exists() {
                if let Ok(slice) = read_local_range(&local_file, offset, limit) {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    let meta = fs::metadata(&local_file)?;
                    let _ = self.index.sqlite.record_cache_entry(
                        &cache_key,
                        &uri,
                        &local_file.to_string_lossy(),
                        &cache_key,
                        meta.len() as i64,
                        Utc::now().timestamp(),
                    );
                    return Ok((slice, true));
                }
            }
        }

        self.misses.fetch_add(1, Ordering::Relaxed);

        // If read-through cache is enabled and object is reasonably sized (<= 64 MiB), populate cache
        if let Some(cache_dir) = &self.cfg.cache_dir {
            if let Ok(st) = op_stat(op, &obj_key).await {
                if st.content_length > 0 && st.content_length <= 64 * 1024 * 1024 && st.content_length <= self.cfg.cache_max_bytes {
                    let full = op_read_all(op, &obj_key).await?;
                    self.populate_cache(cache_dir, &cache_key, &uri, &full)?;
                    let slice = slice_bytes(&full, offset, limit);
                    return Ok((slice, false));
                }
            }
        }

        let bytes = op_read_range(op, &obj_key, offset, limit).await?;
        Ok((bytes, false))
    }

    fn populate_cache(&self, cache_dir: &str, cache_key: &str, uri: &str, data: &[u8]) -> Result<()> {
        let dir = PathBuf::from(cache_dir);
        fs::create_dir_all(&dir)?;
        let local_file = dir.join(cache_key);
        fs::write(&local_file, data)?;
        let digest = sha256_hex(data);
        self.index.sqlite.record_cache_entry(
            cache_key,
            uri,
            &local_file.to_string_lossy(),
            &digest,
            data.len() as i64,
            Utc::now().timestamp(),
        )?;
        self.evict_if_needed()?;
        Ok(())
    }

    fn evict_if_needed(&self) -> Result<()> {
        let mut entries = self.index.sqlite.list_cache_lru()?;
        let mut total: u64 = entries.iter().map(|e| e.2.max(0) as u64).sum();
        while total > self.cfg.cache_max_bytes && !entries.is_empty() {
            let (key, path, sz, _) = entries.remove(0);
            let _ = fs::remove_file(&path);
            let _ = self.index.sqlite.delete_cache_entry(&key);
            total = total.saturating_sub(sz.max(0) as u64);
        }
        Ok(())
    }

    pub fn cache_status(&self) -> Result<CacheStatusReport> {
        let entries = self.index.sqlite.list_cache_lru()?;
        let current_bytes: u64 = entries.iter().map(|e| e.2.max(0) as u64).sum();
        Ok(CacheStatusReport {
            scheme: self.scheme.clone(),
            cache_dir: self.cfg.cache_dir.clone(),
            cache_max_bytes: self.cfg.cache_max_bytes,
            current_bytes,
            entry_count: entries.len(),
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        })
    }
}

fn read_local_range(path: &Path, offset: u64, limit: Option<u64>) -> Result<Bytes> {
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    match limit {
        Some(len) => {
            let mut take = f.take(len);
            take.read_to_end(&mut buf)?;
        }
        None => {
            f.read_to_end(&mut buf)?;
        }
    }
    Ok(Bytes::from(buf))
}

fn slice_bytes(full: &Bytes, offset: u64, limit: Option<u64>) -> Bytes {
    let start = (offset as usize).min(full.len());
    let end = match limit {
        Some(len) => start.saturating_add(len as usize).min(full.len()),
        None => full.len(),
    };
    full.slice(start..end)
}
