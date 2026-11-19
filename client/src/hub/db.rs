//! Dual-Mode Relational Database SSOT (SQLite3 WAL + FTS5/LIKE <-> CNPG PostgreSQL 17.6 Outbox Sync)
//! for PITS Self-Managed Project Hub.

use crate::db_common::{pg_opt_str, pg_str, redact_db_url, PgWireClient, SqlVal, SqliteConn};
use crate::hub::model::{
    normalize_tags, task_portal_url, task_tailscale_url, AcChecklistItem, AcceptSessionRequest,
    CreateSessionLogRequest, CreateTaskRequest, HubAuditEvent, HubDbSyncStatus, HubError,
    HubOverviewResponse, HubProject, HubSearchResponse, HubSessionLog, HubTask,
    HubTaskContextResponse, HubTaskDetailResponse, HumanReviewOutcome, PickupTaskRequest,
    RejectSessionRequest, SessionQueryFilter, SubmitHandoffRequest, TaskQueryFilter,
    UpdateTaskRequest, UpsertProjectRequest, FORBIDDEN_HUMAN_ACCEPTANCE_MSG, HUB_LOG_TYPES,
    HUB_PROJECT_PRIORITIES, HUB_PROJECT_STATUSES, HUB_TASK_PRIORITIES, HUB_TASK_STATUSES,
};
use crate::hub::seed::seed_initial_data;
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Mutex;
use uuid::Uuid;

const SQLITE_HUB_SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;

