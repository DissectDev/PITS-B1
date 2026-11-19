//! Layer 3 VFS Router: 4-step request pipeline (Normalize -> Policy Guard -> Archetype Dispatch -> Telemetry).

use anyhow::{anyhow, Result};
use bytes::Bytes;
use globset::Glob;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::time::Instant;

use crate::archetypes::artifacts::ArtifactsEngine;
use crate::archetypes::datasets::DatasetsEngine;
use crate::archetypes::knowledge::{GrepMatch, KnowledgeEngine};
use crate::archetypes::memory::MemoryEngine;
use crate::archetypes::shared::SharedDriveEngine;
use crate::archetypes::state::StateEngine;
use crate::archetypes::traces::{TraceEvent, TracesEngine};
use crate::archetypes::workspace::WorkspaceEngine;
use crate::core::policy::{check_policy, normalize_uri, ArchetypeKind, OperationKind, ParsedUri};
use crate::core::registry::{
    join_prefix, op_delete, op_exists, op_list_recursive, op_read_all, op_read_range, op_stat,
    op_write, BackendRegistry, ObjectEntry, ObjectStat,
};
use crate::index::IndexCoordinator;
use crate::profile::{AccessMode, ProjectProfile};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountPointInfo {
    pub uri: String,
    pub archetype: String,
    pub backend: String,
    pub backend_scheme: String,
    pub mode: AccessMode,
    pub max_file_size_mb: Option<u64>,
    pub prefix: Option<String>,
}

#[derive(Clone)]
pub struct VfsRouter {
    pub profile: ProjectProfile,
    pub registry: BackendRegistry,
    pub index: IndexCoordinator,
    pub workspace: Option<WorkspaceEngine>,
    pub memory: Option<MemoryEngine>,
    pub knowledge: Option<KnowledgeEngine>,
    pub datasets: Option<DatasetsEngine>,
    pub models: Option<DatasetsEngine>,
    pub artifacts: Option<ArtifactsEngine>,
    pub traces: Option<TracesEngine>,
    pub state: Option<StateEngine>,
    pub shared: Option<SharedDriveEngine>,
}

impl VfsRouter {
    pub fn new(
        profile: ProjectProfile,
        registry: BackendRegistry,
        index: IndexCoordinator,
    ) -> Self {
        let workspace = profile
            .spec
            .archetypes
            .workspace
            .clone()
            .map(|c| WorkspaceEngine::new(c, registry.clone(), index.clone()));
        let memory = profile
            .spec
            .archetypes
            .memory
            .clone()
            .map(|c| MemoryEngine::new(c, registry.clone(), index.clone()));
        let knowledge = profile
            .spec
            .archetypes
            .knowledge
            .clone()
            .map(|c| KnowledgeEngine::new(c, registry.clone(), index.clone()));
        let datasets = profile
            .spec
            .archetypes
            .datasets
            .clone()
            .map(|c| DatasetsEngine::new("data", c, registry.clone(), index.clone()));
        let models = profile
            .spec
            .archetypes
            .models
            .clone()
            .map(|c| DatasetsEngine::new("models", c, registry.clone(), index.clone()));
        let artifacts = profile
            .spec
            .archetypes
            .artifacts
            .clone()
            .map(|c| ArtifactsEngine::new(c, registry.clone(), index.clone()));
        let traces = profile
            .spec
            .archetypes
            .traces
            .clone()
            .map(|c| TracesEngine::new(c, registry.clone()));
        let state = profile
            .spec
            .archetypes
            .state
            .clone()
            .map(|c| StateEngine::new(c, registry.clone(), index.clone()));
        let shared = profile
            .spec
            .archetypes
            .shared_drive
            .clone()
            .map(|c| SharedDriveEngine::new(c, registry.clone(), index.clone()));

        Self {
            profile,
            registry,
            index,
            workspace,
            memory,
            knowledge,
            datasets,
            models,
            artifacts,
            traces,
            state,
            shared,
        }
    }

