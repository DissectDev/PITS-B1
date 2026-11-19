//! Archetype 6: `traces://` — High-Throughput Buffered Audit & Trace Stream with Rolling Flush.

use anyhow::Result;
use bytes::Bytes;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use crate::core::registry::{join_prefix, op_list_recursive, op_read_all, op_write, BackendRegistry};
use crate::profile::TracesArchetypeConfig;

const MAX_RING_EVENTS: usize = 512;
const AUTO_FLUSH_BATCH: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEvent {
    pub event_id: String,
    pub timestamp: String,
    pub agent_id: String,
    pub event_type: String,
    pub scheme: String,
    pub uri: String,
    pub status: String,
    pub duration_ms: u64,
    pub detail: serde_json::Value,
}

#[derive(Clone)]
pub struct TracesEngine {
    cfg: TracesArchetypeConfig,
    registry: BackendRegistry,
    ring: Arc<Mutex<VecDeque<TraceEvent>>>,
    pending_flush: Arc<Mutex<Vec<TraceEvent>>>,
}

impl TracesEngine {
    pub fn new(cfg: TracesArchetypeConfig, registry: BackendRegistry) -> Self {
        Self {
            cfg,
            registry,
            ring: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_RING_EVENTS))),
            pending_flush: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn flush_interval_secs(&self) -> u64 {
        self.cfg.flush_interval_sec.max(2)
    }

    pub fn record(&self, mut ev: TraceEvent) -> bool {
        if ev.event_id.is_empty() {
            ev.event_id = Uuid::new_v4().to_string();
        }
        if ev.timestamp.is_empty() {
            ev.timestamp = Utc::now().to_rfc3339();
        }
        if let Ok(mut ring) = self.ring.lock() {
            if ring.len() >= MAX_RING_EVENTS {
                ring.pop_front();
            }
            ring.push_back(ev.clone());
        }
        let mut should_flush = false;
        if let Ok(mut pending) = self.pending_flush.lock() {
            pending.push(ev);
            if pending.len() >= AUTO_FLUSH_BATCH {
                should_flush = true;
            }
        }
        should_flush
    }

    pub async fn flush(&self) -> Result<Option<String>> {
        let batch = {
            let mut pending = self
                .pending_flush
                .lock()
                .map_err(|_| anyhow::anyhow!("traces pending mutex poisoned"))?;
            if pending.is_empty() {
                return Ok(None);
            }
            std::mem::take(&mut *pending)
        };

        let mut jsonl = String::new();
        for ev in &batch {
            if let Ok(line) = serde_json::to_string(ev) {
                jsonl.push_str(&line);
                jsonl.push('\n');
            }
        }

        let now = Utc::now();
        let rel_key = format!(
            "{}/traces-{}.jsonl",
            now.format("%Y-%m-%d/%H"),
            &Uuid::new_v4().to_string()[..8]
        );
        let full_key = join_prefix(self.cfg.prefix.as_deref(), &rel_key);
        let op = &self.registry.get(&self.cfg.backend)?.operator;
        op_write(op, &full_key, Bytes::from(jsonl)).await?;
        Ok(Some(format!("traces://{}", rel_key)))
    }

    pub async fn query(
        &self,
        agent_id: Option<&str>,
        event_type: Option<&str>,
        scheme: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TraceEvent>> {
        let mut results: Vec<TraceEvent> = {
            let ring = self
                .ring
                .lock()
                .map_err(|_| anyhow::anyhow!("traces ring mutex poisoned"))?;
            ring.iter().cloned().collect()
        };

        // Also pull most recent flushed batches from OpenDAL if caller requests history
        if results.len() < limit {
            let op = &self.registry.get(&self.cfg.backend)?.operator;
            let base = join_prefix(self.cfg.prefix.as_deref(), "");
            if let Ok(mut entries) = op_list_recursive(op, &base).await {
                entries.sort_by(|a, b| b.path.cmp(&a.path));
                for entry in entries.into_iter().filter(|e| !e.is_dir && e.path.ends_with(".jsonl")).take(4) {
                    if let Ok(bytes) = op_read_all(op, &entry.path).await {
                        if let Ok(txt) = std::str::from_utf8(&bytes) {
                            for line in txt.lines() {
                                if let Ok(ev) = serde_json::from_str::<TraceEvent>(line) {
                                    if !results.iter().any(|r| r.event_id == ev.event_id) {
                                        results.push(ev);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        results.retain(|ev| {
            agent_id.map_or(true, |a| ev.agent_id == a)
                && event_type.map_or(true, |t| ev.event_type == t)
                && scheme.map_or(true, |s| ev.scheme == s)
        });
        results.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        results.truncate(limit.max(1));
        Ok(results)
    }
}