CREATE TABLE IF NOT EXISTS hub_projects (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL UNIQUE,
    status          TEXT NOT NULL DEFAULT 'in_progress',
    priority        TEXT NOT NULL DEFAULT 'high',
    workspace_path  TEXT NOT NULL DEFAULT '',
    outcome         TEXT NOT NULL DEFAULT '',
    target_date     TEXT,
    lead            TEXT NOT NULL DEFAULT 'pitsadmin',
    body_md         TEXT NOT NULL DEFAULT '',
    catalog_refs    TEXT NOT NULL DEFAULT '[]',
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    sync_pending    INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS hub_tasks (
    seq_id              INTEGER PRIMARY KEY AUTOINCREMENT,
    task_key            TEXT NOT NULL UNIQUE,
    project_id          TEXT NOT NULL REFERENCES hub_projects(id),
    title               TEXT NOT NULL,
    status              TEXT NOT NULL DEFAULT 'backlog',
    priority            TEXT NOT NULL DEFAULT 'p2_medium',
    tags_json           TEXT NOT NULL DEFAULT '[]',
    workspace_path      TEXT NOT NULL DEFAULT '',
    dispatch_target     TEXT NOT NULL DEFAULT '',
    acceptance_summary  TEXT NOT NULL DEFAULT '',
    ac_checklist_json   TEXT NOT NULL DEFAULT '[]',
    owner               TEXT NOT NULL DEFAULT 'pitsadmin',
    due_date            TEXT,
    last_session_at     TEXT,
    body_md             TEXT NOT NULL DEFAULT '',
    catalog_refs        TEXT NOT NULL DEFAULT '[]',
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    sync_pending        INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS hub_session_logs (
    id                  TEXT PRIMARY KEY,
    project_id          TEXT NOT NULL REFERENCES hub_projects(id),
    summary             TEXT NOT NULL,
    log_type            TEXT NOT NULL,
    tags_json           TEXT NOT NULL DEFAULT '[]',
    workspace_path      TEXT NOT NULL DEFAULT '',
    conversation_id     TEXT NOT NULL DEFAULT '',
    commit_refs_json    TEXT NOT NULL DEFAULT '[]',
    evidence_refs_json  TEXT NOT NULL DEFAULT '[]',
    body_md             TEXT NOT NULL DEFAULT '',
    accepted            INTEGER NOT NULL DEFAULT 0,
    accepted_by         TEXT,
    accepted_at         TEXT,
    review_note         TEXT NOT NULL DEFAULT '',
    session_date        TEXT NOT NULL,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    sync_pending        INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS hub_session_log_tasks (
    session_log_id      TEXT NOT NULL REFERENCES hub_session_logs(id) ON DELETE CASCADE,
    task_key            TEXT NOT NULL REFERENCES hub_tasks(task_key) ON DELETE CASCADE,
    PRIMARY KEY (session_log_id, task_key)
);

CREATE TABLE IF NOT EXISTS hub_audit_events (
    id                  TEXT PRIMARY KEY,
    entity_type         TEXT NOT NULL,
    entity_id           TEXT NOT NULL,
    action              TEXT NOT NULL,
    channel             TEXT NOT NULL,
    actor               TEXT NOT NULL,
    detail_json         TEXT NOT NULL DEFAULT '{}',
    created_at          TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_hub_tasks_project_status ON hub_tasks(project_id, status);
CREATE INDEX IF NOT EXISTS idx_hub_sessions_project_date ON hub_session_logs(project_id, session_date DESC);
CREATE INDEX IF NOT EXISTS idx_hub_audit_entity ON hub_audit_events(entity_type, entity_id, created_at DESC);
"#;

const POSTGRES_HUB_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS hub_projects (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL UNIQUE,
    status          TEXT NOT NULL DEFAULT 'in_progress',
    priority        TEXT NOT NULL DEFAULT 'high',
    workspace_path  TEXT NOT NULL DEFAULT '',
    outcome         TEXT NOT NULL DEFAULT '',
    target_date     TEXT,
    lead            TEXT NOT NULL DEFAULT 'pitsadmin',
    body_md         TEXT NOT NULL DEFAULT '',
    catalog_refs    TEXT NOT NULL DEFAULT '[]',
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    sync_pending    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS hub_tasks (
    seq_id              BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    task_key            TEXT NOT NULL UNIQUE,
    project_id          TEXT NOT NULL REFERENCES hub_projects(id),
    title               TEXT NOT NULL,
    status              TEXT NOT NULL DEFAULT 'backlog',
    priority            TEXT NOT NULL DEFAULT 'p2_medium',
    tags_json           TEXT NOT NULL DEFAULT '[]',
    workspace_path      TEXT NOT NULL DEFAULT '',
    dispatch_target     TEXT NOT NULL DEFAULT '',
    acceptance_summary  TEXT NOT NULL DEFAULT '',
    ac_checklist_json   TEXT NOT NULL DEFAULT '[]',
    owner               TEXT NOT NULL DEFAULT 'pitsadmin',
    due_date            TEXT,
    last_session_at     TEXT,
    body_md             TEXT NOT NULL DEFAULT '',
    catalog_refs        TEXT NOT NULL DEFAULT '[]',
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    sync_pending        INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS hub_session_logs (
    id                  TEXT PRIMARY KEY,
    project_id          TEXT NOT NULL REFERENCES hub_projects(id),
    summary             TEXT NOT NULL,
    log_type            TEXT NOT NULL,
    tags_json           TEXT NOT NULL DEFAULT '[]',
    workspace_path      TEXT NOT NULL DEFAULT '',
    conversation_id     TEXT NOT NULL DEFAULT '',
    commit_refs_json    TEXT NOT NULL DEFAULT '[]',
    evidence_refs_json  TEXT NOT NULL DEFAULT '[]',
    body_md             TEXT NOT NULL DEFAULT '',
    accepted            INTEGER NOT NULL DEFAULT 0,
    accepted_by         TEXT,
    accepted_at         TEXT,
    review_note         TEXT NOT NULL DEFAULT '',
    session_date        TEXT NOT NULL,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    sync_pending        INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS hub_session_log_tasks (
    session_log_id      TEXT NOT NULL REFERENCES hub_session_logs(id) ON DELETE CASCADE,
    task_key            TEXT NOT NULL REFERENCES hub_tasks(task_key) ON DELETE CASCADE,
    PRIMARY KEY (session_log_id, task_key)
);

CREATE TABLE IF NOT EXISTS hub_audit_events (
    id                  TEXT PRIMARY KEY,
    entity_type         TEXT NOT NULL,
    entity_id           TEXT NOT NULL,
    action              TEXT NOT NULL,
    channel             TEXT NOT NULL,
    actor               TEXT NOT NULL,
    detail_json         TEXT NOT NULL DEFAULT '{}',
    created_at          TEXT NOT NULL
);
"#;

#[derive(Debug, Clone)]
struct HubSyncMeta {
    pg_url: Option<String>,
    postgres_connected: bool,
    last_sync_at: Option<String>,
    last_sync_direction: Option<String>,
    last_sync_error: Option<String>,
}

pub struct HubDbHandle {
    conn: Mutex<SqliteConn>,
    sync_meta: Mutex<HubSyncMeta>,
    sqlite_version: String,
    journal_mode: String,
    fts_enabled: bool,
}

impl std::fmt::Debug for HubDbHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubDbHandle")
            .field("sqlite_version", &self.sqlite_version)
            .field("journal_mode", &self.journal_mode)
            .field("fts_enabled", &self.fts_enabled)
            .finish()
    }
}

impl HubDbHandle {
    pub fn open_or_init(
        sqlite_path: PathBuf,
        in_memory: bool,
        pg_url: Option<String>,
    ) -> Result<Self, String> {
        let conn = SqliteConn::open(&sqlite_path, in_memory)?;
        let sqlite_version = conn.version();
        let journal_mode = if in_memory {
            "memory".to_string()
        } else {
            conn.query_rows("PRAGMA journal_mode = WAL;", &[])
                .ok()
                .and_then(|r| r.first().and_then(|c| c.first()).cloned().flatten())
                .unwrap_or_else(|| "wal".to_string())
        };
        let _ = conn.exec_batch("PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;");
        conn.exec_batch(SQLITE_HUB_SCHEMA)?;

        // Probe FTS5 availability dynamically; degrade gracefully to LIKE when absent.
        let fts_enabled = conn
            .exec_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS hub_fts USING fts5(\
                 entity_type UNINDEXED, entity_id UNINDEXED, title, tags, body);",
            )
            .is_ok();

        let _seeded = seed_initial_data(&conn)?;
        if fts_enabled {
            let _ = rebuild_fts_index(&conn);
        }

        Ok(Self {
            conn: Mutex::new(conn),
            sync_meta: Mutex::new(HubSyncMeta {
                pg_url: pg_url.filter(|s| !s.trim().is_empty()),
                postgres_connected: false,
                last_sync_at: None,
                last_sync_direction: None,
                last_sync_error: None,
            }),
            sqlite_version,
            journal_mode,
            fts_enabled,
        })
    }

    pub fn set_pg_url(&self, pg_url: Option<String>) {
        if let Ok(mut meta) = self.sync_meta.lock() {
            meta.pg_url = pg_url.filter(|s| !s.trim().is_empty());
        }
    }

    pub fn status(&self) -> Result<HubDbSyncStatus, HubError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| HubError::Internal("Hub DB mutex poisoned".to_string()))?;
        let meta = self
            .sync_meta
            .lock()
            .map_err(|_| HubError::Internal("Hub sync meta mutex poisoned".to_string()))?
            .clone();

        let count_query = |sql: &str| -> usize {
            conn.query_rows(sql, &[])
                .ok()
                .and_then(|r| r.first().and_then(|c| c.first()).cloned().flatten())
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0)
        };

        let pending = count_query("SELECT COUNT(*) FROM hub_projects WHERE sync_pending = 1;")
            + count_query("SELECT COUNT(*) FROM hub_tasks WHERE sync_pending = 1;")
            + count_query("SELECT COUNT(*) FROM hub_session_logs WHERE sync_pending = 1;");

        let pg_url_redacted = meta
            .pg_url
            .as_deref()
            .map(redact_db_url)
            .unwrap_or_default();

        Ok(HubDbSyncStatus {
            sqlite_path: conn.path_label.clone(),
            sqlite_version: self.sqlite_version.clone(),
            journal_mode: self.journal_mode.clone(),
            fts_enabled: self.fts_enabled,
            postgres_configured: meta.pg_url.is_some(),
            postgres_url_redacted: pg_url_redacted,
            postgres_connected: meta.postgres_connected,
            pending_outbox_rows: pending,
            last_sync_at: meta.last_sync_at,
            last_sync_direction: meta.last_sync_direction,
            last_sync_error: meta.last_sync_error,
            project_count: count_query("SELECT COUNT(*) FROM hub_projects;"),
            task_count: count_query("SELECT COUNT(*) FROM hub_tasks;"),
            session_count: count_query("SELECT COUNT(*) FROM hub_session_logs;"),
            audit_count: count_query("SELECT COUNT(*) FROM hub_audit_events;"),
        })
    }

    pub fn overview(&self) -> Result<HubOverviewResponse, HubError> {
        let projects = self.list_projects()?;
        let tasks = self.query_tasks(&TaskQueryFilter::default())?;
        let recent_sessions = self.query_sessions(&SessionQueryFilter {
            limit: Some(50),
            ..Default::default()
        })?;
        let db_sync = self.status()?;

        let mut tag_set = BTreeSet::new();
        let mut status_counts: BTreeMap<String, usize> = HUB_TASK_STATUSES
            .iter()
            .map(|s| ((*s).to_string(), 0usize))
            .collect();
        for t in &tasks {
            *status_counts.entry(t.status.clone()).or_insert(0) += 1;
            for tag in &t.tags {
                tag_set.insert(tag.clone());
            }
        }
        for s in &recent_sessions {
            for tag in &s.tags {
                tag_set.insert(tag.clone());
            }
        }

        let pending_acceptance_sessions: Vec<HubSessionLog> = recent_sessions
            .iter()
            .filter(|s| s.log_type == "session_handoff" && !s.accepted)
            .cloned()
            .collect();

        let ready_review_tasks = tasks
            .iter()
            .filter(|t| t.status == "ready_for_review")
            .count();
        let pending_review_count = ready_review_tasks.max(pending_acceptance_sessions.len());

        Ok(HubOverviewResponse {
            projects,
            tasks,
            recent_sessions,
            pending_review_count,
            pending_acceptance_sessions,
            tags: tag_set.into_iter().collect(),
            status_counts,
            db_sync,
        })
    }

    // -----------------------------------------------------------------------
    // Projects
    // -----------------------------------------------------------------------

    pub fn list_projects(&self) -> Result<Vec<HubProject>, HubError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| HubError::Internal("Hub DB mutex poisoned".to_string()))?;
        load_projects(&conn).map_err(HubError::Internal)
    }

    pub fn upsert_project(
        &self,
        req: UpsertProjectRequest,
        channel: &str,
        actor: &str,
    ) -> Result<HubProject, HubError> {
        let name = req.name.trim();
        if name.is_empty() {
            return Err(HubError::BadRequest(
                "Project name cannot be empty".to_string(),
            ));
        }
        let id = req
            .id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_else(|| slugify_id(name));
        if id.is_empty() {
            return Err(HubError::BadRequest(
                "Project id cannot be empty".to_string(),
            ));
        }
        let status = req
            .status
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .unwrap_or_else(|| "in_progress".to_string());
        if !HUB_PROJECT_STATUSES.contains(&status.as_str()) {
            return Err(HubError::BadRequest(format!(
                "Invalid project status '{}'; allowed: {}",
                status,
                HUB_PROJECT_STATUSES.join(", ")
            )));
        }
        let priority = req
            .priority
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .unwrap_or_else(|| "high".to_string());
        if !HUB_PROJECT_PRIORITIES.contains(&priority.as_str()) {
            return Err(HubError::BadRequest(format!(
                "Invalid project priority '{}'; allowed: {}",
                priority,
                HUB_PROJECT_PRIORITIES.join(", ")
            )));
        }

        let conn = self
            .conn
            .lock()
            .map_err(|_| HubError::Internal("Hub DB mutex poisoned".to_string()))?;
        let now = Utc::now().to_rfc3339();
        let existing = load_project_by_id(&conn, &id).map_err(HubError::Internal)?;

        let created_at = existing
            .as_ref()
            .map(|p| p.created_at.clone())
            .unwrap_or_else(|| now.clone());
        let workspace_path = req
            .workspace_path
            .or_else(|| existing.as_ref().map(|p| p.workspace_path.clone()))
            .unwrap_or_default();
        let outcome = req
            .outcome
            .or_else(|| existing.as_ref().map(|p| p.outcome.clone()))
            .unwrap_or_default();
        let target_date = req
            .target_date
            .or_else(|| existing.as_ref().and_then(|p| p.target_date.clone()));
        let lead = req
            .lead
            .or_else(|| existing.as_ref().map(|p| p.lead.clone()))
            .unwrap_or_else(|| "pitsadmin".to_string());
        let body_md = req
            .body_md
            .or_else(|| existing.as_ref().map(|p| p.body_md.clone()))
            .unwrap_or_default();
        let catalog_refs = req
            .catalog_refs
            .or_else(|| existing.as_ref().map(|p| p.catalog_refs.clone()))
            .unwrap_or_default();

        conn.execute(
            r#"INSERT INTO hub_projects (
                id, name, status, priority, workspace_path, outcome, target_date, lead, body_md, catalog_refs, created_at, updated_at, sync_pending
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                status = excluded.status,
                priority = excluded.priority,
                workspace_path = excluded.workspace_path,
                outcome = excluded.outcome,
                target_date = excluded.target_date,
                lead = excluded.lead,
                body_md = excluded.body_md,
                catalog_refs = excluded.catalog_refs,
                updated_at = excluded.updated_at,
                sync_pending = 1;"#,
            &[
                SqlVal::Text(id.clone()),
                SqlVal::Text(name.to_string()),
                SqlVal::Text(status),
                SqlVal::Text(priority),
                SqlVal::Text(workspace_path),
                SqlVal::Text(outcome),
                SqlVal::opt_text(target_date.as_deref()),
                SqlVal::Text(lead),
                SqlVal::Text(body_md),
                SqlVal::Text(serde_json::to_string(&catalog_refs).unwrap_or_else(|_| "[]".into())),
                SqlVal::Text(created_at),
                SqlVal::Text(now.clone()),
            ],
        )
        .map_err(HubError::Internal)?;

        let action = if existing.is_some() {
            "update"
        } else {
            "create"
        };
        insert_audit_event(
            &conn,
            "project",
            &id,
            action,
            channel,
            actor,
            &json!({ "name": name }),
            &now,
        )
        .map_err(HubError::Internal)?;

        load_project_by_id(&conn, &id)
            .map_err(HubError::Internal)?
            .ok_or_else(|| HubError::Internal("Project vanished after upsert".to_string()))
    }

    // -----------------------------------------------------------------------
    // Tasks (Global Monotonic AGY-<n> + Protocol Gate)
    // -----------------------------------------------------------------------

    pub fn query_tasks(&self, filter: &TaskQueryFilter) -> Result<Vec<HubTask>, HubError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| HubError::Internal("Hub DB mutex poisoned".to_string()))?;
        let mut tasks = load_all_tasks(&conn).map_err(HubError::Internal)?;

        if let Some(pid) = filter
            .project_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            tasks.retain(|t| t.project_id.eq_ignore_ascii_case(pid));
        }
        if let Some(st) = filter
            .status
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let allowed: Vec<String> = st
                .split(',')
                .map(|p| p.trim().to_ascii_lowercase())
                .filter(|p| !p.is_empty())
                .collect();
            if !allowed.is_empty() {
                tasks.retain(|t| allowed.contains(&t.status));
            }
        }
        if let Some(tag_raw) = filter
            .tag
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let required_tags: Vec<String> = tag_raw
                .split(',')
                .map(|p| p.trim().to_ascii_lowercase())
                .filter(|p| !p.is_empty())
                .collect();
            if !required_tags.is_empty() {
                tasks.retain(|t| {
                    required_tags
                        .iter()
                        .all(|req_tag| t.tags.iter().any(|x| x.eq_ignore_ascii_case(req_tag)))
                });
            }
        }
        if let Some(prio) = filter
            .priority
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            tasks.retain(|t| t.priority.eq_ignore_ascii_case(prio));
        }
        if let Some(ws) = filter
            .workspace
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let needle = ws.to_ascii_lowercase();
            tasks.retain(|t| {
                t.workspace_path.to_ascii_lowercase().contains(&needle)
                    || t.dispatch_target.to_ascii_lowercase().contains(&needle)
                    || t.tags.iter().any(|x| x.contains(&needle))
            });
        }
        if let Some(q) = filter
            .query
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let needle = q.to_ascii_lowercase();
            tasks.retain(|t| {
                t.task_key.to_ascii_lowercase().contains(&needle)
                    || t.title.to_ascii_lowercase().contains(&needle)
                    || t.acceptance_summary.to_ascii_lowercase().contains(&needle)
                    || t.body_md.to_ascii_lowercase().contains(&needle)
                    || t.tags.iter().any(|x| x.contains(&needle))
            });
        }

        Ok(tasks)
    }

    pub fn get_task_detail(&self, task_key: &str) -> Result<HubTaskDetailResponse, HubError> {
        let key = normalize_task_key(task_key);
        let conn = self
            .conn
            .lock()
            .map_err(|_| HubError::Internal("Hub DB mutex poisoned".to_string()))?;
        let task = load_task_by_key(&conn, &key)
            .map_err(HubError::Internal)?
            .ok_or_else(|| HubError::NotFound(format!("Task '{key}' not found")))?;
        let project = load_project_by_id(&conn, &task.project_id).map_err(HubError::Internal)?;
        let sessions = load_sessions_for_task(&conn, &task.task_key).map_err(HubError::Internal)?;
        let audit_events =
            load_audit_events_for_entity(&conn, "task", &task.task_key, 40)
                .map_err(HubError::Internal)?;
        let portal_url = task_portal_url(&task.task_key);
        let tailscale_url = task_tailscale_url(&task.task_key);
        Ok(HubTaskDetailResponse {
            task,
            project,
            sessions,
            audit_events,
            portal_url,
            tailscale_url,
        })
    }

    pub fn get_task_context(&self, task_key: &str) -> Result<HubTaskContextResponse, HubError> {
        let key = normalize_task_key(task_key);
        let conn = self
            .conn
            .lock()
            .map_err(|_| HubError::Internal("Hub DB mutex poisoned".to_string()))?;
        let task = load_task_by_key(&conn, &key)
            .map_err(HubError::Internal)?
            .ok_or_else(|| HubError::NotFound(format!("Task '{key}' not found")))?;
        let project = load_project_by_id(&conn, &task.project_id).map_err(HubError::Internal)?;
        let linked_sessions =
            load_sessions_for_task(&conn, &task.task_key).map_err(HubError::Internal)?;
        let recent_decisions_and_blockers =
            load_recent_decisions_and_blockers(&conn, &task.project_id, 10)
                .map_err(HubError::Internal)?;
        let portal_url = task_portal_url(&task.task_key);
        let tailscale_url = task_tailscale_url(&task.task_key);
        Ok(HubTaskContextResponse {
            task,
            project,
            linked_sessions,
            recent_decisions_and_blockers,
            portal_url,
            tailscale_url,
        })
    }

    pub fn create_task(
        &self,
        req: CreateTaskRequest,
        channel: &str,
        actor: &str,
    ) -> Result<HubTask, HubError> {
        let title = req.title.trim();
        if title.is_empty() {
            return Err(HubError::BadRequest(
                "Task title cannot be empty".to_string(),
            ));
        }
        let project_id = if req.project_id.trim().is_empty() {
            "pits".to_string()
        } else {
            req.project_id.trim().to_ascii_lowercase()
        };
        let status = req
            .status
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "backlog".to_string());
        if !HUB_TASK_STATUSES.contains(&status.as_str()) {
            return Err(HubError::BadRequest(format!(
                "Invalid task status '{}'; allowed: {}",
                status,
                HUB_TASK_STATUSES.join(", ")
            )));
        }
        let priority = req
            .priority
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "p2_medium".to_string());
        if !HUB_TASK_PRIORITIES.contains(&priority.as_str()) {
            return Err(HubError::BadRequest(format!(
                "Invalid task priority '{}'; allowed: {}",
                priority,
                HUB_TASK_PRIORITIES.join(", ")
            )));
        }

        let conn = self
            .conn
            .lock()
            .map_err(|_| HubError::Internal("Hub DB mutex poisoned".to_string()))?;
        let now = Utc::now().to_rfc3339();

        // Protocol-level hard gate on MCP channel
        if channel == "mcp" {
            if status == "done" {
                let _ = insert_audit_event(
                    &conn,
                    "task",
                    "NEW",
                    "forbidden_attempt",
                    channel,
                    actor,
                    &json!({ "attemptedStatus": "done", "title": title }),
                    &now,
                );
                return Err(HubError::Forbidden(
                    FORBIDDEN_HUMAN_ACCEPTANCE_MSG.to_string(),
                ));
            }
            if status != "backlog" && status != "ready" {
                return Err(HubError::BadRequest(format!(
                    "MCP hub_create_task only allows initial status 'backlog' or 'ready' (got '{}')",
                    status
                )));
            }
        }

        // Ensure project exists
        if load_project_by_id(&conn, &project_id)
            .map_err(HubError::Internal)?
            .is_none()
        {
            return Err(HubError::NotFound(format!(
                "Project '{}' not found",
                project_id
            )));
        }

        let tags = normalize_tags(&req.tags);
        let mut checklist = req.ac_checklist;
        for (idx, item) in checklist.iter_mut().enumerate() {
            if item.id.trim().is_empty() {
                item.id = format!("ac-{}", idx + 1);
            }
            if channel == "mcp" {
                item.human_verified = false;
            }
        }
        let owner = req
            .owner
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("pitsadmin")
            .to_string();

        let tmp_key = format!("__alloc_{}", Uuid::new_v4());
        conn.exec_batch("BEGIN IMMEDIATE;")
            .map_err(HubError::Internal)?;

        let insert_res = (|| -> Result<String, String> {
            conn.execute(
                r#"INSERT INTO hub_tasks (
                    task_key, project_id, title, status, priority, tags_json, workspace_path,
                    dispatch_target, acceptance_summary, ac_checklist_json, owner, due_date,
                    last_session_at, body_md, catalog_refs, created_at, updated_at, sync_pending
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, ?13, ?14, ?15, ?16, 1);"#,
                &[
                    SqlVal::Text(tmp_key.clone()),
                    SqlVal::Text(project_id.clone()),
                    SqlVal::Text(title.to_string()),
                    SqlVal::Text(status.clone()),
                    SqlVal::Text(priority.clone()),
                    SqlVal::Text(serde_json::to_string(&tags).unwrap_or_else(|_| "[]".into())),
                    SqlVal::Text(req.workspace_path.trim().to_string()),
                    SqlVal::Text(req.dispatch_target.trim().to_string()),
                    SqlVal::Text(req.acceptance_summary.trim().to_string()),
                    SqlVal::Text(serde_json::to_string(&checklist).unwrap_or_else(|_| "[]".into())),
                    SqlVal::Text(owner),
                    SqlVal::opt_text(req.due_date.as_deref().map(str::trim).filter(|s| !s.is_empty())),
                    SqlVal::Text(req.body_md.clone()),
                    SqlVal::Text(
                        serde_json::to_string(&req.catalog_refs).unwrap_or_else(|_| "[]".into()),
                    ),
                    SqlVal::Text(now.clone()),
                    SqlVal::Text(now.clone()),
                ],
            )?;

            let rows = conn.query_rows(
                "SELECT seq_id FROM hub_tasks WHERE task_key = ?1 LIMIT 1;",
                &[SqlVal::Text(tmp_key.clone())],
            )?;
            let seq_id = rows
                .first()
                .and_then(|r| r.first())
                .and_then(|v| v.as_deref())
                .and_then(|s| s.parse::<i64>().ok())
                .ok_or_else(|| "Failed to allocate AGY seq_id".to_string())?;
            let task_key = format!("AGY-{seq_id}");
            conn.execute(
                "UPDATE hub_tasks SET task_key = ?1 WHERE seq_id = ?2;",
                &[SqlVal::Text(task_key.clone()), SqlVal::Int(seq_id)],
            )?;
            insert_audit_event(
                &conn,
                "task",
                &task_key,
                "create",
                channel,
                actor,
                &json!({ "title": title, "status": status, "tags": tags }),
                &now,
            )?;
            Ok(task_key)
        })();

        let task_key = match insert_res {
            Ok(k) => {
                conn.exec_batch("COMMIT;").map_err(HubError::Internal)?;
                k
            }
            Err(e) => {
                let _ = conn.exec_batch("ROLLBACK;");
                return Err(HubError::Internal(e));
            }
        };

        let task = load_task_by_key(&conn, &task_key)
            .map_err(HubError::Internal)?
            .ok_or_else(|| HubError::Internal("Created task missing".to_string()))?;
        if self.fts_enabled {
            let _ = upsert_fts_task(&conn, &task);
        }
        Ok(task)
    }

    pub fn update_task(
        &self,
        task_key: &str,
        req: UpdateTaskRequest,
        channel: &str,
        actor: &str,
    ) -> Result<HubTask, HubError> {
        let key = normalize_task_key(task_key);
        let conn = self
            .conn
            .lock()
            .map_err(|_| HubError::Internal("Hub DB mutex poisoned".to_string()))?;
        let now = Utc::now().to_rfc3339();

        let existing = load_task_by_key(&conn, &key)
            .map_err(HubError::Internal)?
            .ok_or_else(|| HubError::NotFound(format!("Task '{key}' not found")))?;

        // Protocol-level hard gate on MCP channel
        if channel == "mcp" {
            let tries_done = req
                .status
                .as_deref()
                .map(|s| s.trim().eq_ignore_ascii_case("done"))
                .unwrap_or(false);
            let tries_accept = req.accepted == Some(true);
            let tries_human_verify = req.ac_checklist.as_ref().map_or(false, |incoming| {
                incoming.iter().any(|item| {
                    item.human_verified
                        && !existing
                            .ac_checklist
                            .iter()
                            .any(|old| old.id == item.id && old.human_verified)
                })
            });
            if tries_done || tries_accept || tries_human_verify {
                let _ = insert_audit_event(
                    &conn,
                    "task",
                    &key,
                    "forbidden_attempt",
                    channel,
                    actor,
                    &json!({
                        "attemptedStatus": req.status,
                        "attemptedAccepted": req.accepted,
                        "attemptedHumanVerify": tries_human_verify
                    }),
                    &now,
                );
                return Err(HubError::Forbidden(
                    FORBIDDEN_HUMAN_ACCEPTANCE_MSG.to_string(),
                ));
            }
        }

        let new_status = match req.status.as_deref().map(|s| s.trim().to_ascii_lowercase()) {
            Some(s) if !s.is_empty() => {
                if !HUB_TASK_STATUSES.contains(&s.as_str()) {
                    return Err(HubError::BadRequest(format!(
                        "Invalid task status '{}'; allowed: {}",
                        s,
                        HUB_TASK_STATUSES.join(", ")
                    )));
                }
                s
            }
            _ => existing.status.clone(),
        };
        let new_priority = match req.priority.as_deref().map(|s| s.trim().to_ascii_lowercase()) {
            Some(p) if !p.is_empty() => {
                if !HUB_TASK_PRIORITIES.contains(&p.as_str()) {
                    return Err(HubError::BadRequest(format!(
                        "Invalid task priority '{}'; allowed: {}",
                        p,
                        HUB_TASK_PRIORITIES.join(", ")
                    )));
                }
                p
            }
            _ => existing.priority.clone(),
        };
        let new_title = req
            .title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| existing.title.clone());
        let new_tags = req
            .tags
            .as_ref()
            .map(|t| normalize_tags(t))
            .unwrap_or_else(|| existing.tags.clone());
        let new_ws = req
            .workspace_path
            .unwrap_or_else(|| existing.workspace_path.clone());
        let new_dispatch = req
            .dispatch_target
            .unwrap_or_else(|| existing.dispatch_target.clone());
        let new_ac_summary = req
            .acceptance_summary
            .unwrap_or_else(|| existing.acceptance_summary.clone());
        let new_checklist = req
            .ac_checklist
            .unwrap_or_else(|| existing.ac_checklist.clone());
        let new_owner = req.owner.unwrap_or_else(|| existing.owner.clone());
        let new_due = match req.due_date {
            Some(d) => d.filter(|s| !s.trim().is_empty()),
            None => existing.due_date.clone(),
        };
        let new_body = req.body_md.unwrap_or_else(|| existing.body_md.clone());
        let new_refs = req
            .catalog_refs
            .unwrap_or_else(|| existing.catalog_refs.clone());

        conn.execute(
            r#"UPDATE hub_tasks SET
                title = ?1,
                status = ?2,
                priority = ?3,
                tags_json = ?4,
                workspace_path = ?5,
                dispatch_target = ?6,
                acceptance_summary = ?7,
                ac_checklist_json = ?8,
                owner = ?9,
                due_date = ?10,
                body_md = ?11,
                catalog_refs = ?12,
                updated_at = ?13,
                sync_pending = 1
            WHERE task_key = ?14;"#,
            &[
                SqlVal::Text(new_title),
                SqlVal::Text(new_status.clone()),
                SqlVal::Text(new_priority),
                SqlVal::Text(serde_json::to_string(&new_tags).unwrap_or_else(|_| "[]".into())),
                SqlVal::Text(new_ws),
                SqlVal::Text(new_dispatch),
                SqlVal::Text(new_ac_summary),
                SqlVal::Text(
                    serde_json::to_string(&new_checklist).unwrap_or_else(|_| "[]".into()),
                ),
                SqlVal::Text(new_owner),
                SqlVal::opt_text(new_due.as_deref()),
                SqlVal::Text(new_body),
                SqlVal::Text(serde_json::to_string(&new_refs).unwrap_or_else(|_| "[]".into())),
                SqlVal::Text(now.clone()),
                SqlVal::Text(key.clone()),
            ],
        )
        .map_err(HubError::Internal)?;

        insert_audit_event(
            &conn,
            "task",
            &key,
            "update",
            channel,
            actor,
            &json!({ "prevStatus": existing.status, "newStatus": new_status }),
            &now,
        )
        .map_err(HubError::Internal)?;

        let updated = load_task_by_key(&conn, &key)
            .map_err(HubError::Internal)?
            .ok_or_else(|| HubError::Internal("Updated task missing".to_string()))?;
        if self.fts_enabled {
            let _ = upsert_fts_task(&conn, &updated);
        }
        Ok(updated)
    }

    pub fn pickup_task(
        &self,
        req: PickupTaskRequest,
        channel: &str,
        actor: &str,
    ) -> Result<HubTask, HubError> {
        let key = normalize_task_key(&req.task_key);
        let conn = self
            .conn
            .lock()
            .map_err(|_| HubError::Internal("Hub DB mutex poisoned".to_string()))?;
        let now = Utc::now().to_rfc3339();

        // Protocol-level hard gate
        if channel == "mcp"
            && (req
                .status
                .as_deref()
                .map(|s| s.trim().eq_ignore_ascii_case("done"))
                .unwrap_or(false)
                || req.accepted == Some(true))
        {
            let _ = insert_audit_event(
                &conn,
                "task",
                &key,
                "forbidden_attempt",
                channel,
                actor,
                &json!({ "op": "pickup", "attemptedStatus": req.status, "attemptedAccepted": req.accepted }),
                &now,
            );
            return Err(HubError::Forbidden(
                FORBIDDEN_HUMAN_ACCEPTANCE_MSG.to_string(),
            ));
        }

        let existing = load_task_by_key(&conn, &key)
            .map_err(HubError::Internal)?
            .ok_or_else(|| HubError::NotFound(format!("Task '{key}' not found")))?;

        let new_ws = req
            .workspace_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| existing.workspace_path.clone());
        let effective_actor = req
            .actor
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(actor);

        conn.execute(
            r#"UPDATE hub_tasks SET
                status = 'in_progress',
                workspace_path = ?1,
                last_session_at = ?2,
                updated_at = ?2,
                sync_pending = 1
            WHERE task_key = ?3;"#,
            &[
                SqlVal::Text(new_ws.clone()),
                SqlVal::Text(now.clone()),
                SqlVal::Text(key.clone()),
            ],
        )
        .map_err(HubError::Internal)?;

        insert_audit_event(
            &conn,
            "task",
            &key,
            "pickup",
            channel,
            effective_actor,
            &json!({
                "prevStatus": existing.status,
                "workspacePath": new_ws,
                "conversationId": req.conversation_id.unwrap_or_default()
            }),
            &now,
        )
        .map_err(HubError::Internal)?;

        let updated = load_task_by_key(&conn, &key)
            .map_err(HubError::Internal)?
            .ok_or_else(|| HubError::Internal("Picked up task missing".to_string()))?;
        if self.fts_enabled {
            let _ = upsert_fts_task(&conn, &updated);
        }
        Ok(updated)
    }

    // -----------------------------------------------------------------------
    // Sessions, Handoffs & Human Acceptance Gate
    // -----------------------------------------------------------------------

    pub fn query_sessions(
        &self,
        filter: &SessionQueryFilter,
    ) -> Result<Vec<HubSessionLog>, HubError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| HubError::Internal("Hub DB mutex poisoned".to_string()))?;
        let mut sessions = load_all_sessions(&conn).map_err(HubError::Internal)?;

        if let Some(pid) = filter
            .project_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            sessions.retain(|s| s.project_id.eq_ignore_ascii_case(pid));
        }
        if let Some(lt) = filter
            .log_type
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            sessions.retain(|s| s.log_type.eq_ignore_ascii_case(lt));
        }
        if let Some(tag_raw) = filter
            .tag
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let required: Vec<String> = tag_raw
                .split(',')
                .map(|p| p.trim().to_ascii_lowercase())
                .filter(|p| !p.is_empty())
                .collect();
            if !required.is_empty() {
                sessions.retain(|s| {
                    required
                        .iter()
                        .all(|t| s.tags.iter().any(|x| x.eq_ignore_ascii_case(t)))
                });
            }
        }
        if let Some(tk) = filter
            .task_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let norm = normalize_task_key(tk);
            sessions.retain(|s| s.task_keys.iter().any(|k| k == &norm));
        }
        if let Some(acc) = filter.accepted {
            sessions.retain(|s| s.accepted == acc);
        }
        if let Some(lim) = filter.limit {
            sessions.truncate(lim);
        }
        Ok(sessions)
    }

    pub fn submit_handoff(
        &self,
        req: SubmitHandoffRequest,
        channel: &str,
        actor: &str,
    ) -> Result<(HubSessionLog, Vec<HubTask>), HubError> {
        let summary = req.summary.trim();
        if summary.is_empty() {
            return Err(HubError::BadRequest(
                "Handoff summary cannot be empty".to_string(),
            ));
        }
        if req.task_keys.is_empty() {
            return Err(HubError::BadRequest(
                "submit_handoff requires at least one taskKey".to_string(),
            ));
        }

        let target_status = req
            .target_status
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "ready_for_review".to_string());

        let conn = self
            .conn
            .lock()
            .map_err(|_| HubError::Internal("Hub DB mutex poisoned".to_string()))?;
        let now = Utc::now().to_rfc3339();

        // Protocol-level hard gate: submit_handoff can NEVER mark done or accepted=true
        if target_status == "done" || req.accepted == Some(true) {
            let _ = insert_audit_event(
                &conn,
                "session_log",
                req.task_keys.first().map(String::as_str).unwrap_or("NONE"),
                "forbidden_attempt",
                channel,
                actor,
                &json!({
                    "op": "submit_handoff",
                    "attemptedStatus": target_status,
                    "attemptedAccepted": req.accepted
                }),
                &now,
            );
            return Err(HubError::Forbidden(
                FORBIDDEN_HUMAN_ACCEPTANCE_MSG.to_string(),
            ));
        }
        if target_status != "ready_for_review" && target_status != "blocked" {
            return Err(HubError::BadRequest(format!(
                "submit_handoff targetStatus must be 'ready_for_review' or 'blocked' (got '{}')",
                target_status
            )));
        }

        let normalized_keys: Vec<String> = req
            .task_keys
            .iter()
            .map(|k| normalize_task_key(k))
            .filter(|k| !k.is_empty())
            .collect();
        let mut tasks_before = Vec::with_capacity(normalized_keys.len());
        for k in &normalized_keys {
            let t = load_task_by_key(&conn, k)
                .map_err(HubError::Internal)?
                .ok_or_else(|| HubError::NotFound(format!("Task '{k}' not found")))?;
            tasks_before.push(t);
        }

        let project_id = if req.project_id.trim().is_empty() {
            tasks_before
                .first()
                .map(|t| t.project_id.clone())
                .unwrap_or_else(|| "pits".to_string())
        } else {
            req.project_id.trim().to_ascii_lowercase()
        };

        let mut merged_tags = normalize_tags(&req.tags);
        if merged_tags.is_empty() {
            for t in &tasks_before {
                for tag in &t.tags {
                    if !merged_tags.contains(tag) {
                        merged_tags.push(tag.clone());
                    }
                }
            }
        }

        let session_id = Uuid::new_v4().to_string();
        let session_date = req
            .session_date
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());

        conn.exec_batch("BEGIN IMMEDIATE;")
            .map_err(HubError::Internal)?;

        let tx_res = (|| -> Result<(), String> {
            conn.execute(
                r#"INSERT INTO hub_session_logs (
                    id, project_id, summary, log_type, tags_json, workspace_path, conversation_id,
                    commit_refs_json, evidence_refs_json, body_md, accepted, accepted_by, accepted_at,
                    review_note, session_date, created_at, updated_at, sync_pending
                ) VALUES (?1, ?2, ?3, 'session_handoff', ?4, ?5, ?6, ?7, ?8, ?9, 0, NULL, NULL, '', ?10, ?11, ?12, 1);"#,
                &[
                    SqlVal::Text(session_id.clone()),
                    SqlVal::Text(project_id.clone()),
                    SqlVal::Text(summary.to_string()),
                    SqlVal::Text(
                        serde_json::to_string(&merged_tags).unwrap_or_else(|_| "[]".into()),
                    ),
                    SqlVal::Text(req.workspace_path.trim().to_string()),
                    SqlVal::Text(req.conversation_id.trim().to_string()),
                    SqlVal::Text(
                        serde_json::to_string(&req.commit_refs).unwrap_or_else(|_| "[]".into()),
                    ),
                    SqlVal::Text(
                        serde_json::to_string(&req.evidence_refs).unwrap_or_else(|_| "[]".into()),
                    ),
                    SqlVal::Text(req.body_md.clone()),
                    SqlVal::Text(session_date.clone()),
                    SqlVal::Text(now.clone()),
                    SqlVal::Text(now.clone()),
                ],
            )?;

            for t in &tasks_before {
                conn.execute(
                    "INSERT OR IGNORE INTO hub_session_log_tasks (session_log_id, task_key) VALUES (?1, ?2);",
                    &[SqlVal::Text(session_id.clone()), SqlVal::Text(t.task_key.clone())],
                )?;

                let mut checklist = t.ac_checklist.clone();
                for item in &mut checklist {
                    if req.agent_verified_ac_ids.iter().any(|id| id == &item.id) {
                        item.agent_verified = true;
                    }
                }
                let ws_val = if req.workspace_path.trim().is_empty() {
                    t.workspace_path.clone()
                } else {
                    req.workspace_path.trim().to_string()
                };

                conn.execute(
                    r#"UPDATE hub_tasks SET
                        status = ?1,
                        workspace_path = ?2,
                        ac_checklist_json = ?3,
                        last_session_at = ?4,
                        updated_at = ?4,
                        sync_pending = 1
                    WHERE task_key = ?5;"#,
                    &[
                        SqlVal::Text(target_status.clone()),
                        SqlVal::Text(ws_val),
                        SqlVal::Text(
                            serde_json::to_string(&checklist).unwrap_or_else(|_| "[]".into()),
                        ),
                        SqlVal::Text(now.clone()),
                        SqlVal::Text(t.task_key.clone()),
                    ],
                )?;

                insert_audit_event(
                    &conn,
                    "task",
                    &t.task_key,
                    "submit_handoff",
                    channel,
                    actor,
                    &json!({
                        "sessionId": session_id,
                        "targetStatus": target_status,
                        "commitRefs": req.commit_refs
                    }),
                    &now,
                )?;
            }

            insert_audit_event(
                &conn,
                "session_log",
                &session_id,
                "submit_handoff",
                channel,
                actor,
                &json!({ "taskKeys": normalized_keys, "summary": summary }),
                &now,
            )?;

            Ok(())
        })();

        if let Err(e) = tx_res {
            let _ = conn.exec_batch("ROLLBACK;");
            return Err(HubError::Internal(e));
        }
        conn.exec_batch("COMMIT;").map_err(HubError::Internal)?;

        let session = load_session_by_id(&conn, &session_id)
            .map_err(HubError::Internal)?
            .ok_or_else(|| HubError::Internal("Handoff session missing".to_string()))?;
        let mut updated_tasks = Vec::with_capacity(normalized_keys.len());
        for k in &normalized_keys {
            if let Some(t) = load_task_by_key(&conn, k).map_err(HubError::Internal)? {
                if self.fts_enabled {
                    let _ = upsert_fts_task(&conn, &t);
                }
                updated_tasks.push(t);
            }
        }
        if self.fts_enabled {
            let _ = upsert_fts_session(&conn, &session);
        }
        Ok((session, updated_tasks))
    }

    pub fn create_session_log(
        &self,
        req: CreateSessionLogRequest,
        channel: &str,
        actor: &str,
    ) -> Result<HubSessionLog, HubError> {
        let summary = req.summary.trim();
        if summary.is_empty() {
            return Err(HubError::BadRequest(
                "Session log summary cannot be empty".to_string(),
            ));
        }
        let log_type = req.log_type.trim().to_ascii_lowercase();
        if !HUB_LOG_TYPES.contains(&log_type.as_str()) {
            return Err(HubError::BadRequest(format!(
                "Invalid logType '{}'; allowed: {}",
                log_type,
                HUB_LOG_TYPES.join(", ")
            )));
        }

        let conn = self
            .conn
            .lock()
            .map_err(|_| HubError::Internal("Hub DB mutex poisoned".to_string()))?;
        let now = Utc::now().to_rfc3339();

        // Protocol-level hard gate on MCP channel
        if channel == "mcp" && (req.accepted == Some(true) || log_type == "acceptance") {
            let _ = insert_audit_event(
                &conn,
                "session_log",
                "NEW",
                "forbidden_attempt",
                channel,
                actor,
                &json!({ "op": "create_session_log", "logType": log_type, "attemptedAccepted": req.accepted }),
                &now,
            );
            return Err(HubError::Forbidden(
                FORBIDDEN_HUMAN_ACCEPTANCE_MSG.to_string(),
            ));
        }

        let project_id = if req.project_id.trim().is_empty() {
            "pits".to_string()
        } else {
            req.project_id.trim().to_ascii_lowercase()
        };
        if load_project_by_id(&conn, &project_id)
            .map_err(HubError::Internal)?
            .is_none()
        {
            return Err(HubError::NotFound(format!(
                "Project '{}' not found",
                project_id
            )));
        }

        let normalized_keys: Vec<String> = req
            .task_keys
            .iter()
            .map(|k| normalize_task_key(k))
            .filter(|k| !k.is_empty())
            .collect();
        for k in &normalized_keys {
            if load_task_by_key(&conn, k)
                .map_err(HubError::Internal)?
                .is_none()
            {
                return Err(HubError::NotFound(format!("Linked task '{k}' not found")));
            }
        }

        let id = Uuid::new_v4().to_string();
        let tags = normalize_tags(&req.tags);
        let session_date = req
            .session_date
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());

        conn.execute(
            r#"INSERT INTO hub_session_logs (
                id, project_id, summary, log_type, tags_json, workspace_path, conversation_id,
                commit_refs_json, evidence_refs_json, body_md, accepted, accepted_by, accepted_at,
                review_note, session_date, created_at, updated_at, sync_pending
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, NULL, NULL, '', ?11, ?12, ?13, 1);"#,
            &[
                SqlVal::Text(id.clone()),
                SqlVal::Text(project_id),
                SqlVal::Text(summary.to_string()),
                SqlVal::Text(log_type.clone()),
                SqlVal::Text(serde_json::to_string(&tags).unwrap_or_else(|_| "[]".into())),
                SqlVal::Text(req.workspace_path.trim().to_string()),
                SqlVal::Text(req.conversation_id.trim().to_string()),
                SqlVal::Text(
                    serde_json::to_string(&req.commit_refs).unwrap_or_else(|_| "[]".into()),
                ),
                SqlVal::Text(
                    serde_json::to_string(&req.evidence_refs).unwrap_or_else(|_| "[]".into()),
                ),
                SqlVal::Text(req.body_md),
                SqlVal::Text(session_date),
                SqlVal::Text(now.clone()),
                SqlVal::Text(now.clone()),
            ],
        )
        .map_err(HubError::Internal)?;

        for k in &normalized_keys {
            conn.execute(
                "INSERT OR IGNORE INTO hub_session_log_tasks (session_log_id, task_key) VALUES (?1, ?2);",
                &[SqlVal::Text(id.clone()), SqlVal::Text(k.clone())],
            )
            .map_err(HubError::Internal)?;
        }

        insert_audit_event(
            &conn,
            "session_log",
            &id,
            "create",
            channel,
            actor,
            &json!({ "logType": log_type, "taskKeys": normalized_keys }),
            &now,
        )
        .map_err(HubError::Internal)?;

        let created = load_session_by_id(&conn, &id)
            .map_err(HubError::Internal)?
            .ok_or_else(|| HubError::Internal("Created session missing".to_string()))?;
        if self.fts_enabled {
            let _ = upsert_fts_session(&conn, &created);
        }
        Ok(created)
    }

    pub fn accept_session(
        &self,
        session_id: &str,
        req: AcceptSessionRequest,
        channel: &str,
        actor: &str,
        auth_source: &str,
    ) -> Result<HumanReviewOutcome, HubError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| HubError::Internal("Hub DB mutex poisoned".to_string()))?;
        let now = Utc::now().to_rfc3339();

        // Protocol-level hard gate: ONLY portal_ui can accept!
        if channel != "portal_ui" {
            let _ = insert_audit_event(
                &conn,
                "session_log",
                session_id,
                "forbidden_attempt",
                channel,
                actor,
                &json!({ "op": "accept_session" }),
                &now,
            );
            return Err(HubError::Forbidden(
                FORBIDDEN_HUMAN_ACCEPTANCE_MSG.to_string(),
            ));
        }

        let existing = load_session_by_id(&conn, session_id)
            .map_err(HubError::Internal)?
            .ok_or_else(|| HubError::NotFound(format!("Session log '{session_id}' not found")))?;

        conn.exec_batch("BEGIN IMMEDIATE;")
            .map_err(HubError::Internal)?;

        let tx_res = (|| -> Result<(), String> {
            conn.execute(
                r#"UPDATE hub_session_logs SET
                    accepted = 1,
                    accepted_by = ?1,
                    accepted_at = ?2,
                    review_note = ?3,
                    updated_at = ?2,
                    sync_pending = 1
                WHERE id = ?4;"#,
                &[
                    SqlVal::Text(actor.to_string()),
                    SqlVal::Text(now.clone()),
                    SqlVal::Text(req.review_note.trim().to_string()),
                    SqlVal::Text(existing.id.clone()),
                ],
            )?;

            for tk in &existing.task_keys {
                if let Some(task) = load_task_by_key(&conn, tk)? {
                    let mut checklist = task.ac_checklist.clone();
                    let mut ac_changed = false;
                    for item in &mut checklist {
                        if req.verified_ac_ids.iter().any(|id| id == &item.id)
                            || (req.verified_ac_ids.is_empty() && req.mark_tasks_done)
                        {
                            if !item.human_verified {
                                item.human_verified = true;
                                ac_changed = true;
                            }
                        }
                    }
                    let next_status = if req.mark_tasks_done {
                        "done".to_string()
                    } else {
                        task.status.clone()
                    };
                    if req.mark_tasks_done || ac_changed {
                        conn.execute(
                            r#"UPDATE hub_tasks SET
                                status = ?1,
                                ac_checklist_json = ?2,
                                updated_at = ?3,
                                sync_pending = 1
                            WHERE task_key = ?4;"#,
                            &[
                                SqlVal::Text(next_status.clone()),
                                SqlVal::Text(
                                    serde_json::to_string(&checklist)
                                        .unwrap_or_else(|_| "[]".into()),
                                ),
                                SqlVal::Text(now.clone()),
                                SqlVal::Text(tk.clone()),
                            ],
                        )?;
                        insert_audit_event(
                            &conn,
                            "task",
                            tk,
                            "accept",
                            channel,
                            actor,
                            &json!({
                                "sessionId": existing.id,
                                "markTasksDone": req.mark_tasks_done,
                                "newStatus": next_status,
                                "authSource": auth_source,
                                "reviewNote": req.review_note
                            }),
                            &now,
                        )?;
                    }
                }
            }

            insert_audit_event(
                &conn,
                "session_log",
                &existing.id,
                "accept",
                channel,
                actor,
                &json!({
                    "markTasksDone": req.mark_tasks_done,
                    "verifiedAcIds": req.verified_ac_ids,
                    "authSource": auth_source,
                    "reviewNote": req.review_note
                }),
                &now,
            )?;

            Ok(())
        })();

        if let Err(e) = tx_res {
            let _ = conn.exec_batch("ROLLBACK;");
            return Err(HubError::Internal(e));
        }
        conn.exec_batch("COMMIT;").map_err(HubError::Internal)?;

        let session = load_session_by_id(&conn, &existing.id)
            .map_err(HubError::Internal)?
            .ok_or_else(|| HubError::Internal("Accepted session missing".to_string()))?;
        let mut updated_tasks = Vec::new();
        for tk in &session.task_keys {
            if let Some(t) = load_task_by_key(&conn, tk).map_err(HubError::Internal)? {
                if self.fts_enabled {
                    let _ = upsert_fts_task(&conn, &t);
                }
                updated_tasks.push(t);
            }
        }
        if self.fts_enabled {
            let _ = upsert_fts_session(&conn, &session);
        }

        Ok(HumanReviewOutcome {
            session,
            updated_tasks,
            auth_source: auth_source.to_string(),
            actor: actor.to_string(),
        })
    }

    pub fn reject_session(
        &self,
        session_id: &str,
        req: RejectSessionRequest,
        channel: &str,
        actor: &str,
        auth_source: &str,
    ) -> Result<HumanReviewOutcome, HubError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| HubError::Internal("Hub DB mutex poisoned".to_string()))?;
        let now = Utc::now().to_rfc3339();

        if channel != "portal_ui" {
            let _ = insert_audit_event(
                &conn,
                "session_log",
                session_id,
                "forbidden_attempt",
                channel,
                actor,
                &json!({ "op": "reject_session" }),
                &now,
            );
            return Err(HubError::Forbidden(
                FORBIDDEN_HUMAN_ACCEPTANCE_MSG.to_string(),
            ));
        }

        let target_status = req
            .target_task_status
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "in_progress".to_string());
        if target_status != "in_progress" && target_status != "blocked" {
            return Err(HubError::BadRequest(format!(
                "reject targetTaskStatus must be 'in_progress' or 'blocked' (got '{}')",
                target_status
            )));
        }

        let existing = load_session_by_id(&conn, session_id)
            .map_err(HubError::Internal)?
            .ok_or_else(|| HubError::NotFound(format!("Session log '{session_id}' not found")))?;

        conn.exec_batch("BEGIN IMMEDIATE;")
            .map_err(HubError::Internal)?;

        let tx_res = (|| -> Result<(), String> {
            conn.execute(
                r#"UPDATE hub_session_logs SET
                    accepted = 0,
                    accepted_by = NULL,
                    accepted_at = NULL,
                    review_note = ?1,
                    updated_at = ?2,
                    sync_pending = 1
                WHERE id = ?3;"#,
                &[
                    SqlVal::Text(req.review_note.trim().to_string()),
                    SqlVal::Text(now.clone()),
                    SqlVal::Text(existing.id.clone()),
                ],
            )?;

            for tk in &existing.task_keys {
                conn.execute(
                    r#"UPDATE hub_tasks SET
                        status = ?1,
                        updated_at = ?2,
                        sync_pending = 1
                    WHERE task_key = ?3;"#,
                    &[
                        SqlVal::Text(target_status.clone()),
                        SqlVal::Text(now.clone()),
                        SqlVal::Text(tk.clone()),
                    ],
                )?;
                insert_audit_event(
                    &conn,
                    "task",
                    tk,
                    "reject",
                    channel,
                    actor,
                    &json!({
                        "sessionId": existing.id,
                        "targetStatus": target_status,
                        "authSource": auth_source,
                        "reviewNote": req.review_note
                    }),
                    &now,
                )?;
            }

            insert_audit_event(
                &conn,
                "session_log",
                &existing.id,
                "reject",
                channel,
                actor,
                &json!({
                    "targetTaskStatus": target_status,
                    "authSource": auth_source,
                    "reviewNote": req.review_note
                }),
                &now,
            )?;

            Ok(())
        })();

        if let Err(e) = tx_res {
            let _ = conn.exec_batch("ROLLBACK;");
            return Err(HubError::Internal(e));
        }
        conn.exec_batch("COMMIT;").map_err(HubError::Internal)?;

        let session = load_session_by_id(&conn, &existing.id)
            .map_err(HubError::Internal)?
            .ok_or_else(|| HubError::Internal("Rejected session missing".to_string()))?;
        let mut updated_tasks = Vec::new();
        for tk in &session.task_keys {
            if let Some(t) = load_task_by_key(&conn, tk).map_err(HubError::Internal)? {
                if self.fts_enabled {
                    let _ = upsert_fts_task(&conn, &t);
                }
                updated_tasks.push(t);
            }
        }
        if self.fts_enabled {
            let _ = upsert_fts_session(&conn, &session);
        }

        Ok(HumanReviewOutcome {
            session,
            updated_tasks,
            auth_source: auth_source.to_string(),
            actor: actor.to_string(),
        })
    }

    // -----------------------------------------------------------------------
    // Full-Text Search (FTS5 with automatic LIKE fallback)
    // -----------------------------------------------------------------------

    pub fn search(&self, raw_query: &str, limit: usize) -> Result<HubSearchResponse, HubError> {
        let q = raw_query.trim();
        let max_items = limit.clamp(1, 100);
        if q.is_empty() {
            return Ok(HubSearchResponse {
                query: String::new(),
                engine: if self.fts_enabled {
                    "fts5".to_string()
                } else {
                    "like-fallback".to_string()
                },
                tasks: Vec::new(),
                sessions: Vec::new(),
            });
        }

        let conn = self
            .conn
            .lock()
            .map_err(|_| HubError::Internal("Hub DB mutex poisoned".to_string()))?;

        let mut matched_task_keys = Vec::new();
        let mut matched_session_ids = Vec::new();
        let mut used_fts = false;

        if self.fts_enabled {
            if let Some(fts_query) = sanitize_fts5_query(q) {
                if let Ok(rows) = conn.query_rows(
                    "SELECT entity_type, entity_id FROM hub_fts WHERE hub_fts MATCH ?1 ORDER BY bm25(hub_fts) LIMIT ?2;",
                    &[SqlVal::Text(fts_query), SqlVal::Int((max_items * 2) as i64)],
                ) {
                    used_fts = true;
                    for r in rows {
                        let etype = r.first().and_then(|v| v.as_deref()).unwrap_or("");
                        let eid = r.get(1).and_then(|v| v.as_deref()).unwrap_or("");
                        if etype == "task" && !matched_task_keys.iter().any(|x| x == eid) {
                            matched_task_keys.push(eid.to_string());
                        } else if etype == "session"
                            && !matched_session_ids.iter().any(|x| x == eid)
                        {
                            matched_session_ids.push(eid.to_string());
                        }
                    }
                }
            }
        }

        // Also run LIKE fallback when FTS5 unavailable or when substring/Chinese/punctuation query yields zero FTS hits
        let engine = if used_fts && (!matched_task_keys.is_empty() || !matched_session_ids.is_empty())
        {
            "fts5".to_string()
        } else {
            let pattern = format!("%{}%", q.to_ascii_lowercase());
            if let Ok(rows) = conn.query_rows(
                r#"SELECT task_key FROM hub_tasks
                   WHERE lower(task_key) LIKE ?1
                      OR lower(title) LIKE ?1
                      OR lower(tags_json) LIKE ?1
                      OR lower(acceptance_summary) LIKE ?1
                      OR lower(body_md) LIKE ?1
                   ORDER BY seq_id DESC LIMIT ?2;"#,
                &[SqlVal::Text(pattern.clone()), SqlVal::Int(max_items as i64)],
            ) {
                for r in rows {
                    if let Some(k) = r.first().and_then(|v| v.clone()) {
                        if !matched_task_keys.contains(&k) {
                            matched_task_keys.push(k);
                        }
                    }
                }
            }
            if let Ok(rows) = conn.query_rows(
                r#"SELECT id FROM hub_session_logs
                   WHERE lower(summary) LIKE ?1
                      OR lower(tags_json) LIKE ?1
                      OR lower(conversation_id) LIKE ?1
                      OR lower(commit_refs_json) LIKE ?1
                      OR lower(body_md) LIKE ?1
                   ORDER BY created_at DESC LIMIT ?2;"#,
                &[SqlVal::Text(pattern), SqlVal::Int(max_items as i64)],
            ) {
                for r in rows {
                    if let Some(id) = r.first().and_then(|v| v.clone()) {
                        if !matched_session_ids.contains(&id) {
                            matched_session_ids.push(id);
                        }
                    }
                }
            }
            if used_fts {
                "fts5+like".to_string()
            } else {
                "like-fallback".to_string()
            }
        };

        let mut tasks = Vec::new();
        for k in matched_task_keys.into_iter().take(max_items) {
            if let Some(t) = load_task_by_key(&conn, &k).map_err(HubError::Internal)? {
                tasks.push(t);
            }
        }
        let mut sessions = Vec::new();
        for id in matched_session_ids.into_iter().take(max_items) {
            if let Some(s) = load_session_by_id(&conn, &id).map_err(HubError::Internal)? {
                sessions.push(s);
            }
        }

        Ok(HubSearchResponse {
            query: q.to_string(),
            engine,
            tasks,
            sessions,
        })
    }

    pub fn export_markdown(&self) -> Result<String, HubError> {
        let ov = self.overview()?;
        let mut out = String::new();
        out.push_str("# PITS Self-Managed Project Hub Export\n\n");
        out.push_str(&format!("Generated: `{}`\n\n", Utc::now().to_rfc3339()));

        for p in &ov.projects {
            out.push_str(&format!(
                "## Project: {} (`{}`)\n- Status: `{}` | Priority: `{}` | Lead: `{}`\n- Workspace: `{}`\n- Outcome: {}\n\n",
                p.name, p.id, p.status, p.priority, p.lead, p.workspace_path, p.outcome
            ));
        }

        out.push_str("## Tasks\n\n");
        for t in &ov.tasks {
            out.push_str(&format!(
                "### {} — {} (`{}`, `{}`)\n- Tags: `{}`\n- Workspace: `{}` | Dispatch: `{}`\n- Acceptance Summary: {}\n\n",
                t.task_key,
                t.title,
                t.status,
                t.priority,
                t.tags.join(", "),
                t.workspace_path,
                t.dispatch_target,
                t.acceptance_summary
            ));
            if !t.ac_checklist.is_empty() {
                out.push_str("#### AC Checklist\n");
                for ac in &t.ac_checklist {
                    out.push_str(&format!(
                        "- [{}] `{}` {} (agent={}, human={})\n",
                        if ac.human_verified { "x" } else { " " },
                        ac.id,
                        ac.text,
                        ac.agent_verified,
                        ac.human_verified
                    ));
                }
                out.push('\n');
            }
        }

        out.push_str("## Session Logs & Decisions\n\n");
        for s in &ov.recent_sessions {
            out.push_str(&format!(
                "### [{}] {} (`{}`)\n- Date: `{}` | Tasks: `{}` | Accepted: `{}`\n- Conversation: `{}` | Commits: `{}`\n\n{}\n\n",
                s.log_type,
                s.summary,
                s.id,
                s.session_date,
                s.task_keys.join(", "),
                s.accepted,
                s.conversation_id,
                s.commit_refs.join(", "),
                s.body_md
            ));
        }

        Ok(out)
    }

    // -----------------------------------------------------------------------
    // Bi-Directional SQLite WAL <-> CNPG PostgreSQL Outbox Sync
    // -----------------------------------------------------------------------

    pub async fn sync_with_postgres(&self, pg_url_override: Option<&str>) -> HubDbSyncStatus {
        let target_url = pg_url_override
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                self.sync_meta
                    .lock()
                    .ok()
                    .and_then(|m| m.pg_url.clone())
            });

        let Some(pg_url) = target_url else {
            return self.status().unwrap_or_else(|_| empty_sync_status());
        };

        if let Ok(mut meta) = self.sync_meta.lock() {
            meta.pg_url = Some(pg_url.clone());
        }

        let sync_result = self.run_pg_sync_round(&pg_url).await;
        let now = Utc::now().to_rfc3339();
        if let Ok(mut meta) = self.sync_meta.lock() {
            match sync_result {
                Ok(direction) => {
                    meta.postgres_connected = true;
                    meta.last_sync_at = Some(now);
                    meta.last_sync_direction = Some(direction);
                    meta.last_sync_error = None;
                }
                Err(err) => {
                    meta.postgres_connected = false;
                    meta.last_sync_error = Some(err);
                }
            }
        }

        self.status().unwrap_or_else(|_| empty_sync_status())
    }

    async fn run_pg_sync_round(&self, pg_url: &str) -> Result<String, String> {
        let mut pg = PgWireClient::connect(pg_url).await?;

        // 1. Ensure PG schema & optional pg_trgm extension
        let _ = pg.query("CREATE EXTENSION IF NOT EXISTS pg_trgm;").await;
        pg.query(POSTGRES_HUB_SCHEMA).await?;

        // 2. Snapshot local pending outbox rows & link/audit tables
        let (pending_projects, pending_tasks, pending_sessions, links, audits) = {
            let conn = self
                .conn
                .lock()
                .map_err(|_| "Hub DB mutex poisoned".to_string())?;
            let projects: Vec<HubProject> = load_projects(&conn)?
                .into_iter()
                .filter(|p| p.sync_pending)
                .collect();
            let tasks: Vec<HubTask> = load_all_tasks(&conn)?
                .into_iter()
                .filter(|t| t.sync_pending)
                .collect();
            let sessions: Vec<HubSessionLog> = load_all_sessions(&conn)?
                .into_iter()
                .filter(|s| s.sync_pending)
                .collect();
            let links = load_all_session_task_links(&conn)?;
            let audits = load_recent_audit_events(&conn, 200)?;
            (projects, tasks, sessions, links, audits)
        };

        let mut pushed_count = 0usize;

        // 3. Push pending projects
        for p in &pending_projects {
            let sql = format!(
                "INSERT INTO hub_projects (id, name, status, priority, workspace_path, outcome, target_date, lead, body_md, catalog_refs, created_at, updated_at, sync_pending) \
                 VALUES ({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, 0) \
                 ON CONFLICT (id) DO UPDATE SET \
                   name = EXCLUDED.name, status = EXCLUDED.status, priority = EXCLUDED.priority, \
                   workspace_path = EXCLUDED.workspace_path, outcome = EXCLUDED.outcome, \
                   target_date = EXCLUDED.target_date, lead = EXCLUDED.lead, body_md = EXCLUDED.body_md, \
                   catalog_refs = EXCLUDED.catalog_refs, updated_at = EXCLUDED.updated_at, sync_pending = 0 \
                 WHERE EXCLUDED.updated_at >= hub_projects.updated_at;",
                pg_str(&p.id)?,
                pg_str(&p.name)?,
                pg_str(&p.status)?,
                pg_str(&p.priority)?,
                pg_str(&p.workspace_path)?,
                pg_str(&p.outcome)?,
                pg_opt_str(p.target_date.as_deref())?,
                pg_str(&p.lead)?,
                pg_str(&p.body_md)?,
                pg_str(&serde_json::to_string(&p.catalog_refs).unwrap_or_else(|_| "[]".into()))?,
                pg_str(&p.created_at)?,
                pg_str(&p.updated_at)?,
            );
            pg.query(&sql).await?;
            pushed_count += 1;
        }

        // 4. Push pending tasks (preserving seq_id & task_key)
        for t in &pending_tasks {
            let sql = format!(
                "INSERT INTO hub_tasks (seq_id, task_key, project_id, title, status, priority, tags_json, workspace_path, dispatch_target, acceptance_summary, ac_checklist_json, owner, due_date, last_session_at, body_md, catalog_refs, created_at, updated_at, sync_pending) \
                 VALUES ({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, 0) \
                 ON CONFLICT (task_key) DO UPDATE SET \
                   project_id = EXCLUDED.project_id, title = EXCLUDED.title, status = EXCLUDED.status, \
                   priority = EXCLUDED.priority, tags_json = EXCLUDED.tags_json, \
                   workspace_path = EXCLUDED.workspace_path, dispatch_target = EXCLUDED.dispatch_target, \
                   acceptance_summary = EXCLUDED.acceptance_summary, ac_checklist_json = EXCLUDED.ac_checklist_json, \
                   owner = EXCLUDED.owner, due_date = EXCLUDED.due_date, last_session_at = EXCLUDED.last_session_at, \
                   body_md = EXCLUDED.body_md, catalog_refs = EXCLUDED.catalog_refs, \
                   updated_at = EXCLUDED.updated_at, sync_pending = 0 \
                 WHERE EXCLUDED.updated_at >= hub_tasks.updated_at;",
                t.seq_id,
                pg_str(&t.task_key)?,
                pg_str(&t.project_id)?,
                pg_str(&t.title)?,
                pg_str(&t.status)?,
                pg_str(&t.priority)?,
                pg_str(&serde_json::to_string(&t.tags).unwrap_or_else(|_| "[]".into()))?,
                pg_str(&t.workspace_path)?,
                pg_str(&t.dispatch_target)?,
                pg_str(&t.acceptance_summary)?,
                pg_str(&serde_json::to_string(&t.ac_checklist).unwrap_or_else(|_| "[]".into()))?,
                pg_str(&t.owner)?,
                pg_opt_str(t.due_date.as_deref())?,
                pg_opt_str(t.last_session_at.as_deref())?,
                pg_str(&t.body_md)?,
                pg_str(&serde_json::to_string(&t.catalog_refs).unwrap_or_else(|_| "[]".into()))?,
                pg_str(&t.created_at)?,
                pg_str(&t.updated_at)?,
            );
            pg.query(&sql).await?;
            pushed_count += 1;
        }

        // Re-align PG identity sequence after explicit seq_id upserts
        let _ = pg
            .query(
                "SELECT setval(pg_get_serial_sequence('hub_tasks','seq_id'), COALESCE((SELECT MAX(seq_id) FROM hub_tasks), 1), true);",
            )
            .await;

        // 5. Push pending sessions
        for s in &pending_sessions {
            let sql = format!(
                "INSERT INTO hub_session_logs (id, project_id, summary, log_type, tags_json, workspace_path, conversation_id, commit_refs_json, evidence_refs_json, body_md, accepted, accepted_by, accepted_at, review_note, session_date, created_at, updated_at, sync_pending) \
                 VALUES ({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, 0) \
                 ON CONFLICT (id) DO UPDATE SET \
                   summary = EXCLUDED.summary, log_type = EXCLUDED.log_type, tags_json = EXCLUDED.tags_json, \
                   workspace_path = EXCLUDED.workspace_path, conversation_id = EXCLUDED.conversation_id, \
                   commit_refs_json = EXCLUDED.commit_refs_json, evidence_refs_json = EXCLUDED.evidence_refs_json, \
                   body_md = EXCLUDED.body_md, accepted = EXCLUDED.accepted, accepted_by = EXCLUDED.accepted_by, \
                   accepted_at = EXCLUDED.accepted_at, review_note = EXCLUDED.review_note, \
                   session_date = EXCLUDED.session_date, updated_at = EXCLUDED.updated_at, sync_pending = 0 \
                 WHERE EXCLUDED.updated_at >= hub_session_logs.updated_at;",
                pg_str(&s.id)?,
                pg_str(&s.project_id)?,
                pg_str(&s.summary)?,
                pg_str(&s.log_type)?,
                pg_str(&serde_json::to_string(&s.tags).unwrap_or_else(|_| "[]".into()))?,
                pg_str(&s.workspace_path)?,
                pg_str(&s.conversation_id)?,
                pg_str(&serde_json::to_string(&s.commit_refs).unwrap_or_else(|_| "[]".into()))?,
                pg_str(&serde_json::to_string(&s.evidence_refs).unwrap_or_else(|_| "[]".into()))?,
                pg_str(&s.body_md)?,
                if s.accepted { 1 } else { 0 },
                pg_opt_str(s.accepted_by.as_deref())?,
                pg_opt_str(s.accepted_at.as_deref())?,
                pg_str(&s.review_note)?,
                pg_str(&s.session_date)?,
                pg_str(&s.created_at)?,
                pg_str(&s.updated_at)?,
            );
            pg.query(&sql).await?;
            pushed_count += 1;
        }

        // 6. Push links & audit events idempotently
        for (sid, tk) in &links {
            let sql = format!(
                "INSERT INTO hub_session_log_tasks (session_log_id, task_key) VALUES ({}, {}) ON CONFLICT DO NOTHING;",
                pg_str(sid)?,
                pg_str(tk)?
            );
            let _ = pg.query(&sql).await;
        }
        for ev in &audits {
            let sql = format!(
                "INSERT INTO hub_audit_events (id, entity_type, entity_id, action, channel, actor, detail_json, created_at) \
                 VALUES ({}, {}, {}, {}, {}, {}, {}, {}) ON CONFLICT (id) DO NOTHING;",
                pg_str(&ev.id)?,
                pg_str(&ev.entity_type)?,
                pg_str(&ev.entity_id)?,
                pg_str(&ev.action)?,
                pg_str(&ev.channel)?,
                pg_str(&ev.actor)?,
                pg_str(&ev.detail.to_string())?,
                pg_str(&ev.created_at)?,
            );
            let _ = pg.query(&sql).await;
        }

        // 7. Pull remote rows from PG and merge newer/missing into local SQLite
        let remote_projects = pg
            .query("SELECT id, name, status, priority, workspace_path, outcome, target_date, lead, body_md, catalog_refs, created_at, updated_at FROM hub_projects;")
            .await?;
        let remote_tasks = pg
            .query("SELECT seq_id, task_key, project_id, title, status, priority, tags_json, workspace_path, dispatch_target, acceptance_summary, ac_checklist_json, owner, due_date, last_session_at, body_md, catalog_refs, created_at, updated_at FROM hub_tasks ORDER BY seq_id ASC;")
            .await?;
        let remote_sessions = pg
            .query("SELECT id, project_id, summary, log_type, tags_json, workspace_path, conversation_id, commit_refs_json, evidence_refs_json, body_md, accepted, accepted_by, accepted_at, review_note, session_date, created_at, updated_at FROM hub_session_logs;")
            .await?;
        let remote_links = pg
            .query("SELECT session_log_id, task_key FROM hub_session_log_tasks;")
            .await?;
        let remote_audits = pg
            .query("SELECT id, entity_type, entity_id, action, channel, actor, detail_json, created_at FROM hub_audit_events ORDER BY created_at DESC LIMIT 200;")
            .await?;

        pg.close().await;

        let mut pulled_count = 0usize;
        {
            let conn = self
                .conn
                .lock()
                .map_err(|_| "Hub DB mutex poisoned".to_string())?;
            conn.exec_batch("BEGIN IMMEDIATE;")?;

            let merge_res = (|| -> Result<(), String> {
                // Clear sync_pending for rows we successfully pushed (if not modified concurrently)
                for p in &pending_projects {
                    conn.execute(
                        "UPDATE hub_projects SET sync_pending = 0 WHERE id = ?1 AND updated_at = ?2;",
                        &[SqlVal::Text(p.id.clone()), SqlVal::Text(p.updated_at.clone())],
                    )?;
                }
                for t in &pending_tasks {
                    conn.execute(
                        "UPDATE hub_tasks SET sync_pending = 0 WHERE task_key = ?1 AND updated_at = ?2;",
                        &[
                            SqlVal::Text(t.task_key.clone()),
                            SqlVal::Text(t.updated_at.clone()),
                        ],
                    )?;
                }
                for s in &pending_sessions {
                    conn.execute(
                        "UPDATE hub_session_logs SET sync_pending = 0 WHERE id = ?1 AND updated_at = ?2;",
                        &[SqlVal::Text(s.id.clone()), SqlVal::Text(s.updated_at.clone())],
                    )?;
                }

                // Merge remote projects
                for r in &remote_projects {
                    if r.len() < 12 {
                        continue;
                    }
                    let id = col_str(r, 0);
                    let remote_upd = col_str(r, 11);
                    let local_upd = load_project_by_id(&conn, &id)?.map(|p| p.updated_at);
                    if local_upd.as_deref().map_or(true, |lu| remote_upd.as_str() > lu) {
                        conn.execute(
                            r#"INSERT INTO hub_projects (
                                id, name, status, priority, workspace_path, outcome, target_date, lead, body_md, catalog_refs, created_at, updated_at, sync_pending
                            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0)
                            ON CONFLICT(id) DO UPDATE SET
                                name = excluded.name, status = excluded.status, priority = excluded.priority,
                                workspace_path = excluded.workspace_path, outcome = excluded.outcome,
                                target_date = excluded.target_date, lead = excluded.lead, body_md = excluded.body_md,
                                catalog_refs = excluded.catalog_refs, updated_at = excluded.updated_at, sync_pending = 0;"#,
                            &[
                                SqlVal::Text(id),
                                SqlVal::Text(col_str(r, 1)),
                                SqlVal::Text(col_str(r, 2)),
                                SqlVal::Text(col_str(r, 3)),
                                SqlVal::Text(col_str(r, 4)),
                                SqlVal::Text(col_str(r, 5)),
                                SqlVal::opt_text(r.get(6).and_then(|v| v.as_deref())),
                                SqlVal::Text(col_str(r, 7)),
                                SqlVal::Text(col_str(r, 8)),
                                SqlVal::Text(col_str(r, 9)),
                                SqlVal::Text(col_str(r, 10)),
                                SqlVal::Text(remote_upd),
                            ],
                        )?;
                        pulled_count += 1;
                    }
                }

                // Merge remote tasks
                for r in &remote_tasks {
                    if r.len() < 18 {
                        continue;
                    }
                    let seq_id = col_str(r, 0).parse::<i64>().unwrap_or(0);
                    let task_key = col_str(r, 1);
                    let remote_upd = col_str(r, 17);
                    let local_upd = load_task_by_key(&conn, &task_key)?.map(|t| t.updated_at);
                    if seq_id > 0 && local_upd.as_deref().map_or(true, |lu| remote_upd.as_str() > lu) {
                        conn.execute(
                            r#"INSERT INTO hub_tasks (
                                seq_id, task_key, project_id, title, status, priority, tags_json, workspace_path,
                                dispatch_target, acceptance_summary, ac_checklist_json, owner, due_date, last_session_at,
                                body_md, catalog_refs, created_at, updated_at, sync_pending
                            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, 0)
                            ON CONFLICT(task_key) DO UPDATE SET
                                project_id = excluded.project_id, title = excluded.title, status = excluded.status,
                                priority = excluded.priority, tags_json = excluded.tags_json,
                                workspace_path = excluded.workspace_path, dispatch_target = excluded.dispatch_target,
                                acceptance_summary = excluded.acceptance_summary, ac_checklist_json = excluded.ac_checklist_json,
                                owner = excluded.owner, due_date = excluded.due_date, last_session_at = excluded.last_session_at,
                                body_md = excluded.body_md, catalog_refs = excluded.catalog_refs,
                                updated_at = excluded.updated_at, sync_pending = 0;"#,
                            &[
                                SqlVal::Int(seq_id),
                                SqlVal::Text(task_key),
                                SqlVal::Text(col_str(r, 2)),
                                SqlVal::Text(col_str(r, 3)),
                                SqlVal::Text(col_str(r, 4)),
                                SqlVal::Text(col_str(r, 5)),
                                SqlVal::Text(col_str(r, 6)),
                                SqlVal::Text(col_str(r, 7)),
                                SqlVal::Text(col_str(r, 8)),
                                SqlVal::Text(col_str(r, 9)),
                                SqlVal::Text(col_str(r, 10)),
                                SqlVal::Text(col_str(r, 11)),
                                SqlVal::opt_text(r.get(12).and_then(|v| v.as_deref())),
                                SqlVal::opt_text(r.get(13).and_then(|v| v.as_deref())),
                                SqlVal::Text(col_str(r, 14)),
                                SqlVal::Text(col_str(r, 15)),
                                SqlVal::Text(col_str(r, 16)),
                                SqlVal::Text(remote_upd),
                            ],
                        )?;
                        pulled_count += 1;
                    }
                }

                // Merge remote sessions
                for r in &remote_sessions {
                    if r.len() < 17 {
                        continue;
                    }
                    let id = col_str(r, 0);
                    let remote_upd = col_str(r, 16);
                    let local_upd = load_session_by_id(&conn, &id)?.map(|s| s.updated_at);
                    if local_upd.as_deref().map_or(true, |lu| remote_upd.as_str() > lu) {
                        let accepted = col_str(r, 10).parse::<i64>().unwrap_or(0);
                        conn.execute(
                            r#"INSERT INTO hub_session_logs (
                                id, project_id, summary, log_type, tags_json, workspace_path, conversation_id,
                                commit_refs_json, evidence_refs_json, body_md, accepted, accepted_by, accepted_at,
                                review_note, session_date, created_at, updated_at, sync_pending
                            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, 0)
                            ON CONFLICT(id) DO UPDATE SET
                                summary = excluded.summary, log_type = excluded.log_type, tags_json = excluded.tags_json,
                                workspace_path = excluded.workspace_path, conversation_id = excluded.conversation_id,
                                commit_refs_json = excluded.commit_refs_json, evidence_refs_json = excluded.evidence_refs_json,
                                body_md = excluded.body_md, accepted = excluded.accepted, accepted_by = excluded.accepted_by,
                                accepted_at = excluded.accepted_at, review_note = excluded.review_note,
                                session_date = excluded.session_date, updated_at = excluded.updated_at, sync_pending = 0;"#,
                            &[
                                SqlVal::Text(id),
                                SqlVal::Text(col_str(r, 1)),
                                SqlVal::Text(col_str(r, 2)),
                                SqlVal::Text(col_str(r, 3)),
                                SqlVal::Text(col_str(r, 4)),
                                SqlVal::Text(col_str(r, 5)),
                                SqlVal::Text(col_str(r, 6)),
                                SqlVal::Text(col_str(r, 7)),
                                SqlVal::Text(col_str(r, 8)),
                                SqlVal::Text(col_str(r, 9)),
                                SqlVal::Int(accepted),
                                SqlVal::opt_text(r.get(11).and_then(|v| v.as_deref())),
                                SqlVal::opt_text(r.get(12).and_then(|v| v.as_deref())),
                                SqlVal::Text(col_str(r, 13)),
                                SqlVal::Text(col_str(r, 14)),
                                SqlVal::Text(col_str(r, 15)),
                                SqlVal::Text(remote_upd),
                            ],
                        )?;
                        pulled_count += 1;
                    }
                }

                for r in &remote_links {
                    if r.len() >= 2 {
                        let _ = conn.execute(
                            "INSERT OR IGNORE INTO hub_session_log_tasks (session_log_id, task_key) VALUES (?1, ?2);",
                            &[SqlVal::Text(col_str(r, 0)), SqlVal::Text(col_str(r, 1))],
                        );
                    }
                }
                for r in &remote_audits {
                    if r.len() >= 8 {
                        let _ = conn.execute(
                            "INSERT OR IGNORE INTO hub_audit_events (id, entity_type, entity_id, action, channel, actor, detail_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8);",
                            &[
                                SqlVal::Text(col_str(r, 0)),
                                SqlVal::Text(col_str(r, 1)),
                                SqlVal::Text(col_str(r, 2)),
                                SqlVal::Text(col_str(r, 3)),
                                SqlVal::Text(col_str(r, 4)),
                                SqlVal::Text(col_str(r, 5)),
                                SqlVal::Text(col_str(r, 6)),
                                SqlVal::Text(col_str(r, 7)),
                            ],
                        );
                    }
                }

                Ok(())
            })();

            if let Err(e) = merge_res {
                let _ = conn.exec_batch("ROLLBACK;");
                return Err(e);
            }
            conn.exec_batch("COMMIT;")?;
            if self.fts_enabled && pulled_count > 0 {
                let _ = rebuild_fts_index(&conn);
            }
        }

        let dir = match (pushed_count > 0, pulled_count > 0) {
            (true, true) => format!("bidirectional (pushed={pushed_count}, pulled={pulled_count})"),
            (true, false) => format!("push (pushed={pushed_count})"),
            (false, true) => format!("pull (pulled={pulled_count})"),
            (false, false) => "up-to-date".to_string(),
        };
        Ok(dir)
    }
}

