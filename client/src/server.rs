//! Embedded local Axum HTTP API & Backstage v1.56 Resource Portal server (`pitsctl serve`).
//!
//! Exposes:
//! 1. Native Backstage Software Catalog REST API (`/api/catalog/*`) & Guest Auth (`/api/auth/guest/refresh`)
//! 2. PITS Resource Portal Entity & Contextual Action REST API (`/api/pits/v1/*`)
//! 3. Legacy v1 domain endpoints (`/api/v1/*`) for backward compatibility
//! 4. Embedded Backstage SPA static assets (`portal/dist/*` or fallback `webui/index.html`) with History fallback

use anyhow::Result;
use axum::body::{Body, Bytes};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::Engine as _;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::catalog::{
    redact_db_url, ActionInvokeRequest, CandidateDispositionRequest, CatalogEntitiesQuery,
    CatalogRegistry, SubmitBlindBallotRequest,
};
use crate::config::{
    default_profile_path, effective_hub_db_url, effective_hub_mcp_token, effective_sourcing_db_url,
    generate_agent_mtls_pki, load_kubeconfig_file, load_talosconfig_file, local_state_dir,
    ClientProfile, WorkspaceBundle,
};
use crate::hub::{
    handle_hub_mcp_jsonrpc, verify_hub_mcp_request_auth, AcceptSessionRequest,
    CreateSessionLogRequest, CreateTaskRequest, HubError, HubRegistry, RejectSessionRequest,
    SessionQueryFilter, TaskQueryFilter, UpdateTaskRequest, UpsertProjectRequest,
};
use crate::k8s::{
    audit_fleet_npus, inspect_cluster_summary, launch_npu_validation_job,
    render_npu_validation_manifests, render_san_local_pv_pvc,
};
use crate::pxe::{
    apply_tinkerbell_hardware, pxe_list_inventory, pxe_verify_assets, render_ipxe_script,
    render_tinkerbell_hardware_yaml, PxeBootMode,
};
use crate::router::{
    apply_edge_router, inspect_router_live, pits_core_default_edge_router, recover_router_runtime,
    set_crossplane_paused, validate_edge_router, EdgeRouterManifest,
};
use crate::san::{
    btrfs_maintain, btrfs_status, expected_wwid, render_nvmet_k8s_manifests, san_expand,
    san_inspect, san_provision,
};
use crate::talos::{
    allowed_wwids_for_node, render_talos_imager_profile, render_talos_node_config,
    validate_talos_netboot_config, TalosClient,
};
use crate::transport::{AgentClient, KubeClient};
use crate::vm::{
    finalize_windows11_vm, launch_mstsc, list_cluster_vms, render_access_lab_vm_manifest,
    render_pinned_rdp_profile, render_windows11_vm_manifest, verify_rdp_tls_endpoint,
    vm_lifecycle_action, write_pinned_rdp_profile, VmLifecycleOp,
};

mod portal_assets {
    include!(concat!(env!("OUT_DIR"), "/portal_assets.rs"));
}

pub use portal_assets::{get_portal_asset, get_portal_asset_entry, PortalAsset};

#[derive(Clone)]
pub struct ConsoleState {
    pub profile: Arc<RwLock<ClientProfile>>,
    pub bundle: Arc<RwLock<WorkspaceBundle>>,
    pub catalog: Arc<CatalogRegistry>,
    pub hub: Arc<HubRegistry>,
}

impl ConsoleState {
    pub fn new(profile: ClientProfile, bundle: WorkspaceBundle) -> Self {
        let profile = Arc::new(RwLock::new(profile));
        let bundle = Arc::new(RwLock::new(bundle));
        let catalog = Arc::new(CatalogRegistry::new(profile.clone(), bundle.clone()));
        let hub = Arc::new(HubRegistry::new(profile.clone()));
        Self {
            profile,
            bundle,
            catalog,
            hub,
        }
    }

    #[cfg(test)]
    pub fn new_with_in_memory_hub(profile: ClientProfile, bundle: WorkspaceBundle) -> Self {
        let profile = Arc::new(RwLock::new(profile));
        let bundle = Arc::new(RwLock::new(bundle));
        let catalog = Arc::new(CatalogRegistry::new(profile.clone(), bundle.clone()));
        let hub = Arc::new(HubRegistry::new_in_memory(profile.clone()));
        Self {
            profile,
            bundle,
            catalog,
            hub,
        }
    }

    async fn build_agent(&self) -> Result<AgentClient> {
        let prof = self.profile.read().await;
        AgentClient::from_profile(&prof)
    }

    async fn build_core_k8s(&self) -> Result<KubeClient> {
        let prof = self.profile.read().await;
        let path = prof
            .core_kubeconfig_path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("coreKubeconfigPath is not configured in profile"))?;
        let ep = load_kubeconfig_file(Path::new(path), prof.core_context.as_deref())?;
        KubeClient::from_endpoint("pits-core", ep, None)
    }

    async fn build_lab_k8s(&self) -> Result<KubeClient> {
        let prof = self.profile.read().await;
        let path = prof
            .lab_kubeconfig_path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("labKubeconfigPath is not configured in profile"))?;
        let ep = load_kubeconfig_file(Path::new(path), prof.lab_context.as_deref())?;
        if prof.lab_via_agent_bridge {
            let ag = AgentClient::from_profile(&prof)?;
            KubeClient::from_endpoint("pits-lab", ep, Some((&prof, ag.tls_config)))
        } else {
            KubeClient::from_endpoint("pits-lab", ep, None)
        }
    }

    async fn build_talos(&self) -> Result<TalosClient> {
        let prof = self.profile.read().await;
        let path = prof
            .talosconfig_path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("talosconfigPath is not configured in profile"))?;
        let ctx = load_talosconfig_file(Path::new(path), prof.talos_context.as_deref())?;
        if prof.talos_via_agent_bridge {
            let ag = AgentClient::from_profile(&prof)?;
            TalosClient::from_context(ctx, Some((&prof, ag.tls_config)))
        } else {
            TalosClient::from_context(ctx, None)
        }
    }
}

pub fn build_console_router(state: ConsoleState) -> Router {
    Router::new()
        .route("/", get(serve_index))
        // Backstage Software Catalog REST API & Guest Auth
        .route("/api/catalog/entities", get(api_catalog_entities))
        .route(
            "/api/catalog/entities/by-query",
            get(api_catalog_entities_by_query),
        )
        .route("/api/catalog/entity-facets", get(api_catalog_entity_facets))
        .route(
            "/api/catalog/entities/by-name/:kind/:namespace/:name",
            get(api_catalog_entity_by_name),
        )
        .route(
            "/api/catalog/entities/by-name/:kind/:namespace/:name/ancestors",
            get(api_catalog_entity_ancestors),
        )
        .route(
            "/api/catalog/entities/by-uid/:uid",
            get(api_catalog_entity_by_uid),
        )
        .route(
            "/api/catalog/entities/by-refs",
            post(api_catalog_entities_by_refs),
        )
        .route("/api/catalog/locations", get(api_catalog_locations))
        .route("/api/catalog/locations/:id", get(api_catalog_location_by_id))
        .route(
            "/api/catalog/locations/by-entity/:kind/:namespace/:name",
            get(api_catalog_location_by_entity),
        )
        .route("/api/catalog/refresh", post(api_catalog_refresh_entity))
        .route(
            "/api/auth/guest/refresh",
            get(api_auth_guest_refresh).post(api_auth_guest_refresh),
        )
        // PITS Resource Portal REST API (`/api/pits/v1/*`)
        .route("/api/pits/v1/resource-types", get(api_pits_resource_types))
        .route(
            "/api/pits/v1/entities/:namespace/:name",
            get(api_pits_entity_detail),
        )
        .route(
            "/api/pits/v1/entities/:namespace/:name/detail",
            get(api_pits_entity_detail),
        )
        .route(
            "/api/pits/v1/entities/:namespace/:name/actions/:action_id",
            post(api_pits_execute_action),
        )
        .route(
            "/api/pits/v1/catalog/refresh",
            post(api_pits_catalog_refresh),
        )
        .route("/api/pits/v1/catalog/export", get(api_pits_catalog_export))
        .route(
            "/api/pits/v1/profile",
            get(api_pits_get_profile).post(api_pits_save_profile),
        )
        .route(
            "/api/pits/v1/profile/generate-pki",
            post(api_pits_generate_pki),
        )
        // Tech Insights & Sourcing Governance Audit APIs
        .route(
            "/api/tech-insights/facts/schemas",
            get(api_tech_insights_fact_schemas),
        )
        .route(
            "/api/tech-insights/facts/latest/:namespace/:kind/:name",
            get(api_tech_insights_facts_latest),
        )
        .route("/api/tech-insights/checks", get(api_tech_insights_checks))
        .route(
            "/api/tech-insights/checks/result/:namespace/:kind/:name",
            get(api_tech_insights_checks_result).post(api_tech_insights_checks_result),
        )
        .route(
            "/api/pits/v1/sourcing/overview",
            get(api_pits_sourcing_overview),
        )
        .route(
            "/api/pits/v1/sourcing/validate",
            post(api_pits_sourcing_validate),
        )
        .route(
            "/api/pits/v1/sourcing/entities",
            post(api_pits_sourcing_upsert),
        )
        .route(
            "/api/pits/v1/sourcing/db/sync",
            post(api_pits_sourcing_db_sync),
        )
        .route(
            "/api/pits/v1/sourcing/discover",
            post(api_pits_sourcing_discover),
        )
        .route(
            "/api/pits/v1/sourcing/candidates/disposition",
            post(api_pits_sourcing_candidate_disposition),
        )
        .route(
            "/api/pits/v1/sourcing/ballots",
            post(api_pits_sourcing_submit_ballot),
        )
        // Primary MCP Server Streamable HTTP transport
        .route(
            "/mcp",
            post(api_mcp_post).get(api_mcp_get).delete(api_mcp_delete),
        )
        // Self-Managed Project Hub REST API (Human Portal UI Channel)
        .route("/api/pits/v1/hub/overview", get(api_hub_overview))
        .route(
            "/api/pits/v1/hub/projects",
            get(api_hub_list_projects).post(api_hub_upsert_project),
        )
        .route(
            "/api/pits/v1/hub/tasks",
            get(api_hub_list_tasks).post(api_hub_create_task),
        )
        .route(
            "/api/pits/v1/hub/tasks/:task_key",
            get(api_hub_get_task).patch(api_hub_patch_task),
        )
        .route(
            "/api/pits/v1/hub/sessions",
            get(api_hub_list_sessions).post(api_hub_create_session),
        )
        .route(
            "/api/pits/v1/hub/sessions/:id/accept",
            post(api_hub_accept_session),
        )
        .route(
            "/api/pits/v1/hub/sessions/:id/reject",
            post(api_hub_reject_session),
        )
        .route("/api/pits/v1/hub/search", get(api_hub_search))
        .route("/api/pits/v1/hub/db/sync", post(api_hub_db_sync))
        // Self-Managed Project Hub MCP Streamable HTTP endpoint (Agent Channel, hard-gated)
        .route(
            "/mcp/hub",
            post(api_hub_mcp_post)
                .get(api_hub_mcp_get)
                .delete(api_hub_mcp_delete),
        )
        // Legacy v1 domain endpoints (`/api/v1/*`)
        .route("/api/v1/overview", get(api_overview))
        .route("/api/v1/profile", post(api_save_profile))
        .route("/api/v1/profile/generate-pki", post(api_generate_pki))
        .route("/api/v1/router/status", get(api_router_status))
        .route("/api/v1/router/validate", post(api_router_validate))
        .route("/api/v1/router/apply", post(api_router_apply))
        .route("/api/v1/router/pause", post(api_router_pause))
        .route("/api/v1/router/recover", post(api_router_recover))
        .route("/api/v1/router/vpp-trace", post(api_router_vpp_trace))
        .route("/api/v1/san/render-k8s/:device", get(api_san_render_k8s))
        .route("/api/v1/san/inspect/:device", post(api_san_inspect))
        .route("/api/v1/san/provision/:device", post(api_san_provision))
        .route("/api/v1/san/expand/:device", post(api_san_expand))
        .route("/api/v1/btrfs/status", get(api_btrfs_status))
        .route("/api/v1/btrfs/maintain", post(api_btrfs_maintain))
        .route("/api/v1/pxe/render", post(api_pxe_render))
        .route("/api/v1/pxe/apply-hardware", post(api_pxe_apply_hardware))
        .route("/api/v1/pxe/verify-assets", post(api_pxe_verify_assets))
        .route("/api/v1/pxe/inventory", get(api_pxe_inventory))
        .route("/api/v1/talos/render/:device", get(api_talos_render))
        .route("/api/v1/talos/imager/:device", get(api_talos_imager))
        .route("/api/v1/talos/guard", post(api_talos_guard))
        .route("/api/v1/talos/status/:device", get(api_talos_status))
        .route("/api/v1/talos/apply", post(api_talos_apply))
        .route("/api/v1/talos/reboot", post(api_talos_reboot))
        .route("/api/v1/k8s/summary", get(api_k8s_summary))
        .route("/api/v1/k8s/npu-audit", get(api_k8s_npu_audit))
        .route("/api/v1/k8s/render-san-pv", get(api_k8s_render_san_pv))
        .route("/api/v1/k8s/npu-preview", post(api_k8s_npu_preview))
        .route("/api/v1/k8s/npu-launch", post(api_k8s_npu_launch))
        .route("/api/v1/vms", get(api_vms_list))
        .route("/api/v1/vms/templates", get(api_vms_templates))
        .route("/api/v1/vms/lifecycle", post(api_vms_lifecycle))
        .route("/api/v1/vms/finalize-win11", post(api_vms_finalize_win11))
        .route("/api/v1/vms/rdp", post(api_vms_rdp))
        .fallback(serve_spa_or_404)
        .with_state(state)
}

