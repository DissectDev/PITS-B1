//! Model Context Protocol (MCP 2025-03-26) Server for PITS Self-Managed Project Hub.
//!
//! Exposes exactly 9 high-semantic tools over:
//! - Streamable HTTP (`POST/GET/DELETE /mcp/hub`)
//! - Stdio (`pitsctl hub mcp`)
//!
//! Security & Protocol-Level Acceptance Hard Gate:
//! - NEVER exposes `accept` or `mark_done` tools.
//! - Rejects any attempt to set `status = "done"`, `accepted = true`, or `humanVerified = true`
//!   with HTTP 403 / JSON-RPC -32003 (`FORBIDDEN: Human acceptance required via PITS Portal`).

use anyhow::{Context, Result};
use base64::Engine as _;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};

use crate::hub::model::{
    task_portal_url, task_tailscale_url, CreateSessionLogRequest, CreateTaskRequest, HubError,
    PickupTaskRequest, SubmitHandoffRequest, TaskQueryFilter, UpsertProjectRequest,
    FORBIDDEN_HUMAN_ACCEPTANCE_MSG,
};
use crate::hub::HubRegistry;

const SUPPORTED_PROTOCOL_VERSIONS: [&str; 2] = ["2025-03-26", "2024-11-05"];
const DEFAULT_PROTOCOL_VERSION: &str = "2025-03-26";

#[derive(Debug, Clone)]
pub struct HubMcpReply {
    pub http_status: u16,
    pub body: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct HubMcpAuthContext {
    pub actor: String,
    pub auth_mode: String,
}

fn jsonrpc_ok(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn jsonrpc_err(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message.into(),
        },
    })
}

fn tool_ok(payload: Value) -> Value {
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": payload,
        "isError": false,
    })
}

fn tool_err(message: impl Into<String>) -> Value {
    let msg = message.into();
    json!({
        "content": [{ "type": "text", "text": msg }],
        "structuredContent": { "error": msg },
        "isError": true,
    })
}

/// Decodes unverified JWT payload claims (signature + JWKS already enforced upstream by Istio Envoy
/// `RequestAuthentication` & `AuthorizationPolicy` on `portal.lab.martensite.dev/mcp/hub`).
pub fn decode_jwt_payload_claims(token: &str) -> Option<Value> {
    let mut parts = token.split('.');
    let _hdr = parts.next()?;
    let payload_b64 = parts.next()?;
    let _sig = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64.as_bytes())
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload_b64.as_bytes()))
        .ok()?;
    serde_json::from_slice::<Value>(&decoded).ok()
}

