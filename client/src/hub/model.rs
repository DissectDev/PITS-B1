//! Data models, validation constants, and protocol error types for PITS Self-Managed Project Hub.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const HUB_PROJECT_STATUSES: [&str; 4] = ["not_started", "in_progress", "paused", "done"];
pub const HUB_PROJECT_PRIORITIES: [&str; 3] = ["high", "medium", "low"];

pub const HUB_TASK_STATUSES: [&str; 6] = [
    "backlog",
    "ready",
    "in_progress",
    "ready_for_review",
    "blocked",
    "done",
];

pub const HUB_TASK_PRIORITIES: [&str; 4] = ["p0_critical", "p1_high", "p2_medium", "p3_low"];

pub const HUB_LOG_TYPES: [&str; 4] = [
    "session_handoff",
    "decision_rfc",
    "blocker_pitfall",
    "acceptance",
];

pub const HUB_PORTAL_BASE_URL: &str = "https://portal.lab.martensite.dev";
pub const HUB_TAILSCALE_BASE_URL: &str = "http://100.95.95.88:18090";

pub const FORBIDDEN_HUMAN_ACCEPTANCE_MSG: &str =
    "FORBIDDEN: Human acceptance required via PITS Portal";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubError {
    Forbidden(String),
    NotFound(String),
    BadRequest(String),
    Internal(String),
}

impl std::fmt::Display for HubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Forbidden(m) | Self::NotFound(m) | Self::BadRequest(m) | Self::Internal(m) => {
                write!(f, "{m}")
            }
        }
    }
}

impl std::error::Error for HubError {}

impl HubError {
    pub fn is_forbidden(&self) -> bool {
        matches!(self, Self::Forbidden(_))
    }
}

pub fn task_portal_url(task_key: &str) -> String {
    format!("{HUB_PORTAL_BASE_URL}/project-hub/tasks/{task_key}")
}

pub fn task_tailscale_url(task_key: &str) -> String {
    format!("{HUB_TAILSCALE_BASE_URL}/project-hub/tasks/{task_key}")
}