pub async fn run_console_server(addrs: &[SocketAddr], state: ConsoleState) -> Result<()> {
    if addrs.is_empty() {
        anyhow::bail!("at least one --listen address is required");
    }

    // Seed the full 8-type / 27-entity baseline snapshot in <2ms before accepting sockets,
    // immediately trigger a non-blocking live sweep, and keep the snapshot fresh every 45s.
    state.catalog.seed_baseline_if_empty().await;
    state.catalog.spawn_background_refresh();

    let catalog_reconciler = state.catalog.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(45));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Consume Tokio's immediate first tick because we already triggered startup refresh above.
        interval.tick().await;
        loop {
            interval.tick().await;
            catalog_reconciler.spawn_background_refresh();
        }
    });

    // Periodically sync SQLite WAL outbox with PostgreSQL SSOT when configured.
    let pg_sync_catalog = state.catalog.clone();
    let pg_sync_hub = state.hub.clone();
    let pg_sync_profile = state.profile.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(20));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let (sourcing_configured, hub_configured) = {
                let prof = pg_sync_profile.read().await;
                (
                    effective_sourcing_db_url(None, &prof),
                    effective_hub_db_url(None, &prof),
                )
            };
            if sourcing_configured.is_some() {
                let _ = pg_sync_catalog.sourcing_sync_db(None).await;
            }
            if hub_configured.is_some() {
                let _ = pg_sync_hub.sync_db(None).await;
            }
        }
    });

    let app = build_console_router(state);
    let mut unique_addrs = Vec::with_capacity(addrs.len());
    for &addr in addrs {
        if !unique_addrs.contains(&addr) {
            unique_addrs.push(addr);
        }
    }

    let mut listeners = Vec::with_capacity(unique_addrs.len());
    for addr in unique_addrs {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        println!("PITS Backstage Resource Portal & Catalog API listening on http://{addr}");
        listeners.push(listener);
    }

    let mut tasks = tokio::task::JoinSet::new();
    for listener in listeners {
        let router = app.clone();
        tasks.spawn(async move { axum::serve(listener, router).await });
    }
    while let Some(res) = tasks.join_next().await {
        res??;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Embedded Portal Static Assets & SPA Fallback
// ---------------------------------------------------------------------------

async fn serve_index(method: Method, headers: HeaderMap) -> Response {
    serve_index_asset(&method, &headers)
}

fn accepts_gzip(headers: &HeaderMap) -> bool {
    let mut explicit_gzip: Option<bool> = None;
    let mut wildcard_ok = false;

    for hv in headers.get_all(header::ACCEPT_ENCODING) {
        let Ok(raw) = hv.to_str() else {
            continue;
        };
        for item in raw.split(',') {
            let mut parts = item.split(';');
            let coding = parts.next().unwrap_or("").trim();
            if coding.is_empty() {
                continue;
            }
            let mut q_non_zero = true;
            for param in parts {
                let p = param.trim();
                if let Some(q_val) = p.strip_prefix("q=").or_else(|| p.strip_prefix("Q=")) {
                    if let Ok(q) = q_val.trim().parse::<f32>() {
                        q_non_zero = q > 0.0;
                    }
                }
            }
            if coding.eq_ignore_ascii_case("gzip") {
                explicit_gzip = Some(q_non_zero);
            } else if coding == "*" && q_non_zero {
                wildcard_ok = true;
            }
        }
    }

    explicit_gzip.unwrap_or(wildcard_ok)
}

fn if_none_match_matches(headers: &HeaderMap, etag: &str) -> bool {
    let normalized_target = etag.trim_start_matches("W/");
    for hv in headers.get_all(header::IF_NONE_MATCH) {
        let Ok(raw) = hv.to_str() else {
            continue;
        };
        for candidate in raw.split(',') {
            let token = candidate.trim();
            if token == "*" || token.trim_start_matches("W/") == normalized_target {
                return true;
            }
        }
    }
    false
}

fn cache_control_for_asset(asset: PortalAsset) -> &'static str {
    if asset.immutable {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache, must-revalidate"
    }
}

fn build_portal_asset_response(
    asset: PortalAsset,
    method: &Method,
    headers: &HeaderMap,
) -> Response {
    let cache_ctrl = cache_control_for_asset(asset);
    let has_gzip = asset.gzip.is_some();

    if if_none_match_matches(headers, asset.etag) {
        let mut builder = Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::CACHE_CONTROL, cache_ctrl)
            .header(header::ETAG, asset.etag);
        if has_gzip {
            builder = builder.header(header::VARY, "Accept-Encoding");
        }
        return builder
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::NOT_MODIFIED.into_response());
    }

    let (use_gzip, payload) = match (accepts_gzip(headers), asset.gzip) {
        (true, Some(gz)) => (true, gz),
        _ => (false, asset.raw),
    };

    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, asset.mime)
        .header(header::CACHE_CONTROL, cache_ctrl)
        .header(header::ETAG, asset.etag)
        .header(header::CONTENT_LENGTH, payload.len());
    if has_gzip {
        builder = builder.header(header::VARY, "Accept-Encoding");
    }
    if use_gzip {
        builder = builder.header(header::CONTENT_ENCODING, "gzip");
    }

    let body = if *method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(payload)
    };
    builder
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn serve_index_asset(method: &Method, headers: &HeaderMap) -> Response {
    if let Some(asset) = get_portal_asset_entry("index.html") {
        build_portal_asset_response(asset, method, headers)
    } else {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "Embedded portal index.html missing",
        )
            .into_response()
    }
}

async fn serve_spa_or_404(method: Method, headers: HeaderMap, uri: Uri) -> Response {
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": {
                    "name": "NotFoundError",
                    "message": format!("API endpoint not found: {} {}", method, path),
                },
                "message": format!("API endpoint not found: {} {}", method, path),
            })),
        )
            .into_response();
    }

    if method != Method::GET && method != Method::HEAD {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            Json(serde_json::json!({ "error": "Method not allowed" })),
        )
            .into_response();
    }

    let lookup = path.trim_start_matches('/');
    let key = if lookup.is_empty() {
        "index.html"
    } else {
        lookup
    };
    if let Some(asset) = get_portal_asset_entry(key) {
        return build_portal_asset_response(asset, &method, &headers);
    }

    serve_index_asset(&method, &headers)
}

// ---------------------------------------------------------------------------
// Raw Query String Helpers (supports repeated keys like `?filter=a&filter=b`)
// ---------------------------------------------------------------------------

fn parse_query_pairs(uri: &Uri) -> Vec<(String, String)> {
    let raw = uri.query().unwrap_or("");
    url::form_urlencoded::parse(raw.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn query_flag_true(pairs: &[(String, String)], key: &str) -> bool {
    pairs
        .iter()
        .any(|(k, v)| k == key && matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
}

fn parse_catalog_query(pairs: &[(String, String)]) -> CatalogEntitiesQuery {
    let mut filters = Vec::new();
    let mut field_parts = Vec::new();
    let mut limit: Option<usize> = None;
    let mut offset: Option<usize> = None;
    let mut cursor: Option<String> = None;
    let mut full_text_term: Option<String> = None;

    for (k, v) in pairs {
        match k.as_str() {
            "filter" => {
                if !v.trim().is_empty() {
                    filters.push(v.clone());
                }
            }
            "fields" => {
                if !v.trim().is_empty() {
                    field_parts.push(v.clone());
                }
            }
            "limit" => {
                if let Ok(n) = v.trim().parse::<usize>() {
                    limit = Some(n);
                }
            }
            "offset" => {
                if let Ok(n) = v.trim().parse::<usize>() {
                    offset = Some(n);
                }
            }
            "cursor" | "after" => {
                if !v.trim().is_empty() {
                    cursor = Some(v.trim().to_string());
                }
            }
            "fullTextFilterTerm" | "q" => {
                if !v.trim().is_empty() && (k == "fullTextFilterTerm" || full_text_term.is_none()) {
                    full_text_term = Some(v.trim().to_string());
                }
            }
            _ => {}
        }
    }

    if offset.is_none() {
        if let Some(cur) = cursor {
            if let Ok(n) = cur.parse::<usize>() {
                offset = Some(n);
            } else if let Ok(decoded) = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(cur.as_bytes())
                .or_else(|_| base64::engine::general_purpose::STANDARD.decode(cur.as_bytes()))
            {
                if let Ok(s) = std::str::from_utf8(&decoded) {
                    if let Ok(n) = s.trim_start_matches("offset:").trim().parse::<usize>() {
                        offset = Some(n);
                    }
                }
            }
        }
    }

    let fields = if field_parts.is_empty() {
        None
    } else {
        Some(field_parts.join(","))
    };

    CatalogEntitiesQuery {
        filters,
        fields,
        limit,
        offset,
        full_text_term,
    }
}

fn not_found_json(message: String) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": {
                "name": "NotFoundError",
                "message": message,
            },
            "message": message,
        })),
    )
}

fn err_resp(e: anyhow::Error) -> (StatusCode, Json<serde_json::Value>) {
    let msg = format!("{e:#}");
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "error": msg,
            "message": msg,
        })),
    )
}

// ---------------------------------------------------------------------------
// Backstage Software Catalog REST API (`/api/catalog/*`) & Guest Auth
// ---------------------------------------------------------------------------

async fn api_catalog_entities(State(st): State<ConsoleState>, uri: Uri) -> impl IntoResponse {
    let pairs = parse_query_pairs(&uri);
    let force_live = query_flag_true(&pairs, "refresh") || query_flag_true(&pairs, "live");
    let query = parse_catalog_query(&pairs);
    let resp = st.catalog.query_catalog_entities(&query, force_live).await;
    (
        [(header::HeaderName::from_static("x-total-count"), resp.total_items.to_string())],
        Json(resp.items),
    )
}

async fn api_catalog_entities_by_query(
    State(st): State<ConsoleState>,
    uri: Uri,
) -> impl IntoResponse {
    let pairs = parse_query_pairs(&uri);
    let force_live = query_flag_true(&pairs, "refresh") || query_flag_true(&pairs, "live");
    let query = parse_catalog_query(&pairs);
    let resp = st.catalog.query_catalog_entities(&query, force_live).await;
    Json(resp)
}

async fn api_catalog_entity_facets(State(st): State<ConsoleState>, uri: Uri) -> impl IntoResponse {
    let pairs = parse_query_pairs(&uri);
    let force_live = query_flag_true(&pairs, "refresh") || query_flag_true(&pairs, "live");
    let mut facets = Vec::new();
    let mut filters = Vec::new();
    for (k, v) in &pairs {
        match k.as_str() {
            "facet" if !v.trim().is_empty() => facets.push(v.clone()),
            "filter" if !v.trim().is_empty() => filters.push(v.clone()),
            _ => {}
        }
    }
    let resp = st.catalog.compute_facets(&facets, &filters, force_live).await;
    Json(resp)
}

async fn api_catalog_entity_by_name(
    State(st): State<ConsoleState>,
    AxumPath((kind, namespace, name)): AxumPath<(String, String, String)>,
    uri: Uri,
) -> std::result::Result<Json<crate::catalog::CatalogEntity>, (StatusCode, Json<serde_json::Value>)>
{
    let pairs = parse_query_pairs(&uri);
    let force_live = query_flag_true(&pairs, "refresh") || query_flag_true(&pairs, "live");
    match st
        .catalog
        .get_entity_by_name(&kind, &namespace, &name, force_live)
        .await
    {
        Some(entity) => Ok(Json(entity)),
        None => Err(not_found_json(format!(
            "No entity found matching {kind}:{namespace}/{name}"
        ))),
    }
}

async fn api_catalog_entity_ancestors(
    State(st): State<ConsoleState>,
    AxumPath((kind, namespace, name)): AxumPath<(String, String, String)>,
    uri: Uri,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let pairs = parse_query_pairs(&uri);
    let force_live = query_flag_true(&pairs, "refresh") || query_flag_true(&pairs, "live");
    match st
        .catalog
        .get_entity_by_name(&kind, &namespace, &name, force_live)
        .await
    {
        Some(entity) => {
            let root_ref = format!(
                "{}:{}/{}",
                entity.kind.to_ascii_lowercase(),
                entity.metadata.namespace,
                entity.metadata.name
            );
            Ok(Json(serde_json::json!({
                "rootEntityRef": root_ref,
                "items": [
                    {
                        "entity": entity,
                        "parentEntityRefs": []
                    }
                ]
            })))
        }
        None => Err(not_found_json(format!(
            "No entity found matching {kind}:{namespace}/{name}"
        ))),
    }
}