// ---------------------------------------------------------------------------
// Internal SQLite Row Loaders & FTS5 Helpers
// ---------------------------------------------------------------------------

fn empty_sync_status() -> HubDbSyncStatus {
    HubDbSyncStatus {
        sqlite_path: String::new(),
        sqlite_version: "unknown".to_string(),
        journal_mode: "unknown".to_string(),
        fts_enabled: false,
        postgres_configured: false,
        postgres_url_redacted: String::new(),
        postgres_connected: false,
        pending_outbox_rows: 0,
        last_sync_at: None,
        last_sync_direction: None,
        last_sync_error: None,
        project_count: 0,
        task_count: 0,
        session_count: 0,
        audit_count: 0,
    }
}

fn col_str(row: &[Option<String>], idx: usize) -> String {
    row.get(idx).and_then(|v| v.clone()).unwrap_or_default()
}

fn col_opt(row: &[Option<String>], idx: usize) -> Option<String> {
    row.get(idx)
        .and_then(|v| v.clone())
        .filter(|s| !s.is_empty())
}

fn parse_str_vec(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

fn parse_ac_vec(raw: &str) -> Vec<AcChecklistItem> {
    serde_json::from_str::<Vec<AcChecklistItem>>(raw).unwrap_or_default()
}

fn normalize_task_key(raw: &str) -> String {
    let trimmed = raw.trim().to_ascii_uppercase();
    if trimmed.starts_with("AGY-") {
        trimmed
    } else if trimmed.chars().all(|c| c.is_ascii_digit()) && !trimmed.is_empty() {
        format!("AGY-{trimmed}")
    } else {
        trimmed
    }
}

fn slugify_id(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn sanitize_fts5_query(raw: &str) -> Option<String> {
    let tokens: Vec<String> = raw
        .split_whitespace()
        .map(|w| w.replace('"', "").trim().to_string())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" OR "))
    }
}