    fn resolve_target(&self, kind: &ArchetypeKind) -> Result<(&str, Option<&str>)> {
        let arch = &self.profile.spec.archetypes;
        match kind {
            ArchetypeKind::Workspace => arch
                .workspace
                .as_ref()
                .map(|c| (c.backend.as_str(), c.prefix.as_deref()))
                .ok_or_else(|| anyhow!("archetype 'ws://' is not enabled in ProjectProfile")),
            ArchetypeKind::Memory => arch
                .memory
                .as_ref()
                .map(|c| (c.backend.as_str(), c.prefix.as_deref()))
                .ok_or_else(|| anyhow!("archetype 'mem://' is not enabled in ProjectProfile")),
            ArchetypeKind::Knowledge => arch
                .knowledge
                .as_ref()
                .map(|c| (c.backend.as_str(), c.prefix.as_deref()))
                .ok_or_else(|| anyhow!("archetype 'kb://' is not enabled in ProjectProfile")),
            ArchetypeKind::Datasets => arch
                .datasets
                .as_ref()
                .map(|c| (c.backend.as_str(), c.prefix.as_deref()))
                .ok_or_else(|| anyhow!("archetype 'data://' is not enabled in ProjectProfile")),
            ArchetypeKind::Models => arch
                .models
                .as_ref()
                .or(arch.datasets.as_ref())
                .map(|c| (c.backend.as_str(), c.prefix.as_deref()))
                .ok_or_else(|| anyhow!("archetype 'models://' is not enabled in ProjectProfile")),
            ArchetypeKind::Artifacts => arch
                .artifacts
                .as_ref()
                .map(|c| (c.backend.as_str(), c.prefix.as_deref()))
                .ok_or_else(|| anyhow!("archetype 'artifacts://' is not enabled in ProjectProfile")),
            ArchetypeKind::Traces => arch
                .traces
                .as_ref()
                .map(|c| (c.backend.as_str(), c.prefix.as_deref()))
                .ok_or_else(|| anyhow!("archetype 'traces://' is not enabled in ProjectProfile")),
            ArchetypeKind::State => arch
                .state
                .as_ref()
                .map(|c| (c.backend.as_str(), c.prefix.as_deref()))
                .ok_or_else(|| anyhow!("archetype 'state://' is not enabled in ProjectProfile")),
            ArchetypeKind::SharedDrive => arch
                .shared_drive
                .as_ref()
                .map(|c| (c.backend.as_str(), c.prefix.as_deref()))
                .ok_or_else(|| anyhow!("archetype 'shared://' is not enabled in ProjectProfile")),
        }
    }

    fn emit_telemetry(
        &self,
        agent_id: &str,
        event_type: &str,
        parsed: &ParsedUri,
        status: &str,
        started: Instant,
        detail: serde_json::Value,
    ) {
        if let Some(traces) = &self.traces {
            if parsed.kind == ArchetypeKind::Traces && event_type == "traces_flush" {
                return;
            }
            let should_flush = traces.record(TraceEvent {
                event_id: String::new(),
                timestamp: String::new(),
                agent_id: agent_id.to_string(),
                event_type: event_type.to_string(),
                scheme: parsed.scheme.clone(),
                uri: parsed.canonical.clone(),
                status: status.to_string(),
                duration_ms: started.elapsed().as_millis() as u64,
                detail,
            });
            if should_flush {
                let traces_clone = traces.clone();
                tokio::spawn(async move {
                    let _ = traces_clone.flush().await;
                });
            }
        }
    }

    pub fn mounts_tree(&self) -> Vec<MountPointInfo> {
        let mut out = Vec::new();
        let candidates: [(ArchetypeKind, &str); 9] = [
            (ArchetypeKind::Workspace, "Workspace (mutable sandbox + snapshots)"),
            (ArchetypeKind::Memory, "Memory (scoped TTL + semantic recall)"),
            (ArchetypeKind::Knowledge, "Knowledge (glob/grep/trigram/RAG)"),
            (ArchetypeKind::Datasets, "Datasets (byte-range + NVMe cache)"),
            (ArchetypeKind::Models, "Models (byte-range + NVMe cache)"),
            (ArchetypeKind::Artifacts, "Artifacts (content-addressed SHA256 + lineage)"),
            (ArchetypeKind::Traces, "Traces & Audit (ring buffer + rolling JSONL)"),
            (ArchetypeKind::State, "State & Coordination (CAS + fenced leases)"),
            (ArchetypeKind::SharedDrive, "Shared Drive (cross-sandbox concurrent RW)"),
        ];
        for (kind, desc) in candidates {
            if let Ok((backend, prefix)) = self.resolve_target(&kind) {
                let backend_scheme = self
                    .registry
                    .get(backend)
                    .map(|b| b.scheme.clone())
                    .unwrap_or_else(|_| "unknown".to_string());
                let pol = self.profile.policy_for(kind.scheme());
                out.push(MountPointInfo {
                    uri: format!("{}://", kind.scheme()),
                    archetype: desc.to_string(),
                    backend: backend.to_string(),
                    backend_scheme,
                    mode: pol.mode,
                    max_file_size_mb: pol.max_file_size_mb,
                    prefix: prefix.map(str::to_string),
                });
            }
        }
        out
    }

