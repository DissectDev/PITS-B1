//! Model Context Protocol (MCP) Server for PITS Kraljic × Dutch Windmill Sourcing Governance.
//!
//! Provides two transports wired to the same `CatalogRegistry` + SQLite WAL / PostgreSQL DB core:
//! - Stdio (`pitsctl mcp`): newline-delimited JSON-RPC 2.0 and `Content-Length:` framed JSON-RPC.
//! - Streamable HTTP (`POST /mcp`, `GET /mcp`, `DELETE /mcp`) mounted in `server.rs`.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;

use crate::catalog::sourcing::SOURCING_RESOURCE_TYPES;
use crate::catalog::sourcing_db::{REVIEWER_ROLES, VARIANCE_SPREAD_THRESHOLD};
use crate::catalog::{
    AssessWindmillRequest, CandidateDispositionRequest, CatalogRegistry, SubmitBlindBallotRequest,
};

const SUPPORTED_PROTOCOL_VERSIONS: [&str; 2] = ["2025-03-26", "2024-11-05"];
const DEFAULT_PROTOCOL_VERSION: &str = "2025-03-26";

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

fn tool_definitions() -> Value {
    json!([
        {
            "name": "sourcing_db_status",
            "description": "Inspect SQLite WAL + PostgreSQL bi-directional sync state, pending outbox rows, and ledger totals.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        },
        {
            "name": "sourcing_db_sync",
            "description": "Trigger immediate bi-directional synchronization between local SQLite WAL outbox and PostgreSQL sourcing SSOT.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pgUrlOverride": {
                        "type": "string",
                        "description": "Optional postgres:// URL overriding configured profile/env connection"
                    }
                }
            }
        },
        {
            "name": "sourcing_scan_workspace",
            "description": "Automatically scan Cargo.toml, package.json, go.mod, Nix/K8s manifests, and Anti-Corruption Adapter boundaries to discover external sourcing candidates.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "rootPath": {
                        "type": "string",
                        "description": "Optional workspace root path override (defaults to active workspace bundle or CWD)"
                    },
                    "actor": {
                        "type": "string",
                        "description": "Scanner identity recorded in the audit ledger"
                    }
                }
            }
        },
        {
            "name": "sourcing_list_units",
            "description": "List evaluated Kraljic x Dutch Windmill sourcing Resource entities with optional quadrant, resourceType, and redline filters.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "quadrant": {
                        "type": "string",
                        "enum": ["strategic", "leverage", "bottleneck", "non-critical"]
                    },
                    "resourceType": {
                        "type": "string",
                        "enum": ["vendor-saas", "commercial-sdk", "managed-infra", "outsourced-dev"]
                    },
                    "onlyRedlines": {
                        "type": "boolean",
                        "description": "Return only units triggering WindmillMismatch, ReAuditRequired, Knockout, or BlindVariance"
                    }
                }
            }
        },
        {
            "name": "sourcing_submit_blind_ballot",
            "description": "Submit an independent role-based blind score ballot (architecture, procurement, security-compliance, domain-owner). DB aggregates mean scores and triggers blindScoreVarianceAlert whenever inter-role spread >= 1.5.",
            "inputSchema": {
                "type": "object",
                "required": [
                    "unitName", "reviewerRole", "reviewerId",
                    "y2_coreCompetitiveness", "y4_priceElasticity",
                    "x2_marketScarcity", "x3_customizationDepth", "x4_complianceFragility"
                ],
                "properties": {
                    "unitName": { "type": "string" },
                    "reviewerRole": {
                        "type": "string",
                        "enum": ["architecture", "procurement", "security-compliance", "domain-owner"]
                    },
                    "reviewerId": { "type": "string" },
                    "y2_coreCompetitiveness": { "type": "number", "minimum": 1, "maximum": 5 },
                    "y4_priceElasticity": { "type": "number", "minimum": 1, "maximum": 5 },
                    "x2_marketScarcity": { "type": "number", "minimum": 1, "maximum": 5 },
                    "x3_customizationDepth": { "type": "number", "minimum": 1, "maximum": 5 },
                    "x4_complianceFragility": { "type": "number", "minimum": 1, "maximum": 5 },
                    "evidenceNotes": { "type": "string" }
                }
            }
        },
        {
            "name": "sourcing_assess_windmill",
            "description": "Record supplier-perspective Dutch Windmill counterparty positioning (wallet share tier, attractiveness, perceivedStatus).",
            "inputSchema": {
                "type": "object",
                "required": [
                    "unitName", "estimatedWalletShareTier",
                    "accountAttractivenessScore", "perceivedStatus", "assessedBy"
                ],
                "properties": {
                    "unitName": { "type": "string" },
                    "estimatedWalletShareTier": {
                        "type": "string",
                        "enum": ["under-1pct", "1-to-5pct", "5-to-15pct", "over-15pct"]
                    },
                    "accountAttractivenessScore": { "type": "number", "minimum": 1, "maximum": 5 },
                    "perceivedStatus": {
                        "type": "string",
                        "enum": ["core", "development", "exploitable", "nuisance"]
                    },
                    "assessedBy": { "type": "string" },
                    "rationale": { "type": "string" }
                }
            }
        },
        {
            "name": "sourcing_promote_candidate",
            "description": "Promote a discovered external dependency candidate into an unpacked Backstage Resource sourcing unit. Enforces 4-type anti-monolith rules, Adapter check, and zero plaintext FinOps price hygiene.",
            "inputSchema": {
                "type": "object",
                "required": ["candidateId"],
                "properties": {
                    "candidateId": { "type": "string" },
                    "name": { "type": "string" },
                    "namespace": { "type": "string" },
                    "resourceType": {
                        "type": "string",
                        "enum": ["vendor-saas", "commercial-sdk", "managed-infra", "outsourced-dev"]
                    },
                    "system": { "type": "string" },
                    "owner": { "type": "string" },
                    "costAllocationId": { "type": "string" },
                    "renewalDate": { "type": "string", "description": "YYYY-MM-DD" },
                    "vendorName": { "type": "string" },
                    "adapterAbstracted": { "type": "boolean" },
                    "description": { "type": "string" },
                    "dependsOnComponents": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "actor": { "type": "string" }
                }
            }
        },
        {
            "name": "sourcing_ignore_candidate",
            "description": "Mark a discovered dependency candidate as ignored/internal so it leaves the pending MCP promotion queue.",
            "inputSchema": {
                "type": "object",
                "required": ["candidateId"],
                "properties": {
                    "candidateId": { "type": "string" },
                    "actor": { "type": "string" }
                }
            }
        },
        {
            "name": "sourcing_set_adapter_decoupling",
            "description": "Toggle sourcing.company.io/adapter-abstracted (Anti-Corruption Layer status) on an existing unit and recompute X1 Lock-In Risk and Kraljic Facts.",
            "inputSchema": {
                "type": "object",
                "required": ["unitName", "adapterAbstracted"],
                "properties": {
                    "unitName": { "type": "string" },
                    "adapterAbstracted": { "type": "boolean" },
                    "actor": { "type": "string" }
                }
            }
        }
    ])
}