/// Validates `/mcp/hub` authentication for both endpoints:
/// - Endpoint A (Tailscale direct `http://100.95.95.88:18090/mcp/hub`): optional static `X-PITS-MCP-Token`
///   or `Authorization: Bearer <PITS_HUB_MCP_TOKEN>`.
/// - Endpoint B (Zero-Trust HTTPS `https://portal.lab.martensite.dev/mcp/hub`): Keycloak Service Account
///   Bearer JWT validated upstream by Istio Ambient Gateway (`azp == "pits-project-hub-mcp"`).
pub fn verify_hub_mcp_request_auth(
    configured_static_token: Option<&str>,
    x_pits_mcp_token: Option<&str>,
    authorization_header: Option<&str>,
    host_or_forwarded_host: Option<&str>,
    x_auth_request_user: Option<&str>,
) -> std::result::Result<HubMcpAuthContext, String> {
    let bearer_token = authorization_header.and_then(|h| {
        let trimmed = h.trim();
        trimmed
            .strip_prefix("Bearer ")
            .or_else(|| trimmed.strip_prefix("bearer "))
            .map(str::trim)
            .filter(|s| !s.is_empty())
    });

    // 1. Check Keycloak JWT forwarded by Istio Gateway (`portal.lab.martensite.dev`)
    if let Some(bt) = bearer_token {
        if let Some(claims) = decode_jwt_payload_claims(bt) {
            let iss = claims.get("iss").and_then(|v| v.as_str()).unwrap_or("");
            let azp = claims.get("azp").and_then(|v| v.as_str()).unwrap_or("");
            let sub = claims.get("sub").and_then(|v| v.as_str()).unwrap_or("");
            let pref = claims
                .get("preferred_username")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if iss.contains("/realms/corp")
                && (azp == "pits-project-hub-mcp" || claims.get("acr").and_then(|v| v.as_str()) == Some("fido2"))
            {
                let actor = if !pref.is_empty() {
                    format!("jwt:{pref}")
                } else if !azp.is_empty() && !sub.is_empty() {
                    format!("jwt:{azp}:{sub}")
                } else if !azp.is_empty() {
                    format!("jwt:{azp}")
                } else {
                    "jwt:keycloak".to_string()
                };
                return Ok(HubMcpAuthContext {
                    actor,
                    auth_mode: "keycloak-jwt".to_string(),
                });
            }
        }
    }

    // 2. Check optional static PITS_HUB_MCP_TOKEN (Tailscale direct)
    if let Some(expected) = configured_static_token
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let provided_hdr = x_pits_mcp_token.map(str::trim).unwrap_or("");
        let provided_bearer = bearer_token.unwrap_or("");
        if provided_hdr == expected || provided_bearer == expected {
            return Ok(HubMcpAuthContext {
                actor: "mcp-tailscale-token".to_string(),
                auth_mode: "static-token".to_string(),
            });
        }
        return Err("Unauthorized: valid X-PITS-MCP-Token or Keycloak Bearer JWT required".to_string());
    }

    // 3. No static token configured: allow Tailscale direct or oauth2-proxy authenticated caller
    let actor = x_auth_request_user
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|u| format!("mcp:{u}"))
        .unwrap_or_else(|| "mcp-agent".to_string());
    let auth_mode = if host_or_forwarded_host
        .map(|h| h.contains("portal.lab.martensite.dev"))
        .unwrap_or(false)
    {
        "gateway-passthrough".to_string()
    } else {
        "tailscale-direct".to_string()
    };
    Ok(HubMcpAuthContext { actor, auth_mode })
}