    pub async fn stat(&self, raw_uri: &str, agent_id: &str) -> Result<ObjectStat> {
        let started = Instant::now();
        let parsed = normalize_uri(raw_uri)?;
        check_policy(&self.profile, &parsed, OperationKind::Read, None, false)?;
        let (backend, prefix) = self.resolve_target(&parsed.kind)?;
        let op = &self.registry.get(backend)?.operator;
        let key = join_prefix(prefix, &parsed.rel_path);
        let res = op_stat(op, &key).await;
        self.emit_telemetry(
            agent_id,
            "vfs_stat",
            &parsed,
            if res.is_ok() { "ok" } else { "error" },
            started,
            serde_json::json!({}),
        );
        res.map(|mut s| {
            s.path = parsed.canonical;
            s
        })
    }

    pub async fn list(&self, raw_uri: &str, agent_id: &str) -> Result<Vec<ObjectEntry>> {
        let started = Instant::now();
        let parsed = normalize_uri(raw_uri)?;
        check_policy(&self.profile, &parsed, OperationKind::List, None, false)?;
        let (backend, prefix) = self.resolve_target(&parsed.kind)?;
        let op = &self.registry.get(backend)?.operator;
        let key = join_prefix(prefix, &parsed.rel_path);
        let strip_prefix = prefix.map(|p| format!("{}/", p.trim_matches('/')));
        let res = op_list_recursive(op, &key).await;
        self.emit_telemetry(
            agent_id,
            "vfs_list",
            &parsed,
            if res.is_ok() { "ok" } else { "error" },
            started,
            serde_json::json!({}),
        );
        res.map(|entries| {
            entries
                .into_iter()
                .map(|mut e| {
                    let rel = match &strip_prefix {
                        Some(p) => e.path.strip_prefix(p).unwrap_or(&e.path),
                        None => &e.path,
                    };
                    e.path = format!("{}://{}", parsed.scheme, rel.trim_start_matches('/'));
                    e
                })
                .collect()
        })
    }

    pub async fn read(
        &self,
        raw_uri: &str,
        offset: Option<u64>,
        limit: Option<u64>,
        agent_id: &str,
    ) -> Result<(Bytes, bool)> {
        let started = Instant::now();
        let parsed = normalize_uri(raw_uri)?;
        check_policy(&self.profile, &parsed, OperationKind::Read, None, false)?;

        let out = match parsed.kind {
            ArchetypeKind::Datasets => {
                let ds = self
                    .datasets
                    .as_ref()
                    .ok_or_else(|| anyhow!("datasets archetype unavailable"))?;
                ds.read_range(&parsed.rel_path, offset.unwrap_or(0), limit)
                    .await
            }
            ArchetypeKind::Models => {
                let ds = self
                    .models
                    .as_ref()
                    .or(self.datasets.as_ref())
                    .ok_or_else(|| anyhow!("models archetype unavailable"))?;
                ds.read_range(&parsed.rel_path, offset.unwrap_or(0), limit)
                    .await
            }
            _ => {
                let (backend, prefix) = self.resolve_target(&parsed.kind)?;
                let op = &self.registry.get(backend)?.operator;
                let key = join_prefix(prefix, &parsed.rel_path);
                let bytes = match (offset, limit) {
                    (Some(off), lim) => op_read_range(op, &key, off, lim).await?,
                    (None, Some(lim)) => op_read_range(op, &key, 0, Some(lim)).await?,
                    (None, None) => op_read_all(op, &key).await?,
                };
                Ok((bytes, false))
            }
        };

        self.emit_telemetry(
            agent_id,
            "vfs_read",
            &parsed,
            if out.is_ok() { "ok" } else { "error" },
            started,
            serde_json::json!({
                "offset": offset,
                "limit": limit,
                "cacheHit": out.as_ref().map(|(_, h)| *h).unwrap_or(false),
            }),
        );
        out
    }

    pub async fn write(
        &self,
        raw_uri: &str,
        data: Bytes,
        agent_id: &str,
        run_id: Option<&str>,
    ) -> Result<serde_json::Value> {
        let started = Instant::now();
        let parsed = normalize_uri(raw_uri)?;
        let (backend, prefix) = self.resolve_target(&parsed.kind)?;
        let op = &self.registry.get(backend)?.operator;
        let key = join_prefix(prefix, &parsed.rel_path);
        let exists = op_exists(op, &key).await.unwrap_or(false);
        let op_kind = if exists {
            OperationKind::Overwrite
        } else {
            OperationKind::Create
        };
        check_policy(&self.profile, &parsed, op_kind, Some(data.len()), false)?;

        let result = match parsed.kind {
            ArchetypeKind::Artifacts => {
                let art = self
                    .artifacts
                    .as_ref()
                    .ok_or_else(|| anyhow!("artifacts archetype unavailable"))?;
                let rec = art
                    .publish(
                        &parsed.rel_path,
                        data.clone(),
                        run_id.unwrap_or("vfs-write"),
                        agent_id,
                        None,
                        None,
                        serde_json::json!({}),
                    )
                    .await?;
                Ok(serde_json::to_value(rec)?)
            }
            ArchetypeKind::SharedDrive => {
                let sh = self
                    .shared
                    .as_ref()
                    .ok_or_else(|| anyhow!("sharedDrive archetype unavailable"))?;
                sh.guarded_write(&parsed.rel_path, data.clone(), Some(agent_id))
                    .await?;
                Ok(serde_json::json!({ "uri": parsed.canonical, "bytesWritten": data.len() }))
            }
            _ => {
                op_write(op, &key, data.clone()).await?;
                Ok(serde_json::json!({ "uri": parsed.canonical, "bytesWritten": data.len() }))
            }
        };

        self.emit_telemetry(
            agent_id,
            "vfs_write",
            &parsed,
            if result.is_ok() { "ok" } else { "error" },
            started,
            serde_json::json!({ "bytes": data.len() }),
        );
        result
    }