async fn handle_tool_call(catalog: &CatalogRegistry, name: &str, args: Value) -> Result<Value, (i64, String)> {
    match name {
        "sourcing_db_status" => {
            let status = catalog.sourcing_db_status().await;
            Ok(tool_ok(json!(status)))
        }
        "sourcing_db_sync" => {
            let pg_override = args
                .get("pgUrlOverride")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let status = catalog.sourcing_sync_db(pg_override).await;
            Ok(tool_ok(json!(status)))
        }
        "sourcing_scan_workspace" => {
            let root = args
                .get("rootPath")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from);
            let actor = args
                .get("actor")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("mcp-scanner");
            match catalog
                .sourcing_scan_workspace(root.as_deref(), actor, "mcp")
                .await
            {
                Ok(candidates) => Ok(tool_ok(json!({
                    "totalCandidates": candidates.len(),
                    "pendingCount": candidates.iter().filter(|c| c.status == "pending").count(),
                    "candidates": candidates,
                }))),
                Err(e) => Ok(tool_err(e.to_string())),
            }
        }
        "sourcing_list_units" => {
            let quadrant = args
                .get("quadrant")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_ascii_lowercase());
            let resource_type = args
                .get("resourceType")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_ascii_lowercase());
            let only_redlines = args
                .get("onlyRedlines")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let ov = catalog.sourcing_overview(false).await;
            let filtered: Vec<_> = ov
                .items
                .into_iter()
                .filter(|item| {
                    quadrant
                        .as_deref()
                        .map(|q| item.facts.quadrant.eq_ignore_ascii_case(q))
                        .unwrap_or(true)
                })
                .filter(|item| {
                    resource_type
                        .as_deref()
                        .map(|t| item.record.resource_type.eq_ignore_ascii_case(t))
                        .unwrap_or(true)
                })
                .filter(|item| {
                    if !only_redlines {
                        true
                    } else {
                        item.facts.windmill_mismatch
                            || item.facts.re_audit_required
                            || item.facts.knockout_triggered
                            || item.record.audit.contract_meta.blind_score_variance_alert
                    }
                })
                .collect();
            Ok(tool_ok(json!({
                "totalMatched": filtered.len(),
                "quadrantCounts": ov.quadrant_counts,
                "redlineCounts": ov.redline_counts,
                "dbSync": ov.db_sync,
                "items": filtered,
            })))
        }
        "sourcing_submit_blind_ballot" => {
            let req: SubmitBlindBallotRequest = match serde_json::from_value(args) {
                Ok(r) => r,
                Err(e) => return Ok(tool_err(format!("Invalid blind ballot payload: {e}"))),
            };
            match catalog.sourcing_submit_ballot_with_channel(req.clone(), "mcp").await {
                Ok(stats) => {
                    let eval = catalog
                        .evaluate_sourcing_for_entity("default", "resource", &req.unit_name)
                        .await
                        .ok();
                    Ok(tool_ok(json!({
                        "unitName": req.unit_name,
                        "meanScores": stats.mean_scores,
                        "maxSpread": stats.max_spread,
                        "varianceDimensions": stats.variance_dimensions,
                        "varianceAlert": stats.variance_alert,
                        "varianceThreshold": VARIANCE_SPREAD_THRESHOLD,
                        "evaluation": eval,
                    })))
                }
                Err(e) => Ok(tool_err(e.to_string())),
            }
        }
        "sourcing_assess_windmill" => {
            let req: AssessWindmillRequest = match serde_json::from_value(args) {
                Ok(r) => r,
                Err(e) => return Ok(tool_err(format!("Invalid windmill assessment payload: {e}"))),
            };
            match catalog.sourcing_assess_windmill(req, "mcp").await {
                Ok(eval) => Ok(tool_ok(json!({
                    "evaluation": eval,
                }))),
                Err(e) => Ok(tool_err(e.to_string())),
            }
        }
        "sourcing_promote_candidate" => {
            let mut obj = match args.as_object() {
                Some(m) => m.clone(),
                None => return Ok(tool_err("Arguments must be a JSON object")),
            };
            obj.insert("action".to_string(), json!("promote"));
            let req: CandidateDispositionRequest = match serde_json::from_value(Value::Object(obj)) {
                Ok(r) => r,
                Err(e) => return Ok(tool_err(format!("Invalid promote candidate arguments: {e}"))),
            };
            match catalog.sourcing_promote_or_ignore_candidate(req, "mcp").await {
                Ok((cand, eval)) => Ok(tool_ok(json!({
                    "candidate": cand,
                    "evaluation": eval,
                }))),
                Err(e) => Ok(tool_err(e.to_string())),
            }
        }
        "sourcing_ignore_candidate" => {
            let mut obj = match args.as_object() {
                Some(m) => m.clone(),
                None => return Ok(tool_err("Arguments must be a JSON object")),
            };
            obj.insert("action".to_string(), json!("ignore"));
            let req: CandidateDispositionRequest = match serde_json::from_value(Value::Object(obj)) {
                Ok(r) => r,
                Err(e) => return Ok(tool_err(format!("Invalid ignore candidate arguments: {e}"))),
            };
            match catalog.sourcing_promote_or_ignore_candidate(req, "mcp").await {
                Ok((cand, _)) => Ok(tool_ok(json!({ "candidate": cand }))),
                Err(e) => Ok(tool_err(e.to_string())),
            }
        }
        "sourcing_set_adapter_decoupling" => {
            let unit_name = args
                .get("unitName")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| (-32602, "Missing required 'unitName'".to_string()))?;
            let adapter_abstracted = args
                .get("adapterAbstracted")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| (-32602, "Missing boolean 'adapterAbstracted'".to_string()))?;
            let actor = args
                .get("actor")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("mcp-operator");
            match catalog
                .sourcing_set_adapter_decoupling(unit_name, adapter_abstracted, actor, "mcp")
                .await
            {
                Ok(eval) => Ok(tool_ok(json!({ "evaluation": eval }))),
                Err(e) => Ok(tool_err(e.to_string())),
            }
        }
        other => Err((-32601, format!("Unknown MCP tool '{other}'"))),
    }
}