pub fn hub_tool_definitions() -> Value {
    json!([
        {
            "name": "hub_list_projects",
            "description": "List all projects in PITS Self-Managed Project Hub (single top-level PITS project with workspace metadata).",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        },
        {
            "name": "hub_upsert_project",
            "description": "Create or update a project record in PITS Project Hub.",
            "inputSchema": {
                "type": "object",
                "required": ["name"],
                "properties": {
                    "id": { "type": "string" },
                    "name": { "type": "string" },
                    "status": { "type": "string", "enum": ["not_started", "in_progress", "paused", "done"] },
                    "priority": { "type": "string", "enum": ["high", "medium", "low"] },
                    "workspacePath": { "type": "string" },
                    "outcome": { "type": "string" },
                    "targetDate": { "type": "string" },
                    "lead": { "type": "string" },
                    "bodyMd": { "type": "string" },
                    "catalogRefs": { "type": "array", "items": { "type": "string" } }
                }
            }
        },
        {
            "name": "hub_query_tasks",
            "description": "Query AGY-<n> tasks filtered by projectId, status, first-class tags, priority, workspace, or keyword query.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "projectId": { "type": "string" },
                    "status": { "type": "string", "description": "Comma-separated statuses: backlog,ready,in_progress,ready_for_review,blocked,done" },
                    "tag": { "type": "string", "description": "Single tag or comma-separated AND tags (e.g. 'pits-infra,portal')" },
                    "priority": { "type": "string", "enum": ["p0_critical", "p1_high", "p2_medium", "p3_low"] },
                    "workspace": { "type": "string" },
                    "query": { "type": "string" }
                }
            }
        },
        {
            "name": "hub_get_task_context",
            "description": "Fetch full pickup context for an AGY-<n> task in one call: task spec, AC checklist, parent project, linked historical sessions, and last 10 project decision_rfc/blocker_pitfall entries.",
            "inputSchema": {
                "type": "object",
                "required": ["taskKey"],
                "properties": {
                    "taskKey": { "type": "string", "description": "Task key such as AGY-2" }
                }
            }
        },
        {
            "name": "hub_create_task",
            "description": "Create a new task with automatic globally monotonic AGY-<n> key. Initial status via MCP is restricted to 'backlog' or 'ready' (cannot mark 'done').",
            "inputSchema": {
                "type": "object",
                "required": ["title"],
                "properties": {
                    "projectId": { "type": "string", "default": "pits" },
                    "title": { "type": "string" },
                    "status": { "type": "string", "enum": ["backlog", "ready"] },
                    "priority": { "type": "string", "enum": ["p0_critical", "p1_high", "p2_medium", "p3_low"] },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "workspacePath": { "type": "string" },
                    "dispatchTarget": { "type": "string" },
                    "acceptanceSummary": { "type": "string" },
                    "acChecklist": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["text"],
                            "properties": {
                                "id": { "type": "string" },
                                "text": { "type": "string" },
                                "agentVerified": { "type": "boolean" }
                            }
                        }
                    },
                    "owner": { "type": "string" },
                    "dueDate": { "type": "string" },
                    "bodyMd": { "type": "string" },
                    "catalogRefs": { "type": "array", "items": { "type": "string" } }
                }
            }
        },
        {
            "name": "hub_pickup_task",
            "description": "Atomically claim an AGY-<n> task into 'in_progress', recording workspacePath, conversationId, and lastSessionAt.",
            "inputSchema": {
                "type": "object",
                "required": ["taskKey"],
                "properties": {
                    "taskKey": { "type": "string" },
                    "workspacePath": { "type": "string" },
                    "conversationId": { "type": "string" },
                    "actor": { "type": "string" }
                }
            }
        },
        {
            "name": "hub_submit_handoff",
            "description": "Atomically submit a Session Handoff (accepted=false enforced), transition linked AGY-<n> tasks to 'ready_for_review' or 'blocked', mark agentVerified AC items, and return Portal & Tailscale URLs for human acceptance.",
            "inputSchema": {
                "type": "object",
                "required": ["taskKeys", "summary"],
                "properties": {
                    "projectId": { "type": "string", "default": "pits" },
                    "taskKeys": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                    "summary": { "type": "string" },
                    "targetStatus": { "type": "string", "enum": ["ready_for_review", "blocked"], "default": "ready_for_review" },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "workspacePath": { "type": "string" },
                    "conversationId": { "type": "string" },
                    "commitRefs": { "type": "array", "items": { "type": "string" } },
                    "evidenceRefs": { "type": "array", "items": { "type": "string" } },
                    "bodyMd": { "type": "string" },
                    "agentVerifiedAcIds": { "type": "array", "items": { "type": "string" } },
                    "sessionDate": { "type": "string" }
                }
            }
        },
        {
            "name": "hub_log_entry",
            "description": "Record a durable 'decision_rfc' or 'blocker_pitfall' entry linked to taskKeys and first-class tags.",
            "inputSchema": {
                "type": "object",
                "required": ["summary", "logType"],
                "properties": {
                    "projectId": { "type": "string", "default": "pits" },
                    "summary": { "type": "string" },
                    "logType": { "type": "string", "enum": ["decision_rfc", "blocker_pitfall", "session_handoff"] },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "workspacePath": { "type": "string" },
                    "conversationId": { "type": "string" },
                    "commitRefs": { "type": "array", "items": { "type": "string" } },
                    "evidenceRefs": { "type": "array", "items": { "type": "string" } },
                    "bodyMd": { "type": "string" },
                    "taskKeys": { "type": "array", "items": { "type": "string" } },
                    "sessionDate": { "type": "string" }
                }
            }
        },
        {
            "name": "hub_search",
            "description": "Full-text search (FTS5 with automatic LIKE fallback) across tasks, handoffs, decisions, and blockers.",
            "inputSchema": {
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }
                }
            }
        }
    ])
}

