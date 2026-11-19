//! Layer 5: MCP Server Producer (Streamable HTTP `/mcp` + Singleton Daemon & Thin Stdio Proxy).
//!
//! Produces MCP `Resources` (`profile://active`, `mounts://tree`, `ws://snapshots`),
//! `Prompts` (`storage-workflow-guide`), and `Tools` across all 8 AI Storage Archetypes.

use anyhow::{anyhow, Context, Result};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use bytes::Bytes;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

use crate::archetypes::traces::TraceEvent;
use crate::core::registry::BackendRegistry;
use crate::core::router::VfsRouter;
use crate::index::IndexCoordinator;
use crate::profile::{
    default_daemon_dir, resolve_secret_reference, sha256_hex, ProjectProfile,
};

const SUPPORTED_PROTOCOL_VERSIONS: [&str; 2] = ["2025-03-26", "2024-11-05"];
const DEFAULT_PROTOCOL_VERSION: &str = "2025-03-26";

#[derive(Clone)]
pub struct AppState {
    pub router: Arc<RwLock<Arc<VfsRouter>>>,
    pub profile_path: PathBuf,
    pub profile_digest: Arc<RwLock<String>>,
    pub sessions: Arc<RwLock<HashSet<String>>>,
    pub bearer_token: Option<String>,
    pub request_counter: Arc<AtomicU64>,
    pub error_counter: Arc<AtomicU64>,
}

impl AppState {
    pub async fn initialize(profile_path: Option<&Path>) -> Result<Self> {
        let (profile, resolved_path, digest) = ProjectProfile::load_or_default(profile_path)?;
        let bearer_token = std::env::var("AI_STORE_BEARER_TOKEN")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| {
                profile
                    .spec
                    .exposure
                    .bearer_token_ref
                    .as_deref()
                    .and_then(resolve_secret_reference)
            });
        let router = build_router(&profile, &resolved_path).await?;
        Ok(Self {
            router: Arc::new(RwLock::new(Arc::new(router))),
            profile_path: resolved_path,
            profile_digest: Arc::new(RwLock::new(digest)),
            sessions: Arc::new(RwLock::new(HashSet::new())),
            bearer_token,
            request_counter: Arc::new(AtomicU64::new(0)),
            error_counter: Arc::new(AtomicU64::new(0)),
        })
    }

    pub async fn reload_if_changed(&self) -> Result<bool> {
        if !self.profile_path.exists() {
            return Ok(false);
        }
        let raw = tokio::fs::read(&self.profile_path).await?;
        let new_digest = sha256_hex(&raw);
        {
            let cur = self.profile_digest.read().await;
            if *cur == new_digest {
                return Ok(false);
            }
        }
        let profile: ProjectProfile = serde_yaml::from_slice(&raw)?;
        profile.validate()?;
        let new_router = build_router(&profile, &self.profile_path).await?;
        {
            let mut w = self.router.write().await;
            *w = Arc::new(new_router);
        }
        {
            let mut d = self.profile_digest.write().await;
            *d = new_digest;
        }
        info!("Hot-reloaded ProjectProfile from {}", self.profile_path.display());
        Ok(true)
    }

    pub async fn current_router(&self) -> Arc<VfsRouter> {
        self.router.read().await.clone()
    }
}

pub async fn build_router(profile: &ProjectProfile, profile_path: &Path) -> Result<VfsRouter> {
    let registry = BackendRegistry::from_profile(profile)?;
    let index = IndexCoordinator::from_profile(profile, profile_path).await?;
    Ok(VfsRouter::new(profile.clone(), registry, index))
}

fn jsonrpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn jsonrpc_err(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

fn tool_ok(payload: Value) -> Value {
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": payload,
        "isError": false
    })
}

fn tool_err(message: impl Into<String>) -> Value {
    let msg = message.into();
    json!({
        "content": [{ "type": "text", "text": msg }],
        "structuredContent": { "error": msg },
        "isError": true
    })
}

