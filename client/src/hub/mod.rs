//! Self-Managed Project Hub (`client/src/hub/`).
//!
//! Replaces cloud Notion `Antigravity Project Hub` with an embedded SQLite3 WAL + FTS5
//! relational store synchronized bi-directionally with `cluster-pits-lab` CNPG PostgreSQL (`hub-pg`),
//! serving both human portal REST APIs (`/api/pits/v1/hub/*`) and dual-path MCP (`/mcp/hub` + `pitsctl hub mcp`).

pub mod db;
pub mod mcp;
pub mod model;
pub mod seed;

pub use db::HubDbHandle;
pub use mcp::{
    handle_hub_mcp_jsonrpc, hub_tool_definitions, run_hub_mcp_stdio, verify_hub_mcp_request_auth,
    HubMcpAuthContext, HubMcpReply,
};
pub use model::{
    AcChecklistItem, AcceptSessionRequest, CreateSessionLogRequest, CreateTaskRequest,
    HubAuditEvent, HubDbSyncStatus, HubError, HubOverviewResponse, HubProject, HubSearchResponse,
    HubSessionLog, HubTask, HubTaskContextResponse, HubTaskDetailResponse, HumanReviewOutcome,
    PickupTaskRequest, RejectSessionRequest, SessionQueryFilter, SubmitHandoffRequest,
    TaskQueryFilter, UpdateTaskRequest, UpsertProjectRequest, FORBIDDEN_HUMAN_ACCEPTANCE_MSG,
};

use crate::config::{effective_hub_db_url, local_state_dir, ClientProfile};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct HubRegistry {
    pub db: Arc<HubDbHandle>,
    pub profile: Arc<RwLock<ClientProfile>>,
}

impl HubRegistry {
    pub fn new(profile: Arc<RwLock<ClientProfile>>) -> Self {
        let pg_url = profile
            .try_read()
            .ok()
            .and_then(|p| effective_hub_db_url(None, &p));
        let sqlite_path = local_state_dir().join("project-hub.sqlite3");
        let db = match HubDbHandle::open_or_init(sqlite_path, false, pg_url.clone()) {
            Ok(handle) => Arc::new(handle),
            Err(e) => {
                eprintln!(
                    "[pits-hub] warning: failed to open disk SQLite ({e}); falling back to in-memory SQLite"
                );
                Arc::new(
                    HubDbHandle::open_or_init(PathBuf::from(":memory:"), true, pg_url)
                        .expect("in-memory SQLite hub init"),
                )
            }
        };
        Self { db, profile }
    }

    #[cfg(test)]
    pub fn new_in_memory(profile: Arc<RwLock<ClientProfile>>) -> Self {
        let db = Arc::new(
            HubDbHandle::open_or_init(PathBuf::from(":memory:"), true, None)
                .expect("in-memory SQLite hub init"),
        );
        Self { db, profile }
    }

    pub async fn sync_db(&self, pg_url_override: Option<&str>) -> HubDbSyncStatus {
        let effective = {
            let prof = self.profile.read().await;
            effective_hub_db_url(pg_url_override, &prof)
        };
        if let Some(url) = effective {
            self.db.sync_with_postgres(Some(&url)).await
        } else {
            self.db.sync_with_postgres(pg_url_override).await
        }
    }
}