fn parse_project_row(r: &[Option<String>]) -> Option<HubProject> {
    if r.len() < 13 {
        return None;
    }
    Some(HubProject {
        id: col_str(r, 0),
        name: col_str(r, 1),
        status: col_str(r, 2),
        priority: col_str(r, 3),
        workspace_path: col_str(r, 4),
        outcome: col_str(r, 5),
        target_date: col_opt(r, 6),
        lead: col_str(r, 7),
        body_md: col_str(r, 8),
        catalog_refs: parse_str_vec(&col_str(r, 9)),
        created_at: col_str(r, 10),
        updated_at: col_str(r, 11),
        sync_pending: col_str(r, 12) == "1",
    })
}

fn load_projects(conn: &SqliteConn) -> Result<Vec<HubProject>, String> {
    let rows = conn.query_rows(
        "SELECT id, name, status, priority, workspace_path, outcome, target_date, lead, body_md, catalog_refs, created_at, updated_at, sync_pending FROM hub_projects ORDER BY created_at ASC;",
        &[],
    )?;
    Ok(rows.iter().filter_map(|r| parse_project_row(r)).collect())
}

fn load_project_by_id(conn: &SqliteConn, id: &str) -> Result<Option<HubProject>, String> {
    let rows = conn.query_rows(
        "SELECT id, name, status, priority, workspace_path, outcome, target_date, lead, body_md, catalog_refs, created_at, updated_at, sync_pending FROM hub_projects WHERE id = ?1 LIMIT 1;",
        &[SqlVal::Text(id.to_string())],
    )?;
    Ok(rows.first().and_then(|r| parse_project_row(r)))
}