pub fn all_tool_definitions(tools_preset: &str) -> Vec<Value> {
    let readonly = tools_preset.eq_ignore_ascii_case("readonly");
    let sandbox_worker = tools_preset.eq_ignore_ascii_case("sandbox-worker");

    let mut tools = vec![
        json!({
            "name": "vfs_stat",
            "description": "Inspect metadata (size, dir flag, etag, content-type) for any AI Storage URI (ws://, mem://, kb://, data://, models://, artifacts://, traces://, state://, shared://).",
            "inputSchema": {
                "type": "object",
                "required": ["uri"],
                "properties": {
                    "uri": { "type": "string" },
                    "agentId": { "type": "string" }
                }
            }
        }),
        json!({
            "name": "vfs_list",
            "description": "List objects under an AI Storage URI prefix.",
            "inputSchema": {
                "type": "object",
                "required": ["uri"],
                "properties": {
                    "uri": { "type": "string" },
                    "agentId": { "type": "string" }
                }
            }
        }),
        json!({
            "name": "vfs_read",
            "description": "Read text or byte-range slice from an AI Storage URI (supports offset & limit; data:// & models:// use NVMe read-through cache).",
            "inputSchema": {
                "type": "object",
                "required": ["uri"],
                "properties": {
                    "uri": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 0 },
                    "limit": { "type": "integer", "minimum": 1 },
                    "encoding": { "type": "string", "enum": ["utf8", "base64"], "default": "utf8" },
                    "agentId": { "type": "string" }
                }
            }
        }),
        json!({
            "name": "vfs_glob",
            "description": "Find files matching a glob pattern under a base AI Storage URI (e.g. kb:// or ws://).",
            "inputSchema": {
                "type": "object",
                "required": ["uri", "pattern"],
                "properties": {
                    "uri": { "type": "string" },
                    "pattern": { "type": "string" },
                    "limit": { "type": "integer", "default": 100 },
                    "agentId": { "type": "string" }
                }
            }
        }),
        json!({
            "name": "vfs_grep",
            "description": "Search lines matching a regex pattern under a base AI Storage URI (e.g. kb:// or ws://).",
            "inputSchema": {
                "type": "object",
                "required": ["uri", "pattern"],
                "properties": {
                    "uri": { "type": "string" },
                    "pattern": { "type": "string" },
                    "maxMatches": { "type": "integer", "default": 50 },
                    "agentId": { "type": "string" }
                }
            }
        }),
        json!({
            "name": "mem_recall",
            "description": "Recall scoped AI memories (mem://) ranked by keyword overlap, TTL recency decay, importance, and optional vector similarity.",
            "inputSchema": {
                "type": "object",
                "required": ["query"],
                "properties": {
                    "scope": { "type": "string" },
                    "query": { "type": "string" },
                    "embedding": { "type": "array", "items": { "type": "number" } },
                    "topK": { "type": "integer", "default": 10 }
                }
            }
        }),
        json!({
            "name": "kb_search",
            "description": "Hybrid trigram + keyword + optional vector search across kb:// knowledge documents.",
            "inputSchema": {
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": { "type": "string" },
                    "embedding": { "type": "array", "items": { "type": "number" } },
                    "topK": { "type": "integer", "default": 10 }
                }
            }
        }),
        json!({
            "name": "data_cache_status",
            "description": "Inspect NVMe/disk read-through cache metrics for data:// and models://.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scheme": { "type": "string", "enum": ["data", "models"], "default": "data" }
                }
            }
        }),
        json!({
            "name": "artifact_lineage",
            "description": "Query immutable artifact lineage records (by sha256, runId, or uri).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filter": { "type": "string" }
                }
            }
        }),
        json!({
            "name": "traces_query",
            "description": "Query buffered and OpenDAL-flushed audit trace events from traces://.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agentId": { "type": "string" },
                    "eventType": { "type": "string" },
                    "scheme": { "type": "string" },
                    "limit": { "type": "integer", "default": 50 }
                }
            }
        }),
    ];

    if !readonly {
        tools.extend([
            json!({
                "name": "vfs_write",
                "description": "Write UTF-8 or base64 content to a Policy-Guard-checked AI Storage URI.",
                "inputSchema": {
                    "type": "object",
                    "required": ["uri", "content"],
                    "properties": {
                        "uri": { "type": "string" },
                        "content": { "type": "string" },
                        "encoding": { "type": "string", "enum": ["utf8", "base64"], "default": "utf8" },
                        "agentId": { "type": "string" },
                        "runId": { "type": "string" }
                    }
                }
            }),
            json!({
                "name": "vfs_delete",
                "description": "Delete an object under a read-write AI Storage URI (denied on read-only and append-only schemes).",
                "inputSchema": {
                    "type": "object",
                    "required": ["uri"],
                    "properties": {
                        "uri": { "type": "string" },
                        "agentId": { "type": "string" }
                    }
                }
            }),
            json!({
                "name": "artifact_publish",
                "description": "Publish an immutable content-addressed artifact to artifacts:// with mandatory run/agent lineage.",
                "inputSchema": {
                    "type": "object",
                    "required": ["path", "content", "runId", "agentId"],
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" },
                        "encoding": { "type": "string", "enum": ["utf8", "base64"], "default": "utf8" },
                        "runId": { "type": "string" },
                        "agentId": { "type": "string" },
                        "sourceWs": { "type": "string" },
                        "promptHash": { "type": "string" },
                        "metadata": { "type": "object" }
                    }
                }
            }),
            json!({
                "name": "traces_emit",
                "description": "Append an explicit trace/audit event to traces:// and optionally flush immediately.",
                "inputSchema": {
                    "type": "object",
                    "required": ["agentId", "eventType"],
                    "properties": {
                        "agentId": { "type": "string" },
                        "eventType": { "type": "string" },
                        "scheme": { "type": "string", "default": "traces" },
                        "uri": { "type": "string", "default": "traces://manual" },
                        "status": { "type": "string", "default": "ok" },
                        "durationMs": { "type": "integer", "default": 0 },
                        "detail": { "type": "object" },
                        "flush": { "type": "boolean", "default": false }
                    }
                }
            }),
            json!({
                "name": "shared_lock",
                "description": "Acquire or renew a path-level fenced lease lock on shared:// for safe cross-sandbox concurrency.",
                "inputSchema": {
                    "type": "object",
                    "required": ["path", "owner"],
                    "properties": {
                        "path": { "type": "string" },
                        "owner": { "type": "string" },
                        "ttlSecs": { "type": "integer", "default": 120 }
                    }
                }
            }),
            json!({
                "name": "shared_unlock",
                "description": "Release a path-level lease lock on shared://.",
                "inputSchema": {
                    "type": "object",
                    "required": ["path", "owner"],
                    "properties": {
                        "path": { "type": "string" },
                        "owner": { "type": "string" }
                    }
                }
            }),
            json!({
                "name": "state_cas",
                "description": "Compare-And-Swap atomic update on state:// coordination keys.",
                "inputSchema": {
                    "type": "object",
                    "required": ["key", "value"],
                    "properties": {
                        "key": { "type": "string" },
                        "value": {},
                        "expectedVersion": { "type": "integer" },
                        "expectedEtag": { "type": "string" }
                    }
                }
            }),
            json!({
                "name": "state_lease",
                "description": "Acquire or release a distributed fenced lease lock in state://.",
                "inputSchema": {
                    "type": "object",
                    "required": ["lockKey", "owner"],
                    "properties": {
                        "lockKey": { "type": "string" },
                        "owner": { "type": "string" },
                        "ttlSecs": { "type": "integer", "default": 60 },
                        "release": { "type": "boolean", "default": false }
                    }
                }
            }),
        ]);
    }

    if !readonly && !sandbox_worker {
        tools.extend([
            json!({
                "name": "ws_snapshot",
                "description": "Create an immutable snapshot of ws:// files archived via OpenDAL.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "workspacePrefix": { "type": "string", "default": "" },
                        "label": { "type": "string", "default": "checkpoint" }
                    }
                }
            }),
            json!({
                "name": "ws_diff",
                "description": "Diff two ws:// snapshots or a snapshot against live workspace files.",
                "inputSchema": {
                    "type": "object",
                    "required": ["fromSnapshot"],
                    "properties": {
                        "fromSnapshot": { "type": "string" },
                        "toTarget": { "type": "string" }
                    }
                }
            }),
            json!({
                "name": "ws_rollback",
                "description": "Rollback ws:// files to a prior snapshot.",
                "inputSchema": {
                    "type": "object",
                    "required": ["snapshotId"],
                    "properties": {
                        "snapshotId": { "type": "string" },
                        "workspacePrefix": { "type": "string", "default": "" }
                    }
                }
            }),
            json!({
                "name": "ws_fork",
                "description": "Fork a ws:// snapshot into a new isolated workspace prefix.",
                "inputSchema": {
                    "type": "object",
                    "required": ["snapshotId", "targetPrefix"],
                    "properties": {
                        "snapshotId": { "type": "string" },
                        "targetPrefix": { "type": "string" }
                    }
                }
            }),
            json!({
                "name": "mem_commit",
                "description": "Commit a scoped memory entry to mem:// with optional TTL and vector embedding.",
                "inputSchema": {
                    "type": "object",
                    "required": ["scope", "title", "content"],
                    "properties": {
                        "scope": { "type": "string" },
                        "key": { "type": "string" },
                        "title": { "type": "string" },
                        "content": { "type": "string" },
                        "tags": { "type": "array", "items": { "type": "string" } },
                        "importance": { "type": "number", "default": 1.0 },
                        "ttlDays": { "type": "integer" },
                        "embedding": { "type": "array", "items": { "type": "number" } }
                    }
                }
            }),
            json!({
                "name": "mem_forget",
                "description": "Delete a memory entry by memId or mem:// URI.",
                "inputSchema": {
                    "type": "object",
                    "required": ["memIdOrUri"],
                    "properties": {
                        "memIdOrUri": { "type": "string" }
                    }
                }
            }),
            json!({
                "name": "kb_ingest",
                "description": "Ingest and index a document into kb:// (pipeline indexing write).",
                "inputSchema": {
                    "type": "object",
                    "required": ["path", "title", "content"],
                    "properties": {
                        "path": { "type": "string" },
                        "title": { "type": "string" },
                        "content": { "type": "string" },
                        "metadata": { "type": "object" },
                        "embedding": { "type": "array", "items": { "type": "number" } }
                    }
                }
            }),
            json!({
                "name": "profile_reload",
                "description": "Reload ProjectProfile from disk if changed.",
                "inputSchema": { "type": "object", "properties": {} }
            }),
        ]);
    }

    tools
}

