//! Archetype 3: `kb://` Knowledge — Glob, Regex/Trigram Grep & Hybrid Vector/BM25 Search.

use anyhow::Result;
use bytes::Bytes;
use chrono::Utc;
use globset::Glob;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::archetypes::memory::{cosine_similarity, tokenize};
use crate::core::registry::{join_prefix, op_list_recursive, op_read_all, op_write, BackendRegistry};
use crate::index::sqlite::KbDocRecord;
use crate::index::IndexCoordinator;
use crate::profile::{sha256_hex, KnowledgeArchetypeConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepMatch {
    pub uri: String,
    pub line_number: usize,
    pub line: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KbSearchHit {
    pub uri: String,
    pub title: String,
    pub score: f64,
    pub snippet: String,
    pub metadata: serde_json::Value,
}

#[derive(Clone)]
pub struct KnowledgeEngine {
    cfg: KnowledgeArchetypeConfig,
    registry: BackendRegistry,
    index: IndexCoordinator,
}

impl KnowledgeEngine {
    pub fn new(
        cfg: KnowledgeArchetypeConfig,
        registry: BackendRegistry,
        index: IndexCoordinator,
    ) -> Self {
        Self { cfg, registry, index }
    }

    pub async fn ingest(
        &self,
        rel_path: &str,
        title: &str,
        content: &str,
        metadata: serde_json::Value,
        embedding: Option<Vec<f32>>,
    ) -> Result<KbDocRecord> {
        let clean_rel = rel_path.trim_start_matches('/');
        let uri = format!("kb://{}", clean_rel);
        let op = &self.registry.get(&self.cfg.backend)?.operator;
        let obj_key = join_prefix(self.cfg.prefix.as_deref(), clean_rel);
        op_write(op, &obj_key, Bytes::from(content.to_string())).await?;

        let rec = KbDocRecord {
            uri,
            title: title.to_string(),
            content: content.to_string(),
            sha256: sha256_hex(content.as_bytes()),
            embedding,
            metadata,
            indexed_at: Utc::now().to_rfc3339(),
        };
        self.index.upsert_kb_doc(&rec).await?;
        Ok(rec)
    }

    pub async fn glob(&self, pattern: &str, limit: usize) -> Result<Vec<String>> {
        let matcher = Glob::new(pattern)?.compile_matcher();
        let op = &self.registry.get(&self.cfg.backend)?.operator;
        let base = join_prefix(self.cfg.prefix.as_deref(), "");
        let entries = op_list_recursive(op, &base).await?;
        let prefix_strip = self
            .cfg
            .prefix
            .as_deref()
            .map(|p| format!("{}/", p.trim_matches('/')));

        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for e in entries {
            if e.is_dir {
                continue;
            }
            let rel = match &prefix_strip {
                Some(p) => e.path.strip_prefix(p).unwrap_or(&e.path),
                None => &e.path,
            };
            if matcher.is_match(rel) {
                let uri = format!("kb://{}", rel);
                if seen.insert(uri.clone()) {
                    out.push(uri);
                }
            }
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    pub async fn grep(&self, pattern: &str, prefix: Option<&str>, max_matches: usize) -> Result<Vec<GrepMatch>> {
        let re = Regex::new(pattern)?;
        let mut matches = Vec::new();
        let mut scanned_uris = HashSet::new();

        // First scan indexed KB docs (fast path)
        for doc in self.index.list_kb_docs()? {
            let rel = doc.uri.strip_prefix("kb://").unwrap_or(&doc.uri);
            if let Some(p) = prefix {
                if !rel.starts_with(p.trim_start_matches('/')) {
                    continue;
                }
            }
            scanned_uris.insert(doc.uri.clone());
            for (idx, line) in doc.content.lines().enumerate() {
                if re.is_match(line) {
                    matches.push(GrepMatch {
                        uri: doc.uri.clone(),
                        line_number: idx + 1,
                        line: line.chars().take(240).collect(),
                    });
                    if matches.len() >= max_matches {
                        return Ok(matches);
                    }
                }
            }
        }

        // Also scan any unindexed objects in OpenDAL backend
        let op = &self.registry.get(&self.cfg.backend)?.operator;
        let list_key = join_prefix(self.cfg.prefix.as_deref(), prefix.unwrap_or(""));
        let prefix_strip = self
            .cfg
            .prefix
            .as_deref()
            .map(|p| format!("{}/", p.trim_matches('/')));
        if let Ok(entries) = op_list_recursive(op, &list_key).await {
            for e in entries {
                if e.is_dir || e.content_length > 4 * 1024 * 1024 {
                    continue;
                }
                let rel = match &prefix_strip {
                    Some(p) => e.path.strip_prefix(p).unwrap_or(&e.path),
                    None => &e.path,
                };
                let uri = format!("kb://{}", rel);
                if scanned_uris.contains(&uri) {
                    continue;
                }
                if let Ok(bytes) = op_read_all(op, &e.path).await {
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        for (idx, line) in text.lines().enumerate() {
                            if re.is_match(line) {
                                matches.push(GrepMatch {
                                    uri: uri.clone(),
                                    line_number: idx + 1,
                                    line: line.chars().take(240).collect(),
                                });
                                if matches.len() >= max_matches {
                                    return Ok(matches);
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(matches)
    }

    pub fn search(
        &self,
        query: &str,
        query_embedding: Option<&[f32]>,
        top_k: usize,
    ) -> Result<Vec<KbSearchHit>> {
        let docs = self.index.list_kb_docs()?;
        let q_tokens = tokenize(query);
        let q_trigrams = trigrams(&query.to_ascii_lowercase());

        let mut hits = Vec::new();
        for doc in docs {
            let hay = format!("{} {}", doc.title, doc.content).to_ascii_lowercase();
            let kw_score = if q_tokens.is_empty() {
                0.0
            } else {
                let c = q_tokens.iter().filter(|t| hay.contains(t.as_str())).count();
                c as f64 / q_tokens.len() as f64
            };
            let tri_score = if self.cfg.enable_trigram && !q_trigrams.is_empty() {
                let doc_tri = trigrams(&hay);
                let inter = q_trigrams.intersection(&doc_tri).count();
                inter as f64 / q_trigrams.len() as f64
            } else {
                0.0
            };
            let vec_score = if self.cfg.enable_vector {
                match (query_embedding, doc.embedding.as_deref()) {
                    (Some(qv), Some(dv)) => cosine_similarity(qv, dv).max(0.0),
                    _ => 0.0,
                }
            } else {
                0.0
            };

            let score = kw_score * 0.5 + tri_score * 0.25 + vec_score * 0.45;
            if score <= 0.0 {
                continue;
            }
            let snippet = doc.content.chars().take(280).collect::<String>();
            hits.push(KbSearchHit {
                uri: doc.uri,
                title: doc.title,
                score: (score * 1000.0).round() / 1000.0,
                snippet,
                metadata: doc.metadata,
            });
        }
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(top_k.max(1));
        Ok(hits)
    }
}

fn trigrams(s: &str) -> HashSet<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut set = HashSet::new();
    if chars.len() < 3 {
        return set;
    }
    for w in chars.windows(3) {
        set.insert(w.iter().collect());
    }
    set
}
