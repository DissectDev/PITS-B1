//! Archetype 2: `mem://` Memory — Scoped Hierarchical Memory + TTL Decay + Semantic Recall.

use anyhow::{anyhow, Result};
use bytes::Bytes;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::core::registry::{join_prefix, op_delete, op_write, BackendRegistry};
use crate::index::sqlite::MemoryRecord;
use crate::index::IndexCoordinator;
use crate::profile::MemoryArchetypeConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRecallHit {
    pub mem_id: String,
    pub scope: String,
    pub uri: String,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    pub score: f64,
    pub created_at: String,
    pub expires_at: Option<String>,
}

#[derive(Clone)]
pub struct MemoryEngine {
    cfg: MemoryArchetypeConfig,
    registry: BackendRegistry,
    index: IndexCoordinator,
}

impl MemoryEngine {
    pub fn new(
        cfg: MemoryArchetypeConfig,
        registry: BackendRegistry,
        index: IndexCoordinator,
    ) -> Self {
        Self { cfg, registry, index }
    }

    pub async fn commit(
        &self,
        scope: &str,
        key: Option<&str>,
        title: &str,
        content: &str,
        tags: Vec<String>,
        importance: Option<f64>,
        ttl_days: Option<u64>,
        embedding: Option<Vec<f32>>,
    ) -> Result<MemoryRecord> {
        let clean_scope = scope.trim_matches('/');
        if clean_scope.is_empty() {
            return Err(anyhow!("memory commit requires non-empty scope (e.g. 'project/session')"));
        }
        let slug = key
            .map(|k| k.trim_matches('/').to_string())
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| format!("mem-{}.md", &Uuid::new_v4().to_string()[..8]));
        let rel_path = format!("{}/{}", clean_scope, slug);
        let uri = format!("mem://{}", rel_path);

        let now = Utc::now();
        let eff_ttl = ttl_days.unwrap_or(self.cfg.ttl_days);
        let expires_at = if eff_ttl > 0 {
            Some((now + Duration::days(eff_ttl as i64)).to_rfc3339())
        } else {
            None
        };

        let rec = MemoryRecord {
            mem_id: Uuid::new_v4().to_string(),
            scope: clean_scope.to_string(),
            uri: uri.clone(),
            title: title.to_string(),
            content: content.to_string(),
            tags,
            embedding,
            importance: importance.unwrap_or(1.0).clamp(0.1, 10.0),
            created_at: now.to_rfc3339(),
            expires_at,
        };

        let doc_md = format!(
            "---\nid: {}\nscope: {}\ntitle: {}\nimportance: {}\ncreated_at: {}\nexpires_at: {}\n---\n\n{}\n",
            rec.mem_id,
            rec.scope,
            rec.title,
            rec.importance,
            rec.created_at,
            rec.expires_at.as_deref().unwrap_or("never"),
            rec.content
        );
        let op = &self.registry.get(&self.cfg.backend)?.operator;
        let obj_key = join_prefix(self.cfg.prefix.as_deref(), &rel_path);
        op_write(op, &obj_key, Bytes::from(doc_md)).await?;
        self.index.upsert_memory(&rec).await?;
        Ok(rec)
    }

    pub fn recall(
        &self,
        scope_prefix: Option<&str>,
        query: &str,
        query_embedding: Option<&[f32]>,
        top_k: usize,
    ) -> Result<Vec<MemoryRecallHit>> {
        let now = Utc::now();
        let all = self.index.list_memories(scope_prefix)?;
        let q_tokens = tokenize(query);

        let mut scored = Vec::new();
        for rec in all {
            if let Some(exp) = &rec.expires_at {
                if let Ok(dt) = DateTime::parse_from_rfc3339(exp) {
                    if dt.with_timezone(&Utc) <= now {
                        continue;
                    }
                }
            }

            let age_hours = DateTime::parse_from_rfc3339(&rec.created_at)
                .map(|dt| (now - dt.with_timezone(&Utc)).num_hours().max(0) as f64)
                .unwrap_or(0.0);
            let recency_decay = (-age_hours / (24.0 * 30.0)).exp();

            let text_score = if q_tokens.is_empty() {
                1.0
            } else {
                let hay = format!("{} {} {}", rec.title, rec.content, rec.tags.join(" ")).to_ascii_lowercase();
                let hits = q_tokens.iter().filter(|t| hay.contains(t.as_str())).count();
                if hits == 0 && query_embedding.is_none() {
                    continue;
                }
                hits as f64 / q_tokens.len() as f64
            };

            let vec_score = match (query_embedding, rec.embedding.as_deref()) {
                (Some(qv), Some(dv)) => cosine_similarity(qv, dv).max(0.0),
                _ => 0.0,
            };

            let combined = (text_score * 0.6 + vec_score * 0.4 + 0.1 * recency_decay) * rec.importance;
            scored.push(MemoryRecallHit {
                mem_id: rec.mem_id,
                scope: rec.scope,
                uri: rec.uri,
                title: rec.title,
                content: rec.content,
                tags: rec.tags,
                score: (combined * 1000.0).round() / 1000.0,
                created_at: rec.created_at,
                expires_at: rec.expires_at,
            });
        }

        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k.max(1));
        Ok(scored)
    }

    pub async fn forget(&self, mem_id_or_uri: &str) -> Result<()> {
        let all = self.index.list_memories(None)?;
        let op = &self.registry.get(&self.cfg.backend)?.operator;
        for rec in all {
            if rec.mem_id == mem_id_or_uri || rec.uri == mem_id_or_uri {
                let rel = rec.uri.strip_prefix("mem://").unwrap_or(&rec.uri);
                let obj_key = join_prefix(self.cfg.prefix.as_deref(), rel);
                let _ = op_delete(op, &obj_key).await;
            }
        }
        self.index.delete_memory(mem_id_or_uri)
    }
}

pub fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .map(|w| w.trim().to_ascii_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let xf = *x as f64;
        let yf = *y as f64;
        dot += xf * yf;
        na += xf * xf;
        nb += yf * yf;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}