    pub async fn delete(&self, raw_uri: &str, agent_id: &str) -> Result<()> {
        let started = Instant::now();
        let parsed = normalize_uri(raw_uri)?;
        check_policy(&self.profile, &parsed, OperationKind::Delete, None, false)?;
        let (backend, prefix) = self.resolve_target(&parsed.kind)?;
        let op = &self.registry.get(backend)?.operator;
        let key = join_prefix(prefix, &parsed.rel_path);
        let res = op_delete(op, &key).await;
        self.emit_telemetry(
            agent_id,
            "vfs_delete",
            &parsed,
            if res.is_ok() { "ok" } else { "error" },
            started,
            serde_json::json!({}),
        );
        res
    }

    pub async fn glob(&self, base_uri: &str, pattern: &str, limit: usize, agent_id: &str) -> Result<Vec<String>> {
        let started = Instant::now();
        let parsed = normalize_uri(base_uri)?;
        check_policy(&self.profile, &parsed, OperationKind::List, None, false)?;
        let out = if parsed.kind == ArchetypeKind::Knowledge {
            if let Some(kb) = &self.knowledge {
                kb.glob(pattern, limit).await
            } else {
                Err(anyhow!("knowledge archetype unavailable"))
            }
        } else {
            let matcher = Glob::new(pattern)?.compile_matcher();
            let entries = self.list(base_uri, agent_id).await?;
            let prefix_uri = format!("{}://", parsed.scheme);
            Ok(entries
                .into_iter()
                .filter(|e| !e.is_dir)
                .filter_map(|e| {
                    let rel = e.path.strip_prefix(&prefix_uri).unwrap_or(&e.path);
                    if matcher.is_match(rel) {
                        Some(e.path)
                    } else {
                        None
                    }
                })
                .take(limit)
                .collect())
        };
        self.emit_telemetry(
            agent_id,
            "vfs_glob",
            &parsed,
            if out.is_ok() { "ok" } else { "error" },
            started,
            serde_json::json!({ "pattern": pattern }),
        );
        out
    }

    pub async fn grep(
        &self,
        base_uri: &str,
        pattern: &str,
        max_matches: usize,
        agent_id: &str,
    ) -> Result<Vec<GrepMatch>> {
        let started = Instant::now();
        let parsed = normalize_uri(base_uri)?;
        check_policy(&self.profile, &parsed, OperationKind::Read, None, false)?;
        let out = if parsed.kind == ArchetypeKind::Knowledge {
            if let Some(kb) = &self.knowledge {
                let p = if parsed.rel_path.is_empty() {
                    None
                } else {
                    Some(parsed.rel_path.as_str())
                };
                kb.grep(pattern, p, max_matches).await
            } else {
                Err(anyhow!("knowledge archetype unavailable"))
            }
        } else {
            let re = Regex::new(pattern)?;
            let entries = self.list(base_uri, agent_id).await?;
            let (backend, prefix) = self.resolve_target(&parsed.kind)?;
            let op = &self.registry.get(backend)?.operator;
            let scheme_prefix = format!("{}://", parsed.scheme);
            let mut matches = Vec::new();
            for e in entries {
                if e.is_dir || e.content_length > 4 * 1024 * 1024 {
                    continue;
                }
                let rel = e.path.strip_prefix(&scheme_prefix).unwrap_or(&e.path);
                let obj_key = join_prefix(prefix, rel);
                if let Ok(bytes) = op_read_all(op, &obj_key).await {
                    if let Ok(txt) = std::str::from_utf8(&bytes) {
                        for (idx, line) in txt.lines().enumerate() {
                            if re.is_match(line) {
                                matches.push(GrepMatch {
                                    uri: e.path.clone(),
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
            Ok(matches)
        };
        self.emit_telemetry(
            agent_id,
            "vfs_grep",
            &parsed,
            if out.is_ok() { "ok" } else { "error" },
            started,
            serde_json::json!({ "pattern": pattern }),
        );
        out
    }
}