async fn api_catalog_entity_by_uid(
    State(st): State<ConsoleState>,
    AxumPath(uid): AxumPath<String>,
    uri: Uri,
) -> std::result::Result<Json<crate::catalog::CatalogEntity>, (StatusCode, Json<serde_json::Value>)>
{
    let pairs = parse_query_pairs(&uri);
    let force_live = query_flag_true(&pairs, "refresh") || query_flag_true(&pairs, "live");
    match st.catalog.get_entity_by_uid(&uid, force_live).await {
        Some(entity) => Ok(Json(entity)),
        None => Err(not_found_json(format!("No entity found with uid {uid}"))),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EntitiesByRefsReq {
    #[serde(default)]
    entity_refs: Vec<String>,
}

async fn api_catalog_entities_by_refs(
    State(st): State<ConsoleState>,
    uri: Uri,
    Json(req): Json<EntitiesByRefsReq>,
) -> impl IntoResponse {
    let pairs = parse_query_pairs(&uri);
    let force_live = query_flag_true(&pairs, "refresh") || query_flag_true(&pairs, "live");
    let items = st
        .catalog
        .get_entities_by_refs(&req.entity_refs, force_live)
        .await;
    Json(serde_json::json!({ "items": items }))
}

fn canonical_location_entry() -> serde_json::Value {
    serde_json::json!({
        "id": "pits-live-catalog",
        "type": "pits-synthesizer",
        "target": "pits://default/pits-infrastructure",
    })
}

async fn api_catalog_locations() -> impl IntoResponse {
    let loc = canonical_location_entry();
    Json(serde_json::json!([
        {
            "data": loc.clone(),
            "id": "pits-live-catalog",
            "type": "pits-synthesizer",
            "target": "pits://default/pits-infrastructure",
        }
    ]))
}

async fn api_catalog_location_by_id(
    AxumPath(id): AxumPath<String>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if id == "pits-live-catalog" {
        Ok(Json(serde_json::json!({
            "location": canonical_location_entry(),
            "entities": [],
        })))
    } else {
        Err(not_found_json(format!("Location {id} not found")))
    }
}

async fn api_catalog_location_by_entity() -> impl IntoResponse {
    Json(canonical_location_entry())
}

async fn api_catalog_refresh_entity(State(st): State<ConsoleState>) -> impl IntoResponse {
    st.catalog.invalidate_cache().await;
    st.catalog.spawn_background_refresh();
    StatusCode::OK
}

async fn api_auth_guest_refresh(headers: HeaderMap) -> impl IntoResponse {
    let header_val = |name: &str| -> Option<String> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let gw_user = header_val("x-auth-request-user")
        .or_else(|| header_val("x-forwarded-user"));
    let gw_email = header_val("x-auth-request-email")
        .or_else(|| header_val("x-forwarded-email"));

    if gw_user.is_some() || gw_email.is_some() {
        let email = gw_email.unwrap_or_else(|| "pitsadmin@pits.home.arpa".to_string());
        let raw_user = gw_user.unwrap_or_else(|| {
            email
                .split('@')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("pitsadmin")
                .to_string()
        });
        let user_slug: String = raw_user
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect();
        let user_ref = format!(
            "user:default/{}",
            if user_slug.is_empty() { "pitsadmin" } else { &user_slug }
        );
        let identity = serde_json::json!({
            "type": "user",
            "userEntityRef": user_ref,
            "ownershipEntityRefs": [user_ref, "group:default/operators", "group:default/risk-engineering"],
        });
        // Behind Zero-Trust Gateway (oauth2-proxy + Istio RequestAuthentication), omit the
        // synthetic unsigned guest JWT so Backstage fetchApi relies on the gateway session cookie
        // instead of sending an unsigned `Authorization: Bearer` header that Envoy JWKS rejects.
        return Json(serde_json::json!({
            "providerInfo": { "gatewayAuth": "oauth2-proxy" },
            "profile": {
                "displayName": raw_user,
                "email": email,
            },
            "backstageIdentity": {
                "expiresInSeconds": 3600,
                "identity": identity.clone(),
            },
            "expiresInSeconds": 3600,
            "identity": identity,
        }));
    }

    let now = chrono::Utc::now().timestamp();
    let exp = now + 86400;
    let payload = serde_json::json!({
        "sub": "user:default/pitsadmin",
        "ent": ["user:default/pitsadmin", "group:default/operators"],
        "iat": now,
        "exp": exp,
        "iss": "pitsctl-backstage-auth",
    });
    let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&payload).unwrap_or_default());
    let jwt = format!("eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.{payload_b64}.pits");

    let identity = serde_json::json!({
        "type": "user",
        "userEntityRef": "user:default/pitsadmin",
        "ownershipEntityRefs": ["user:default/pitsadmin", "group:default/operators"],
    });

    Json(serde_json::json!({
        "providerInfo": {},
        "profile": {
            "displayName": "PITS Operator",
            "email": "pitsadmin@pits.home.arpa",
        },
        "backstageIdentity": {
            "token": jwt,
            "expiresInSeconds": 86400,
            "identity": identity.clone(),
        },
        "token": jwt,
        "expiresInSeconds": 86400,
        "identity": identity,
    }))
}

// ---------------------------------------------------------------------------
// PITS Resource Portal REST API (`/api/pits/v1/*`)
// ---------------------------------------------------------------------------

async fn api_pits_resource_types(State(st): State<ConsoleState>, uri: Uri) -> impl IntoResponse {
    let pairs = parse_query_pairs(&uri);
    let refresh = query_flag_true(&pairs, "refresh") || query_flag_true(&pairs, "live");
    let types = st.catalog.list_resource_types(refresh).await;
    Json(types)
}

async fn api_pits_entity_detail(
    State(st): State<ConsoleState>,
    AxumPath((namespace, name)): AxumPath<(String, String)>,
    uri: Uri,
) -> std::result::Result<Json<crate::catalog::PitsEntityDetail>, (StatusCode, Json<serde_json::Value>)>
{
    let pairs = parse_query_pairs(&uri);
    let refresh = query_flag_true(&pairs, "refresh") || query_flag_true(&pairs, "live");
    match st
        .catalog
        .get_pits_entity_detail(&namespace, &name, refresh)
        .await
    {
        Ok(detail) => Ok(Json(detail)),
        Err(e) => {
            let msg = format!("{e:#}");
            if msg.to_ascii_lowercase().contains("not found")
                || msg.to_ascii_lowercase().contains("unknown")
            {
                Err(not_found_json(msg))
            } else {
                Err(err_resp(e))
            }
        }
    }
}

async fn api_pits_execute_action(
    State(st): State<ConsoleState>,
    AxumPath((namespace, name, action_id)): AxumPath<(String, String, String)>,
    body: Bytes,
) -> std::result::Result<
    Json<crate::catalog::ActionExecutionResult>,
    (StatusCode, Json<serde_json::Value>),
> {
    let req: ActionInvokeRequest = if body.is_empty() {
        ActionInvokeRequest {
            parameters: BTreeMap::new(),
            dry_run: false,
        }
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| err_resp(anyhow::anyhow!("invalid ActionInvokeRequest JSON: {e}")))?
    };

    let res = st
        .catalog
        .execute_entity_action(&namespace, &name, &action_id, req)
        .await
        .map_err(err_resp)?;
    Ok(Json(res))
}

async fn api_pits_catalog_refresh(State(st): State<ConsoleState>) -> impl IntoResponse {
    st.catalog.invalidate_cache().await;
    let types = st.catalog.list_resource_types(true).await;
    Json(types)
}

async fn api_pits_catalog_export(
    State(st): State<ConsoleState>,
    uri: Uri,
) -> std::result::Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let pairs = parse_query_pairs(&uri);
    let refresh = query_flag_true(&pairs, "refresh") || query_flag_true(&pairs, "live");
    let yaml = st.catalog.export_catalog_yaml(refresh).await.map_err(err_resp)?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/yaml; charset=utf-8")],
        yaml,
    )
        .into_response())
}

fn sanitized_profile_json(prof: &ClientProfile) -> serde_json::Value {
    let has_ca = prof
        .agent_ca_pem
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let has_cert = prof
        .agent_client_cert_pem
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let has_key = prof
        .agent_client_key_pem
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let has_token = prof
        .agent_bearer_token
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

    let eff_db_url = effective_sourcing_db_url(None, prof);

    serde_json::json!({
        "profilePath": default_profile_path().display().to_string(),
        "stateDir": local_state_dir().display().to_string(),
        "activeWorkspaceSource": prof.active_workspace_source,
        "sourcingDbConfigured": eff_db_url.is_some(),
        "sourcingDbEndpointRedacted": eff_db_url.as_deref().map(redact_db_url),
        "agentEndpoint": prof.agent_endpoint,
        "hasBearerToken": has_token,
        "hasClientPki": has_ca && has_cert && has_key,
        "hasCaPem": has_ca,
        "hasClientCertPem": has_cert,
        "coreKubeconfigPath": prof.core_kubeconfig_path,
        "coreContext": prof.core_context,
        "labKubeconfigPath": prof.lab_kubeconfig_path,
        "labContext": prof.lab_context,
        "labViaAgentBridge": prof.lab_via_agent_bridge,
        "talosconfigPath": prof.talosconfig_path,
        "talosContext": prof.talos_context,
        "talosViaAgentBridge": prof.talos_via_agent_bridge,
        "extraClusters": prof.extra_clusters,
    })
}