fn parse_task_row(r: &[Option<String>]) -> Option<HubTask> {
    if r.len() < 19 {
        return None;
    }
    Some(HubTask {
        seq_id: col_str(r, 0).parse::<i64>().unwrap_or(0),
        task_key: col_str(r, 1),
        project_id: col_str(r, 2),
        title: col_str(r, 3),
        status: col_str(r, 4),
        priority: col_str(r, 5),
        tags: parse_str_vec(&col_str(r, 6)),
        workspace_path: col_str(r, 7),
        dispatch_target: col_str(r, 8),
        acceptance_summary: col_str(r, 9),
        ac_checklist: parse_ac_vec(&col_str(r, 10)),
        owner: col_str(r, 11),
        due_date: col_opt(r, 12),
        last_session_at: col_opt(r, 13),
        body_md: col_str(r, 14),
        catalog_refs: parse_str_vec(&col_str(r, 15)),
        created_at: col_str(r, 16),
        updated_at: col_str(r, 17),
        sync_pending: col_str(r, 18) == "1",
    })
}

fn load_all_tasks(conn: &SqliteConn) -> Result<Vec<HubTask>, String> {
    let rows = conn.query_rows(
        "SELECT seq_id, task_key, project_id, title, status, priority, tags_json, workspace_path, dispatch_target, acceptance_summary, ac_checklist_json, owner, due_date, last_session_at, body_md, catalog_refs, created_at, updated_at, sync_pending FROM hub_tasks ORDER BY seq_id DESC;",
        &[],
    )?;
    Ok(rows.iter().filter_map(|r| parse_task_row(r)).collect())
}