async fn handle_resources_read(catalog: &CatalogRegistry, uri: &str) -> Result<Value, (i64, String)> {
    let trimmed = uri.trim();
    if trimmed == "sourcing://portfolio/overview" {
        let ov = catalog.sourcing_overview(false).await;
        let text = serde_json::to_string_pretty(&ov).unwrap_or_default();
        return Ok(json!({
            "contents": [{
                "uri": trimmed,
                "mimeType": "application/json",
                "text": text,
            }]
        }));
    }
    if trimmed == "sourcing://discovery/candidates" {
        let ov = catalog.sourcing_overview(false).await;
        let text = serde_json::to_string_pretty(&json!({
            "dbSync": ov.db_sync,
            "candidates": ov.discovery_candidates,
        }))
        .unwrap_or_default();
        return Ok(json!({
            "contents": [{
                "uri": trimmed,
                "mimeType": "application/json",
                "text": text,
            }]
        }));
    }
    if let Some(unit_name) = trimmed.strip_prefix("sourcing://unit/") {
        let unit = unit_name.trim().trim_matches('/');
        if unit.is_empty() {
            return Err((-32602, "Unit name cannot be empty in sourcing://unit/{name}".to_string()));
        }
        let eval = catalog
            .evaluate_sourcing_for_entity("default", "resource", unit)
            .await
            .map_err(|e| (-32002, e.to_string()))?;
        let text = serde_json::to_string_pretty(&eval).unwrap_or_default();
        return Ok(json!({
            "contents": [{
                "uri": trimmed,
                "mimeType": "application/json",
                "text": text,
            }]
        }));
    }
    Err((-32002, format!("Unknown MCP resource URI '{trimmed}'")))
}