fn decode_content(content: &str, encoding: Option<&str>) -> Result<Bytes> {
    match encoding.unwrap_or("utf8") {
        "base64" => {
            let raw = base64::engine::general_purpose::STANDARD.decode(content)?;
            Ok(Bytes::from(raw))
        }
        _ => Ok(Bytes::from(content.as_bytes().to_vec())),
    }
}

pub async fn handle_mcp_request(state: &AppState, req: Value) -> Option<Value> {
    state.request_counter.fetch_add(1, Ordering::Relaxed);

    let id = req.get("id").cloned();
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or_else(|| json!({}));

    if id.is_none() && method.starts_with("notifications/") {
        return None;
    }

    let req_id = id.unwrap_or(Value::Null);
    let router = state.current_router().await;

    let response = match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(DEFAULT_PROTOCOL_VERSION);
            let negotiated = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
                requested
            } else {
                DEFAULT_PROTOCOL_VERSION
            };
            jsonrpc_ok(
                req_id,
                json!({
                    "protocolVersion": negotiated,
                    "capabilities": {
                        "tools": { "listChanged": true },
                        "resources": { "subscribe": false, "listChanged": true },
                        "prompts": { "listChanged": true }
                    },
                    "serverInfo": {
                        "name": "pits-ai-store",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
        }
        "ping" => jsonrpc_ok(req_id, json!({})),
        "tools/list" => {
            let preset = &router.profile.spec.exposure.tools_preset;
            jsonrpc_ok(req_id, json!({ "tools": all_tool_definitions(preset) }))
        }
        "resources/list" => jsonrpc_ok(
            req_id,
            json!({
                "resources": [
                    {
                        "uri": "profile://active",
                        "name": "Active ProjectProfile (Redacted)",
                        "description": "Current declarative storage profile, backends, archetypes, and Policy Guard rules.",
                        "mimeType": "application/json"
                    },
                    {
                        "uri": "mounts://tree",
                        "name": "Semantic VFS Mount Tree",
                        "description": "Live mount table mapping AI Storage URI schemes to OpenDAL backends and modes.",
                        "mimeType": "application/json"
                    },
                    {
                        "uri": "ws://snapshots",
                        "name": "Workspace Snapshots",
                        "description": "Available ws:// snapshots recorded in the workspace index.",
                        "mimeType": "application/json"
                    }
                ]
            }),
        ),
        "resources/read" => {
            let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");
            let res = match uri {
                "profile://active" => {
                    let digest = state.profile_digest.read().await.clone();
                    let redacted = router.profile.redacted();
                    Ok(json!({
                        "digestSha256": digest,
                        "profile": redacted
                    }))
                }
                "mounts://tree" => Ok(json!({
                    "profileName": router.profile.metadata.name,
                    "mounts": router.mounts_tree()
                })),
                "ws://snapshots" => router
                    .index
                    .list_snapshots(None)
                    .map(|snaps| json!({ "snapshots": snaps })),
                other => Err(anyhow!("unknown MCP resource '{}'", other)),
            };
            match res {
                Ok(payload) => jsonrpc_ok(
                    req_id,
                    json!({
                        "contents": [{
                            "uri": uri,
                            "mimeType": "application/json",
                            "text": serde_json::to_string_pretty(&payload).unwrap_or_default()
                        }]
                    }),
                ),
                Err(e) => jsonrpc_err(req_id, -32602, e.to_string()),
            }
        }
        "prompts/list" => jsonrpc_ok(
            req_id,
            json!({
                "prompts": [{
                    "name": "storage-workflow-guide",
                    "description": "Dynamic Agent guide generated from active ProjectProfile mounts and policies.",
                    "arguments": []
                }]
            }),
        ),
        "prompts/get" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name == "storage-workflow-guide" {
                let mounts = router.mounts_tree();
                let mut guide = String::from(
                    "Use semantic AI Storage URIs rather than raw backend credentials:\n",
                );
                for m in mounts {
                    guide.push_str(&format!(
                        "- `{}` ({:?}, backend `{}` / `{}`): {}\n",
                        m.uri, m.mode, m.backend, m.backend_scheme, m.archetype
                    ));
                }
                guide.push_str(
                    "\nBest practices:\n\
                     1. Search `kb://` with `vfs_grep` / `vfs_glob` / `kb_search` before full reads.\n\
                     2. Checkpoint mutable `ws://` work via `ws_snapshot` before risky edits.\n\
                     3. Publish immutable outputs to `artifacts://` via `artifact_publish` with `runId` & `agentId`.\n\
                     4. Acquire `shared_lock` before concurrent cross-sandbox writes to `shared://`.\n",
                );
                jsonrpc_ok(
                    req_id,
                    json!({
                        "description": "AI Storage workflow contract for current ProjectProfile",
                        "messages": [{
                            "role": "user",
                            "content": { "type": "text", "text": guide }
                        }]
                    }),
                )
            } else {
                jsonrpc_err(req_id, -32602, format!("unknown prompt '{}'", name))
            }
        }
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let allowed: HashSet<String> = all_tool_definitions(&router.profile.spec.exposure.tools_preset)
                .into_iter()
                .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect();
            if !allowed.contains(name) {
                state.error_counter.fetch_add(1, Ordering::Relaxed);
                jsonrpc_ok(
                    req_id,
                    tool_err(format!(
                        "tool '{}' is not enabled in toolsPreset '{}'",
                        name, router.profile.spec.exposure.tools_preset
                    )),
                )
            } else {
                let call_res = execute_tool(state, &router, name, &args).await;
                if call_res
                    .get("isError")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    state.error_counter.fetch_add(1, Ordering::Relaxed);
                }
                jsonrpc_ok(req_id, call_res)
            }
        }
        other => jsonrpc_err(req_id, -32601, format!("method not found: {}", other)),
    };

    Some(response)
}