async fn dispatch_tool(
    hub: &HubRegistry,
    name: &str,
    args: Value,
    actor: &str,
) -> std::result::Result<Value, (u16, i64, String)> {
    // Hard-block any hypothetical accept/done tool invocation by name
    let lower = name.to_ascii_lowercase();
    if lower.contains("accept") || lower.contains("mark_done") || lower.contains("approve") {
        return Err((403, -32003, FORBIDDEN_HUMAN_ACCEPTANCE_MSG.to_string()));
    }

    match name {
        "hub_list_projects" => match hub.db.list_projects() {
            Ok(projects) => Ok(tool_ok(json!({ "projects": projects }))),
            Err(e) => map_hub_err(e),
        },
        "hub_upsert_project" => {
            let req: UpsertProjectRequest = match serde_json::from_value(args) {
                Ok(r) => r,
                Err(e) => return Ok(tool_err(format!("Invalid hub_upsert_project arguments: {e}"))),
            };
            match hub.db.upsert_project(req, "mcp", actor) {
                Ok(project) => Ok(tool_ok(json!({ "project": project }))),
                Err(e) => map_hub_err(e),
            }
        }
        "hub_query_tasks" => {
            let filter: TaskQueryFilter = match serde_json::from_value(args) {
                Ok(f) => f,
                Err(e) => return Ok(tool_err(format!("Invalid hub_query_tasks arguments: {e}"))),
            };
            match hub.db.query_tasks(&filter) {
                Ok(tasks) => Ok(tool_ok(json!({
                    "totalMatched": tasks.len(),
                    "tasks": tasks,
                }))),
                Err(e) => map_hub_err(e),
            }
        }
        "hub_get_task_context" => {
            let task_key = args
                .get("taskKey")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| (400, -32602, "Missing required 'taskKey'".to_string()))?;
            match hub.db.get_task_context(task_key) {
                Ok(ctx) => Ok(tool_ok(json!(ctx))),
                Err(e) => map_hub_err(e),
            }
        }
        "hub_create_task" => {
            // Catch smuggled `accepted: true` even before deserialization
            if args.get("accepted").and_then(|v| v.as_bool()) == Some(true) {
                return Err((403, -32003, FORBIDDEN_HUMAN_ACCEPTANCE_MSG.to_string()));
            }
            let req: CreateTaskRequest = match serde_json::from_value(args) {
                Ok(r) => r,
                Err(e) => return Ok(tool_err(format!("Invalid hub_create_task arguments: {e}"))),
            };
            match hub.db.create_task(req, "mcp", actor) {
                Ok(task) => {
                    let portal_url = task_portal_url(&task.task_key);
                    let tailscale_url = task_tailscale_url(&task.task_key);
                    Ok(tool_ok(json!({
                        "task": task,
                        "portalUrl": portal_url,
                        "tailscaleUrl": tailscale_url,
                    })))
                }
                Err(e) => map_hub_err(e),
            }
        }
        "hub_pickup_task" => {
            let req: PickupTaskRequest = match serde_json::from_value(args) {
                Ok(r) => r,
                Err(e) => return Ok(tool_err(format!("Invalid hub_pickup_task arguments: {e}"))),
            };
            match hub.db.pickup_task(req, "mcp", actor) {
                Ok(task) => {
                    let portal_url = task_portal_url(&task.task_key);
                    let tailscale_url = task_tailscale_url(&task.task_key);
                    Ok(tool_ok(json!({
                        "task": task,
                        "portalUrl": portal_url,
                        "tailscaleUrl": tailscale_url,
                    })))
                }
                Err(e) => map_hub_err(e),
            }
        }
        "hub_submit_handoff" => {
            let req: SubmitHandoffRequest = match serde_json::from_value(args) {
                Ok(r) => r,
                Err(e) => return Ok(tool_err(format!("Invalid hub_submit_handoff arguments: {e}"))),
            };
            match hub.db.submit_handoff(req, "mcp", actor) {
                Ok((session, updated_tasks)) => {
                    let primary_key = updated_tasks
                        .first()
                        .map(|t| t.task_key.as_str())
                        .unwrap_or("AGY-1");
                    let portal_urls: Vec<String> = updated_tasks
                        .iter()
                        .map(|t| task_portal_url(&t.task_key))
                        .collect();
                    let tailscale_urls: Vec<String> = updated_tasks
                        .iter()
                        .map(|t| task_tailscale_url(&t.task_key))
                        .collect();
                    Ok(tool_ok(json!({
                        "session": session,
                        "updatedTasks": updated_tasks,
                        "portalUrl": task_portal_url(primary_key),
                        "tailscaleUrl": task_tailscale_url(primary_key),
                        "portalUrls": portal_urls,
                        "tailscaleUrls": tailscale_urls,
                    })))
                }
                Err(e) => map_hub_err(e),
            }
        }
        "hub_log_entry" => {
            let req: CreateSessionLogRequest = match serde_json::from_value(args) {
                Ok(r) => r,
                Err(e) => return Ok(tool_err(format!("Invalid hub_log_entry arguments: {e}"))),
            };
            match hub.db.create_session_log(req, "mcp", actor) {
                Ok(session) => Ok(tool_ok(json!({ "session": session }))),
                Err(e) => map_hub_err(e),
            }
        }
        "hub_search" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(20) as usize;
            match hub.db.search(query, limit) {
                Ok(res) => Ok(tool_ok(json!(res))),
                Err(e) => map_hub_err(e),
            }
        }
        other => Err((400, -32601, format!("Unknown hub MCP tool '{other}'"))),
    }
}