async fn handle_prompts_get(catalog: &CatalogRegistry, params: &Value) -> Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));

    match name {
        "audit-external-dependency" => {
            let unit_hint = args
                .get("unitName")
                .and_then(|v| v.as_str())
                .unwrap_or("<target-unit>");
            let ecosystem = args
                .get("ecosystem")
                .and_then(|v| v.as_str())
                .unwrap_or("cargo / npm / go / nix");
            let text = format!(
                "Conduct a DB-First Modern Kraljic x Dutch Windmill sourcing audit for '{unit_hint}' ({ecosystem}):\n\
                 1. Run `sourcing_scan_workspace` to verify manifest origin, dependent components, and Anti-Corruption Adapter status.\n\
                 2. Validate granularity against the 4 allowed Resource types ([{}]) — reject vendor-level monoliths and never store plaintext contract pricing (reference `finops.company.io/cost-allocation-id` only).\n\
                 3. Promote verified candidates via `sourcing_promote_candidate`.\n\
                 4. Collect independent blind ballots across [{}] via `sourcing_submit_blind_ballot`.\n\
                 5. Record supplier-side Dutch Windmill status via `sourcing_assess_windmill` and check any 5-point knockouts or WindmillMismatch redlines.",
                SOURCING_RESOURCE_TYPES.join(", "),
                REVIEWER_ROLES.join(", ")
            );
            Ok(json!({
                "description": "Guided workflow for discovering, promoting, and blind-scoring an external dependency.",
                "messages": [{
                    "role": "user",
                    "content": { "type": "text", "text": text }
                }]
            }))
        }
        "reconcile-blind-variance" => {
            let unit_name = args
                .get("unitName")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| (-32602, "Prompt 'reconcile-blind-variance' requires 'unitName' argument".to_string()))?;
            let eval = catalog
                .evaluate_sourcing_for_entity("default", "resource", unit_name)
                .await
                .map_err(|e| (-32002, e.to_string()))?;
            let ballots_json = serde_json::to_string_pretty(&eval.ballots).unwrap_or_default();
            let text = format!(
                "Reconcile inter-role blind ballot variance for unit '{}' (current maxSpread={:.2}, threshold={:.1}, quadrant='{}'):\n\
                 Submitted ballots:\n{}\n\n\
                 Inspect technical lock-in evidence (`adapterAbstracted={}`, prodDependents={}) versus procurement switching alternatives, align the outlier dimension(s), and re-submit updated role ballot(s) via `sourcing_submit_blind_ballot`.",
                eval.record.name,
                eval.max_ballot_spread,
                VARIANCE_SPREAD_THRESHOLD,
                eval.facts.quadrant,
                ballots_json,
                eval.record.adapter_abstracted,
                eval.facts.prod_dependents
            );
            Ok(json!({
                "description": "Arbitration prompt when multi-role blind ballots diverge >= 1.5.",
                "messages": [{
                    "role": "user",
                    "content": { "type": "text", "text": text }
                }]
            }))
        }
        other => Err((-32602, format!("Unknown MCP prompt '{other}'"))),
    }
}