async fn execute_tool(state: &AppState, router: &VfsRouter, name: &str, args: &Value) -> Value {
    let agent_id = args
        .get("agentId")
        .and_then(|v| v.as_str())
        .unwrap_or("mcp-client");
    let res: Result<Value> = match name {
        "vfs_stat" => {
            let uri = req_str(args, "uri");
            match uri {
                Ok(u) => router.stat(u, agent_id).await.and_then(|s| Ok(serde_json::to_value(s)?)),
                Err(e) => Err(e),
            }
        }
        "vfs_list" => {
            let uri = req_str(args, "uri");
            match uri {
                Ok(u) => router.list(u, agent_id).await.map(|entries| json!({ "entries": entries })),
                Err(e) => Err(e),
            }
        }
        "vfs_read" => {
            let uri = req_str(args, "uri");
            let offset = args.get("offset").and_then(|v| v.as_u64());
            let limit = args.get("limit").and_then(|v| v.as_u64());
            let enc = args.get("encoding").and_then(|v| v.as_str()).unwrap_or("utf8");
            match uri {
                Ok(u) => router.read(u, offset, limit, agent_id).await.map(|(bytes, cache_hit)| {
                    if enc == "base64" {
                        json!({
                            "uri": u,
                            "encoding": "base64",
                            "bytes": bytes.len(),
                            "cacheHit": cache_hit,
                            "content": base64::engine::general_purpose::STANDARD.encode(&bytes)
                        })
                    } else {
                        json!({
                            "uri": u,
                            "encoding": "utf8",
                            "bytes": bytes.len(),
                            "cacheHit": cache_hit,
                            "content": String::from_utf8_lossy(&bytes).to_string()
                        })
                    }
                }),
                Err(e) => Err(e),
            }
        }
        "vfs_write" => {
            let uri = req_str(args, "uri");
            let content = req_str(args, "content");
            let enc = args.get("encoding").and_then(|v| v.as_str());
            let run_id = args.get("runId").and_then(|v| v.as_str());
            match (uri, content) {
                (Ok(u), Ok(c)) => match decode_content(c, enc) {
                    Ok(bytes) => router.write(u, bytes, agent_id, run_id).await,
                    Err(e) => Err(e),
                },
                (Err(e), _) | (_, Err(e)) => Err(e),
            }
        }
        "vfs_delete" => match req_str(args, "uri") {
            Ok(u) => router.delete(u, agent_id).await.map(|_| json!({ "deleted": u })),
            Err(e) => Err(e),
        },
        "vfs_glob" => match (req_str(args, "uri"), req_str(args, "pattern")) {
            (Ok(u), Ok(pat)) => {
                let lim = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
                router.glob(u, pat, lim, agent_id).await.map(|m| json!({ "matches": m }))
            }
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
        "vfs_grep" => match (req_str(args, "uri"), req_str(args, "pattern")) {
            (Ok(u), Ok(pat)) => {
                let lim = args.get("maxMatches").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
                router.grep(u, pat, lim, agent_id).await.map(|m| json!({ "matches": m }))
            }
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
        "ws_snapshot" => {
            let prefix = args.get("workspacePrefix").and_then(|v| v.as_str()).unwrap_or("");
            let label = args.get("label").and_then(|v| v.as_str()).unwrap_or("checkpoint");
            match &router.workspace {
                Some(ws) => ws.snapshot(prefix, label).await.and_then(|m| Ok(serde_json::to_value(m)?)),
                None => Err(anyhow!("workspace archetype disabled")),
            }
        }
        "ws_diff" => match req_str(args, "fromSnapshot") {
            Ok(from) => {
                let to = args.get("toTarget").and_then(|v| v.as_str());
                match &router.workspace {
                    Some(ws) => ws.diff(from, to).await.and_then(|d| Ok(serde_json::to_value(d)?)),
                    None => Err(anyhow!("workspace archetype disabled")),
                }
            }
            Err(e) => Err(e),
        },
        "ws_rollback" => match req_str(args, "snapshotId") {
            Ok(snap) => {
                let prefix = args.get("workspacePrefix").and_then(|v| v.as_str()).unwrap_or("");
                match &router.workspace {
                    Some(ws) => ws.rollback(snap, prefix).await.map(|n| json!({ "snapshotId": snap, "restoredFiles": n })),
                    None => Err(anyhow!("workspace archetype disabled")),
                }
            }
            Err(e) => Err(e),
        },
        "ws_fork" => match (req_str(args, "snapshotId"), req_str(args, "targetPrefix")) {
            (Ok(snap), Ok(target)) => match &router.workspace {
                Some(ws) => ws.fork(snap, target).await.map(|n| json!({ "snapshotId": snap, "targetPrefix": target, "copiedFiles": n })),
                None => Err(anyhow!("workspace archetype disabled")),
            },
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
        "mem_commit" => match (req_str(args, "scope"), req_str(args, "title"), req_str(args, "content")) {
            (Ok(scope), Ok(title), Ok(content)) => {
                let key = args.get("key").and_then(|v| v.as_str());
                let tags: Vec<String> = args
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                let importance = args.get("importance").and_then(|v| v.as_f64());
                let ttl_days = args.get("ttlDays").and_then(|v| v.as_u64());
                let emb = parse_embedding(args.get("embedding"));
                match &router.memory {
                    Some(mem) => mem
                        .commit(scope, key, title, content, tags, importance, ttl_days, emb)
                        .await
                        .and_then(|r| Ok(serde_json::to_value(r)?)),
                    None => Err(anyhow!("memory archetype disabled")),
                }
            }
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => Err(e),
        },
        "mem_recall" => match req_str(args, "query") {
            Ok(query) => {
                let scope = args.get("scope").and_then(|v| v.as_str());
                let top_k = args.get("topK").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
                let emb = parse_embedding(args.get("embedding"));
                match &router.memory {
                    Some(mem) => mem
                        .recall(scope, query, emb.as_deref(), top_k)
                        .map(|hits| json!({ "hits": hits })),
                    None => Err(anyhow!("memory archetype disabled")),
                }
            }
            Err(e) => Err(e),
        },
        "mem_forget" => match req_str(args, "memIdOrUri") {
            Ok(id) => match &router.memory {
                Some(mem) => mem.forget(id).await.map(|_| json!({ "deleted": id })),
                None => Err(anyhow!("memory archetype disabled")),
            },
            Err(e) => Err(e),
        },
        "kb_ingest" => match (req_str(args, "path"), req_str(args, "title"), req_str(args, "content")) {
            (Ok(p), Ok(t), Ok(c)) => {
                let meta = args.get("metadata").cloned().unwrap_or_else(|| json!({}));
                let emb = parse_embedding(args.get("embedding"));
                match &router.knowledge {
                    Some(kb) => kb.ingest(p, t, c, meta, emb).await.and_then(|r| Ok(serde_json::to_value(r)?)),
                    None => Err(anyhow!("knowledge archetype disabled")),
                }
            }
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => Err(e),
        },
        "kb_search" => match req_str(args, "query") {
            Ok(q) => {
                let top_k = args.get("topK").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
                let emb = parse_embedding(args.get("embedding"));
                match &router.knowledge {
                    Some(kb) => kb.search(q, emb.as_deref(), top_k).map(|hits| json!({ "hits": hits })),
                    None => Err(anyhow!("knowledge archetype disabled")),
                }
            }
            Err(e) => Err(e),
        },
        "data_cache_status" => {
            let scheme = args.get("scheme").and_then(|v| v.as_str()).unwrap_or("data");
            let eng = if scheme == "models" {
                router.models.as_ref().or(router.datasets.as_ref())
            } else {
                router.datasets.as_ref()
            };
            match eng {
                Some(d) => d.cache_status().and_then(|s| Ok(serde_json::to_value(s)?)),
                None => Err(anyhow!("datasets/models archetype disabled")),
            }
        }
        "artifact_publish" => match (
            req_str(args, "path"),
            req_str(args, "content"),
            req_str(args, "runId"),
            req_str(args, "agentId"),
        ) {
            (Ok(p), Ok(c), Ok(run_id), Ok(ag)) => {
                let enc = args.get("encoding").and_then(|v| v.as_str());
                let source_ws = args.get("sourceWs").and_then(|v| v.as_str()).map(str::to_string);
                let prompt_hash = args.get("promptHash").and_then(|v| v.as_str()).map(str::to_string);
                let metadata = args.get("metadata").cloned().unwrap_or_else(|| json!({}));
                match (decode_content(c, enc), &router.artifacts) {
                    (Ok(bytes), Some(art)) => art
                        .publish(p, bytes, run_id, ag, source_ws, prompt_hash, metadata)
                        .await
                        .and_then(|r| Ok(serde_json::to_value(r)?)),
                    (Err(e), _) => Err(e),
                    (_, None) => Err(anyhow!("artifacts archetype disabled")),
                }
            }
            (Err(e), _, _, _) | (_, Err(e), _, _) | (_, _, Err(e), _) | (_, _, _, Err(e)) => Err(e),
        },
        "artifact_lineage" => {
            let filter = args.get("filter").and_then(|v| v.as_str());
            match &router.artifacts {
                Some(art) => art.lineage(filter).map(|items| json!({ "artifacts": items })),
                None => Err(anyhow!("artifacts archetype disabled")),
            }
        }
        "traces_emit" => match (req_str(args, "agentId"), req_str(args, "eventType")) {
            (Ok(ag), Ok(ev_type)) => match &router.traces {
                Some(tr) => {
                    let ev = TraceEvent {
                        event_id: Uuid::new_v4().to_string(),
                        timestamp: Utc::now().to_rfc3339(),
                        agent_id: ag.to_string(),
                        event_type: ev_type.to_string(),
                        scheme: args.get("scheme").and_then(|v| v.as_str()).unwrap_or("traces").to_string(),
                        uri: args.get("uri").and_then(|v| v.as_str()).unwrap_or("traces://manual").to_string(),
                        status: args.get("status").and_then(|v| v.as_str()).unwrap_or("ok").to_string(),
                        duration_ms: args.get("durationMs").and_then(|v| v.as_u64()).unwrap_or(0),
                        detail: args.get("detail").cloned().unwrap_or_else(|| json!({})),
                    };
                    let id = ev.event_id.clone();
                    let need_flush = tr.record(ev) || args.get("flush").and_then(|v| v.as_bool()).unwrap_or(false);
                    if need_flush {
                        tr.flush().await.map(|flushed_uri| json!({ "eventId": id, "flushedUri": flushed_uri }))
                    } else {
                        Ok(json!({ "eventId": id, "flushedUri": Value::Null }))
                    }
                }
                None => Err(anyhow!("traces archetype disabled")),
            },
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
        "traces_query" => match &router.traces {
            Some(tr) => {
                let ag = args.get("agentId").and_then(|v| v.as_str());
                let et = args.get("eventType").and_then(|v| v.as_str());
                let sc = args.get("scheme").and_then(|v| v.as_str());
                let lim = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
                tr.query(ag, et, sc, lim).await.map(|events| json!({ "events": events }))
            }
            None => Err(anyhow!("traces archetype disabled")),
        },
        "state_cas" => match (req_str(args, "key"), args.get("value")) {
            (Ok(k), Some(v)) => {
                let exp_v = args.get("expectedVersion").and_then(|x| x.as_i64());
                let exp_e = args.get("expectedEtag").and_then(|x| x.as_str());
                match &router.state {
                    Some(st) => st.cas(k, v, exp_v, exp_e).await.and_then(|r| Ok(serde_json::to_value(r)?)),
                    None => Err(anyhow!("state archetype disabled")),
                }
            }
            (Err(e), _) => Err(e),
            (_, None) => Err(anyhow!("missing required field 'value'")),
        },
        "state_lease" => match (req_str(args, "lockKey"), req_str(args, "owner")) {
            (Ok(lk), Ok(owner)) => {
                let release = args.get("release").and_then(|v| v.as_bool()).unwrap_or(false);
                let ttl = args.get("ttlSecs").and_then(|v| v.as_i64()).unwrap_or(60);
                match &router.state {
                    Some(st) => {
                        if release {
                            st.release_lease(lk, owner).await.map(|rel| json!({ "released": rel }))
                        } else {
                            st.acquire_lease(lk, owner, ttl).await.and_then(|r| Ok(serde_json::to_value(r)?))
                        }
                    }
                    None => Err(anyhow!("state archetype disabled")),
                }
            }
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
        "shared_lock" => match (req_str(args, "path"), req_str(args, "owner")) {
            (Ok(p), Ok(owner)) => {
                let ttl = args.get("ttlSecs").and_then(|v| v.as_i64()).unwrap_or(120);
                match &router.shared {
                    Some(sh) => sh.lock(p, owner, ttl).await.and_then(|r| Ok(serde_json::to_value(r)?)),
                    None => Err(anyhow!("sharedDrive archetype disabled")),
                }
            }
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
        "shared_unlock" => match (req_str(args, "path"), req_str(args, "owner")) {
            (Ok(p), Ok(owner)) => match &router.shared {
                Some(sh) => sh.unlock(p, owner).await.map(|rel| json!({ "released": rel })),
                None => Err(anyhow!("sharedDrive archetype disabled")),
            },
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
        "profile_reload" => match state.reload_if_changed().await {
            Ok(reloaded) => Ok(json!({ "reloaded": reloaded })),
            Err(e) => Err(e),
        },
        other => Err(anyhow!("unknown tool '{}'", other)),
    };

    match res {
        Ok(val) => tool_ok(val),
        Err(e) => tool_err(e.to_string()),
    }
}

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing required string argument '{}'", key))
}

fn parse_embedding(v: Option<&Value>) -> Option<Vec<f32>> {
    v.and_then(|arr| arr.as_array()).map(|items| {
        items
            .iter()
            .filter_map(|x| x.as_f64().map(|f| f as f32))
            .collect()
    })
}

// ---------------------------------------------------------------------------
// Streamable HTTP Transport (`/mcp`, `/healthz`, `/readyz`, `/metrics`)
// ---------------------------------------------------------------------------

pub fn build_http_router(state: Arc<AppState>, mcp_path: &str) -> Router {
    let path = if mcp_path.starts_with('/') {
        mcp_path.to_string()
    } else {
        format!("/{}", mcp_path)
    };
    Router::new()
        .route("/healthz", get(healthz_handler))
        .route("/readyz", get(readyz_handler))
        .route("/metrics", get(metrics_handler))
        .route(
            &path,
            post(mcp_post_handler)
                .get(mcp_get_handler)
                .delete(mcp_delete_handler),
        )
        .with_state(state)
}

pub async fn run_http_server(
    state: Arc<AppState>,
    host: &str,
    port: u16,
    mcp_path: &str,
) -> Result<()> {
    let addr: SocketAddr = format!("{}:{}", host, port)
        .parse()
        .with_context(|| format!("invalid bind address {}:{}", host, port))?;

    // Background trace flusher & profile hot-reload watcher
    let bg_state = state.clone();
    tokio::spawn(async move {
        let mut tick = 0u64;
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            tick += 1;
            if let Err(e) = bg_state.reload_if_changed().await {
                warn!("profile hot-reload error: {}", e);
            }
            let r = bg_state.current_router().await;
            if let Some(tr) = &r.traces {
                let every_ticks = (tr.flush_interval_secs() / 5).max(1);
                if tick % every_ticks == 0 {
                    if let Err(e) = tr.flush().await {
                        warn!("traces background flush error: {}", e);
                    }
                }
            }
        }
    });

    let app = build_http_router(state, mcp_path);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("ai-store MCP Streamable HTTP listening on http://{}{}", addr, mcp_path);
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

fn check_bearer_auth(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(expected) = &state.bearer_token else {
        return true;
    };
    let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    auth.strip_prefix("Bearer ")
        .map(|tok| tok.trim() == expected)
        .unwrap_or(false)
}

async fn healthz_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let router = state.current_router().await;
    Json(json!({
        "status": "ok",
        "profile": router.profile.metadata.name,
        "backends": router.registry.list_names().len(),
        "mounts": router.mounts_tree().len(),
    }))
}

async fn readyz_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let router = state.current_router().await;
    let mounts = router.mounts_tree();
    Json(json!({
        "ready": !mounts.is_empty(),
        "sqliteVersion": router.index.sqlite.version,
        "postgresConnected": router.index.postgres.is_some(),
        "redisConnected": router.index.redis.is_some(),
        "mounts": mounts.len()
    }))
}

async fn metrics_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let reqs = state.request_counter.load(Ordering::Relaxed);
    let errs = state.error_counter.load(Ordering::Relaxed);
    let router = state.current_router().await;
    let body = format!(
        "# HELP ai_store_mcp_requests_total Total MCP JSON-RPC requests\n\
         # TYPE ai_store_mcp_requests_total counter\n\
         ai_store_mcp_requests_total {}\n\
         # HELP ai_store_mcp_errors_total Total MCP tool errors\n\
         # TYPE ai_store_mcp_errors_total counter\n\
         ai_store_mcp_errors_total {}\n\
         # HELP ai_store_mounts_active Active AI Storage archetype mounts\n\
         # TYPE ai_store_mounts_active gauge\n\
         ai_store_mounts_active {}\n",
        reqs,
        errs,
        router.mounts_tree().len()
    );
    (StatusCode::OK, [("content-type", "text/plain; version=0.0.4")], body)
}

async fn mcp_post_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !check_bearer_auth(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "missing or invalid Bearer token" })),
        )
            .into_response();
    }

    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(jsonrpc_err(Value::Null, -32700, format!("parse error: {}", e))),
            )
                .into_response()
        }
    };

    let incoming_sid = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let is_init = payload
        .get("method")
        .and_then(|v| v.as_str())
        .map(|m| m == "initialize")
        .unwrap_or(false);

    let session_id = if is_init || incoming_sid.is_none() {
        let sid = Uuid::new_v4().to_string();
        state.sessions.write().await.insert(sid.clone());
        sid
    } else {
        incoming_sid.unwrap_or_default()
    };

    match handle_mcp_request(&state, payload).await {
        Some(resp) => (
            StatusCode::OK,
            [
                ("content-type", "application/json"),
                ("mcp-session-id", session_id.as_str()),
            ],
            serde_json::to_string(&resp).unwrap_or_default(),
        )
            .into_response(),
        None => (
            StatusCode::ACCEPTED,
            [("mcp-session-id", session_id.as_str())],
            "",
        )
            .into_response(),
    }
}