fn map_hub_err(err: HubError) -> std::result::Result<Value, (u16, i64, String)> {
    match err {
        HubError::Forbidden(msg) => Err((403, -32003, msg)),
        HubError::NotFound(msg) => Ok(tool_err(msg)),
        HubError::BadRequest(msg) => Ok(tool_err(msg)),
        HubError::Internal(msg) => Ok(tool_err(msg)),
    }
}

pub async fn handle_hub_mcp_jsonrpc(
    hub: &HubRegistry,
    msg: Value,
    actor: &str,
) -> HubMcpReply {
    let Some(obj) = msg.as_object() else {
        return HubMcpReply {
            http_status: 400,
            body: Some(jsonrpc_err(
                Value::Null,
                -32600,
                "Invalid Request: expected JSON object",
            )),
        };
    };

    let id = obj.get("id").cloned();
    let method = match obj.get("method").and_then(|v| v.as_str()) {
        Some(m) => m,
        None => {
            return HubMcpReply {
                http_status: 400,
                body: Some(jsonrpc_err(
                    id.unwrap_or(Value::Null),
                    -32600,
                    "Invalid Request: missing 'method'",
                )),
            };
        }
    };

    // Notifications without `id` return 202 Accepted with no body
    if id.is_none() || method.starts_with("notifications/") {
        return HubMcpReply {
            http_status: 202,
            body: None,
        };
    }

    let req_id = id.unwrap_or(Value::Null);
    let params = obj.get("params").cloned().unwrap_or_else(|| json!({}));

    match method {
        "initialize" => {
            let client_ver = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(DEFAULT_PROTOCOL_VERSION);
            let chosen_ver = if SUPPORTED_PROTOCOL_VERSIONS.contains(&client_ver) {
                client_ver
            } else {
                DEFAULT_PROTOCOL_VERSION
            };
            HubMcpReply {
                http_status: 200,
                body: Some(jsonrpc_ok(
                    req_id,
                    json!({
                        "protocolVersion": chosen_ver,
                        "capabilities": {
                            "tools": { "listChanged": false },
                            "resources": { "subscribe": false, "listChanged": false }
                        },
                        "serverInfo": {
                            "name": "pits-project-hub-mcp",
                            "version": env!("CARGO_PKG_VERSION")
                        },
                        "instructions": "PITS Self-Managed Project Hub MCP. Use hub_query_tasks -> hub_get_task_context -> hub_pickup_task -> hub_submit_handoff. Human acceptance/Done transitions are strictly restricted to PITS Portal (/project-hub)."
                    }),
                )),
            }
        }
        "ping" => HubMcpReply {
            http_status: 200,
            body: Some(jsonrpc_ok(req_id, json!({}))),
        },
        "tools/list" => HubMcpReply {
            http_status: 200,
            body: Some(jsonrpc_ok(req_id, json!({ "tools": hub_tool_definitions() }))),
        },
        "tools/call" => {
            let tool_name = match params.get("name").and_then(|v| v.as_str()) {
                Some(n) if !n.trim().is_empty() => n.trim(),
                _ => {
                    return HubMcpReply {
                        http_status: 400,
                        body: Some(jsonrpc_err(req_id, -32602, "tools/call requires 'name'")),
                    };
                }
            };
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            match dispatch_tool(hub, tool_name, args, actor).await {
                Ok(res) => HubMcpReply {
                    http_status: 200,
                    body: Some(jsonrpc_ok(req_id, res)),
                },
                Err((http_status, code, msg)) => HubMcpReply {
                    http_status,
                    body: Some(jsonrpc_err(req_id, code, msg)),
                },
            }
        }
        "resources/list" => HubMcpReply {
            http_status: 200,
            body: Some(jsonrpc_ok(
                req_id,
                json!({
                    "resources": [
                        {
                            "uri": "hub://overview",
                            "name": "PITS Project Hub Overview & Review Queue",
                            "mimeType": "application/json"
                        }
                    ]
                }),
            )),
        },
        "resources/read" => {
            let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");
            if uri == "hub://overview" {
                match hub.db.overview() {
                    Ok(ov) => HubMcpReply {
                        http_status: 200,
                        body: Some(jsonrpc_ok(
                            req_id,
                            json!({
                                "contents": [{
                                    "uri": uri,
                                    "mimeType": "application/json",
                                    "text": serde_json::to_string_pretty(&ov).unwrap_or_default()
                                }]
                            }),
                        )),
                    },
                    Err(e) => HubMcpReply {
                        http_status: 500,
                        body: Some(jsonrpc_err(req_id, -32603, e.to_string())),
                    },
                }
            } else {
                HubMcpReply {
                    http_status: 404,
                    body: Some(jsonrpc_err(req_id, -32002, format!("Unknown hub resource '{uri}'"))),
                }
            }
        }
        other => HubMcpReply {
            http_status: 400,
            body: Some(jsonrpc_err(req_id, -32601, format!("Method '{other}' not found"))),
        },
    }
}