pub fn normalize_tags(tags: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in tags {
        let cleaned = raw.trim().to_ascii_lowercase();
        if !cleaned.is_empty() && !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HubProject {
    pub id: String,
    pub name: String,
    pub status: String,
    pub priority: String,
    pub workspace_path: String,
    pub outcome: String,
    pub target_date: Option<String>,
    pub lead: String,
    pub body_md: String,
    pub catalog_refs: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub sync_pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AcChecklistItem {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub agent_verified: bool,
    #[serde(default)]
    pub human_verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HubTask {
    pub seq_id: i64,
    pub task_key: String,
    pub project_id: String,
    pub title: String,
    pub status: String,
    pub priority: String,
    pub tags: Vec<String>,
    pub workspace_path: String,
    pub dispatch_target: String,
    pub acceptance_summary: String,
    pub ac_checklist: Vec<AcChecklistItem>,
    pub owner: String,
    pub due_date: Option<String>,
    pub last_session_at: Option<String>,
    pub body_md: String,
    pub catalog_refs: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub sync_pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HubSessionLog {
    pub id: String,
    pub project_id: String,
    pub summary: String,
    pub log_type: String,
    pub tags: Vec<String>,
    pub workspace_path: String,
    pub conversation_id: String,
    pub commit_refs: Vec<String>,
    pub evidence_refs: Vec<String>,
    pub body_md: String,
    pub accepted: bool,
    pub accepted_by: Option<String>,
    pub accepted_at: Option<String>,
    pub review_note: String,
    pub session_date: String,
    pub task_keys: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub sync_pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HubAuditEvent {
    pub id: String,
    pub entity_type: String,
    pub entity_id: String,
    pub action: String,
    pub channel: String,
    pub actor: String,
    pub detail: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HubDbSyncStatus {
    pub sqlite_path: String,
    pub sqlite_version: String,
    pub journal_mode: String,
    pub fts_enabled: bool,
    pub postgres_configured: bool,
    pub postgres_url_redacted: String,
    pub postgres_connected: bool,
    pub pending_outbox_rows: usize,
    pub last_sync_at: Option<String>,
    pub last_sync_direction: Option<String>,
    pub last_sync_error: Option<String>,
    pub project_count: usize,
    pub task_count: usize,
    pub session_count: usize,
    pub audit_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubOverviewResponse {
    pub projects: Vec<HubProject>,
    pub tasks: Vec<HubTask>,
    pub recent_sessions: Vec<HubSessionLog>,
    pub pending_review_count: usize,
    pub pending_acceptance_sessions: Vec<HubSessionLog>,
    pub tags: Vec<String>,
    pub status_counts: BTreeMap<String, usize>,
    pub db_sync: HubDbSyncStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubTaskDetailResponse {
    pub task: HubTask,
    pub project: Option<HubProject>,
    pub sessions: Vec<HubSessionLog>,
    pub audit_events: Vec<HubAuditEvent>,
    pub portal_url: String,
    pub tailscale_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubTaskContextResponse {
    pub task: HubTask,
    pub project: Option<HubProject>,
    pub linked_sessions: Vec<HubSessionLog>,
    pub recent_decisions_and_blockers: Vec<HubSessionLog>,
    pub portal_url: String,
    pub tailscale_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSearchResponse {
    pub query: String,
    pub engine: String,
    pub tasks: Vec<HubTask>,
    pub sessions: Vec<HubSessionLog>,
}

// ---------------------------------------------------------------------------
// Request DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskQueryFilter {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionQueryFilter {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub log_type: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub task_key: Option<String>,
    #[serde(default)]
    pub accepted: Option<bool>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertProjectRequest {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub target_date: Option<String>,
    #[serde(default)]
    pub lead: Option<String>,
    #[serde(default)]
    pub body_md: Option<String>,
    #[serde(default)]
    pub catalog_refs: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTaskRequest {
    #[serde(default = "default_project_id")]
    pub project_id: String,
    pub title: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub workspace_path: String,
    #[serde(default)]
    pub dispatch_target: String,
    #[serde(default)]
    pub acceptance_summary: String,
    #[serde(default)]
    pub ac_checklist: Vec<AcChecklistItem>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub due_date: Option<String>,
    #[serde(default)]
    pub body_md: String,
    #[serde(default)]
    pub catalog_refs: Vec<String>,
}

fn default_project_id() -> String {
    "pits".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateTaskRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub dispatch_target: Option<String>,
    #[serde(default)]
    pub acceptance_summary: Option<String>,
    #[serde(default)]
    pub ac_checklist: Option<Vec<AcChecklistItem>>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub due_date: Option<Option<String>>,
    #[serde(default)]
    pub body_md: Option<String>,
    #[serde(default)]
    pub catalog_refs: Option<Vec<String>>,
    /// Explicitly caught so MCP attempts to set `accepted: true` are rejected with 403.
    #[serde(default)]
    pub accepted: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickupTaskRequest {
    pub task_key: String,
    #[serde(default)]
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub actor: Option<String>,
    /// Explicitly caught so MCP attempts to smuggle `status: "done"` or `accepted: true` are rejected.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub accepted: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitHandoffRequest {
    #[serde(default = "default_project_id")]
    pub project_id: String,
    pub task_keys: Vec<String>,
    pub summary: String,
    #[serde(default)]
    pub target_status: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub workspace_path: String,
    #[serde(default)]
    pub conversation_id: String,
    #[serde(default)]
    pub commit_refs: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub body_md: String,
    #[serde(default)]
    pub agent_verified_ac_ids: Vec<String>,
    #[serde(default)]
    pub session_date: Option<String>,
    /// Explicitly caught so MCP attempts to set `accepted: true` are rejected with 403.
    #[serde(default)]
    pub accepted: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionLogRequest {
    #[serde(default = "default_project_id")]
    pub project_id: String,
    pub summary: String,
    pub log_type: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub workspace_path: String,
    #[serde(default)]
    pub conversation_id: String,
    #[serde(default)]
    pub commit_refs: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub body_md: String,
    #[serde(default)]
    pub task_keys: Vec<String>,
    #[serde(default)]
    pub session_date: Option<String>,
    #[serde(default)]
    pub accepted: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptSessionRequest {
    #[serde(default = "default_true")]
    pub mark_tasks_done: bool,
    #[serde(default)]
    pub review_note: String,
    #[serde(default)]
    pub verified_ac_ids: Vec<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectSessionRequest {
    #[serde(default)]
    pub review_note: String,
    #[serde(default)]
    pub target_task_status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanReviewOutcome {
    pub session: HubSessionLog,
    pub updated_tasks: Vec<HubTask>,
    pub auth_source: String,
    pub actor: String,
}