async fn mcp_get_handler(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !check_bearer_auth(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({
        "transport": "streamable-http",
        "protocolVersions": SUPPORTED_PROTOCOL_VERSIONS,
        "status": "ready"
    }))
    .into_response()
}

async fn mcp_delete_handler(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !check_bearer_auth(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if let Some(sid) = headers.get("mcp-session-id").and_then(|v| v.to_str().ok()) {
        state.sessions.write().await.remove(sid);
    }
    StatusCode::NO_CONTENT.into_response()
}

// ---------------------------------------------------------------------------
// Singleton Daemon + Thin Stdio Proxy (`ai-store mcp`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonRecord {
    pub pid: u32,
    pub host: String,
    pub port: u16,
    pub url: String,
    pub profile_path: String,
    pub started_at: String,
}

pub fn daemon_state_file(daemon_dir: Option<&Path>) -> PathBuf {
    daemon_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(default_daemon_dir)
        .join("daemon.json")
}

pub async fn ensure_singleton_daemon(
    profile_path: Option<&Path>,
    daemon_dir: Option<&Path>,
) -> Result<DaemonRecord> {
    let (profile, resolved_profile, _) = ProjectProfile::load_or_default(profile_path)?;
    let state_file = daemon_state_file(daemon_dir);
    if let Some(parent) = state_file.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if state_file.exists() {
        if let Ok(raw) = std::fs::read_to_string(&state_file) {
            if let Ok(rec) = serde_json::from_str::<DaemonRecord>(&raw) {
                if probe_healthz(&rec.host, rec.port).await {
                    return Ok(rec);
                }
            }
        }
    }

    let host = profile.spec.exposure.host.clone();
    let mut port = profile.spec.exposure.port;
    if probe_healthz(&host, port).await {
        let rec = DaemonRecord {
            pid: 0,
            host: host.clone(),
            port,
            url: format!("http://{}:{}{}", host, port, profile.spec.exposure.path),
            profile_path: resolved_profile.to_string_lossy().to_string(),
            started_at: Utc::now().to_rfc3339(),
        };
        let _ = std::fs::write(&state_file, serde_json::to_string_pretty(&rec)?);
        return Ok(rec);
    }

    // If configured port is occupied by something else, pick an ephemeral free port
    if tokio::net::TcpListener::bind(format!("{}:{}", host, port))
        .await
        .is_err()
    {
        if let Ok(l) = tokio::net::TcpListener::bind(format!("{}:0", host)).await {
            port = l.local_addr()?.port();
        }
    }

    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("serve")
        .arg("--profile")
        .arg(&resolved_profile)
        .arg("--host")
        .arg(&host)
        .arg("--port")
        .arg(port.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    let child = cmd.spawn().context("failed to spawn detached ai-store daemon")?;
    let pid = child.id();

    for _ in 0..40 {
        if probe_healthz(&host, port).await {
            let rec = DaemonRecord {
                pid,
                host: host.clone(),
                port,
                url: format!("http://{}:{}{}", host, port, profile.spec.exposure.path),
                profile_path: resolved_profile.to_string_lossy().to_string(),
                started_at: Utc::now().to_rfc3339(),
            };
            std::fs::write(&state_file, serde_json::to_string_pretty(&rec)?)?;
            return Ok(rec);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    Err(anyhow!("timed out waiting for ai-store daemon on {}:{}", host, port))
}

async fn probe_healthz(host: &str, port: u16) -> bool {
    let url = format!("http://{}:{}/healthz", host, port);
    http_request_json("GET", &url, None, None).await.is_ok()
}

async fn http_request_json(
    method: &str,
    url_str: &str,
    body: Option<Vec<u8>>,
    session_id: Option<&str>,
) -> Result<(u16, Option<String>, Vec<u8>)> {
    use http_body_util::{BodyExt, Full};
    use hyper_util::rt::TokioIo;

    let parsed = url::Url::parse(url_str)?;
    let host = parsed.host_str().ok_or_else(|| anyhow!("missing host"))?;
    let port = parsed.port_or_known_default().unwrap_or(80);
    let stream = tokio::net::TcpStream::connect((host, port)).await?;
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let path_q = match parsed.query() {
        Some(q) => format!("{}?{}", parsed.path(), q),
        None => parsed.path().to_string(),
    };
    let mut builder = hyper::Request::builder()
        .method(method)
        .uri(path_q)
        .header("host", format!("{}:{}", host, port))
        .header("content-type", "application/json");
    if let Some(sid) = session_id {
        builder = builder.header("mcp-session-id", sid);
    }
    if let Ok(tok) = std::env::var("AI_STORE_BEARER_TOKEN") {
        if !tok.is_empty() {
            builder = builder.header("authorization", format!("Bearer {}", tok));
        }
    }
    let req_body = Full::new(Bytes::from(body.unwrap_or_default()));
    let req = builder.body(req_body)?;
    let resp = sender.send_request(req).await?;
    let status = resp.status().as_u16();
    let sid = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let collected = resp.into_body().collect().await?.to_bytes().to_vec();
    if status >= 400 {
        return Err(anyhow!(
            "HTTP {} from {}: {}",
            status,
            url_str,
            String::from_utf8_lossy(&collected)
        ));
    }
    Ok((status, sid, collected))
}

#[derive(Clone, Copy)]
enum FramingMode {
    Newline,
    ContentLength,
}

pub async fn run_stdio_proxy(
    profile_path: Option<&Path>,
    daemon_dir: Option<&Path>,
    no_daemon: bool,
) -> Result<()> {
    let direct_state = if no_daemon {
        Some(Arc::new(AppState::initialize(profile_path).await?))
    } else {
        None
    };
    let daemon = if no_daemon {
        None
    } else {
        Some(ensure_singleton_daemon(profile_path, daemon_dir).await?)
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel::<(FramingMode, Value)>(32);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if trimmed.to_ascii_lowercase().starts_with("content-length:") {
                        let len = trimmed
                            .split_once(':')
                            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        loop {
                            let mut hdr = String::new();
                            if reader.read_line(&mut hdr).unwrap_or(0) == 0 || hdr.trim().is_empty() {
                                break;
                            }
                        }
                        let mut buf = vec![0u8; len];
                        if reader.read_exact(&mut buf).is_err() {
                            break;
                        }
                        if let Ok(val) = serde_json::from_slice::<Value>(&buf) {
                            if tx.blocking_send((FramingMode::ContentLength, val)).is_err() {
                                break;
                            }
                        }
                    } else if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
                        if tx.blocking_send((FramingMode::Newline, val)).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });

    let mut session_id: Option<String> = None;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    while let Some((mode, req)) = rx.recv().await {
        let resp_opt = if let Some(st) = &direct_state {
            handle_mcp_request(st, req).await
        } else if let Some(d) = &daemon {
            let body = serde_json::to_vec(&req)?;
            let (status, new_sid, resp_bytes) =
                http_request_json("POST", &d.url, Some(body), session_id.as_deref()).await?;
            if let Some(s) = new_sid {
                session_id = Some(s);
            }
            if status == 202 || resp_bytes.is_empty() {
                None
            } else {
                Some(serde_json::from_slice::<Value>(&resp_bytes)?)
            }
        } else {
            None
        };

        if let Some(resp) = resp_opt {
            let text = serde_json::to_string(&resp)?;
            match mode {
                FramingMode::Newline => {
                    writeln!(out, "{}", text)?;
                }
                FramingMode::ContentLength => {
                    write!(out, "Content-Length: {}\r\n\r\n{}", text.len(), text)?;
                }
            }
            out.flush()?;
        }
    }

    Ok(())
}