async fn api_pits_get_profile(State(st): State<ConsoleState>) -> impl IntoResponse {
    let prof = st.profile.read().await;
    Json(sanitized_profile_json(&prof))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PitsProfilePatch {
    active_workspace_source: Option<String>,
    sourcing_db_url: Option<String>,
    agent_endpoint: Option<String>,
    agent_bearer_token: Option<String>,
    core_kubeconfig_path: Option<String>,
    core_context: Option<String>,
    lab_kubeconfig_path: Option<String>,
    lab_context: Option<String>,
    lab_via_agent_bridge: Option<bool>,
    talosconfig_path: Option<String>,
    talos_context: Option<String>,
    talos_via_agent_bridge: Option<bool>,
    extra_clusters: Option<Vec<crate::config::ExtraClusterConfig>>,
}

fn apply_optional_field(slot: &mut Option<String>, incoming: Option<String>) {
    if let Some(val) = incoming {
        let trimmed = val.trim();
        if trimmed.is_empty() {
            *slot = None;
        } else {
            *slot = Some(trimmed.to_string());
        }
    }
}

async fn api_pits_save_profile(
    State(st): State<ConsoleState>,
    Json(patch): Json<PitsProfilePatch>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let (saved_json, next_pg_url) = {
        let mut prof = st.profile.write().await;
        if let Some(ep) = patch.agent_endpoint {
            let trimmed = ep.trim();
            if !trimmed.is_empty() {
                prof.agent_endpoint = trimmed.to_string();
            }
        }
        apply_optional_field(&mut prof.active_workspace_source, patch.active_workspace_source);
        apply_optional_field(&mut prof.sourcing_db_url, patch.sourcing_db_url);
        apply_optional_field(&mut prof.agent_bearer_token, patch.agent_bearer_token);
        apply_optional_field(&mut prof.core_kubeconfig_path, patch.core_kubeconfig_path);
        apply_optional_field(&mut prof.core_context, patch.core_context);
        apply_optional_field(&mut prof.lab_kubeconfig_path, patch.lab_kubeconfig_path);
        apply_optional_field(&mut prof.lab_context, patch.lab_context);
        apply_optional_field(&mut prof.talosconfig_path, patch.talosconfig_path);
        apply_optional_field(&mut prof.talos_context, patch.talos_context);

        if let Some(v) = patch.lab_via_agent_bridge {
            prof.lab_via_agent_bridge = v;
        }
        if let Some(v) = patch.talos_via_agent_bridge {
            prof.talos_via_agent_bridge = v;
        }
        if let Some(extras) = patch.extra_clusters {
            prof.extra_clusters = extras
                .into_iter()
                .filter(|c| !c.name.trim().is_empty() && !c.kubeconfig_path.trim().is_empty())
                .collect();
        }

        if let Some(ref ws_src) = prof.active_workspace_source {
            if let Ok(reloaded) = WorkspaceBundle::load_from_edge_router(Path::new(ws_src)) {
                let mut bundle = st.bundle.write().await;
                *bundle = reloaded;
            }
        }

        let saved_path = prof.save(None).map_err(err_resp)?;
        let next_pg = effective_sourcing_db_url(None, &prof);
        let mut out = sanitized_profile_json(&prof);
        if let Some(obj) = out.as_object_mut() {
            obj.insert("saved".to_string(), serde_json::Value::Bool(true));
            obj.insert(
                "savedPath".to_string(),
                serde_json::Value::String(saved_path.display().to_string()),
            );
        }
        (out, next_pg)
    };

    st.catalog.sourcing_set_postgres_url(next_pg_url).await;
    st.catalog.invalidate_cache().await;
    Ok(Json(saved_json))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PitsGenPkiReq {
    server_san: Option<String>,
}

async fn api_pits_generate_pki(
    State(st): State<ConsoleState>,
    body: Bytes,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let req: PitsGenPkiReq = if body.is_empty() {
        PitsGenPkiReq::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| err_resp(anyhow::anyhow!("invalid GeneratePKI JSON: {e}")))?
    };

    let san = req
        .server_san
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("100.95.95.88");
    let pki = generate_agent_mtls_pki(san).map_err(err_resp)?;

    let out_dir = local_state_dir().join("agent-pki");
    fs::create_dir_all(&out_dir).map_err(|e| err_resp(e.into()))?;
    fs::write(out_dir.join("ca.crt"), &pki.ca_cert_pem).map_err(|e| err_resp(e.into()))?;
    fs::write(out_dir.join("server.crt"), &pki.server_cert_pem).map_err(|e| err_resp(e.into()))?;
    fs::write(out_dir.join("server.key"), &pki.server_key_pem).map_err(|e| err_resp(e.into()))?;

    let saved_path = {
        let mut prof = st.profile.write().await;
        prof.agent_ca_pem = Some(pki.ca_cert_pem.clone());
        prof.agent_client_cert_pem = Some(pki.client_cert_pem.clone());
        prof.agent_client_key_pem = Some(pki.client_key_pem.clone());
        prof.save(None).map_err(err_resp)?
    };

    st.catalog.invalidate_cache().await;

    Ok(Json(serde_json::json!({
        "saved": true,
        "hasClientPki": true,
        "savedProfile": saved_path.display().to_string(),
        "exportedAgentServerCertsDir": out_dir.display().to_string(),
        "serverSan": san,
        "caCertPem": pki.ca_cert_pem,
        "serverCertPem": pki.server_cert_pem,
    })))
}

// ---------------------------------------------------------------------------
// Legacy v1 Domain Endpoints (`/api/v1/*`) — Preserved for Backward Compat
// ---------------------------------------------------------------------------

async fn api_overview(State(st): State<ConsoleState>) -> impl IntoResponse {
    let prof = st.profile.read().await.clone();
    let bundle = st.bundle.read().await.clone();

    let agent_health = match AgentClient::from_profile(&prof) {
        Ok(ag) => ag.get_json::<serde_json::Value>("/v1/health").await.ok(),
        Err(_) => None,
    };

    let default_er = pits_core_default_edge_router();
    let router_val = validate_edge_router(&default_er);
    let default_er_yaml = serde_yaml::to_string(&default_er).unwrap_or_default();

    let nodes: Vec<serde_json::Value> = bundle
        .nodes
        .iter()
        .map(|n| {
            serde_json::json!({
                "deviceId": n.device_id,
                "model": n.model,
                "cpuModel": n.cpu_model,
                "npuVendor": n.npu_vendor,
                "npuPciId": n.npu_pci_id,
                "nodeIp": n.node_ip,
                "targetIp": n.target_ip,
                "pxeMac": n.pxe_mac,
                "hasIntelNpu": n.has_intel_npu,
                "vmStore": n.san.vm_store,
                "systemWwid": expected_wwid(&n.san.subsystem_nqn, "system"),
                "userdataWwid": expected_wwid(&n.san.subsystem_nqn, "userdata"),
            })
        })
        .collect();

    Json(serde_json::json!({
        "profile": sanitized_profile_json(&prof),
        "stateDir": local_state_dir().display().to_string(),
        "workspaceSource": bundle.source_dir,
        "importedFilesCount": bundle.imported_files.len(),
        "agentHealth": agent_health,
        "routerValidation": router_val,
        "defaultRouterYaml": default_er_yaml,
        "nodes": nodes,
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfilePatch {
    agent_endpoint: Option<String>,
    core_kubeconfig_path: Option<String>,
    lab_kubeconfig_path: Option<String>,
    talosconfig_path: Option<String>,
}

async fn api_save_profile(
    State(st): State<ConsoleState>,
    Json(patch): Json<ProfilePatch>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let saved_path = {
        let mut prof = st.profile.write().await;
        if let Some(ep) = patch.agent_endpoint.filter(|s| !s.trim().is_empty()) {
            prof.agent_endpoint = ep;
        }
        prof.core_kubeconfig_path = patch.core_kubeconfig_path.filter(|s| !s.trim().is_empty());
        prof.lab_kubeconfig_path = patch.lab_kubeconfig_path.filter(|s| !s.trim().is_empty());
        prof.talosconfig_path = patch.talosconfig_path.filter(|s| !s.trim().is_empty());
        prof.save(None).map_err(err_resp)?
    };
    st.catalog.invalidate_cache().await;
    Ok(Json(serde_json::json!({
        "saved": true,
        "path": saved_path.display().to_string(),
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenPkiReq {
    server_san: Option<String>,
}

async fn api_generate_pki(
    State(st): State<ConsoleState>,
    Json(req): Json<GenPkiReq>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let san = req.server_san.as_deref().unwrap_or("192.168.71.50");
    let pki = generate_agent_mtls_pki(san).map_err(err_resp)?;
    let saved_path = {
        let mut prof = st.profile.write().await;
        prof.agent_ca_pem = Some(pki.ca_cert_pem.clone());
        prof.agent_client_cert_pem = Some(pki.client_cert_pem.clone());
        prof.agent_client_key_pem = Some(pki.client_key_pem.clone());
        prof.save(None).map_err(err_resp)?
    };
    st.catalog.invalidate_cache().await;
    Ok(Json(serde_json::json!({
        "savedProfile": saved_path.display().to_string(),
        "caCertPem": pki.ca_cert_pem,
        "serverCertPem": pki.server_cert_pem,
        "serverKeyPem": pki.server_key_pem,
    })))
}

async fn api_router_status(State(st): State<ConsoleState>) -> impl IntoResponse {
    let core_k8s = st.build_core_k8s().await.ok();
    let agent = st.build_agent().await.ok();
    let summary = inspect_router_live(core_k8s.as_ref(), agent.as_ref()).await;
    Json(summary)
}

#[derive(Deserialize)]
struct YamlPayload {
    yaml: String,
}

async fn api_router_validate(
    Json(req): Json<YamlPayload>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let manifest: EdgeRouterManifest = serde_yaml::from_str(&req.yaml)
        .map_err(|e| err_resp(anyhow::anyhow!("EdgeRouter YAML parse error: {e}")))?;
    let rep = validate_edge_router(&manifest);
    Ok(Json(serde_json::to_value(rep).unwrap()))
}

async fn api_router_apply(
    State(st): State<ConsoleState>,
    Json(req): Json<YamlPayload>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let manifest: EdgeRouterManifest = serde_yaml::from_str(&req.yaml)
        .map_err(|e| err_resp(anyhow::anyhow!("EdgeRouter YAML parse error: {e}")))?;
    let kc = st.build_core_k8s().await.map_err(err_resp)?;
    let out = apply_edge_router(&kc, &manifest).await.map_err(err_resp)?;
    st.catalog.invalidate_cache().await;
    Ok(Json(out))
}

#[derive(Deserialize)]
struct PausePayload {
    paused: bool,
}

async fn api_router_pause(
    State(st): State<ConsoleState>,
    Json(req): Json<PausePayload>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let kc = st.build_core_k8s().await.map_err(err_resp)?;
    set_crossplane_paused(&kc, "edge", "pits-core", req.paused)
        .await
        .map_err(err_resp)?;
    st.catalog.invalidate_cache().await;
    Ok(Json(serde_json::json!({ "paused": req.paused })))
}

async fn api_router_recover(
    State(st): State<ConsoleState>,
    Json(req): Json<serde_json::Value>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let ag = st.build_agent().await.map_err(err_resp)?;
    let cleanup = req
        .get("cleanupPerfNetns")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let refresh = req
        .get("refreshDevicePlugins")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let out = recover_router_runtime(&ag, cleanup, refresh, 60)
        .await
        .map_err(err_resp)?;
    st.catalog.invalidate_cache().await;
    Ok(Json(out))
}

async fn api_router_vpp_trace(
    State(st): State<ConsoleState>,
    Json(req): Json<serde_json::Value>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let ag = st.build_agent().await.map_err(err_resp)?;
    let out = ag.post_json("/v1/vpp/trace", &req).await.map_err(err_resp)?;
    Ok(Json(out))
}

async fn api_san_render_k8s(
    State(st): State<ConsoleState>,
    AxumPath(device): AxumPath<String>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let bundle = st.bundle.read().await;
    let node = bundle
        .find_node(&device)
        .ok_or_else(|| err_resp(anyhow::anyhow!("unknown node {device}")))?;
    let manifest = render_nvmet_k8s_manifests(&node.san).map_err(err_resp)?;
    Ok(Json(serde_json::json!({ "device": device, "manifest": manifest })))
}

async fn api_san_inspect(
    State(st): State<ConsoleState>,
    AxumPath(device): AxumPath<String>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let spec = {
        let bundle = st.bundle.read().await;
        bundle
            .find_node(&device)
            .ok_or_else(|| err_resp(anyhow::anyhow!("unknown node {device}")))?
            .san
            .clone()
    };
    let ag = st.build_agent().await.map_err(err_resp)?;
    let out = san_inspect(&ag, &spec).await.map_err(err_resp)?;
    Ok(Json(out))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SanProvReq {
    #[serde(default)]
    hot_add_only: bool,
    #[serde(default = "bool_true")]
    configure_listener: bool,
    #[serde(default)]
    dry_run: bool,
}

fn bool_true() -> bool {
    true
}

async fn api_san_provision(
    State(st): State<ConsoleState>,
    AxumPath(device): AxumPath<String>,
    Json(req): Json<SanProvReq>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let spec = {
        let bundle = st.bundle.read().await;
        bundle
            .find_node(&device)
            .ok_or_else(|| err_resp(anyhow::anyhow!("unknown node {device}")))?
            .san
            .clone()
    };
    let ag = st.build_agent().await.map_err(err_resp)?;
    let out = san_provision(&ag, &spec, req.hot_add_only, req.configure_listener, req.dry_run)
        .await
        .map_err(err_resp)?;
    if !req.dry_run {
        st.catalog.invalidate_cache().await;
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SanExpReq {
    namespace_id: i32,
    new_bytes: i64,
}

async fn api_san_expand(
    State(st): State<ConsoleState>,
    AxumPath(device): AxumPath<String>,
    Json(req): Json<SanExpReq>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let ag = st.build_agent().await.map_err(err_resp)?;
    let out = san_expand(&ag, &device, req.namespace_id, req.new_bytes, false)
        .await
        .map_err(err_resp)?;
    st.catalog.invalidate_cache().await;
    Ok(Json(out))
}

async fn api_btrfs_status(
    State(st): State<ConsoleState>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let ag = st.build_agent().await.map_err(err_resp)?;
    let out = btrfs_status(&ag).await.map_err(err_resp)?;
    Ok(Json(out))
}

#[derive(Deserialize)]
struct MaintainReq {
    operation: String,
}

async fn api_btrfs_maintain(
    State(st): State<ConsoleState>,
    Json(req): Json<MaintainReq>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let ag = st.build_agent().await.map_err(err_resp)?;
    let out = btrfs_maintain(&ag, &req.operation).await.map_err(err_resp)?;
    st.catalog.invalidate_cache().await;
    Ok(Json(out))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PxeReq {
    device_id: String,
    #[serde(default)]
    mode: Option<PxeBootMode>,
}

async fn api_pxe_render(
    State(st): State<ConsoleState>,
    Json(req): Json<PxeReq>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let bundle = st.bundle.read().await;
    let node = bundle
        .find_node(&req.device_id)
        .ok_or_else(|| err_resp(anyhow::anyhow!("unknown node {}", req.device_id)))?;
    let mode = req.mode.unwrap_or(PxeBootMode::TalosNetboot);
    Ok(Json(serde_json::json!({
        "ipxeScript": render_ipxe_script(node, mode, None),
        "hardwareYaml": render_tinkerbell_hardware_yaml(node, mode, None),
    })))
}

async fn api_pxe_apply_hardware(
    State(st): State<ConsoleState>,
    Json(req): Json<PxeReq>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let node = {
        let bundle = st.bundle.read().await;
        bundle
            .find_node(&req.device_id)
            .ok_or_else(|| err_resp(anyhow::anyhow!("unknown node {}", req.device_id)))?
            .clone()
    };
    let kc = st.build_core_k8s().await.map_err(err_resp)?;
    let mode = req.mode.unwrap_or(PxeBootMode::TalosNetboot);
    let out = apply_tinkerbell_hardware(&kc, &node, mode, None)
        .await
        .map_err(err_resp)?;
    st.catalog.invalidate_cache().await;
    Ok(Json(out))
}

async fn api_pxe_verify_assets(
    State(st): State<ConsoleState>,
    Json(req): Json<PxeReq>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let ag = st.build_agent().await.map_err(err_resp)?;
    let out = pxe_verify_assets(&ag, &req.device_id, BTreeMap::new())
        .await
        .map_err(err_resp)?;
    Ok(Json(out))
}

async fn api_pxe_inventory(
    State(st): State<ConsoleState>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let ag = st.build_agent().await.map_err(err_resp)?;
    let out = pxe_list_inventory(&ag).await.map_err(err_resp)?;
    Ok(Json(serde_json::to_value(out).unwrap()))
}

async fn api_talos_render(
    State(st): State<ConsoleState>,
    AxumPath(device): AxumPath<String>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let bundle = st.bundle.read().await;
    let node = bundle
        .find_node(&device)
        .ok_or_else(|| err_resp(anyhow::anyhow!("unknown node {device}")))?;
    let yaml = render_talos_node_config(node, "https://10.89.20.21:6443").map_err(err_resp)?;
    Ok(Json(serde_json::json!({ "device": device, "yaml": yaml })))
}

async fn api_talos_imager(
    State(st): State<ConsoleState>,
    AxumPath(device): AxumPath<String>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let bundle = st.bundle.read().await;
    let node = bundle
        .find_node(&device)
        .ok_or_else(|| err_resp(anyhow::anyhow!("unknown node {device}")))?;
    let profile = render_talos_imager_profile(node, true);
    Ok(Json(serde_json::json!({ "device": device, "profileYaml": profile })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TalosGuardReq {
    device_id: String,
    yaml: String,
}

async fn api_talos_guard(
    State(st): State<ConsoleState>,
    Json(req): Json<TalosGuardReq>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let bundle = st.bundle.read().await;
    let node = bundle
        .find_node(&req.device_id)
        .ok_or_else(|| err_resp(anyhow::anyhow!("unknown node {}", req.device_id)))?;
    let allowed = allowed_wwids_for_node(node);
    let rep = validate_talos_netboot_config(&req.yaml, &allowed, &node.approved_vfio_pci);
    Ok(Json(serde_json::to_value(rep).unwrap()))
}

async fn api_talos_status(
    State(st): State<ConsoleState>,
    AxumPath(device): AxumPath<String>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let node_ip = {
        let bundle = st.bundle.read().await;
        bundle
            .find_node(&device)
            .map(|n| n.node_ip.clone())
            .unwrap_or(device)
    };
    let tc = st.build_talos().await.map_err(err_resp)?;
    let status = tc.inspect_node(&node_ip).await;
    Ok(Json(serde_json::to_value(status).unwrap()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TalosApplyReq {
    device_id: String,
    yaml: String,
    #[serde(default = "default_apply_mode")]
    mode: u64,
    #[serde(default = "bool_true")]
    verify_live_wwids: bool,
}

fn default_apply_mode() -> u64 {
    2 // NO_REBOOT
}

async fn api_talos_apply(
    State(st): State<ConsoleState>,
    Json(req): Json<TalosApplyReq>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let node = {
        let bundle = st.bundle.read().await;
        bundle
            .find_node(&req.device_id)
            .ok_or_else(|| err_resp(anyhow::anyhow!("unknown node {}", req.device_id)))?
            .clone()
    };
    let tc = st.build_talos().await.map_err(err_resp)?;
    let messages = tc
        .apply_configuration(&node, &req.yaml, req.mode, false, req.verify_live_wwids)
        .await
        .map_err(err_resp)?;
    st.catalog.invalidate_cache().await;
    Ok(Json(serde_json::json!({ "applied": true, "messages": messages })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TalosRebootReq {
    device_id: String,
    #[serde(default = "bool_true")]
    powercycle: bool,
}

async fn api_talos_reboot(
    State(st): State<ConsoleState>,
    Json(req): Json<TalosRebootReq>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let node_ip = {
        let bundle = st.bundle.read().await;
        bundle
            .find_node(&req.device_id)
            .map(|n| n.node_ip.clone())
            .unwrap_or(req.device_id.clone())
    };
    let tc = st.build_talos().await.map_err(err_resp)?;
    tc.reboot_node(&node_ip, req.powercycle)
        .await
        .map_err(err_resp)?;
    st.catalog.invalidate_cache().await;
    Ok(Json(serde_json::json!({ "rebooted": node_ip, "powercycle": req.powercycle })))
}

async fn api_k8s_summary(State(st): State<ConsoleState>) -> impl IntoResponse {
    let core = match st.build_core_k8s().await {
        Ok(kc) => Some(inspect_cluster_summary(&kc).await),
        Err(e) => Some(crate::k8s::ClusterControlPlaneSummary {
            cluster: "pits-core".to_string(),
            server_url: String::new(),
            via_agent_bridge: false,
            reachable: false,
            checked_at: chrono::Utc::now(),
            git_version: None,
            nodes: vec![],
            running_pods: 0,
            total_pods: 0,
            bound_pvs: 0,
            npu_slices: vec![],
            dra_slices: vec![],
            dra_claims: vec![],
            device_plugin_gpu_used: std::collections::BTreeMap::new(),
            device_classes: vec![],
            error: Some(e.to_string()),
        }),
    };
    let lab = match st.build_lab_k8s().await {
        Ok(kc) => Some(inspect_cluster_summary(&kc).await),
        Err(e) => Some(crate::k8s::ClusterControlPlaneSummary {
            cluster: "pits-lab".to_string(),
            server_url: String::new(),
            via_agent_bridge: true,
            reachable: false,
            checked_at: chrono::Utc::now(),
            git_version: None,
            nodes: vec![],
            running_pods: 0,
            total_pods: 0,
            bound_pvs: 0,
            npu_slices: vec![],
            dra_slices: vec![],
            dra_claims: vec![],
            device_plugin_gpu_used: std::collections::BTreeMap::new(),
            device_classes: vec![],
            error: Some(e.to_string()),
        }),
    };
    Json(serde_json::json!({ "core": core, "lab": lab }))
}

async fn api_k8s_npu_audit(State(st): State<ConsoleState>) -> impl IntoResponse {
    let bundle = st.bundle.read().await.clone();
    let ag = st.build_agent().await.ok();
    let core = st.build_core_k8s().await.ok();
    let lab = st.build_lab_k8s().await.ok();
    let tc = st.build_talos().await.ok();
    let rep = audit_fleet_npus(
        &bundle,
        ag.as_ref(),
        core.as_ref(),
        lab.as_ref(),
        tc.as_ref(),
    )
    .await;
    Json(rep)
}

async fn api_k8s_render_san_pv() -> impl IntoResponse {
    let yaml = render_san_local_pv_pvc(
        "pv-win11-vmstore",
        "pits-vms",
        "win11-vmstore-pvc",
        "netboot-004",
        "pits-vm-local",
        "/var/mnt/vmstore/win11-001",
        320,
    );
    Json(serde_json::json!({ "yaml": yaml }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NpuReq {
    node: String,
    device_class: String,
}

async fn api_k8s_npu_preview(Json(req): Json<NpuReq>) -> impl IntoResponse {
    let claim = format!("npu-claim-{}", req.node);
    let job = format!("npu-verify-{}", req.node);
    let yaml = render_npu_validation_manifests(
        "default",
        &claim,
        &job,
        &req.device_class,
        Some(&req.node),
    );
    Json(serde_json::json!({ "yaml": yaml }))
}

async fn api_k8s_npu_launch(
    State(st): State<ConsoleState>,
    Json(req): Json<NpuReq>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let kc = st.build_lab_k8s().await.map_err(err_resp)?;
    let claim = format!("npu-claim-{}", req.node);
    let job = format!("npu-verify-{}", req.node);
    launch_npu_validation_job(
        &kc,
        "default",
        &claim,
        &job,
        &req.device_class,
        Some(&req.node),
    )
    .await
    .map_err(err_resp)?;
    st.catalog.invalidate_cache().await;
    Ok(Json(serde_json::json!({ "launchedJob": job, "claim": claim })))
}

async fn api_vms_list(State(st): State<ConsoleState>) -> impl IntoResponse {
    let mut all = Vec::new();
    let mut errors = Vec::new();
    if let Ok(core) = st.build_core_k8s().await {
        match list_cluster_vms(&core).await {
            Ok(v) => all.extend(v),
            Err(e) => errors.push(format!("core: {e}")),
        }
    }
    if let Ok(lab) = st.build_lab_k8s().await {
        match list_cluster_vms(&lab).await {
            Ok(v) => all.extend(v),
            Err(e) => errors.push(format!("lab: {e}")),
        }
    }
    Json(serde_json::json!({ "vms": all, "errors": errors }))
}

async fn api_vms_templates() -> impl IntoResponse {
    Json(serde_json::json!({
        "win11": render_windows11_vm_manifest(false),
        "accessLab": render_access_lab_vm_manifest(),
    }))
}

#[derive(Deserialize)]
struct VmActionReq {
    cluster: String,
    namespace: String,
    name: String,
    action: VmLifecycleOp,
}

async fn api_vms_lifecycle(
    State(st): State<ConsoleState>,
    Json(req): Json<VmActionReq>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let kc = if req.cluster == "core" {
        st.build_core_k8s().await.map_err(err_resp)?
    } else {
        st.build_lab_k8s().await.map_err(err_resp)?
    };
    vm_lifecycle_action(&kc, &req.namespace, &req.name, req.action)
        .await
        .map_err(err_resp)?;
    st.catalog.invalidate_cache().await;
    Ok(Json(serde_json::json!({
        "cluster": req.cluster,
        "vm": format!("{}/{}", req.namespace, req.name),
        "action": req.action,
    })))
}

async fn api_vms_finalize_win11(
    State(st): State<ConsoleState>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let kc = st.build_lab_k8s().await.map_err(err_resp)?;
    finalize_windows11_vm(&kc).await.map_err(err_resp)?;
    st.catalog.invalidate_cache().await;
    Ok(Json(serde_json::json!({ "finalized": "pits-vms/win11-001" })))
}

#[derive(Deserialize)]
struct RdpReq {
    host: String,
    port: u16,
    #[serde(default)]
    launch: bool,
}

async fn api_vms_rdp(
    State(st): State<ConsoleState>,
    Json(req): Json<RdpReq>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let prof = st.profile.read().await.clone();
    let bridge_ag = if prof.lab_via_agent_bridge {
        AgentClient::from_profile(&prof).ok()
    } else {
        None
    };
    let bridge_pair = bridge_ag.as_ref().map(|ag| (&prof, ag.tls_config.clone()));
    let tls_info = verify_rdp_tls_endpoint(&req.host, req.port, bridge_pair)
        .await
        .map_err(err_resp)?;
    let rdp_content = render_pinned_rdp_profile(
        &req.host,
        req.port,
        "pitsadmin",
        Some(&tls_info.cert_sha1_hex),
        3072,
        1920,
    );
    let rdp_path = write_pinned_rdp_profile(
        "win11-001",
        &req.host,
        req.port,
        "pitsadmin",
        Some(&tls_info.cert_sha1_hex),
        3072,
        1920,
    )
    .map_err(err_resp)?;
    let mut launched = false;
    if req.launch && cfg!(target_os = "windows") {
        launch_mstsc(&rdp_path).map_err(err_resp)?;
        launched = true;
    }
    Ok(Json(serde_json::json!({
        "verification": tls_info,
        "rdpProfilePath": rdp_path.display().to_string(),
        "rdpContent": rdp_content,
        "launched": launched,
    })))
}

// ---------------------------------------------------------------------------
// Tech Insights (`@backstage-community/plugin-tech-insights` protocol) & Sourcing Governance APIs
// ---------------------------------------------------------------------------

fn sourcing_json_error(
    status: StatusCode,
    msg: impl Into<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let m = msg.into();
    (
        status,
        Json(serde_json::json!({
            "error": {
                "name": if status == StatusCode::NOT_FOUND { "NotFoundError" } else { "ValidationError" },
                "message": m,
            },
            "message": m,
        })),
    )
}

async fn api_tech_insights_fact_schemas() -> Json<serde_json::Value> {
    let schema = crate::catalog::sourcing::tech_insights_fact_schema();
    Json(serde_json::json!([schema]))
}

async fn api_tech_insights_checks() -> Json<serde_json::Value> {
    let checks = crate::catalog::sourcing::tech_insights_check_definitions();
    Json(serde_json::json!(checks))
}

async fn api_tech_insights_facts_latest(
    State(st): State<ConsoleState>,
    axum::extract::Path((namespace, kind, name)): axum::extract::Path<(String, String, String)>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let eval = st
        .catalog
        .evaluate_sourcing_for_entity(&namespace, &kind, &name)
        .await
        .map_err(|e| sourcing_json_error(StatusCode::NOT_FOUND, e.to_string()))?;

    let resp = crate::catalog::sourcing::TechInsightsLatestFactsResponse {
        entity: serde_json::json!({
            "namespace": namespace,
            "kind": "Resource",
            "name": name,
        }),
        retriever_id: "sourcing-kraljic-windmill-retriever".to_string(),
        version: "1.0.0".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        facts: eval.facts.clone(),
        breakdown: eval.facts.breakdown.clone(),
        lineage: eval.lineage,
    };
    Ok(Json(serde_json::to_value(resp).unwrap_or_default()))
}

async fn api_tech_insights_checks_result(
    State(st): State<ConsoleState>,
    axum::extract::Path((namespace, kind, name)): axum::extract::Path<(String, String, String)>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let eval = st
        .catalog
        .evaluate_sourcing_for_entity(&namespace, &kind, &name)
        .await
        .map_err(|e| sourcing_json_error(StatusCode::NOT_FOUND, e.to_string()))?;

    let redline_count = eval.checks.iter().filter(|c| c.triggered).count();
    let resp = crate::catalog::sourcing::TechInsightsCheckEntityResponse {
        entity_ref: eval.entity_ref,
        entity: serde_json::json!({
            "namespace": namespace,
            "kind": "Resource",
            "name": name,
        }),
        evaluated_at: chrono::Utc::now().to_rfc3339(),
        passed_all: redline_count == 0,
        redline_count,
        facts: eval.facts,
        checks: eval.checks,
        lineage: eval.lineage,
        remediation_playbook: eval.remediation_playbook,
    };
    Ok(Json(serde_json::to_value(resp).unwrap_or_default()))
}

async fn api_pits_sourcing_overview(
    State(st): State<ConsoleState>,
    axum::extract::Query(params): axum::extract::Query<BTreeMap<String, String>>,
) -> Json<serde_json::Value> {
    let force_live = params
        .get("forceLive")
        .map(|s| s == "true" || s == "1")
        .unwrap_or(false);
    let overview = st.catalog.sourcing_overview(force_live).await;
    Json(serde_json::to_value(overview).unwrap_or_default())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourcingValidateBody {
    yaml: Option<String>,
    record: Option<crate::catalog::SourcingResourceRecord>,
    raw_payload: Option<serde_json::Value>,
}

async fn api_pits_sourcing_validate(
    State(st): State<ConsoleState>,
    Json(body): Json<SourcingValidateBody>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let snap = st.catalog.get_snapshot(false).await;
    let mut sys_domains: BTreeMap<String, (String, String)> =
        crate::catalog::sourcing::default_system_domain_map()
            .into_iter()
            .map(|(k, (d, t))| (k, (d.to_string(), t.to_string())))
            .collect();
    for ent in &snap.entities {
        if ent.kind == "System" {
            let sys_ref = format!(
                "system:{}/{}",
                ent.metadata.namespace.to_ascii_lowercase(),
                ent.metadata.name.to_ascii_lowercase()
            );
            let dom_raw = ent
                .spec
                .get("domain")
                .and_then(|v| v.as_str())
                .unwrap_or("platform-infra");
            let dom_ref = if dom_raw.contains(':') {
                dom_raw.to_ascii_lowercase()
            } else {
                format!("domain:default/{}", dom_raw.to_ascii_lowercase())
            };
            sys_domains
                .entry(sys_ref)
                .or_insert_with(|| (dom_ref, "Registered System Domain".to_string()));
        }
    }

    let report = if let Some(yaml_str) = body.yaml.as_deref().filter(|s| !s.trim().is_empty()) {
        crate::catalog::sourcing::validate_raw_sourcing_yaml_payload(yaml_str, &sys_domains)
    } else if let Some(ref rec) = body.record {
        crate::catalog::sourcing::validate_sourcing_record(
            rec,
            body.raw_payload.as_ref(),
            &sys_domains,
        )
    } else {
        return Err(sourcing_json_error(
            StatusCode::BAD_REQUEST,
            "Provide either 'yaml' or 'record' to /api/pits/v1/sourcing/validate",
        ));
    };

    Ok(Json(serde_json::to_value(report).unwrap_or_default()))
}

async fn api_pits_sourcing_upsert(
    State(st): State<ConsoleState>,
    Json(req): Json<crate::catalog::SourcingWizardUpsertRequest>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let resp = st
        .catalog
        .upsert_sourcing_from_wizard(req)
        .await
        .map_err(|e| sourcing_json_error(StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(serde_json::to_value(resp).unwrap_or_default()))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SourcingDbSyncBody {
    pg_url_override: Option<String>,
}

async fn api_pits_sourcing_db_sync(
    State(st): State<ConsoleState>,
    body: Bytes,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let parsed: SourcingDbSyncBody = if body.is_empty() {
        SourcingDbSyncBody::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| sourcing_json_error(StatusCode::BAD_REQUEST, format!("Invalid sync JSON: {e}")))?
    };
    let pg_override = parsed
        .pg_url_override
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let status = st.catalog.sourcing_sync_db(pg_override).await;
    Ok(Json(serde_json::to_value(status).unwrap_or_default()))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SourcingDiscoverBody {
    root_path: Option<String>,
    actor: Option<String>,
}

async fn api_pits_sourcing_discover(
    State(st): State<ConsoleState>,
    body: Bytes,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let parsed: SourcingDiscoverBody = if body.is_empty() {
        SourcingDiscoverBody::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| sourcing_json_error(StatusCode::BAD_REQUEST, format!("Invalid discover JSON: {e}")))?
    };
    let root = parsed
        .root_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let actor = parsed
        .actor
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("portal-discovery");
    let candidates = st
        .catalog
        .sourcing_scan_workspace(root.as_deref(), actor, "portal-discovery")
        .await
        .map_err(|e| sourcing_json_error(StatusCode::BAD_REQUEST, e.to_string()))?;
    let db_sync = st.catalog.sourcing_db_status().await;
    Ok(Json(serde_json::json!({
        "ok": true,
        "totalCandidates": candidates.len(),
        "pendingCount": candidates.iter().filter(|c| c.status == "pending").count(),
        "candidates": candidates,
        "dbSync": db_sync,
    })))
}

async fn api_pits_sourcing_candidate_disposition(
    State(st): State<ConsoleState>,
    Json(req): Json<CandidateDispositionRequest>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let (candidate, evaluation) = st
        .catalog
        .sourcing_promote_or_ignore_candidate(req, "portal-discovery")
        .await
        .map_err(|e| sourcing_json_error(StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "candidate": candidate,
        "evaluation": evaluation,
    })))
}

async fn api_pits_sourcing_submit_ballot(
    State(st): State<ConsoleState>,
    Json(req): Json<SubmitBlindBallotRequest>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let unit_name = req.unit_name.clone();
    let stats = st
        .catalog
        .sourcing_submit_ballot_with_channel(req, "portal-ballot")
        .await
        .map_err(|e| sourcing_json_error(StatusCode::BAD_REQUEST, e.to_string()))?;
    let evaluation = st
        .catalog
        .evaluate_sourcing_for_entity("default", "resource", &unit_name)
        .await
        .ok();
    Ok(Json(serde_json::json!({
        "ok": true,
        "unitName": unit_name,
        "meanScores": stats.mean_scores,
        "maxSpread": stats.max_spread,
        "varianceDimensions": stats.variance_dimensions,
        "varianceAlert": stats.variance_alert,
        "evaluation": evaluation,
    })))
}

async fn api_mcp_post(
    State(st): State<ConsoleState>,
    Json(msg): Json<serde_json::Value>,
) -> Response {
    match Box::pin(crate::mcp::handle_mcp_jsonrpc_value(&st.catalog, msg)).await {
        Some(reply) => {
            let body = serde_json::to_vec(&reply).unwrap_or_else(|_| b"{}".to_vec());
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response()
        }
        None => StatusCode::ACCEPTED.into_response(),
    }
}

async fn api_mcp_get() -> impl IntoResponse {
    Json(serde_json::json!({
        "name": "pits-sourcing-mcp",
        "version": env!("CARGO_PKG_VERSION"),
        "transport": "streamable-http",
        "protocolVersion": "2025-03-26",
        "postEndpoint": "/mcp",
        "stdioCommand": "pitsctl mcp"
    }))
}

async fn api_mcp_delete() -> StatusCode {
    StatusCode::NO_CONTENT
}

// ---------------------------------------------------------------------------
// Self-Managed Project Hub REST API & Streamable HTTP MCP (`/mcp/hub`)
// ---------------------------------------------------------------------------

fn hub_err_response(err: HubError) -> Response {
    let status = match &err {
        HubError::Forbidden(_) => StatusCode::FORBIDDEN,
        HubError::NotFound(_) => StatusCode::NOT_FOUND,
        HubError::BadRequest(_) => StatusCode::BAD_REQUEST,
        HubError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(serde_json::json!({
            "error": err.to_string(),
            "status": status.as_u16(),
        })),
    )
        .into_response()
}

fn extract_portal_human_actor(headers: &HeaderMap) -> (String, String) {
    let fido_user = headers
        .get("x-auth-request-user")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let fido_email = headers
        .get("x-auth-request-email")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(u) = fido_user.or(fido_email) {
        return (u.to_string(), "keycloak-fido2".to_string());
    }
    let direct_op = headers
        .get("x-pits-operator")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("pitsadmin");
    (direct_op.to_string(), "tailscale-direct".to_string())
}

async fn api_hub_overview(State(st): State<ConsoleState>) -> Response {
    match st.hub.db.overview() {
        Ok(ov) => Json(ov).into_response(),
        Err(e) => hub_err_response(e),
    }
}

async fn api_hub_list_projects(State(st): State<ConsoleState>) -> Response {
    match st.hub.db.list_projects() {
        Ok(projects) => Json(serde_json::json!({ "projects": projects })).into_response(),
        Err(e) => hub_err_response(e),
    }
}

async fn api_hub_upsert_project(
    State(st): State<ConsoleState>,
    headers: HeaderMap,
    Json(req): Json<UpsertProjectRequest>,
) -> Response {
    let (actor, _) = extract_portal_human_actor(&headers);
    match st.hub.db.upsert_project(req, "portal_ui", &actor) {
        Ok(project) => Json(serde_json::json!({ "project": project })).into_response(),
        Err(e) => hub_err_response(e),
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct HubTasksQueryParams {
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    query: Option<String>,
}

async fn api_hub_list_tasks(
    State(st): State<ConsoleState>,
    Query(q): Query<HubTasksQueryParams>,
) -> Response {
    let filter = TaskQueryFilter {
        project_id: q.project_id,
        status: q.status,
        tag: q.tag,
        priority: q.priority,
        workspace: q.workspace,
        query: q.q.or(q.query),
    };
    match st.hub.db.query_tasks(&filter) {
        Ok(tasks) => Json(serde_json::json!({ "tasks": tasks })).into_response(),
        Err(e) => hub_err_response(e),
    }
}

async fn api_hub_create_task(
    State(st): State<ConsoleState>,
    headers: HeaderMap,
    Json(req): Json<CreateTaskRequest>,
) -> Response {
    let (actor, _) = extract_portal_human_actor(&headers);
    match st.hub.db.create_task(req, "portal_ui", &actor) {
        Ok(task) => (StatusCode::CREATED, Json(serde_json::json!({ "task": task }))).into_response(),
        Err(e) => hub_err_response(e),
    }
}

async fn api_hub_get_task(
    State(st): State<ConsoleState>,
    AxumPath(task_key): AxumPath<String>,
) -> Response {
    match st.hub.db.get_task_detail(&task_key) {
        Ok(detail) => Json(detail).into_response(),
        Err(e) => hub_err_response(e),
    }
}

async fn api_hub_patch_task(
    State(st): State<ConsoleState>,
    AxumPath(task_key): AxumPath<String>,
    headers: HeaderMap,
    Json(req): Json<UpdateTaskRequest>,
) -> Response {
    let (actor, _) = extract_portal_human_actor(&headers);
    match st.hub.db.update_task(&task_key, req, "portal_ui", &actor) {
        Ok(task) => Json(serde_json::json!({ "task": task })).into_response(),
        Err(e) => hub_err_response(e),
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct HubSessionsQueryParams {
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    log_type: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    task_key: Option<String>,
    #[serde(default)]
    accepted: Option<bool>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn api_hub_list_sessions(
    State(st): State<ConsoleState>,
    Query(q): Query<HubSessionsQueryParams>,
) -> Response {
    let filter = SessionQueryFilter {
        project_id: q.project_id,
        log_type: q.log_type,
        tag: q.tag,
        task_key: q.task_key,
        accepted: q.accepted,
        limit: q.limit,
    };
    match st.hub.db.query_sessions(&filter) {
        Ok(sessions) => Json(serde_json::json!({ "sessions": sessions })).into_response(),
        Err(e) => hub_err_response(e),
    }
}

async fn api_hub_create_session(
    State(st): State<ConsoleState>,
    headers: HeaderMap,
    Json(req): Json<CreateSessionLogRequest>,
) -> Response {
    let (actor, _) = extract_portal_human_actor(&headers);
    match st.hub.db.create_session_log(req, "portal_ui", &actor) {
        Ok(session) => {
            (StatusCode::CREATED, Json(serde_json::json!({ "session": session }))).into_response()
        }
        Err(e) => hub_err_response(e),
    }
}

async fn api_hub_accept_session(
    State(st): State<ConsoleState>,
    AxumPath(session_id): AxumPath<String>,
    headers: HeaderMap,
    payload: Option<Json<AcceptSessionRequest>>,
) -> Response {
    let (actor, auth_source) = extract_portal_human_actor(&headers);
    let req = payload.map(|Json(v)| v).unwrap_or_default();
    match st
        .hub
        .db
        .accept_session(&session_id, req, "portal_ui", &actor, &auth_source)
    {
        Ok(outcome) => Json(outcome).into_response(),
        Err(e) => hub_err_response(e),
    }
}

async fn api_hub_reject_session(
    State(st): State<ConsoleState>,
    AxumPath(session_id): AxumPath<String>,
    headers: HeaderMap,
    Json(req): Json<RejectSessionRequest>,
) -> Response {
    let (actor, auth_source) = extract_portal_human_actor(&headers);
    match st
        .hub
        .db
        .reject_session(&session_id, req, "portal_ui", &actor, &auth_source)
    {
        Ok(outcome) => Json(outcome).into_response(),
        Err(e) => hub_err_response(e),
    }
}

#[derive(Debug, Deserialize, Default)]
struct HubSearchQueryParams {
    #[serde(default)]
    q: String,
    #[serde(default)]
    limit: Option<usize>,
}

async fn api_hub_search(
    State(st): State<ConsoleState>,
    Query(params): Query<HubSearchQueryParams>,
) -> Response {
    match st.hub.db.search(&params.q, params.limit.unwrap_or(20)) {
        Ok(res) => Json(res).into_response(),
        Err(e) => hub_err_response(e),
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct HubDbSyncRequest {
    #[serde(default)]
    pg_url_override: Option<String>,
}

async fn api_hub_db_sync(
    State(st): State<ConsoleState>,
    payload: Option<Json<HubDbSyncRequest>>,
) -> Response {
    let pg_override = payload.and_then(|Json(r)| r.pg_url_override);
    let status = st.hub.sync_db(pg_override.as_deref()).await;
    Json(status).into_response()
}

async fn api_hub_mcp_post(
    State(st): State<ConsoleState>,
    headers: HeaderMap,
    Json(msg): Json<serde_json::Value>,
) -> Response {
    let configured_token = {
        let prof = st.profile.read().await;
        effective_hub_mcp_token(&prof)
    };
    let x_token = headers
        .get("x-pits-mcp-token")
        .and_then(|v| v.to_str().ok());
    let auth_hdr = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let host_hdr = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|v| v.to_str().ok());
    let x_user = headers
        .get("x-auth-request-user")
        .and_then(|v| v.to_str().ok());

    let auth_ctx = match verify_hub_mcp_request_auth(
        configured_token.as_deref(),
        x_token,
        auth_hdr,
        host_hdr,
        x_user,
    ) {
        Ok(ctx) => ctx,
        Err(msg) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32001, "message": msg }
                })),
            )
                .into_response();
        }
    };

    let reply = handle_hub_mcp_jsonrpc(&st.hub, msg, &auth_ctx.actor).await;
    let status = StatusCode::from_u16(reply.http_status).unwrap_or(StatusCode::OK);
    match reply.body {
        Some(body_val) => {
            let body = serde_json::to_vec(&body_val).unwrap_or_else(|_| b"{}".to_vec());
            (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
        }
        None => status.into_response(),
    }
}

async fn api_hub_mcp_get() -> impl IntoResponse {
    Json(serde_json::json!({
        "name": "pits-project-hub-mcp",
        "version": env!("CARGO_PKG_VERSION"),
        "transport": "streamable-http",
        "protocolVersion": "2025-03-26",
        "postEndpoint": "/mcp/hub",
        "stdioCommand": "pitsctl hub mcp",
        "humanAcceptanceGate": true
    }))
}

async fn api_hub_mcp_delete() -> StatusCode {
    StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use hyper::service::Service;
    use hyper_util::service::TowerToHyperService;

    async fn call_raw(
        app: &Router,
        req: Request<Body>,
    ) -> (StatusCode, axum::http::HeaderMap, bytes::Bytes) {
        let app_cloned = app.clone();
        Box::pin(async move {
            let svc = TowerToHyperService::new(app_cloned);
            let resp = svc.call(req).await.unwrap();
            let status = resp.status();
            let headers = resp.headers().clone();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            (status, headers, bytes)
        })
        .await
    }

    async fn call_json(app: &Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
        let (status, _headers, bytes) = call_raw(app, req).await;
        let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!(
                "Failed to parse JSON (status={}): {} | body={:?}",
                status,
                e,
                String::from_utf8_lossy(&bytes)
            )
        });
        (status, val)
    }

    #[tokio::test]
    async fn test_web_console_index_and_domain_apis() {
        let state = ConsoleState::new(
            ClientProfile::default(),
            WorkspaceBundle::builtin_pits_baseline(),
        );
        let app = build_console_router(state);

        // 1. Embedded HTML Portal / Web Console & SPA deep-link fallback
        let (idx_status, idx_headers, idx_bytes) = call_raw(
            &app,
            Request::builder().uri("/").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(idx_status, StatusCode::OK);
        assert!(idx_headers
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/html"));
        let html = String::from_utf8(idx_bytes.to_vec()).unwrap();
        assert!(html.to_ascii_uppercase().contains("PITS") || html.contains("<html"));

        let (spa_status, spa_headers, _spa_bytes) = call_raw(
            &app,
            Request::builder()
                .uri("/catalog/default/resource/netboot-004")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(spa_status, StatusCode::OK);
        assert!(spa_headers
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/html"));

        // Unknown /api/* path must return 404 JSON rather than SPA HTML
        let (api404_status, _api404_body) = call_json(
            &app,
            Request::builder()
                .uri("/api/catalog/nonexistent-endpoint")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(api404_status, StatusCode::NOT_FOUND);

        // 2. Legacy Overview API
        let (st, ov) = call_json(
            &app,
            Request::builder()
                .uri("/api/v1/overview")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(ov["nodes"].as_array().unwrap().len(), 4);
        assert_eq!(ov["routerValidation"]["valid"], true);

        // 3. Router Validate API
        let default_yaml = ov["defaultRouterYaml"].as_str().unwrap().to_string();
        let (st, rv) = call_json(
            &app,
            Request::builder()
                .method("POST")
                .uri("/api/v1/router/validate")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "yaml": default_yaml }).to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(rv["valid"], true);

        // 4. SAN K8s render API
        let (st, san_k8s) = call_json(
            &app,
            Request::builder()
                .uri("/api/v1/san/render-k8s/netboot-004")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(san_k8s["manifest"]
            .as_str()
            .unwrap()
            .contains("nqn.2026-11.arpa.home.pits:netboot-004"));

        // 5. PXE Render API
        let (st, pxe_out) = call_json(
            &app,
            Request::builder()
                .method("POST")
                .uri("/api/v1/pxe/render")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "deviceId": "netboot-004" }).to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(pxe_out["ipxeScript"].as_str().unwrap().contains("#!ipxe"));
        assert!(pxe_out["hardwareYaml"]
            .as_str()
            .unwrap()
            .contains("allowPXE: true"));

        // 6. Talos Render + Netboot Guard API
        let (st, talos_out) = call_json(
            &app,
            Request::builder()
                .uri("/api/v1/talos/render/netboot-004")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let talos_yaml = talos_out["yaml"].as_str().unwrap().to_string();

        let (st, guard_out) = call_json(
            &app,
            Request::builder()
                .method("POST")
                .uri("/api/v1/talos/guard")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "deviceId": "netboot-004",
                        "yaml": talos_yaml,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(guard_out["passed"], true);

        // 7. K8s NPU DRA preview & VM templates API
        let (st, npu_out) = call_json(
            &app,
            Request::builder()
                .method("POST")
                .uri("/api/v1/k8s/npu-preview")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "node": "netboot-002",
                        "deviceClass": "npu.intel.com",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(npu_out["yaml"]
            .as_str()
            .unwrap()
            .contains("resource.k8s.io/v1"));

        let (st, vm_tpl) = call_json(
            &app,
            Request::builder()
                .uri("/api/v1/vms/templates")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(vm_tpl["win11"].as_str().unwrap().contains("win11-001"));
        assert!(vm_tpl["accessLab"]
            .as_str()
            .unwrap()
            .contains("7e:0a:35:9a:5a:d3"));
    }

    #[tokio::test]
    async fn test_backstage_catalog_and_pits_resource_portal_apis() {
        let mut prof = ClientProfile::default();
        prof.agent_client_key_pem = Some("-----BEGIN PRIVATE KEY-----\nSECRET\n-----END PRIVATE KEY-----".to_string());
        prof.agent_client_cert_pem = Some("-----BEGIN CERTIFICATE-----\nCERT\n-----END CERTIFICATE-----".to_string());
        prof.agent_ca_pem = Some("-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----".to_string());

        let state = ConsoleState::new(prof, WorkspaceBundle::builtin_pits_baseline());
        let app = build_console_router(state);

        // 1. Backstage Guest Auth Refresh (`GET` & `POST`)
        for method in ["GET", "POST"] {
            let (st, auth) = call_json(
                &app,
                Request::builder()
                    .method(method)
                    .uri("/api/auth/guest/refresh")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(st, StatusCode::OK);
            let token = auth["backstageIdentity"]["token"].as_str().unwrap();
            assert_eq!(token, auth["token"].as_str().unwrap());
            let parts: Vec<&str> = token.split('.').collect();
            assert_eq!(parts.len(), 3);
            assert_eq!(parts[0], "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0");
            assert_eq!(parts[2], "pits");
            let claims_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(parts[1])
                .unwrap();
            let claims: serde_json::Value = serde_json::from_slice(&claims_bytes).unwrap();
            assert_eq!(claims["sub"], "user:default/pitsadmin");
            assert_eq!(claims["iss"], "pitsctl-backstage-auth");
            assert_eq!(
                auth["backstageIdentity"]["identity"]["userEntityRef"],
                "user:default/pitsadmin"
            );
        }

        // 1b. Behind Zero-Trust Gateway (`X-Auth-Request-User`/`Email`), omit synthetic guest JWT
        let (st, gw_auth) = call_json(
            &app,
            Request::builder()
                .method("POST")
                .uri("/api/auth/guest/refresh")
                .header("x-auth-request-user", "admin")
                .header("x-auth-request-email", "admin@martensite.dev")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(gw_auth.get("token").is_none());
        assert!(gw_auth["backstageIdentity"].get("token").is_none());
        assert_eq!(gw_auth["profile"]["displayName"], "admin");
        assert_eq!(gw_auth["profile"]["email"], "admin@martensite.dev");
        assert_eq!(
            gw_auth["backstageIdentity"]["identity"]["userEntityRef"],
            "user:default/admin"
        );

        // 2. Backstage Catalog `/api/catalog/entity-facets` (repeated `facet` + `filter`)
        let (st, facets) = call_json(
            &app,
            Request::builder()
                .uri("/api/catalog/entity-facets?facet=spec.type&facet=kind&filter=kind=resource")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let spec_types = facets["facets"]["spec.type"].as_array().unwrap();
        assert_eq!(spec_types.len(), 13);
        let total_resources: usize = spec_types
            .iter()
            .map(|entry| entry["count"].as_u64().unwrap() as usize)
            .sum();
        assert_eq!(total_resources, 33);

        // 3. Backstage Catalog `/api/catalog/entities/by-query` (repeated `filter` + pagination)
        let (st, by_query) = call_json(
            &app,
            Request::builder()
                .uri("/api/catalog/entities/by-query?filter=kind=resource,spec.type=npu-accelerator&filter=kind=resource,spec.type=storage-pool&limit=3&offset=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(by_query["totalItems"].as_u64().unwrap(), 5);
        assert_eq!(by_query["items"].as_array().unwrap().len(), 3);

        // 4. Backstage Catalog `/api/catalog/entities` (returns array directly)
        let (st, entities_arr) = call_json(
            &app,
            Request::builder()
                .uri("/api/catalog/entities?filter=kind=resource,spec.type=san-volume")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(entities_arr.as_array().unwrap().len(), 9);

        // 5. Backstage Catalog `/api/catalog/entities/by-name/:kind/:namespace/:name` + 404 + UID + by-refs
        let (st, npu3) = call_json(
            &app,
            Request::builder()
                .uri("/api/catalog/entities/by-name/resource/default/netboot-003-npu")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(npu3["metadata"]["name"], "netboot-003-npu");
        assert_eq!(npu3["spec"]["type"], "npu-accelerator");
        assert!(!npu3["relations"].as_array().unwrap().is_empty());

        let npu3_uid = npu3["metadata"]["uid"].as_str().unwrap();
        let (st, by_uid) = call_json(
            &app,
            Request::builder()
                .uri(&format!("/api/catalog/entities/by-uid/{npu3_uid}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(by_uid["metadata"]["name"], "netboot-003-npu");

        let (st_missing, _) = call_json(
            &app,
            Request::builder()
                .uri("/api/catalog/entities/by-name/resource/default/missing-entity")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st_missing, StatusCode::NOT_FOUND);

        let (st, by_refs) = call_json(
            &app,
            Request::builder()
                .method("POST")
                .uri("/api/catalog/entities/by-refs")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "entityRefs": [
                            "resource:default/netboot-004",
                            "system:default/pits-infrastructure",
                            "resource:default/does-not-exist"
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let ref_items = by_refs["items"].as_array().unwrap();
        assert_eq!(ref_items.len(), 3);
        assert_eq!(ref_items[0]["metadata"]["name"], "netboot-004");
        assert_eq!(ref_items[1]["metadata"]["name"], "pits-infrastructure");
        assert!(ref_items[2].is_null());

        // 6. Backstage Locations
        let (st, locs) = call_json(
            &app,
            Request::builder()
                .uri("/api/catalog/locations")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(locs[0]["data"]["id"], "pits-live-catalog");

        // 7. PITS Resource Portal `/api/pits/v1/resource-types` & `/api/pits/v1/entities/default/netboot-004`
        let (st, rtypes) = call_json(
            &app,
            Request::builder()
                .uri("/api/pits/v1/resource-types")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(rtypes.as_array().unwrap().len(), 16);

        let (st, cicd_stack) = call_json(
            &app,
            Request::builder()
                .uri("/api/pits/v1/entities/default/pits-lab-acceleration")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(cicd_stack["resourceType"], "cicd-pipeline");

        let (st, host4) = call_json(
            &app,
            Request::builder()
                .uri("/api/pits/v1/entities/default/netboot-004")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(host4["resourceType"], "baremetal-host");
        assert_eq!(host4["entity"]["metadata"]["name"], "netboot-004");
        assert!(!host4["attributeSections"].as_array().unwrap().is_empty());
        assert!(!host4["actions"].as_array().unwrap().is_empty());

        // 8. Verify Profile endpoint never leaks private key PEM
        let (st, prof_resp) = call_json(
            &app,
            Request::builder()
                .uri("/api/pits/v1/profile")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(prof_resp["hasClientPki"], true);
        assert!(prof_resp.get("agentClientKeyPem").is_none());
        assert!(!prof_resp.to_string().contains("BEGIN PRIVATE KEY"));

        // 9. Tech Insights & Sourcing Governance APIs
        let (st, schemas) = call_json(
            &app,
            Request::builder()
                .uri("/api/tech-insights/facts/schemas")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(schemas[0]["id"], "sourcing-kraljic-windmill-retriever");

        let (st, fraud_facts) = call_json(
            &app,
            Request::builder()
                .uri("/api/tech-insights/facts/latest/default/resource/payment-fraud-engine-sdk")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(fraud_facts["facts"]["quadrant"], "strategic");
        assert_eq!(fraud_facts["facts"]["windmillMismatch"], true);

        let (st, fraud_checks) = call_json(
            &app,
            Request::builder()
                .uri("/api/tech-insights/checks/result/default/resource/payment-fraud-engine-sdk")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(fraud_checks["passedAll"], false);
        assert_eq!(fraud_checks["redlineCount"], 2);

        let (st, overview) = call_json(
            &app,
            Request::builder()
                .uri("/api/pits/v1/sourcing/overview")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(overview["totalUnits"], 5);

        let (st, val_bad) = call_json(
            &app,
            Request::builder()
                .method("POST")
                .uri("/api/pits/v1/sourcing/validate")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "yaml": "apiVersion: backstage.io/v1alpha1\nkind: Resource\nmetadata:\n  name: aliyun-all\n  annotations:\n    finops.company.io/cost-allocation-id: cc-1\n    sourcing.company.io/adapter-abstracted: \"false\"\n    contractPrice: \"9999\"\nspec:\n  type: vendor\n  owner: group:default/operators\n  system: pits-infrastructure\n  sourcingAudit:\n    contractMeta: { vendorName: Aliyun, renewalDate: '2027-06-01', lastAuditedAt: '2026-11-15', baselineProdDependents: 1 }\n    manualScores: { y2_coreCompetitiveness: 3, y4_priceElasticity: 3, x2_marketScarcity: 3, x3_customizationDepth: 3, x4_complianceFragility: 3 }\n    supplierPreference: { estimatedWalletShareTier: under-1pct, accountAttractivenessScore: 2, perceivedStatus: nuisance }\n"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(val_bad["valid"], false);
    }

    #[tokio::test]
    async fn test_static_asset_gzip_etag_and_cache_control() {
        let state = ConsoleState::new(ClientProfile::default(), WorkspaceBundle::builtin_pits_baseline());
        let app = build_console_router(state);

        // 1. Entry HTML serves no-cache + ETag and gzip when precompressed `.gz` exists
        let index_entry = get_portal_asset_entry("index.html").expect("index.html embedded");
        let (st_gz, hdr_gz, body_gz) = call_raw(
            &app,
            Request::builder()
                .uri("/")
                .header(header::ACCEPT_ENCODING, "gzip, deflate")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st_gz, StatusCode::OK);
        assert_eq!(
            hdr_gz.get(header::CACHE_CONTROL).unwrap(),
            "no-cache, must-revalidate"
        );
        let etag = hdr_gz.get(header::ETAG).unwrap().to_str().unwrap().to_string();
        assert_eq!(etag, index_entry.etag);
        if let Some(expected_gz) = index_entry.gzip {
            assert_eq!(hdr_gz.get(header::CONTENT_ENCODING).unwrap(), "gzip");
            assert_eq!(hdr_gz.get(header::VARY).unwrap(), "Accept-Encoding");
            assert_eq!(body_gz.as_ref(), expected_gz);
        }

        // 2. `gzip;q=0` must receive uncompressed raw payload
        let (st_raw, hdr_raw, raw_body) = call_raw(
            &app,
            Request::builder()
                .uri("/")
                .header(header::ACCEPT_ENCODING, "gzip;q=0, *")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st_raw, StatusCode::OK);
        assert!(hdr_raw.get(header::CONTENT_ENCODING).is_none());
        assert_eq!(raw_body.as_ref(), index_entry.raw);

        // 3. Matching `If-None-Match` yields 304 Not Modified with empty body
        let (st_304, _hdr_304, body_304) = call_raw(
            &app,
            Request::builder()
                .uri("/")
                .header(header::IF_NONE_MATCH, &etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st_304, StatusCode::NOT_MODIFIED);
        assert!(body_304.is_empty());

        // 4. `immutable` asset path contract (when dist bundle is built, `/assets/*` serves immutable)
        assert!(!index_entry.immutable);
        if portal_assets::PORTAL_FROM_DIST_BUNDLE {
            let html = std::str::from_utf8(index_entry.raw).unwrap();
            assert!(
                !html.contains("hljs-languages"),
                "index.html should not preload hljs-languages"
            );
            let marker = "src=\"/assets/";
            let start = html.find(marker).expect("entry script tag in index.html") + marker.len();
            let end = html[start..].find('"').unwrap() + start;
            let asset_uri = format!("/assets/{}", &html[start..end]);

            let (st_asset, hdr_asset, _body_asset) = call_raw(
                &app,
                Request::builder()
                    .uri(&asset_uri)
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(st_asset, StatusCode::OK);
            assert_eq!(
                hdr_asset.get(header::CACHE_CONTROL).unwrap(),
                "public, max-age=31536000, immutable"
            );
            assert_eq!(hdr_asset.get(header::CONTENT_ENCODING).unwrap(), "gzip");

            let (st_head, hdr_head, body_head) = call_raw(
                &app,
                Request::builder()
                    .method(Method::HEAD)
                    .uri(&asset_uri)
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(st_head, StatusCode::OK);
            assert_eq!(hdr_head.get(header::CONTENT_ENCODING).unwrap(), "gzip");
            assert!(body_head.is_empty());
        }
    }

    #[tokio::test]
    async fn mcp_stdio_and_streamable_http_discover_and_promote_end_to_end() {
        let prof = ClientProfile::default();
        let app = build_console_router(ConsoleState::new(
            prof,
            WorkspaceBundle::builtin_pits_baseline(),
        ));

        // 1. Initialize & tools/list via Streamable HTTP POST /mcp
        let (st_init, init_resp) = call_json(
            &app,
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "initialize",
                        "params": { "protocolVersion": "2025-03-26" }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st_init, StatusCode::OK);
        assert_eq!(init_resp["result"]["serverInfo"]["name"], "pits-sourcing-mcp");

        // Notification must yield 202 Accepted
        let (st_notif, _, _) = call_raw(
            &app,
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/initialized"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st_notif, StatusCode::ACCEPTED);

        // 2. Scan a temp workspace containing external dependencies
        let tmp = std::env::temp_dir().join(format!("pits-mcp-scan-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(tmp.join("client")).unwrap();
        std::fs::create_dir_all(tmp.join("portal")).unwrap();
        std::fs::write(
            tmp.join("client").join("Cargo.toml"),
            "[package]\nname = \"test-client\"\nversion = \"0.1.0\"\n[dependencies]\nstripe-rust = \"0.28\"\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("portal").join("package.json"),
            "{\"name\":\"portal\",\"dependencies\":{\"@datadog/browser-rum\":\"5.0.0\"}}",
        )
        .unwrap();

        let (st_scan, scan_resp) = call_json(
            &app,
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 2,
                        "method": "tools/call",
                        "params": {
                            "name": "sourcing_scan_workspace",
                            "arguments": { "rootPath": tmp.display().to_string() }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st_scan, StatusCode::OK);
        let candidates = scan_resp["result"]["structuredContent"]["candidates"]
            .as_array()
            .expect("candidates array");
        assert!(
            !candidates.is_empty(),
            "expected discovered candidates from temp manifests, got: {scan_resp}"
        );
        let first_cid = candidates[0]["candidateId"]
            .as_str()
            .expect("candidateId")
            .to_string();

        // 3. Monolith type must be rejected by CatalogProcessor inside sourcing_promote_candidate
        let (st_mono, mono_resp) = call_json(
            &app,
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 3,
                        "method": "tools/call",
                        "params": {
                            "name": "sourcing_promote_candidate",
                            "arguments": {
                                "candidateId": first_cid,
                                "resourceType": "vendor",
                                "costAllocationId": "cc-sec-999"
                            }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st_mono, StatusCode::OK);
        assert_eq!(mono_resp["result"]["isError"], true);

        // 4. Plaintext pricing leak must also be rejected (FINOPS_SECRET_LEAK)
        let (st_leak, leak_resp) = call_json(
            &app,
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 4,
                        "method": "tools/call",
                        "params": {
                            "name": "sourcing_promote_candidate",
                            "arguments": {
                                "candidateId": first_cid,
                                "resourceType": "commercial-sdk",
                                "description": "contractPrice: $50000 annual",
                                "costAllocationId": "cc-sec-999"
                            }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st_leak, StatusCode::OK);
        assert_eq!(leak_resp["result"]["isError"], true);
        let leak_msg = leak_resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(
            leak_msg.contains("FINOPS_SECRET_LEAK"),
            "expected FINOPS_SECRET_LEAK, got: {leak_msg}"
        );

        // 5. Promote cleanly & verify blind ballot works via MCP
        let (st_prom, prom_resp) = call_json(
            &app,
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 5,
                        "method": "tools/call",
                        "params": {
                            "name": "sourcing_promote_candidate",
                            "arguments": {
                                "candidateId": first_cid,
                                "name": "discovered-stripe-billing",
                                "resourceType": "vendor-saas",
                                "costAllocationId": "cc-fin-101"
                            }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st_prom, StatusCode::OK);
        assert_eq!(prom_resp["result"]["isError"], false, "promote error: {prom_resp}");
        assert_eq!(
            prom_resp["result"]["structuredContent"]["evaluation"]["sourceOrigin"],
            "mcp-discovery"
        );

        // 5b. Blind ballots & resource read over Streamable HTTP /mcp
        let (st_b1, b1_resp) = call_json(
            &app,
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 51,
                        "method": "tools/call",
                        "params": {
                            "name": "sourcing_submit_blind_ballot",
                            "arguments": {
                                "unitName": "discovered-stripe-billing",
                                "reviewerRole": "architecture",
                                "reviewerId": "arch-mcp@pits.local",
                                "y2_coreCompetitiveness": 4.7,
                                "y4_priceElasticity": 2.0,
                                "x2_marketScarcity": 4.5,
                                "x3_customizationDepth": 3.5,
                                "x4_complianceFragility": 4.0,
                                "evidenceNotes": "Direct billing SDK hook"
                            }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st_b1, StatusCode::OK);
        assert_eq!(b1_resp["result"]["isError"], false);

        let (st_b2, b2_resp) = call_json(
            &app,
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 52,
                        "method": "tools/call",
                        "params": {
                            "name": "sourcing_submit_blind_ballot",
                            "arguments": {
                                "unitName": "discovered-stripe-billing",
                                "reviewerRole": "procurement",
                                "reviewerId": "proc-mcp@pits.local",
                                "y2_coreCompetitiveness": 2.5,
                                "y4_priceElasticity": 2.0,
                                "x2_marketScarcity": 3.5,
                                "x3_customizationDepth": 3.5,
                                "x4_complianceFragility": 4.0,
                                "evidenceNotes": "Alt gateways available"
                            }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st_b2, StatusCode::OK);
        assert_eq!(b2_resp["result"]["structuredContent"]["varianceAlert"], true);

        let (st_res, res_resp) = call_json(
            &app,
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 53,
                        "method": "resources/read",
                        "params": { "uri": "sourcing://unit/discovered-stripe-billing" }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st_res, StatusCode::OK);
        assert!(
            res_resp["result"]["contents"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .contains("discovered-stripe-billing")
        );

        // 6. Emergency fallback Wizard POST without overrideReason must be rejected
        let rec_val = prom_resp["result"]["structuredContent"]["evaluation"]["record"].clone();
        let (st_wiz_fail, wiz_fail_resp) = call_json(
            &app,
            Request::builder()
                .method(Method::POST)
                .uri("/api/pits/v1/sourcing/entities")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "record": rec_val,
                        "writeWorkspaceYaml": false,
                        "dryRun": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st_wiz_fail, StatusCode::BAD_REQUEST);
        let err_msg = wiz_fail_resp["message"]
            .as_str()
            .or_else(|| wiz_fail_resp["error"]["message"].as_str())
            .or_else(|| wiz_fail_resp["error"].as_str())
            .unwrap_or_default();
        assert!(
            err_msg.contains("overrideReason"),
            "expected overrideReason error, got: {wiz_fail_resp}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn project_hub_http_mcp_forbids_done_and_portal_human_gate_accepts() {
        let profile = ClientProfile::default();
        let bundle = WorkspaceBundle::builtin_pits_baseline();
        let state = ConsoleState::new_with_in_memory_hub(profile, bundle);
        let app = build_console_router(state);

        // 1. Overview returns seeded PITS + AGY-1 + AGY-2
        let (st_ov, ov) = call_json(
            &app,
            Request::builder()
                .method(Method::GET)
                .uri("/api/pits/v1/hub/overview")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st_ov, StatusCode::OK);
        assert_eq!(ov["projects"][0]["id"], "pits");
        assert!(ov["pendingReviewCount"].as_u64().unwrap_or(0) >= 2);

        // 2. MCP /mcp/hub attempting to mark AGY-2 done must fail with HTTP 403
        let (st_forbid, forbid_body) = call_json(
            &app,
            Request::builder()
                .method(Method::POST)
                .uri("/mcp/hub")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 91,
                        "method": "tools/call",
                        "params": {
                            "name": "hub_submit_handoff",
                            "arguments": {
                                "taskKeys": ["AGY-2"],
                                "summary": "Illegal self-accept",
                                "targetStatus": "done",
                                "accepted": true
                            }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st_forbid, StatusCode::FORBIDDEN);
        assert!(forbid_body["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("FORBIDDEN: Human acceptance required"));

        // 3. Human channel accepts AGY-2 handoff via /api/pits/v1/hub/sessions/:id/accept
        let (st_acc, acc_body) = call_json(
            &app,
            Request::builder()
                .method(Method::POST)
                .uri("/api/pits/v1/hub/sessions/seed-session-agy-2/accept")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-auth-request-user", "pitsadmin")
                .body(Body::from(
                    serde_json::json!({
                        "markTasksDone": true,
                        "reviewNote": "LGTM on portal"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st_acc, StatusCode::OK);
        assert_eq!(acc_body["session"]["accepted"], true);
        assert_eq!(acc_body["authSource"], "keycloak-fido2");
        assert_eq!(acc_body["updatedTasks"][0]["status"], "done");
    }
}