fn load_task_by_key(conn: &SqliteConn, key: &str) -> Result<Option<HubTask>, String> {
    let rows = conn.query_rows(
        "SELECT seq_id, task_key, project_id, title, status, priority, tags_json, workspace_path, dispatch_target, acceptance_summary, ac_checklist_json, owner, due_date, last_session_at, body_md, catalog_refs, created_at, updated_at, sync_pending FROM hub_tasks WHERE task_key = ?1 LIMIT 1;",
        &[SqlVal::Text(key.to_string())],
    )?;
    Ok(rows.first().and_then(|r| parse_task_row(r)))
}

fn load_session_task_map(conn: &SqliteConn) -> Result<BTreeMap<String, Vec<String>>, String> {
    let rows = conn.query_rows(
        "SELECT session_log_id, task_key FROM hub_session_log_tasks ORDER BY task_key ASC;",
        &[],
    )?;
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in rows {
        if r.len() >= 2 {
            map.entry(col_str(&r, 0)).or_default().push(col_str(&r, 1));
        }
    }
    Ok(map)
}

fn load_all_session_task_links(conn: &SqliteConn) -> Result<Vec<(String, String)>, String> {
    let rows = conn.query_rows(
        "SELECT session_log_id, task_key FROM hub_session_log_tasks;",
        &[],
    )?;
    Ok(rows
        .iter()
        .filter(|r| r.len() >= 2)
        .map(|r| (col_str(r, 0), col_str(r, 1)))
        .collect())
}