/// Runs the Project Hub MCP server over Stdio (`pitsctl hub mcp`).
pub async fn run_hub_mcp_stdio(hub: HubRegistry) -> Result<()> {
    let (tx, mut rx) =
        tokio::sync::mpsc::unbounded_channel::<(std::result::Result<Value, String>, bool)>();

    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        loop {
            let mut line = String::new();
            let Ok(n) = reader.read_line(&mut line) else {
                break;
            };
            if n == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.to_ascii_lowercase().starts_with("content-length:") {
                let len_opt = trimmed
                    .split_once(':')
                    .map(|(_, v)| v.trim())
                    .and_then(|v| v.parse::<usize>().ok());
                let Some(len) = len_opt else {
                    let _ = tx.send((Err(format!("Invalid Content-Length: {trimmed}")), true));
                    break;
                };
                loop {
                    let mut hdr = String::new();
                    match reader.read_line(&mut hdr) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if hdr.trim().is_empty() => break,
                        Ok(_) => {}
                    }
                }
                let mut buf = vec![0u8; len];
                if let Err(e) = reader.read_exact(&mut buf) {
                    let _ = tx.send((Err(format!("Read body failed: {e}")), true));
                    break;
                }
                let parsed =
                    serde_json::from_slice::<Value>(&buf).map_err(|e| format!("Parse error: {e}"));
                if tx.send((parsed, true)).is_err() {
                    break;
                }
            } else {
                let parsed =
                    serde_json::from_str::<Value>(trimmed).map_err(|e| format!("Parse error: {e}"));
                if tx.send((parsed, false)).is_err() {
                    break;
                }
            }
        }
    });

    while let Some((msg_res, use_content_length)) = rx.recv().await {
        let reply = match msg_res {
            Ok(val) => handle_hub_mcp_jsonrpc(&hub, val, "mcp-stdio").await,
            Err(e) => HubMcpReply {
                http_status: 400,
                body: Some(jsonrpc_err(Value::Null, -32700, e)),
            },
        };
        if let Some(resp) = reply.body {
            let encoded = serde_json::to_string(&resp).context("serialize hub MCP reply")?;
            let mut out = std::io::stdout().lock();
            if use_content_length {
                write!(out, "Content-Length: {}\r\n\r\n{}", encoded.len(), encoded)
                    .context("write Content-Length hub MCP reply")?;
            } else {
                out.write_all(encoded.as_bytes())
                    .context("write NDJSON hub MCP reply")?;
                out.write_all(b"\n").context("write hub MCP newline")?;
            }
            out.flush().context("flush hub MCP stdout")?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClientProfile;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[tokio::test]
    async fn mcp_exposes_9_tools_and_forbids_accept_or_done_with_403() {
        let profile = Arc::new(RwLock::new(ClientProfile::default()));
        let hub = HubRegistry::new_in_memory(profile);

        // 1. tools/list must contain exactly 9 tools and ZERO accept/done tools
        let list_reply = handle_hub_mcp_jsonrpc(
            &hub,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            "test-agent",
        )
        .await;
        assert_eq!(list_reply.http_status, 200);
        let tools = list_reply.body.unwrap()["result"]["tools"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(tools.len(), 9);
        assert!(tools.iter().all(|t| {
            let n = t["name"].as_str().unwrap_or("");
            !n.contains("accept") && !n.contains("done")
        }));

        // 2. hub_get_task_context(AGY-2) works
        let ctx_reply = handle_hub_mcp_jsonrpc(
            &hub,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": { "name": "hub_get_task_context", "arguments": { "taskKey": "AGY-2" } }
            }),
            "test-agent",
        )
        .await;
        assert_eq!(ctx_reply.http_status, 200);
        let ctx_body = ctx_reply.body.unwrap();
        assert_eq!(ctx_body["result"]["isError"], false);
        assert_eq!(ctx_body["result"]["structuredContent"]["task"]["taskKey"], "AGY-2");

        // 3. Attempting submit_handoff with targetStatus=done returns HTTP 403
        let forbid_reply = handle_hub_mcp_jsonrpc(
            &hub,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "hub_submit_handoff",
                    "arguments": {
                        "taskKeys": ["AGY-2"],
                        "summary": "try self done",
                        "targetStatus": "done"
                    }
                }
            }),
            "test-agent",
        )
        .await;
        assert_eq!(forbid_reply.http_status, 403);
        let err_msg = forbid_reply.body.unwrap()["error"]["message"]
            .as_str()
            .unwrap_or("")
            .to_string();
        assert!(err_msg.contains("FORBIDDEN: Human acceptance required"));

        // 4. Attempting create_task with status=done returns HTTP 403
        let forbid_create = handle_hub_mcp_jsonrpc(
            &hub,
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "hub_create_task",
                    "arguments": { "title": "Sneaky", "status": "done" }
                }
            }),
            "test-agent",
        )
        .await;
        assert_eq!(forbid_create.http_status, 403);
    }

    #[test]
    fn verify_auth_supports_static_token_and_keycloak_sa_jwt() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            json!({
                "iss": "https://sso.martensite.dev/realms/corp",
                "azp": "pits-project-hub-mcp",
                "sub": "sa-uuid-123"
            })
            .to_string(),
        );
        let fake_jwt = format!("eyJhbGciOiJSUzI1NiJ9.{payload}.sig");

        // Static token configured: missing token rejected
        assert!(verify_hub_mcp_request_auth(Some("secret-tok"), None, None, None, None).is_err());

        // Static token via X-PITS-MCP-Token accepted
        let ok_static = verify_hub_mcp_request_auth(
            Some("secret-tok"),
            Some("secret-tok"),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(ok_static.auth_mode, "static-token");

        // Keycloak SA JWT accepted even when static token is configured
        let ok_jwt = verify_hub_mcp_request_auth(
            Some("secret-tok"),
            None,
            Some(&format!("Bearer {fake_jwt}")),
            Some("portal.lab.martensite.dev"),
            None,
        )
        .unwrap();
        assert_eq!(ok_jwt.auth_mode, "keycloak-jwt");
        assert!(ok_jwt.actor.contains("pits-project-hub-mcp"));
    }
}