/// Processes a single MCP JSON-RPC 2.0 request or notification.
/// Returns `None` for notifications (no response required per JSON-RPC 2.0 / MCP spec).
pub async fn handle_mcp_jsonrpc_value(catalog: &CatalogRegistry, msg: Value) -> Option<Value> {
    if let Value::Array(batch) = msg {
        if batch.is_empty() {
            return Some(jsonrpc_err(Value::Null, -32600, "Empty JSON-RPC batch"));
        }
        let mut replies = Vec::new();
        for item in batch {
            if let Some(resp) = Box::pin(handle_mcp_jsonrpc_value(catalog, item)).await {
                replies.push(resp);
            }
        }
        if replies.is_empty() {
            return None;
        }
        return Some(Value::Array(replies));
    }

    let obj = match msg.as_object() {
        Some(m) => m,
        None => return Some(jsonrpc_err(Value::Null, -32600, "JSON-RPC payload must be an object")),
    };

    let id = obj.get("id").cloned();
    let method = match obj.get("method").and_then(|v| v.as_str()) {
        Some(m) => m,
        None => {
            return id.map(|req_id| jsonrpc_err(req_id, -32600, "Missing JSON-RPC 'method'"));
        }
    };

    // Notifications (no id or `notifications/*`) do not produce a JSON-RPC response.
    if id.is_none() || method.starts_with("notifications/") {
        return None;
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
            Some(jsonrpc_ok(
                req_id,
                json!({
                    "protocolVersion": chosen_ver,
                    "capabilities": {
                        "tools": { "listChanged": false },
                        "resources": { "subscribe": false, "listChanged": false },
                        "prompts": { "listChanged": false }
                    },
                    "serverInfo": {
                        "name": "pits-sourcing-mcp",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "instructions": "Use `sourcing_scan_workspace` -> `sourcing_promote_candidate` -> `sourcing_submit_blind_ballot` -> `sourcing_assess_windmill` for DB-first sourcing governance."
                }),
            ))
        }
        "ping" => Some(jsonrpc_ok(req_id, json!({}))),
        "tools/list" => Some(jsonrpc_ok(req_id, json!({ "tools": tool_definitions() }))),
        "tools/call" => {
            let tool_name = match params.get("name").and_then(|v| v.as_str()) {
                Some(n) if !n.trim().is_empty() => n.trim(),
                _ => return Some(jsonrpc_err(req_id, -32602, "tools/call requires 'name'")),
            };
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match Box::pin(handle_tool_call(catalog, tool_name, args)).await {
                Ok(res) => Some(jsonrpc_ok(req_id, res)),
                Err((code, msg)) => Some(jsonrpc_err(req_id, code, msg)),
            }
        }
        "resources/list" => Some(jsonrpc_ok(
            req_id,
            json!({
                "resources": [
                    {
                        "uri": "sourcing://portfolio/overview",
                        "name": "Sourcing Portfolio Overview & DB Sync Telemetry",
                        "mimeType": "application/json"
                    },
                    {
                        "uri": "sourcing://discovery/candidates",
                        "name": "Discovered Workspace Dependency Candidates",
                        "mimeType": "application/json"
                    }
                ]
            }),
        )),
        "resources/templates/list" => Some(jsonrpc_ok(
            req_id,
            json!({
                "resourceTemplates": [{
                    "uriTemplate": "sourcing://unit/{name}",
                    "name": "Single Unpacked Sourcing Unit Evaluation & Blind Ballots",
                    "mimeType": "application/json"
                }]
            }),
        )),
        "resources/read" => {
            let uri = match params.get("uri").and_then(|v| v.as_str()) {
                Some(u) => u,
                None => return Some(jsonrpc_err(req_id, -32602, "resources/read requires 'uri'")),
            };
            match Box::pin(handle_resources_read(catalog, uri)).await {
                Ok(res) => Some(jsonrpc_ok(req_id, res)),
                Err((code, msg)) => Some(jsonrpc_err(req_id, code, msg)),
            }
        }
        "prompts/list" => Some(jsonrpc_ok(
            req_id,
            json!({
                "prompts": [
                    {
                        "name": "audit-external-dependency",
                        "description": "End-to-end DB-First external dependency discovery, promotion, and blind evaluation guide",
                        "arguments": [
                            { "name": "unitName", "required": false },
                            { "name": "ecosystem", "required": false }
                        ]
                    },
                    {
                        "name": "reconcile-blind-variance",
                        "description": "Arbitrate multi-role blind score disagreement (>= 1.5 spread)",
                        "arguments": [
                            { "name": "unitName", "required": true }
                        ]
                    }
                ]
            }),
        )),
        "prompts/get" => match Box::pin(handle_prompts_get(catalog, &params)).await {
            Ok(res) => Some(jsonrpc_ok(req_id, res)),
            Err((code, msg)) => Some(jsonrpc_err(req_id, code, msg)),
        },
        other => Some(jsonrpc_err(req_id, -32601, format!("Method '{other}' not found"))),
    }
}