fn parse_session_row(
    r: &[Option<String>],
    task_map: &BTreeMap<String, Vec<String>>,
) -> Option<HubSessionLog> {
    if r.len() < 18 {
        return None;
    }
    let id = col_str(r, 0);
    let task_keys = task_map.get(&id).cloned().unwrap_or_default();
    Some(HubSessionLog {
        id,
        project_id: col_str(r, 1),
        summary: col_str(r, 2),
        log_type: col_str(r, 3),
        tags: parse_str_vec(&col_str(r, 4)),
        workspace_path: col_str(r, 5),
        conversation_id: col_str(r, 6),
        commit_refs: parse_str_vec(&col_str(r, 7)),
        evidence_refs: parse_str_vec(&col_str(r, 8)),
        body_md: col_str(r, 9),
        accepted: col_str(r, 10) == "1",
        accepted_by: col_opt(r, 11),
        accepted_at: col_opt(r, 12),
        review_note: col_str(r, 13),
        session_date: col_str(r, 14),
        task_keys,
        created_at: col_str(r, 15),
        updated_at: col_str(r, 16),
        sync_pending: col_str(r, 17) == "1",
    })
}

fn load_all_sessions(conn: &SqliteConn) -> Result<Vec<HubSessionLog>, String> {
    let task_map = load_session_task_map(conn)?;
    let rows = conn.query_rows(
        "SELECT id, project_id, summary, log_type, tags_json, workspace_path, conversation_id, commit_refs_json, evidence_refs_json, body_md, accepted, accepted_by, accepted_at, review_note, session_date, created_at, updated_at, sync_pending FROM hub_session_logs ORDER BY created_at DESC;",
        &[],
    )?;
    Ok(rows
        .iter()
        .filter_map(|r| parse_session_row(r, &task_map))
        .collect())
}

fn load_session_by_id(conn: &SqliteConn, id: &str) -> Result<Option<HubSessionLog>, String> {
    let task_map = load_session_task_map(conn)?;
    let rows = conn.query_rows(
        "SELECT id, project_id, summary, log_type, tags_json, workspace_path, conversation_id, commit_refs_json, evidence_refs_json, body_md, accepted, accepted_by, accepted_at, review_note, session_date, created_at, updated_at, sync_pending FROM hub_session_logs WHERE id = ?1 LIMIT 1;",
        &[SqlVal::Text(id.to_string())],
    )?;
    Ok(rows.first().and_then(|r| parse_session_row(r, &task_map)))
}

fn load_sessions_for_task(
    conn: &SqliteConn,
    task_key: &str,
) -> Result<Vec<HubSessionLog>, String> {
    let task_map = load_session_task_map(conn)?;
    let rows = conn.query_rows(
        r#"SELECT s.id, s.project_id, s.summary, s.log_type, s.tags_json, s.workspace_path,
                  s.conversation_id, s.commit_refs_json, s.evidence_refs_json, s.body_md,
                  s.accepted, s.accepted_by, s.accepted_at, s.review_note, s.session_date,
                  s.created_at, s.updated_at, s.sync_pending
           FROM hub_session_logs s
           INNER JOIN hub_session_log_tasks l ON l.session_log_id = s.id
           WHERE l.task_key = ?1
           ORDER BY s.created_at DESC;"#,
        &[SqlVal::Text(task_key.to_string())],
    )?;
    Ok(rows
        .iter()
        .filter_map(|r| parse_session_row(r, &task_map))
        .collect())
}

fn load_recent_decisions_and_blockers(
    conn: &SqliteConn,
    project_id: &str,
    limit: usize,
) -> Result<Vec<HubSessionLog>, String> {
    let task_map = load_session_task_map(conn)?;
    let rows = conn.query_rows(
        r#"SELECT id, project_id, summary, log_type, tags_json, workspace_path, conversation_id,
                  commit_refs_json, evidence_refs_json, body_md, accepted, accepted_by, accepted_at,
                  review_note, session_date, created_at, updated_at, sync_pending
           FROM hub_session_logs
           WHERE project_id = ?1 AND log_type IN ('decision_rfc', 'blocker_pitfall')
           ORDER BY created_at DESC LIMIT ?2;"#,
        &[
            SqlVal::Text(project_id.to_string()),
            SqlVal::Int(limit as i64),
        ],
    )?;
    Ok(rows
        .iter()
        .filter_map(|r| parse_session_row(r, &task_map))
        .collect())
}

fn insert_audit_event(
    conn: &SqliteConn,
    entity_type: &str,
    entity_id: &str,
    action: &str,
    channel: &str,
    actor: &str,
    detail: &Value,
    created_at: &str,
) -> Result<(), String> {
    conn.execute(
        r#"INSERT INTO hub_audit_events (
            id, entity_type, entity_id, action, channel, actor, detail_json, created_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8);"#,
        &[
            SqlVal::Text(Uuid::new_v4().to_string()),
            SqlVal::Text(entity_type.to_string()),
            SqlVal::Text(entity_id.to_string()),
            SqlVal::Text(action.to_string()),
            SqlVal::Text(channel.to_string()),
            SqlVal::Text(actor.to_string()),
            SqlVal::Text(detail.to_string()),
            SqlVal::Text(created_at.to_string()),
        ],
    )
}

fn parse_audit_row(r: &[Option<String>]) -> Option<HubAuditEvent> {
    if r.len() < 8 {
        return None;
    }
    Some(HubAuditEvent {
        id: col_str(r, 0),
        entity_type: col_str(r, 1),
        entity_id: col_str(r, 2),
        action: col_str(r, 3),
        channel: col_str(r, 4),
        actor: col_str(r, 5),
        detail: serde_json::from_str(&col_str(r, 6)).unwrap_or_else(|_| json!({})),
        created_at: col_str(r, 7),
    })
}

fn load_audit_events_for_entity(
    conn: &SqliteConn,
    entity_type: &str,
    entity_id: &str,
    limit: usize,
) -> Result<Vec<HubAuditEvent>, String> {
    let rows = conn.query_rows(
        "SELECT id, entity_type, entity_id, action, channel, actor, detail_json, created_at FROM hub_audit_events WHERE entity_type = ?1 AND entity_id = ?2 ORDER BY created_at DESC LIMIT ?3;",
        &[
            SqlVal::Text(entity_type.to_string()),
            SqlVal::Text(entity_id.to_string()),
            SqlVal::Int(limit as i64),
        ],
    )?;
    Ok(rows.iter().filter_map(|r| parse_audit_row(r)).collect())
}

fn load_recent_audit_events(
    conn: &SqliteConn,
    limit: usize,
) -> Result<Vec<HubAuditEvent>, String> {
    let rows = conn.query_rows(
        "SELECT id, entity_type, entity_id, action, channel, actor, detail_json, created_at FROM hub_audit_events ORDER BY created_at DESC LIMIT ?1;",
        &[SqlVal::Int(limit as i64)],
    )?;
    Ok(rows.iter().filter_map(|r| parse_audit_row(r)).collect())
}

fn upsert_fts_task(conn: &SqliteConn, task: &HubTask) -> Result<(), String> {
    conn.execute(
        "DELETE FROM hub_fts WHERE entity_type = 'task' AND entity_id = ?1;",
        &[SqlVal::Text(task.task_key.clone())],
    )?;
    conn.execute(
        "INSERT INTO hub_fts (entity_type, entity_id, title, tags, body) VALUES ('task', ?1, ?2, ?3, ?4);",
        &[
            SqlVal::Text(task.task_key.clone()),
            SqlVal::Text(format!("{} {}", task.task_key, task.title)),
            SqlVal::Text(task.tags.join(" ")),
            SqlVal::Text(format!("{}\n{}", task.acceptance_summary, task.body_md)),
        ],
    )
}

fn upsert_fts_session(conn: &SqliteConn, session: &HubSessionLog) -> Result<(), String> {
    conn.execute(
        "DELETE FROM hub_fts WHERE entity_type = 'session' AND entity_id = ?1;",
        &[SqlVal::Text(session.id.clone())],
    )?;
    conn.execute(
        "INSERT INTO hub_fts (entity_type, entity_id, title, tags, body) VALUES ('session', ?1, ?2, ?3, ?4);",
        &[
            SqlVal::Text(session.id.clone()),
            SqlVal::Text(session.summary.clone()),
            SqlVal::Text(session.tags.join(" ")),
            SqlVal::Text(format!(
                "{} {} {}",
                session.conversation_id,
                session.commit_refs.join(" "),
                session.body_md
            )),
        ],
    )
}

fn rebuild_fts_index(conn: &SqliteConn) -> Result<(), String> {
    conn.exec_batch("DELETE FROM hub_fts;")?;
    for t in load_all_tasks(conn)? {
        upsert_fts_task(conn, &t)?;
    }
    for s in load_all_sessions(conn)? {
        upsert_fts_session(conn, &s)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::seed::{SEED_SESSION_AGY1_ID, SEED_SESSION_AGY2_ID};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn open_mem_hub() -> HubDbHandle {
        HubDbHandle::open_or_init(PathBuf::from(":memory:"), true, None).expect("open mem hub")
    }

    #[test]
    fn seed_is_idempotent_and_preserves_agy1_and_agy2() {
        let dir = std::env::temp_dir().join(format!("pits_hub_seed_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("project-hub.sqlite3");

        let h1 = HubDbHandle::open_or_init(db_path.clone(), false, None).expect("open 1");
        let ov1 = h1.overview().expect("ov1");
        assert_eq!(ov1.projects.len(), 1);
        assert_eq!(ov1.projects[0].id, "pits");
        assert_eq!(ov1.projects[0].name, "PITS");
        assert_eq!(ov1.tasks.len(), 2);

        let agy2 = ov1
            .tasks
            .iter()
            .find(|t| t.task_key == "AGY-2")
            .expect("AGY-2 seeded");
        assert_eq!(agy2.status, "ready_for_review");
        assert!(agy2.tags.contains(&"pits-infra".to_string()));
        assert!(agy2.tags.contains(&"portal".to_string()));

        let s2 = ov1
            .recent_sessions
            .iter()
            .find(|s| s.id == SEED_SESSION_AGY2_ID)
            .expect("AGY-2 handoff seeded");
        assert!(!s2.accepted);
        assert!(s2.commit_refs.contains(&"d19c0e6".to_string()));
        drop(h1);

        // Re-open on same file: seed must be idempotent and never duplicate rows
        let h2 = HubDbHandle::open_or_init(db_path, false, None).expect("open 2");
        let ov2 = h2.overview().expect("ov2");
        assert_eq!(ov2.projects.len(), 1);
        assert_eq!(ov2.tasks.len(), 2);
        assert_eq!(ov2.recent_sessions.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn monotonic_agy_sequence_and_tag_filtering() {
        let hub = open_mem_hub();

        let t3 = hub
            .create_task(
                CreateTaskRequest {
                    project_id: "pits".to_string(),
                    title: "Implement Project Hub Phase 1".to_string(),
                    status: Some("ready".to_string()),
                    priority: Some("p1_high".to_string()),
                    tags: vec!["PITS-Infra".to_string(), "portal".to_string(), "portal".to_string()],
                    workspace_path: "c:/Project/PITS/Infra".to_string(),
                    dispatch_target: "pits-infra".to_string(),
                    acceptance_summary: "Self-managed hub online".to_string(),
                    ac_checklist: vec![AcChecklistItem {
                        id: "ac-1".to_string(),
                        text: "Dual MCP online".to_string(),
                        agent_verified: false,
                        human_verified: true, // MCP must strip this!
                    }],
                    owner: None,
                    due_date: None,
                    body_md: "Phase 1 spec".to_string(),
                    catalog_refs: vec![],
                },
                "mcp",
                "agent-infra",
            )
            .expect("create AGY-3");

        assert_eq!(t3.task_key, "AGY-3");
        assert_eq!(t3.seq_id, 3);
        assert_eq!(t3.tags, vec!["pits-infra", "portal"]);
        assert!(!t3.ac_checklist[0].human_verified);

        let t4 = hub
            .create_task(
                CreateTaskRequest {
                    project_id: "pits".to_string(),
                    title: "Service worker rollout".to_string(),
                    status: Some("backlog".to_string()),
                    priority: Some("p2_medium".to_string()),
                    tags: vec!["pits-service".to_string()],
                    workspace_path: "c:/Project/PITS/Service".to_string(),
                    dispatch_target: "pits-service".to_string(),
                    acceptance_summary: String::new(),
                    ac_checklist: vec![],
                    owner: None,
                    due_date: None,
                    body_md: String::new(),
                    catalog_refs: vec![],
                },
                "mcp",
                "agent-service",
            )
            .expect("create AGY-4");
        assert_eq!(t4.task_key, "AGY-4");
        assert_eq!(t4.seq_id, 4);

        let filtered = hub
            .query_tasks(&TaskQueryFilter {
                tag: Some("pits-infra,portal".to_string()),
                ..Default::default()
            })
            .expect("tag filter");
        let keys: Vec<String> = filtered.iter().map(|t| t.task_key.clone()).collect();
        assert!(keys.contains(&"AGY-2".to_string()));
        assert!(keys.contains(&"AGY-3".to_string()));
        assert!(!keys.contains(&"AGY-4".to_string()));
        assert!(!keys.contains(&"AGY-1".to_string()));
    }

    #[test]
    fn mcp_channel_is_physically_forbidden_from_accept_or_done() {
        let hub = open_mem_hub();

        // 1. MCP cannot create task with status=done
        let err1 = hub
            .create_task(
                CreateTaskRequest {
                    project_id: "pits".to_string(),
                    title: "Cheat task".to_string(),
                    status: Some("done".to_string()),
                    priority: None,
                    tags: vec![],
                    workspace_path: String::new(),
                    dispatch_target: String::new(),
                    acceptance_summary: String::new(),
                    ac_checklist: vec![],
                    owner: None,
                    due_date: None,
                    body_md: String::new(),
                    catalog_refs: vec![],
                },
                "mcp",
                "rogue-agent",
            )
            .unwrap_err();
        assert!(err1.is_forbidden());

        // 2. MCP cannot update AGY-2 to status=done
        let err2 = hub
            .update_task(
                "AGY-2",
                UpdateTaskRequest {
                    status: Some("done".to_string()),
                    ..Default::default()
                },
                "mcp",
                "rogue-agent",
            )
            .unwrap_err();
        assert!(err2.is_forbidden());

        // 3. MCP cannot submit_handoff with accepted=true or target_status=done
        let err3 = hub
            .submit_handoff(
                SubmitHandoffRequest {
                    project_id: "pits".to_string(),
                    task_keys: vec!["AGY-2".to_string()],
                    summary: "Self-accept attempt".to_string(),
                    target_status: Some("done".to_string()),
                    tags: vec![],
                    workspace_path: String::new(),
                    conversation_id: String::new(),
                    commit_refs: vec![],
                    evidence_refs: vec![],
                    body_md: String::new(),
                    agent_verified_ac_ids: vec![],
                    session_date: None,
                    accepted: Some(true),
                },
                "mcp",
                "rogue-agent",
            )
            .unwrap_err();
        assert!(err3.is_forbidden());

        // 4. MCP cannot call accept_session or reject_session
        let err4 = hub
            .accept_session(
                SEED_SESSION_AGY2_ID,
                AcceptSessionRequest::default(),
                "mcp",
                "rogue-agent",
                "mcp",
            )
            .unwrap_err();
        assert!(err4.is_forbidden());

        // Verify forbidden attempts were recorded in audit trail and AGY-2 remains untouched
        let detail = hub.get_task_detail("AGY-2").expect("detail");
        assert_eq!(detail.task.status, "ready_for_review");
        assert!(detail
            .audit_events
            .iter()
            .any(|e| e.action == "forbidden_attempt" && e.channel == "mcp"));
    }

    #[test]
    fn human_portal_ui_accept_and_reject_with_audit_trail() {
        let hub = open_mem_hub();

        // Human rejects AGY-1 session -> moves AGY-1 back to in_progress
        let rej = hub
            .reject_session(
                SEED_SESSION_AGY1_ID,
                RejectSessionRequest {
                    review_note: "Need more evidence screenshots".to_string(),
                    target_task_status: Some("in_progress".to_string()),
                },
                "portal_ui",
                "pitsadmin",
                "keycloak-fido2",
            )
            .expect("reject");
        assert!(!rej.session.accepted);
        assert_eq!(rej.session.review_note, "Need more evidence screenshots");
        assert_eq!(rej.updated_tasks[0].status, "in_progress");

        // Human accepts AGY-2 session -> marks accepted=true, AGY-2=done, humanVerified=true
        let acc = hub
            .accept_session(
                SEED_SESSION_AGY2_ID,
                AcceptSessionRequest {
                    mark_tasks_done: true,
                    review_note: "Verified live on pits-core".to_string(),
                    verified_ac_ids: vec!["ac-1".to_string(), "ac-2".to_string()],
                },
                "portal_ui",
                "pitsadmin",
                "keycloak-fido2",
            )
            .expect("accept");
        assert!(acc.session.accepted);
        assert_eq!(acc.session.accepted_by.as_deref(), Some("pitsadmin"));
        assert!(acc.session.accepted_at.is_some());
        assert_eq!(acc.updated_tasks[0].status, "done");
        assert!(acc.updated_tasks[0]
            .ac_checklist
            .iter()
            .find(|i| i.id == "ac-1")
            .unwrap()
            .human_verified);
        assert!(!acc.updated_tasks[0]
            .ac_checklist
            .iter()
            .find(|i| i.id == "ac-3")
            .unwrap()
            .human_verified);

        let detail = hub.get_task_detail("AGY-2").expect("detail");
        assert!(detail
            .audit_events
            .iter()
            .any(|e| e.action == "accept" && e.channel == "portal_ui" && e.actor == "pitsadmin"));
    }

    #[test]
    fn search_finds_seeded_tasks_and_sessions() {
        let hub = open_mem_hub();
        let res = hub.search("GoVPP", 10).expect("search");
        assert!(res.tasks.iter().any(|t| t.task_key == "AGY-2"));
        assert!(res.sessions.iter().any(|s| s.id == SEED_SESSION_AGY2_ID));
    }

    #[tokio::test]
    async fn bidirectional_outbox_sync_with_mock_pg_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Read Startup packet (no SSL since sslmode=disable)
            let mut len_buf = [0u8; 4];
            sock.read_exact(&mut len_buf).await.unwrap();
            let startup_len = i32::from_be_bytes(len_buf) as usize;
            let mut body = vec![0u8; startup_len - 4];
            sock.read_exact(&mut body).await.unwrap();

            async fn write_pkt(sock: &mut tokio::net::TcpStream, tag: u8, payload: &[u8]) {
                let len = (payload.len() + 4) as i32;
                let mut buf = Vec::with_capacity(1 + len as usize);
                buf.push(tag);
                buf.extend_from_slice(&len.to_be_bytes());
                buf.extend_from_slice(payload);
                sock.write_all(&buf).await.unwrap();
            }

            // AuthenticationOk + ReadyForQuery
            write_pkt(&mut sock, b'R', &0i32.to_be_bytes()).await;
            write_pkt(&mut sock, b'Z', b"I").await;

            let mut received_queries = Vec::new();
            loop {
                let mut hdr = [0u8; 5];
                if sock.read_exact(&mut hdr).await.is_err() {
                    break;
                }
                let tag = hdr[0];
                let total_len = i32::from_be_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize;
                let mut payload = vec![0u8; total_len - 4];
                if !payload.is_empty() {
                    sock.read_exact(&mut payload).await.unwrap();
                }
                if tag == b'X' {
                    break;
                }
                if tag == b'Q' {
                    let sql = String::from_utf8_lossy(&payload)
                        .trim_end_matches('\0')
                        .to_string();
                    received_queries.push(sql);
                    write_pkt(&mut sock, b'C', b"OK\0").await;
                    write_pkt(&mut sock, b'Z', b"I").await;
                }
            }
            received_queries
        });

        let hub = open_mem_hub();
        let pg_url = format!("postgres://pitshub:secret@127.0.0.1:{}/project_hub?sslmode=disable", addr.port());
        let st = hub.sync_with_postgres(Some(&pg_url)).await;
        let queries = server.await.unwrap();

        assert!(st.postgres_connected, "sync error: {:?}", st.last_sync_error);
        assert_eq!(st.pending_outbox_rows, 0);
        assert!(queries.iter().any(|q| q.contains("INSERT INTO hub_projects")));
        assert!(queries.iter().any(|q| q.contains("INSERT INTO hub_tasks")));
        assert!(queries.iter().any(|q| q.contains("setval(pg_get_serial_sequence")));
    }
}