/// Runs the MCP server over Standard Input / Standard Output.
/// Automatically detects newline-delimited JSON-RPC (standard MCP stdio) vs `Content-Length:` framing.
pub async fn run_mcp_stdio(catalog: CatalogRegistry) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(std::result::Result<Value, String>, bool)>();

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
                    let _ = tx.send((Err(format!("Invalid Content-Length header: {trimmed}")), true));
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
                    let _ = tx.send((Err(format!("Read Content-Length body failed: {e}")), true));
                    break;
                }
                let parsed = serde_json::from_slice::<Value>(&buf)
                    .map_err(|e| format!("Parse error: {e}"));
                if tx.send((parsed, true)).is_err() {
                    break;
                }
            } else {
                let parsed = serde_json::from_str::<Value>(trimmed)
                    .map_err(|e| format!("Parse error: {e}"));
                if tx.send((parsed, false)).is_err() {
                    break;
                }
            }
        }
    });

    while let Some((msg_res, use_content_length)) = rx.recv().await {
        let response_opt = match msg_res {
            Ok(val) => handle_mcp_jsonrpc_value(&catalog, val).await,
            Err(e) => Some(jsonrpc_err(Value::Null, -32700, e)),
        };

        if let Some(resp) = response_opt {
            let encoded = serde_json::to_string(&resp).context("serialize MCP JSON-RPC reply")?;
            let mut out = std::io::stdout().lock();
            if use_content_length {
                write!(out, "Content-Length: {}\r\n\r\n{}", encoded.len(), encoded)
                    .context("write Content-Length MCP reply")?;
            } else {
                out.write_all(encoded.as_bytes())
                    .context("write NDJSON MCP reply")?;
                out.write_all(b"\n").context("write MCP newline")?;
            }
            out.flush().context("flush MCP stdout")?;
        }
    }

    Ok(())
}
