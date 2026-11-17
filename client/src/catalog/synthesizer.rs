//! Live + Offline PITS Software Catalog Synthesizer, Stale-While-Revalidate Snapshot Cache (60s),
//! Singleflight Live Refresh, Symmetric Backstage Relation Graph Builder, and Query/Facet Engine.

use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, Semaphore};
use uuid::Uuid;

use crate::catalog::actions::{
    actions_for_edge_router, actions_for_k8s_cluster, actions_for_npu, actions_for_pits_core_host,
    actions_for_pxe_profile, actions_for_san_volume, actions_for_sourcing_resource,
    actions_for_storage_pool, actions_for_talos_host, actions_for_vm,
    canonical_action_ids_for_type,
};
use crate::catalog::discovery::{
    build_discovered_k8s_entity_detail, collect_live_discovered_nodes,
    discover_k8s_crossplane_fast, enrich_existing_detail_from_k8s,
    extract_sourcing_candidates_from_k8s, resolve_baseline_alias, should_ingest_k8s_object,
    DiscoveredK8sResource,
};
use crate::catalog::model::{
    AttributeItem, AttributeSection, CatalogEntitiesQuery, CatalogEntity, CatalogFacetsResponse,
    CatalogQueryResponse, CatalogSnapshot, EntityMetadata, EntityRelation, FacetEntry,
    PitsEntityDetail, ResourceTypeSummary,
};
use crate::catalog::sourcing::{
    canonical_component_ref, canonical_system_ref, compute_unified_diff, default_system_domain_map,
    evaluate_sourcing_audit_at, is_sourcing_resource_type, render_catalog_info_yaml,
    today_utc_date, validate_sourcing_record, ContractMeta, DiscoveryCandidateRecord,
    DomainLineageEntry, ManualScores, SourcingAuditSpec, SourcingDbSyncStatus, SourcingEvaluation,
    SourcingOverviewResponse, SourcingResourceRecord, SourcingStore, SourcingWizardUpsertRequest,
    SourcingWizardUpsertResponse, SupplierPreference,
};
use crate::catalog::sourcing_db::{
    normalize_renewal_date, scan_workspace_dependencies, validate_suggested_sourcing_type,
    AggregatedBallotStats, AssessWindmillRequest, CandidateDispositionRequest, SourcingDbHandle,
    SubmitBlindBallotRequest,
};
use crate::config::{
    effective_sourcing_db_url, load_kubeconfig_file, load_talosconfig_file, local_state_dir,
    ClientProfile, EnrolledNodeContract, WorkspaceBundle,
};
use crate::k8s::{
    render_npu_validation_manifests, render_san_local_pv_pvc, ClusterControlPlaneSummary,
    KubeNodeSummary, NpuDraDeviceSummary, NpuSliceSummary, PITS_NPU_DRA_DRIVER,
};
use crate::pxe::{
    analyze_hook_inventory, classify_pci_npu, expected_npu_for_cpu, list_tinkerbell_hardware,
    list_tinkerbell_workflows, pxe_list_inventory, render_ipxe_script,
    render_tinkerbell_hardware_yaml, resolve_retail_machine_model, DetectedNpuHardware,
    PxeBootMode, ASSET_HTTP_BASE_DEFAULT,
};
use crate::router::{
    inspect_router_live, pits_core_default_edge_router, validate_edge_router, EdgeRouterManifest,
    RouterLiveSummary, LAN_TRUNK_VF_PCI, MGMT0_PCI, WAN_ROUTER_PCI,
};
use crate::san::{
    btrfs_status, expected_wwid, render_nvmet_k8s_manifests, san_inspect, EXPECTED_BTRFS_UUID,
};
use crate::talos::{
    allowed_wwids_for_node, render_talos_imager_profile, render_talos_node_config, TalosClient,
    TalosNodeLiveStatus, LAB_K8S_VERSION, TALOS_VERSION,
};
use crate::transport::{AgentClient, KubeClient};
use crate::vm::{
    render_access_lab_vm_manifest, render_windows11_vm_manifest, RdpTlsVerification,
    VirtualMachineSummary, VmInterfaceInfo, ACCESS_LAB_FIXED_MAC, WIN11_DEFAULT_HOST_IP,
    WIN11_DEFAULT_RDP_NODEPORT,
};

const CACHE_TTL: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const NODES_PROBE_TIMEOUT: Duration = Duration::from_secs(12);
const RETRY_TIMEOUT_CUTOFF: Duration = Duration::from_secs(6);
const SINGLEFLIGHT_REUSE_WINDOW: Duration = Duration::from_secs(4);

pub(crate) struct ResourceTypeMeta {
    pub(crate) resource_type: &'static str,
    pub(crate) category: &'static str,
    pub(crate) title: &'static str,
    pub(crate) description: &'static str,
}

pub(crate) const RESOURCE_TYPE_METAS: &[ResourceTypeMeta] = &[
    ResourceTypeMeta {
        resource_type: "baremetal-host",
        category: "Compute & Accelerators",
        title: "Bare-Metal Hosts",
        description: "Physical x86_64 nodes across Core NixOS RKE2 (pits-core) and Diskless Talos VLAN 20 Fleet (netboot-001..004)",
    },
    ResourceTypeMeta {
        resource_type: "npu-accelerator",
        category: "Compute & Accelerators",
        title: "AI NPU Accelerators",
        description: "Physical Neural Processing Units (Intel AI Boost VPU & AMD Ryzen AI XDNA) orchestrated via Kubernetes v1.37 DRA v1",
    },
    ResourceTypeMeta {
        resource_type: "virtual-machine",
        category: "Compute & Accelerators",
        title: "KubeVirt Virtual Machines",
        description: "Cross-cluster KubeVirt virtual machines (Windows 11 workstation on Lab SAN vmstore & Access Lab appliance on Core VLAN 10)",
    },
    ResourceTypeMeta {
        resource_type: "edge-router",
        category: "Network & Boot",
        title: "Edge Routers & VPP Gateways",
        description: "Crossplane EdgeRouter & DPDK/GoVPP dataplane enforcing VLAN 10/20 mutual isolation and mgmt0 PCI safety",
    },
    ResourceTypeMeta {
        resource_type: "pxe-boot-profile",
        category: "Network & Boot",
        title: "PXE Boot Profiles",
        description: "Tinkerbell iPXE boot templates, kernel command-line policies, and staged HTTP boot assets",
    },
    ResourceTypeMeta {
        resource_type: "storage-pool",
        category: "Storage Fabric",
        title: "Btrfs Storage Pools",
        description: "Enterprise NVMe Btrfs storage pools backing PITS SAN extents, snapshots, and archives",
    },
    ResourceTypeMeta {
        resource_type: "san-volume",
        category: "Storage Fabric",
        title: "NVMe/TCP SAN Volumes",
        description: "Deterministic UUIDv5 NVMe/TCP namespaces exported from pits-core over Lab VLAN 20",
    },
    ResourceTypeMeta {
        resource_type: "kubernetes-cluster",
        category: "Control Planes",
        title: "Kubernetes Clusters",
        description: "Dual Kubernetes control planes: Core RKE2 v1.35.8 infrastructure cluster and Lab Talos K8s v1.37.1 workload/DRA cluster",
    },
    ResourceTypeMeta {
        resource_type: "vendor-saas",
        category: "Sourcing Governance",
        title: "Vendor SaaS Control Planes",
        description: "External SaaS control planes, coordination relays, and subscription services scored under Kraljic × Dutch Windmill",
    },
    ResourceTypeMeta {
        resource_type: "commercial-sdk",
        category: "Sourcing Governance",
        title: "Commercial Closed-Source SDKs",
        description: "Licensed proprietary SDKs, risk models, and solvers governed for blast-radius lock-in, adapters, and renewal SLA",
    },
    ResourceTypeMeta {
        resource_type: "managed-infra",
        category: "Sourcing Governance",
        title: "Managed External Infrastructure",
        description: "External carrier BGP transit circuits and managed infra dependencies with production component attribution",
    },
    ResourceTypeMeta {
        resource_type: "outsourced-dev",
        category: "Sourcing Governance",
        title: "Outsourced Dev & Art Units",
        description: "Granular outsourced engineering and asset production units audited for dependency exposure and supplier attractiveness",
    },
];

#[derive(Default)]
pub(crate) struct LiveTelemetry {
    pub(crate) any_connected: bool,
    pub(crate) connection_note: Option<String>,
    pub(crate) agent_health: Option<Value>,
    pub(crate) router_live: Option<RouterLiveSummary>,
    pub(crate) btrfs: Option<Value>,
    pub(crate) san_by_device: BTreeMap<String, Value>,
    pub(crate) tink_hardware: Option<Value>,
    pub(crate) tink_workflows: Option<Value>,
    pub(crate) hook_inventory: BTreeMap<String, Value>,
    pub(crate) pxe_asset_verify: BTreeMap<String, Value>,
    pub(crate) core_cluster: Option<ClusterControlPlaneSummary>,
    pub(crate) lab_cluster: Option<ClusterControlPlaneSummary>,
    pub(crate) lab_device_classes: Vec<String>,
    pub(crate) talos_by_device: BTreeMap<String, TalosNodeLiveStatus>,
    pub(crate) core_vms: Option<Vec<VirtualMachineSummary>>,
    pub(crate) lab_vms: Option<Vec<VirtualMachineSummary>>,
    pub(crate) discovered_k8s_resources: Vec<DiscoveredK8sResource>,
    pub(crate) discovered_sourcing_candidates: Vec<DiscoveryCandidateRecord>,
}

/// Unified PITS Software Catalog & Contextual Action Engine with 60-second Stale-While-Revalidate
/// snapshot caching, `<2ms` zero-I/O baseline seeding, and process-wide singleflight live refresh.
#[derive(Clone)]
pub struct CatalogRegistry {
    pub(crate) profile: Arc<RwLock<ClientProfile>>,
    pub(crate) bundle: Arc<RwLock<WorkspaceBundle>>,
    pub(crate) sourcing: Arc<RwLock<SourcingStore>>,
    pub(crate) sourcing_db: Arc<SourcingDbHandle>,
    pub(crate) cache: Arc<RwLock<Option<(Instant, Arc<CatalogSnapshot>)>>>,
    pub(crate) last_rdp_tls: Arc<RwLock<Option<RdpTlsVerification>>>,
    pub(crate) last_pxe_verify: Arc<RwLock<BTreeMap<String, Value>>>,
    pub(crate) last_talos_by_device: Arc<RwLock<BTreeMap<String, TalosNodeLiveStatus>>>,
    pub(crate) refresh_lock: Arc<Mutex<()>>,
    pub(crate) seed_lock: Arc<Mutex<()>>,
    pub(crate) has_live_snapshot: Arc<AtomicBool>,
    pub(crate) refresh_in_flight: Arc<AtomicBool>,
    pub(crate) last_live_completed: Arc<RwLock<Option<Instant>>>,
}

fn detect_workspace_sourcing_dir(source_dir: Option<&str>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(src) = source_dir.filter(|s| !s.trim().is_empty()) {
        let p = Path::new(src);
        candidates.push(p.join("catalog").join("sourcing"));
        if let Some(parent) = p.parent() {
            candidates.push(parent.join("catalog").join("sourcing"));
            if let Some(gp) = parent.parent() {
                candidates.push(gp.join("catalog").join("sourcing"));
            }
        }
    }
    candidates.push(PathBuf::from("catalog").join("sourcing"));
    candidates.push(PathBuf::from("..").join("catalog").join("sourcing"));
    candidates.into_iter().find(|c| c.is_dir())
}

impl CatalogRegistry {
    pub fn new(
        profile: Arc<RwLock<ClientProfile>>,
        bundle: Arc<RwLock<WorkspaceBundle>>,
    ) -> Self {
        let pg_url = profile
            .try_read()
            .ok()
            .and_then(|p| effective_sourcing_db_url(None, &p));
        let store = if cfg!(test) {
            let p = std::env::temp_dir().join(format!(
                "pits-test-sourcing-{}.yaml",
                Uuid::new_v4()
            ));
            SourcingStore::load_with_options(p, None, pg_url, true)
        } else {
            let ws_src = bundle.try_read().ok().and_then(|b| b.source_dir.clone());
            let ws_dir = detect_workspace_sourcing_dir(ws_src.as_deref());
            let state_path = local_state_dir().join("sourcing-audit.sqlite3");
            SourcingStore::load_with_options(state_path, ws_dir, pg_url, false)
        };
        Self::new_with_sourcing_store(profile, bundle, store)
    }

    pub fn new_with_sourcing_store(
        profile: Arc<RwLock<ClientProfile>>,
        bundle: Arc<RwLock<WorkspaceBundle>>,
        sourcing_store: SourcingStore,
    ) -> Self {
        let sourcing_db = sourcing_store.db.clone();
        Self {
            profile,
            bundle,
            sourcing: Arc::new(RwLock::new(sourcing_store)),
            sourcing_db,
            cache: Arc::new(RwLock::new(None)),
            last_rdp_tls: Arc::new(RwLock::new(None)),
            last_pxe_verify: Arc::new(RwLock::new(BTreeMap::new())),
            last_talos_by_device: Arc::new(RwLock::new(BTreeMap::new())),
            refresh_lock: Arc::new(Mutex::new(())),
            seed_lock: Arc::new(Mutex::new(())),
            has_live_snapshot: Arc::new(AtomicBool::new(false)),
            refresh_in_flight: Arc::new(AtomicBool::new(false)),
            last_live_completed: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn invalidate_cache(&self) {
        {
            let mut guard = self.cache.write().await;
            *guard = None;
        }
        {
            let mut talos_guard = self.last_talos_by_device.write().await;
            talos_guard.clear();
        }
        self.has_live_snapshot.store(false, Ordering::Release);
        {
            let mut last = self.last_live_completed.write().await;
            *last = None;
        }
    }

    pub(crate) async fn is_profile_configured(&self) -> bool {
        let prof = self.profile.read().await;
        let has_agent_creds = prof.agent_client_cert_pem.is_some()
            || prof.agent_bearer_token.is_some()
            || prof.agent_ca_pem.is_some()
            || prof.agent_endpoint != "https://100.74.25.42:18443";
        has_agent_creds
            || prof.core_kubeconfig_path.is_some()
            || prof.lab_kubeconfig_path.is_some()
            || prof.talosconfig_path.is_some()
    }

    pub(crate) async fn build_agent_req(&self) -> Result<AgentClient> {
        let prof = self.profile.read().await;
        AgentClient::from_profile(&prof)
    }

    pub(crate) async fn build_core_k8s_req(&self) -> Result<KubeClient> {
        let prof = self.profile.read().await;
        let path = prof
            .core_kubeconfig_path
            .as_deref()
            .ok_or_else(|| anyhow!("coreKubeconfigPath is not configured in operator profile"))?;
        let ep = load_kubeconfig_file(Path::new(path), prof.core_context.as_deref())?;
        KubeClient::from_endpoint("pits-core", ep, None)
    }

    pub(crate) async fn build_lab_k8s_req(&self) -> Result<KubeClient> {
        let prof = self.profile.read().await;
        let path = prof
            .lab_kubeconfig_path
            .as_deref()
            .ok_or_else(|| anyhow!("labKubeconfigPath is not configured in operator profile"))?;
        let ep = load_kubeconfig_file(Path::new(path), prof.lab_context.as_deref())?;
        if prof.lab_via_agent_bridge {
            let ag = AgentClient::from_profile(&prof)?;
            KubeClient::from_endpoint("pits-lab", ep, Some((&prof, ag.tls_config)))
        } else {
            KubeClient::from_endpoint("pits-lab", ep, None)
        }
    }

    pub(crate) async fn build_talos_req(&self) -> Result<TalosClient> {
        let prof = self.profile.read().await;
        let path = prof
            .talosconfig_path
            .as_deref()
            .ok_or_else(|| anyhow!("talosconfigPath is not configured in operator profile"))?;
        let ctx = load_talosconfig_file(Path::new(path), prof.talos_context.as_deref())?;
        if prof.talos_via_agent_bridge {
            let ag = AgentClient::from_profile(&prof)?;
            TalosClient::from_context(ctx, Some((&prof, ag.tls_config)))
        } else {
            TalosClient::from_context(ctx, None)
        }
    }

    /// Builds the full 8-Resource-Type / 26-Resource + 1-System catalog purely in memory
    /// (`<2ms`, zero network I/O).
    pub(crate) async fn synthesize_baseline_snapshot(&self) -> CatalogSnapshot {
        let now = Utc::now();
        let bundle_guard = self.bundle.read().await;
        let sourcing_guard = self.sourcing.read().await;
        let cached_rdp = self.last_rdp_tls.read().await.clone();
        let cached_pxe = self.last_pxe_verify.read().await.clone();
        let live = LiveTelemetry {
            any_connected: false,
            connection_note: Some(
                "Baseline catalog loaded instantly; syncing live telemetry in background..."
                    .to_string(),
            ),
            pxe_asset_verify: cached_pxe,
            ..LiveTelemetry::default()
        };
        synthesize_snapshot_from_inputs(
            &bundle_guard,
            cached_rdp.as_ref(),
            &live,
            &sourcing_guard,
            now,
        )
    }

    /// Populates `self.cache` in `<2ms` with zero network I/O when `cache` is currently `None`,
    /// never overwriting an already-populated snapshot.
    pub(crate) async fn seed_baseline_if_empty(&self) -> Arc<CatalogSnapshot> {
        if let Some((_, snap)) = self.cache.read().await.as_ref() {
            return Arc::clone(snap);
        }
        let _seed_guard = self.seed_lock.lock().await;
        if let Some((_, existing)) = self.cache.read().await.as_ref() {
            return Arc::clone(existing);
        }
        let baseline = Arc::new(self.synthesize_baseline_snapshot().await);
        let mut guard = self.cache.write().await;
        if let Some((_, existing)) = guard.as_ref() {
            return Arc::clone(existing);
        }
        *guard = Some((Instant::now(), Arc::clone(&baseline)));
        baseline
    }

    /// Process-wide singleflight live telemetry refresh. Stacked callers arriving within `4s` of a
    /// completed live pass immediately share the fresh snapshot instead of launching redundant sweeps.
    pub(crate) async fn refresh_live_singleflight(&self) -> Arc<CatalogSnapshot> {
        let _singleflight = self.refresh_lock.lock().await;

        if self.has_live_snapshot.load(Ordering::Acquire) {
            let recently_completed = self
                .last_live_completed
                .read()
                .await
                .map(|ts| ts.elapsed() < SINGLEFLIGHT_REUSE_WINDOW)
                .unwrap_or(false);
            if recently_completed {
                if let Some((_, snap)) = self.cache.read().await.as_ref() {
                    return Arc::clone(snap);
                }
            }
        }

        if !self.is_profile_configured().await {
            let snap = Arc::new(self.synthesize_baseline_snapshot().await);
            let mut guard = self.cache.write().await;
            *guard = Some((Instant::now(), Arc::clone(&snap)));
            return snap;
        }

        let snap = Arc::new(Box::pin(self.synthesize_live_snapshot()).await);
        let completed_at = Instant::now();
        {
            let mut guard = self.cache.write().await;
            *guard = Some((completed_at, Arc::clone(&snap)));
        }
        {
            let mut last = self.last_live_completed.write().await;
            *last = Some(completed_at);
        }
        self.has_live_snapshot.store(true, Ordering::Release);
        snap
    }

    /// Non-blocking background live telemetry trigger guarded by `refresh_in_flight` so at most
    /// one background Tokio refresh task runs at any moment.
    pub fn spawn_background_refresh(&self) {
        if self
            .refresh_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            self.refresh_in_flight.store(false, Ordering::Release);
            return;
        };
        let reg = self.clone();
        handle.spawn(async move {
            struct ResetInFlight(Arc<AtomicBool>);
            impl Drop for ResetInFlight {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _reset = ResetInFlight(reg.refresh_in_flight.clone());
            if reg.is_profile_configured().await {
                let _ = reg.refresh_live_singleflight().await;
            }
        });
    }

    pub(crate) async fn get_snapshot_arc(&self, force_live: bool) -> Arc<CatalogSnapshot> {
        if self.sourcing_db.check_external_db_mutation() {
            {
                let mut guard = self.sourcing.write().await;
                let _ = guard.refresh_from_db();
            }
            self.invalidate_cache().await;
        }

        if force_live {
            return self.refresh_live_singleflight().await;
        }

        let cached_opt = {
            let guard = self.cache.read().await;
            guard
                .as_ref()
                .map(|(ts, snap)| (ts.elapsed(), Arc::clone(snap)))
        };

        let (elapsed, snap) = match cached_opt {
            Some(pair) => pair,
            None => {
                let seeded = self.seed_baseline_if_empty().await;
                (Duration::ZERO, seeded)
            }
        };

        let has_live = self.has_live_snapshot.load(Ordering::Acquire);
        if (!has_live || elapsed >= CACHE_TTL)
            && !self.refresh_in_flight.load(Ordering::Acquire)
            && self.is_profile_configured().await
        {
            self.spawn_background_refresh();
        }

        snap
    }

    pub(crate) async fn get_snapshot(&self, force_live: bool) -> CatalogSnapshot {
        (*self.get_snapshot_arc(force_live).await).clone()
    }

    pub async fn list_resource_types(&self, force_live: bool) -> Vec<ResourceTypeSummary> {
        self.get_snapshot_arc(force_live)
            .await
            .resource_types
            .clone()
    }

    pub async fn list_entities(&self, force_live: bool) -> Vec<CatalogEntity> {
        self.get_snapshot_arc(force_live).await.entities.clone()
    }

    pub async fn query_catalog_entities(
        &self,
        query: &CatalogEntitiesQuery,
        force_live: bool,
    ) -> CatalogQueryResponse {
        let snap = self.get_snapshot_arc(force_live).await;
        let field_paths: Vec<String> = query
            .fields
            .as_deref()
            .map(|f| {
                f.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let search_tokens: Vec<String> = query
            .full_text_term
            .as_deref()
            .map(|t| {
                t.split_whitespace()
                    .map(|s| s.to_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let mut matched: Vec<Value> = Vec::new();
        for ent in &snap.entities {
            let Ok(val) = serde_json::to_value(ent) else {
                continue;
            };
            if !entity_matches_filters(&val, &query.filters) {
                continue;
            }
            if !search_tokens.is_empty() && !entity_matches_full_text(&val, &search_tokens) {
                continue;
            }
            matched.push(val);
        }

        let total_items = matched.len();
        let offset = query.offset.unwrap_or(0);
        let limit = query.limit.unwrap_or(usize::MAX);

        let items: Vec<Value> = matched
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|v| {
                if field_paths.is_empty() {
                    v
                } else {
                    project_entity_fields(&v, &field_paths)
                }
            })
            .collect();

        let mut page_info_map = serde_json::Map::new();
        if offset + items.len() < total_items && !items.is_empty() {
            page_info_map.insert(
                "nextCursor".to_string(),
                Value::String((offset + items.len()).to_string()),
            );
        }
        if offset > 0 {
            let prev = if limit == usize::MAX {
                0
            } else {
                offset.saturating_sub(limit)
            };
            page_info_map.insert("prevCursor".to_string(), Value::String(prev.to_string()));
        }

        CatalogQueryResponse {
            items,
            total_items,
            page_info: Value::Object(page_info_map),
        }
    }

    pub async fn compute_facets(
        &self,
        facets: &[String],
        filters: &[String],
        force_live: bool,
    ) -> CatalogFacetsResponse {
        let snap = self.get_snapshot_arc(force_live).await;
        let can_fast_project = filters.is_empty()
            && facets.iter().all(|f| {
                matches!(
                    f.trim(),
                    "kind"
                        | "metadata.namespace"
                        | "spec.type"
                        | "spec.lifecycle"
                        | "spec.owner"
                        | "spec.system"
                )
            });

        let mut out = BTreeMap::new();
        if can_fast_project {
            for facet_raw in facets {
                let facet_key = facet_raw.trim();
                if facet_key.is_empty() {
                    continue;
                }
                let mut counts: BTreeMap<String, (String, usize)> = BTreeMap::new();
                for ent in &snap.entities {
                    let val_opt = match facet_key {
                        "kind" => Some(ent.kind.as_str()),
                        "metadata.namespace" => Some(ent.metadata.namespace.as_str()),
                        "spec.type" => ent.spec.get("type").and_then(|v| v.as_str()),
                        "spec.lifecycle" => ent.spec.get("lifecycle").and_then(|v| v.as_str()),
                        "spec.owner" => ent.spec.get("owner").and_then(|v| v.as_str()),
                        "spec.system" => ent.spec.get("system").and_then(|v| v.as_str()),
                        _ => None,
                    };
                    if let Some(val_str) = val_opt.filter(|s| !s.is_empty()) {
                        let norm = val_str.to_lowercase();
                        let entry = counts.entry(norm).or_insert_with(|| (val_str.to_string(), 0));
                        entry.1 += 1;
                    }
                }
                let mut entries: Vec<FacetEntry> = counts
                    .into_values()
                    .map(|(value, count)| FacetEntry { value, count })
                    .collect();
                entries.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
                out.insert(facet_key.to_string(), entries);
            }
            return CatalogFacetsResponse { facets: out };
        }

        let filtered_vals: Vec<Value> = snap
            .entities
            .iter()
            .filter_map(|e| serde_json::to_value(e).ok())
            .filter(|v| entity_matches_filters(v, filters))
            .collect();

        for facet_raw in facets {
            let facet_key = facet_raw.trim();
            if facet_key.is_empty() {
                continue;
            }
            // Group case-insensitively while preserving canonical display casing of first seen value
            let mut counts: BTreeMap<String, (String, usize)> = BTreeMap::new();
            for ev in &filtered_vals {
                let mut seen_on_entity = BTreeSet::new();
                for val_str in resolve_scalar_values_at_path(ev, facet_key) {
                    let norm = val_str.to_lowercase();
                    if seen_on_entity.insert(norm.clone()) {
                        let entry = counts.entry(norm).or_insert_with(|| (val_str, 0));
                        entry.1 += 1;
                    }
                }
            }
            let mut entries: Vec<FacetEntry> = counts
                .into_values()
                .map(|(value, count)| FacetEntry { value, count })
                .collect();
            entries.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
            out.insert(facet_key.to_string(), entries);
        }

        CatalogFacetsResponse { facets: out }
    }

    pub async fn get_entity_by_name(
        &self,
        kind: &str,
        namespace: &str,
        name: &str,
        force_live: bool,
    ) -> Option<CatalogEntity> {
        let ns = if namespace.trim().is_empty() {
            "default"
        } else {
            namespace.trim()
        };
        let snap = self.get_snapshot_arc(force_live).await;
        snap.entities
            .iter()
            .find(|e| {
                e.kind.eq_ignore_ascii_case(kind.trim())
                    && e.metadata.namespace.eq_ignore_ascii_case(ns)
                    && e.metadata.name.eq_ignore_ascii_case(name.trim())
            })
            .cloned()
    }

    pub async fn get_entity_by_uid(&self, uid: &str, force_live: bool) -> Option<CatalogEntity> {
        let snap = self.get_snapshot_arc(force_live).await;
        snap.entities
            .iter()
            .find(|e| e.metadata.uid.eq_ignore_ascii_case(uid.trim()))
            .cloned()
    }

    pub async fn get_entities_by_refs(
        &self,
        refs: &[String],
        force_live: bool,
    ) -> Vec<Option<CatalogEntity>> {
        let snap = self.get_snapshot_arc(force_live).await;
        refs.iter()
            .map(|r| {
                let (kind, ns, name) = parse_entity_ref(r);
                snap.entities
                    .iter()
                    .find(|e| {
                        e.kind.eq_ignore_ascii_case(&kind)
                            && e.metadata.namespace.eq_ignore_ascii_case(&ns)
                            && e.metadata.name.eq_ignore_ascii_case(&name)
                    })
                    .cloned()
            })
            .collect()
    }

    pub async fn get_pits_entity_detail(
        &self,
        namespace: &str,
        target: &str,
        force_live: bool,
    ) -> Result<PitsEntityDetail> {
        let ns = if namespace.trim().is_empty() {
            "default"
        } else {
            namespace.trim()
        };
        let snap = self.get_snapshot_arc(force_live).await;

        let trimmed = target.trim().trim_matches('/');
        let (expected_type, name_part) = if let Some((_k, rest)) = trimmed.split_once(':') {
            if let Some((_n, nm)) = rest.split_once('/') {
                (None, nm)
            } else {
                (None, rest)
            }
        } else if let Some((first, second)) = trimmed.split_once('/') {
            if first.eq_ignore_ascii_case(ns) {
                (None, second)
            } else {
                (Some(first), second)
            }
        } else {
            (None, trimmed)
        };

        let key = format!("{}/{}", ns.to_lowercase(), name_part.to_lowercase());
        let detail = snap
            .details
            .get(&key)
            .cloned()
            .ok_or_else(|| anyhow!("entity {:?} not found in namespace {:?}", target, ns))?;

        if let Some(req_type) = expected_type {
            if !detail.resource_type.eq_ignore_ascii_case(req_type)
                && !detail.entity.kind.eq_ignore_ascii_case(req_type)
            {
                bail!(
                    "entity {:?} has resourceType {:?}, not {:?}",
                    name_part,
                    detail.resource_type,
                    req_type
                );
            }
        }

        Ok(detail)
    }

    pub async fn export_catalog_yaml(&self, force_live: bool) -> Result<String> {
        let entities = self.list_entities(force_live).await;
        let mut docs = Vec::with_capacity(entities.len());
        for ent in &entities {
            docs.push(serde_yaml::to_string(ent)?.trim().to_string());
        }
        Ok(docs.join("\n---\n") + "\n")
    }

    async fn collect_live_telemetry(&self, bundle: &WorkspaceBundle) -> LiveTelemetry {
        if !self.is_profile_configured().await {
            return LiveTelemetry::default();
        }

        let agent_opt = self.build_agent_req().await.ok();
        let core_k8s_opt = self.build_core_k8s_req().await.ok();
        let lab_k8s_opt = self.build_lab_k8s_req().await.ok();
        let talos_opt = self.build_talos_req().await.ok();
        let cached_pxe_verify = self.last_pxe_verify.read().await.clone();

        // `pits-agent` (:18443) enforces `ReadHeaderTimeout: 10s` from TCP `Accept()` through
        // TLS 1.3 + HTTP request headers. Decouple `agent_sem` (4 REST streams) from `lab_sem`
        // (4 HTTP CONNECT K8s streams) and `core_sem` (4 direct `:6443` streams) so large Lab K8s
        // `/api/v1/nodes` payloads never starve behind agent/router probes, then publish the Wave-1
        // snapshot immediately (`~5–8s`) before running Wave-2 Talos COSI (`2` nodes at a time).
        let agent_sem = Arc::new(Semaphore::new(4));
        let core_sem = Arc::new(Semaphore::new(4));
        let lab_sem = Arc::new(Semaphore::new(4));

        let agent_fut = async {
            let Some(ref ag) = agent_opt else {
                return (None, None, BTreeMap::new(), BTreeMap::new());
            };
            let health_f = async {
                for attempt in 0..2 {
                    let (res, attempt_elapsed) = {
                        let _p = agent_sem.acquire().await.ok();
                        let attempt_start = Instant::now();
                        let r =
                            tokio::time::timeout(PROBE_TIMEOUT, ag.get_json::<Value>("/v1/health"))
                                .await;
                        (r, attempt_start.elapsed())
                    };
                    if let Ok(Ok(v)) = res {
                        return Some(v);
                    }
                    if attempt == 0 {
                        if attempt_elapsed >= RETRY_TIMEOUT_CUTOFF {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(150)).await;
                    }
                }
                None
            };

            let btrfs_f = async {
                for attempt in 0..2 {
                    let (res, attempt_elapsed) = {
                        let _p = agent_sem.acquire().await.ok();
                        let attempt_start = Instant::now();
                        let r = tokio::time::timeout(PROBE_TIMEOUT, btrfs_status(ag)).await;
                        (r, attempt_start.elapsed())
                    };
                    if let Ok(Ok(v)) = res {
                        return Some(v);
                    }
                    if attempt == 0 {
                        if attempt_elapsed >= RETRY_TIMEOUT_CUTOFF {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(150)).await;
                    }
                }
                None
            };

            let inv_f = async {
                for attempt in 0..2 {
                    let (res, attempt_elapsed) = {
                        let _p = agent_sem.acquire().await.ok();
                        let attempt_start = Instant::now();
                        let r = tokio::time::timeout(PROBE_TIMEOUT, pxe_list_inventory(ag)).await;
                        (r, attempt_start.elapsed())
                    };
                    if let Ok(Ok(v)) = res {
                        return v;
                    }
                    if attempt == 0 {
                        if attempt_elapsed >= RETRY_TIMEOUT_CUTOFF {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(150)).await;
                    }
                }
                BTreeMap::new()
            };

            let san_f = async {
                let mut futs = Vec::with_capacity(bundle.nodes.len());
                for n in &bundle.nodes {
                    let ag_c = ag.clone();
                    let dev = n.device_id.clone();
                    let spec = n.san.clone();
                    let sem_n = agent_sem.clone();
                    futs.push(tokio::spawn(async move {
                        for attempt in 0..2 {
                            let (res, attempt_elapsed) = {
                                let _p = sem_n.acquire().await.ok();
                                let attempt_start = Instant::now();
                                let r =
                                    tokio::time::timeout(PROBE_TIMEOUT, san_inspect(&ag_c, &spec))
                                        .await;
                                (r, attempt_start.elapsed())
                            };
                            if let Ok(Ok(v)) = res {
                                return (dev, Some(v));
                            }
                            if attempt == 0 {
                                if attempt_elapsed >= RETRY_TIMEOUT_CUTOFF {
                                    break;
                                }
                                tokio::time::sleep(Duration::from_millis(150)).await;
                            }
                        }
                        (dev, None)
                    }));
                }
                let mut map = BTreeMap::new();
                for h in futs {
                    if let Ok((d, Some(v))) = h.await {
                        map.insert(d, v);
                    }
                }
                map
            };

            tokio::join!(health_f, btrfs_f, inv_f, san_f)
        };

        let router_fut = async {
            if core_k8s_opt.is_none() && agent_opt.is_none() {
                return None;
            }
            let mut last = None;
            for attempt in 0..2 {
                let (res, attempt_elapsed) = {
                    let _p = agent_sem.acquire().await.ok();
                    let attempt_start = Instant::now();
                    let r = tokio::time::timeout(
                        PROBE_TIMEOUT,
                        inspect_router_live(core_k8s_opt.as_ref(), agent_opt.as_ref()),
                    )
                    .await;
                    (r, attempt_start.elapsed())
                };
                if let Ok(summary) = res {
                    let healthy_enough =
                        summary.edge_router_present || summary.vpp_report.is_some();
                    last = Some(summary);
                    if healthy_enough {
                        return last;
                    }
                }
                if attempt == 0 {
                    if attempt_elapsed >= RETRY_TIMEOUT_CUTOFF {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
            }
            last
        };

        let core_k8s_fut = async {
            let Some(ref kc) = core_k8s_opt else {
                return (None, None, None, None, Vec::new());
            };
            let sem_ref = core_sem.as_ref();
            let sum_f = inspect_cluster_fast(kc, Some(sem_ref));
            let hw_f = async {
                for attempt in 0..2 {
                    let (res, attempt_elapsed) = {
                        let _p = sem_ref.acquire().await.ok();
                        let attempt_start = Instant::now();
                        let r =
                            tokio::time::timeout(PROBE_TIMEOUT, list_tinkerbell_hardware(kc)).await;
                        (r, attempt_start.elapsed())
                    };
                    if let Ok(Ok(v)) = res {
                        return Some(v);
                    }
                    if attempt == 0 {
                        if attempt_elapsed >= RETRY_TIMEOUT_CUTOFF {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(150)).await;
                    }
                }
                None
            };
            let wf_f = async {
                for attempt in 0..2 {
                    let (res, attempt_elapsed) = {
                        let _p = sem_ref.acquire().await.ok();
                        let attempt_start = Instant::now();
                        let r = tokio::time::timeout(PROBE_TIMEOUT, list_tinkerbell_workflows(kc))
                            .await;
                        (r, attempt_start.elapsed())
                    };
                    if let Ok(Ok(v)) = res {
                        return Some(v);
                    }
                    if attempt == 0 {
                        if attempt_elapsed >= RETRY_TIMEOUT_CUTOFF {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(150)).await;
                    }
                }
                None
            };
            let vm_f = list_cluster_vms_fast(kc, Some(sem_ref));
            let disc_f = discover_k8s_crossplane_fast(
                kc,
                "cluster-pits-core",
                "resource:default/cluster-pits-core",
                Some(&core_sem),
            );
            let (s, hw, wf, vms, disc) = tokio::join!(sum_f, hw_f, wf_f, vm_f, disc_f);
            (Some(s), hw, wf, vms.ok(), disc)
        };

        let lab_k8s_fut = async {
            let Some(ref kc) = lab_k8s_opt else {
                return (None, Vec::new(), None, Vec::new());
            };
            let sem_ref = lab_sem.as_ref();
            let sum_f = inspect_cluster_fast(kc, Some(sem_ref));
            let dc_f = kube_get_retry(
                kc,
                "/apis/resource.k8s.io/v1/deviceclasses",
                None,
                Some(sem_ref),
            );
            let vm_f = list_cluster_vms_fast(kc, Some(sem_ref));
            let disc_f = discover_k8s_crossplane_fast(
                kc,
                "cluster-pits-lab",
                "resource:default/cluster-pits-lab",
                Some(&lab_sem),
            );
            let (s, dcs, vms, disc) = tokio::join!(sum_f, dc_f, vm_f, disc_f);
            let mut dc_names = Vec::new();
            if let Ok(dc_val) = dcs {
                if let Some(items) = dc_val.get("items").and_then(|v| v.as_array()) {
                    for it in items {
                        if let Some(nm) = it.pointer("/metadata/name").and_then(|v| v.as_str()) {
                            dc_names.push(nm.to_string());
                        }
                    }
                }
            }
            dc_names.sort();
            (Some(s), dc_names, vms.ok(), disc)
        };

        // Wave 1: pits-agent REST + Core RKE2 + Lab K8s (bounded by decoupled `agent_sem`, `core_sem`, `lab_sem`)
        let (
            (agent_health, btrfs, hook_inventory, san_by_device),
            router_live,
            (core_cluster, tink_hardware, tink_workflows, core_vms, mut core_disc),
            (lab_cluster, lab_device_classes, lab_vms, mut lab_disc),
        ) = tokio::join!(agent_fut, router_fut, core_k8s_fut, lab_k8s_fut);

        core_disc.append(&mut lab_disc);
        let registered_units = {
            let guard = self.sourcing.read().await;
            guard
                .records()
                .map(|r| (r.name.to_ascii_lowercase(), r.clone()))
                .collect::<BTreeMap<String, SourcingResourceRecord>>()
        };
        let discovered_sourcing_candidates =
            extract_sourcing_candidates_from_k8s(&core_disc, &registered_units);

        let prior_talos = self.last_talos_by_device.read().await.clone();
        let wave1_connected = agent_health.is_some()
            || core_cluster.as_ref().map(|c| c.reachable).unwrap_or(false)
            || lab_cluster.as_ref().map(|c| c.reachable).unwrap_or(false)
            || prior_talos.values().any(|t| t.reachable);

        let mut live = LiveTelemetry {
            any_connected: wave1_connected,
            connection_note: Some(if wave1_connected {
                "Live telemetry synchronized from PITS control planes".to_string()
            } else {
                "Live telemetry endpoints unreachable; serving baseline catalog state".to_string()
            }),
            agent_health,
            router_live,
            btrfs,
            san_by_device,
            tink_hardware,
            tink_workflows,
            hook_inventory,
            pxe_asset_verify: cached_pxe_verify,
            core_cluster,
            lab_cluster,
            lab_device_classes,
            talos_by_device: prior_talos,
            core_vms,
            lab_vms,
            discovered_k8s_resources: core_disc,
            discovered_sourcing_candidates,
        };

        // Immediately publish the Wave-1 live snapshot (`~5–8s`) into `self.cache` BEFORE Wave 2
        // (`talos_by_device`) runs so Portal & API readers see `liveConnected: true`, Btrfs capacity,
        // SAN exports, Core/Lab K8s node readiness, and DRA v1 NPU slices without waiting on COSI!
        if talos_opt.is_some() && live.any_connected {
            let cached_rdp = self.last_rdp_tls.read().await.clone();
            let sourcing_snap = self.sourcing.read().await.clone();
            let wave1_snap = synthesize_snapshot_from_inputs(
                bundle,
                cached_rdp.as_ref(),
                &live,
                &sourcing_snap,
                Utc::now(),
            );
            {
                let mut guard = self.cache.write().await;
                *guard = Some((Instant::now(), Arc::new(wave1_snap)));
            }
            self.has_live_snapshot.store(true, Ordering::Release);
        }

        // Wave 2: Talos COSI gRPC (`2` nodes at a time = max 12 streams on an otherwise idle bridge)
        if let Some(ref tc) = talos_opt {
            let talos_sem = Arc::new(Semaphore::new(2));
            let mut futs = Vec::with_capacity(bundle.nodes.len());
            for n in &bundle.nodes {
                let tc_c = tc.clone();
                let dev = n.device_id.clone();
                let ip = n.node_ip.clone();
                let sem_t = talos_sem.clone();
                futs.push(tokio::spawn(async move {
                    let (first, first_elapsed) = {
                        let _p = sem_t.acquire().await.ok();
                        let attempt_start = Instant::now();
                        let r = tokio::time::timeout(PROBE_TIMEOUT, tc_c.inspect_node(&ip))
                            .await
                            .ok();
                        (r, attempt_start.elapsed())
                    };
                    let needs_retry = first_elapsed < RETRY_TIMEOUT_CUTOFF
                        && match &first {
                            Some(st) => {
                                !st.reachable
                                    || st.cosi_disk_wwids.is_empty()
                                    || st.physical_npus.is_empty()
                            }
                            None => true,
                        };
                    let merged = if needs_retry {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        let second = {
                            let _p = sem_t.acquire().await.ok();
                            tokio::time::timeout(PROBE_TIMEOUT, tc_c.inspect_node(&ip))
                                .await
                                .ok()
                        };
                        merge_talos_status(first, second)
                    } else {
                        first
                    };
                    (dev, merged)
                }));
            }
            for h in futs {
                if let Ok((dev, Some(st))) = h.await {
                    let combined = match live.talos_by_device.remove(&dev) {
                        Some(prev) => merge_talos_status(Some(prev), Some(st)).unwrap(),
                        None => st,
                    };
                    live.talos_by_device.insert(dev, combined);
                }
            }
            if !live.talos_by_device.is_empty() {
                let mut talos_guard = self.last_talos_by_device.write().await;
                *talos_guard = live.talos_by_device.clone();
            }
        }

        live.any_connected = live.agent_health.is_some()
            || live.core_cluster.as_ref().map(|c| c.reachable).unwrap_or(false)
            || live.lab_cluster.as_ref().map(|c| c.reachable).unwrap_or(false)
            || live.talos_by_device.values().any(|t| t.reachable);

        live.connection_note = Some(if live.any_connected {
            "Live telemetry synchronized from PITS control planes".to_string()
        } else {
            "Live telemetry endpoints unreachable; serving baseline catalog state".to_string()
        });

        live
    }

    async fn synthesize_live_snapshot(&self) -> CatalogSnapshot {
        let now = Utc::now();
        let bundle = self.bundle.read().await.clone();
        let cached_rdp = self.last_rdp_tls.read().await.clone();
        let live = self.collect_live_telemetry(&bundle).await;
        if !live.discovered_sourcing_candidates.is_empty() {
            let _ = self.sourcing_db.upsert_discovery_candidates(
                &live.discovered_sourcing_candidates,
                "k8s-crossplane-connector",
                "k8s-discovery",
            );
            let mut guard = self.sourcing.write().await;
            let _ = guard.refresh_from_db();
        }
        let sourcing = self.sourcing.read().await.clone();
        synthesize_snapshot_from_inputs(&bundle, cached_rdp.as_ref(), &live, &sourcing, now)
    }

    pub async fn sourcing_overview(&self, force_live: bool) -> SourcingOverviewResponse {
        let snap = self.get_snapshot_arc(force_live).await;
        let ws_dir = {
            let guard = self.sourcing.read().await;
            guard
                .workspace_yaml_dir
                .as_ref()
                .map(|p| p.display().to_string())
        };
        let mut items: Vec<SourcingEvaluation> = Vec::new();
        let mut quadrant_counts: BTreeMap<String, usize> = BTreeMap::from([
            ("strategic".to_string(), 0),
            ("leverage".to_string(), 0),
            ("bottleneck".to_string(), 0),
            ("non-critical".to_string(), 0),
        ]);
        let mut redline_counts: BTreeMap<String, usize> = BTreeMap::from([
            ("windmillMismatch".to_string(), 0),
            ("reAuditRequired".to_string(), 0),
            ("inCriticalZone".to_string(), 0),
            ("knockoutTriggered".to_string(), 0),
        ]);

        for ent in &snap.entities {
            if ent.kind == "Resource" {
                let stype = ent
                    .spec
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if is_sourcing_resource_type(stype) {
                    let key = format!(
                        "{}/{}",
                        ent.metadata.namespace.to_ascii_lowercase(),
                        ent.metadata.name.to_ascii_lowercase()
                    );
                    if let Some(detail) = snap.details.get(&key) {
                        if let Ok(eval) = serde_json::from_value::<SourcingEvaluation>(
                            detail.diagnostics.clone(),
                        ) {
                            *quadrant_counts
                                .entry(eval.facts.quadrant.clone())
                                .or_insert(0) += 1;
                            if eval.facts.windmill_mismatch {
                                *redline_counts
                                    .entry("windmillMismatch".to_string())
                                    .or_insert(0) += 1;
                            }
                            if eval.facts.re_audit_required {
                                *redline_counts
                                    .entry("reAuditRequired".to_string())
                                    .or_insert(0) += 1;
                            }
                            if eval.facts.in_critical_zone {
                                *redline_counts
                                    .entry("inCriticalZone".to_string())
                                    .or_insert(0) += 1;
                            }
                            if eval.facts.knockout_triggered {
                                *redline_counts
                                    .entry("knockoutTriggered".to_string())
                                    .or_insert(0) += 1;
                            }
                            items.push(eval);
                        }
                    }
                }
            }
        }

        let available_systems: Vec<Value> = snap
            .entities
            .iter()
            .filter(|e| e.kind == "System")
            .map(|e| {
                json!({
                    "ref": format!("system:{}/{}", e.metadata.namespace, e.metadata.name),
                    "name": e.metadata.name,
                    "title": e.metadata.title.clone().unwrap_or_else(|| e.metadata.name.clone()),
                    "domain": e.spec.get("domain").and_then(|v| v.as_str()).unwrap_or("platform-infra"),
                })
            })
            .collect();

        let available_components: Vec<Value> = snap
            .entities
            .iter()
            .filter(|e| e.kind == "Component")
            .map(|e| {
                json!({
                    "ref": format!("component:{}/{}", e.metadata.namespace, e.metadata.name),
                    "name": e.metadata.name,
                    "title": e.metadata.title.clone().unwrap_or_else(|| e.metadata.name.clone()),
                    "system": e.spec.get("system").and_then(|v| v.as_str()).unwrap_or("pits-infrastructure"),
                })
            })
            .collect();

        let (db_sync, discovery_candidates, blind_ballots, recent_events) = {
            let guard = self.sourcing.read().await;
            (
                guard.db.sync_status(),
                guard.db.list_candidates(None).unwrap_or_default(),
                guard.db.list_ballots(None).unwrap_or_default(),
                guard.db.list_recent_events(40).unwrap_or_default(),
            )
        };
        let mcp_endpoints = json!({
            "stdioCommand": "pitsctl mcp",
            "streamableHttpPath": "/mcp",
            "protocolVersion": "2025-03-26",
            "resources": [
                "sourcing://portfolio/overview",
                "sourcing://discovery/candidates",
                "sourcing://unit/{name}"
            ],
            "prompts": [
                "audit-external-dependency",
                "reconcile-blind-variance"
            ]
        });

        SourcingOverviewResponse {
            generated_at: Utc::now().to_rfc3339(),
            total_units: items.len(),
            quadrant_counts,
            redline_counts,
            items,
            available_systems,
            available_components,
            workspace_yaml_dir: ws_dir,
            db_sync,
            discovery_candidates,
            blind_ballots,
            recent_events,
            mcp_endpoints,
        }
    }

    pub async fn evaluate_sourcing_for_entity(
        &self,
        namespace: &str,
        kind: &str,
        name: &str,
    ) -> Result<SourcingEvaluation> {
        if !kind.eq_ignore_ascii_case("resource") {
            bail!("Sourcing Tech Insights only evaluates kind: Resource (got {kind})");
        }
        let detail = self.get_pits_entity_detail(namespace, name, false).await?;
        if !is_sourcing_resource_type(&detail.resource_type) {
            bail!(
                "Entity '{}/{}' (type '{}') is not a Sourcing Governance resource",
                namespace,
                name,
                detail.resource_type
            );
        }
        serde_json::from_value::<SourcingEvaluation>(detail.diagnostics)
            .map_err(|e| anyhow!("missing sourcing evaluation on '{}/{}': {}", namespace, name, e))
    }

    pub async fn upsert_sourcing_from_wizard(
        &self,
        req: SourcingWizardUpsertRequest,
    ) -> Result<SourcingWizardUpsertResponse> {
        let system_domains: BTreeMap<String, (String, String)> = default_system_domain_map()
            .into_iter()
            .map(|(k, (d, t))| (k, (d.to_string(), t.to_string())))
            .collect();

        let validation = validate_sourcing_record(&req.record, None, &system_domains);
        if !validation.valid {
            bail!(
                "CatalogProcessor validation failed: {}",
                validation.errors.join("; ")
            );
        }

        let override_reason = req
            .override_reason
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        if !req.dry_run && override_reason.is_empty() {
            bail!(
                "Emergency fallback overrideReason is required when saving via Sourcing Wizard; prefer MCP discovery & blind ballots for standard workflow"
            );
        }
        let actor = req
            .actor
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("portal-operator")
            .to_string();

        let snap = self.get_snapshot_arc(false).await;
        let mut comp_info: BTreeMap<String, (String, String)> = BTreeMap::new();
        for ent in &snap.entities {
            if ent.kind == "Component" {
                let c_ref = format!(
                    "component:{}/{}",
                    ent.metadata.namespace.to_ascii_lowercase(),
                    ent.metadata.name.to_ascii_lowercase()
                );
                let title = ent
                    .metadata
                    .title
                    .clone()
                    .unwrap_or_else(|| ent.metadata.name.clone());
                let sys = ent
                    .spec
                    .get("system")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&req.record.system);
                comp_info.insert(c_ref, (title, canonical_system_ref(sys)));
            }
        }

        let target_res_ref = format!(
            "resource:{}/{}",
            req.record.namespace.to_ascii_lowercase(),
            req.record.name.to_ascii_lowercase()
        );
        let mut dep_comp_refs: BTreeSet<String> = req
            .record
            .depends_on_components
            .iter()
            .map(|c| canonical_component_ref(c))
            .collect();
        // If user left depends_on_components empty during edit, preserve existing live component relations.
        if dep_comp_refs.is_empty() {
            if let Some(existing_detail) = snap
                .details
                .get(&format!(
                    "{}/{}",
                    req.record.namespace.to_ascii_lowercase(),
                    req.record.name.to_ascii_lowercase()
                ))
            {
                for r in &existing_detail.entity.relations {
                    if r.relation_type == "dependencyOf"
                        && r.target_ref.starts_with("component:")
                        && !r.target_ref.eq_ignore_ascii_case(&target_res_ref)
                    {
                        dep_comp_refs.insert(r.target_ref.to_ascii_lowercase());
                    }
                }
            }
        }

        let lineage: Vec<DomainLineageEntry> = dep_comp_refs
            .iter()
            .map(|c_ref| {
                let (title, sys_ref) = comp_info.get(c_ref).cloned().unwrap_or_else(|| {
                    (
                        c_ref.clone(),
                        canonical_system_ref(&req.record.system),
                    )
                });
                let (dom_ref, dom_tier) = system_domains.get(&sys_ref).cloned().unwrap_or_else(|| {
                    (
                        "domain:default/platform-infra".to_string(),
                        "Platform Infrastructure Domain".to_string(),
                    )
                });
                DomainLineageEntry {
                    component_ref: c_ref.clone(),
                    component_title: title,
                    system_ref: sys_ref,
                    domain_ref: dom_ref,
                    domain_tier: dom_tier,
                }
            })
            .collect();

        let mut evaluation = evaluate_sourcing_audit_at(
            &req.record,
            lineage,
            &system_domains,
            today_utc_date(),
        );
        let generated_yaml = render_catalog_info_yaml(&req.record, Some(&evaluation.facts));

        let (state_store_path, existing_ws_yaml, ws_dir_opt) = {
            let guard = self.sourcing.read().await;
            (
                guard.state_store_path.display().to_string(),
                guard.read_existing_workspace_yaml(&req.record.name),
                guard.workspace_yaml_dir.clone(),
            )
        };
        let baseline_yaml = if existing_ws_yaml.is_empty() {
            String::new()
        } else {
            existing_ws_yaml
        };
        let unified_diff = compute_unified_diff(
            &baseline_yaml,
            &generated_yaml,
            &format!("catalog/sourcing/{}.yaml", req.record.name.to_ascii_lowercase()),
        );

        let mut workspace_yaml_written = false;
        let mut workspace_yaml_path = ws_dir_opt
            .as_ref()
            .map(|d| d.join(format!("{}.yaml", req.record.name.to_ascii_lowercase())).display().to_string());

        if !req.dry_run {
            {
                let mut guard = self.sourcing.write().await;
                let mut stored_rec = req.record.clone();
                if stored_rec.depends_on_components.is_empty() && !dep_comp_refs.is_empty() {
                    stored_rec.depends_on_components = dep_comp_refs.into_iter().collect();
                }
                let event_summary = format!(
                    "Emergency fallback override on '{}': {}",
                    stored_rec.name, override_reason
                );
                guard
                    .upsert_record_with_meta(
                        stored_rec.clone(),
                        true,
                        "wizard-fallback",
                        &actor,
                        "portal-wizard-fallback",
                        &event_summary,
                        false,
                    )
                    .map_err(|e| anyhow!(e))?;
                guard.enrich_evaluation(&mut evaluation);
                evaluation.source_origin = "wizard-fallback".to_string();
                if req.write_workspace_yaml {
                    let path = guard
                        .write_workspace_yaml(&stored_rec, Some(&evaluation.facts))
                        .map_err(|e| anyhow!(e))?;
                    workspace_yaml_written = true;
                    workspace_yaml_path = Some(path.display().to_string());
                }
            }
            self.invalidate_cache().await;
        } else {
            let guard = self.sourcing.read().await;
            guard.enrich_evaluation(&mut evaluation);
            evaluation.source_origin = "wizard-fallback".to_string();
        }

        let persistence_mode = if req.dry_run {
            "dry-run-preview".to_string()
        } else if workspace_yaml_written {
            "state-store+workspace-yaml".to_string()
        } else {
            "state-store".to_string()
        };

        Ok(SourcingWizardUpsertResponse {
            ok: true,
            dry_run: req.dry_run,
            entity_ref: evaluation.entity_ref.clone(),
            persistence_mode,
            state_store_path,
            workspace_yaml_path,
            workspace_yaml_written,
            generated_yaml,
            unified_diff,
            evaluation,
        })
    }

    pub async fn sourcing_set_postgres_url(&self, pg_url: Option<String>) {
        let guard = self.sourcing.read().await;
        guard.db.set_postgres_url(pg_url);
    }

    pub async fn sourcing_db_status(&self) -> SourcingDbSyncStatus {
        let guard = self.sourcing.read().await;
        guard.db.sync_status()
    }

    pub async fn sourcing_sync_db(&self, pg_url_override: Option<&str>) -> SourcingDbSyncStatus {
        let effective_override = if pg_url_override.is_some() {
            pg_url_override.map(str::to_string)
        } else {
            let prof = self.profile.read().await;
            effective_sourcing_db_url(None, &prof)
        };
        let db = {
            let guard = self.sourcing.read().await;
            guard.db.clone()
        };
        let status = db.sync_with_postgres(effective_override.as_deref()).await;
        {
            let mut guard = self.sourcing.write().await;
            let _ = guard.refresh_from_db();
        }
        self.invalidate_cache().await;
        status
    }

    pub async fn sourcing_scan_workspace(
        &self,
        custom_root: Option<&Path>,
        actor: &str,
        channel: &str,
    ) -> Result<Vec<DiscoveryCandidateRecord>> {
        let root = if let Some(p) = custom_root {
            p.to_path_buf()
        } else if let Some(src) = self.bundle.read().await.source_dir.clone() {
            PathBuf::from(src)
        } else {
            std::env::current_dir()?
        };

        let (db, registered_units) = {
            let guard = self.sourcing.read().await;
            let map: BTreeMap<String, SourcingResourceRecord> = guard
                .records()
                .map(|r| (r.name.to_ascii_lowercase(), r.clone()))
                .collect();
            (guard.db.clone(), map)
        };

        let candidates = scan_workspace_dependencies(&root, &registered_units);
        let _ = db
            .upsert_discovery_candidates(&candidates, actor, channel)
            .map_err(|e| anyhow!(e))?;
        {
            let mut guard = self.sourcing.write().await;
            let _ = guard.refresh_from_db();
        }
        db.list_candidates(None).map_err(|e| anyhow!(e))
    }

    pub async fn sourcing_submit_ballot(
        &self,
        req: SubmitBlindBallotRequest,
    ) -> Result<AggregatedBallotStats> {
        self.sourcing_submit_ballot_with_channel(req, "mcp").await
    }

    pub async fn sourcing_submit_ballot_with_channel(
        &self,
        req: SubmitBlindBallotRequest,
        channel: &str,
    ) -> Result<AggregatedBallotStats> {
        let stats = {
            let mut guard = self.sourcing.write().await;
            let stats = guard
                .db
                .submit_blind_ballot(&req, channel)
                .map_err(|e| anyhow!(e))?;
            guard.refresh_from_db().map_err(|e| anyhow!(e))?;
            stats
        };
        self.invalidate_cache().await;
        Ok(stats)
    }

    pub async fn sourcing_assess_windmill(
        &self,
        req: AssessWindmillRequest,
        channel: &str,
    ) -> Result<SourcingEvaluation> {
        {
            let mut guard = self.sourcing.write().await;
            guard
                .db
                .assess_windmill(&req, channel)
                .map_err(|e| anyhow!(e))?;
            guard.refresh_from_db().map_err(|e| anyhow!(e))?;
        }
        self.invalidate_cache().await;
        self.evaluate_sourcing_for_entity("default", "resource", &req.unit_name)
            .await
    }

    pub async fn sourcing_promote_or_ignore_candidate(
        &self,
        req: CandidateDispositionRequest,
        channel: &str,
    ) -> Result<(DiscoveryCandidateRecord, Option<SourcingEvaluation>)> {
        let action = req.action.trim().to_ascii_lowercase();
        let actor = req
            .actor
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("mcp-auditor")
            .to_string();

        let candidates = {
            let guard = self.sourcing.read().await;
            guard.db.list_candidates(None).map_err(|e| anyhow!(e))?
        };
        let cand = candidates
            .into_iter()
            .find(|c| c.candidate_id == req.candidate_id)
            .ok_or_else(|| anyhow!("Unknown discovery candidate '{}'", req.candidate_id))?;

        match action.as_str() {
            "ignore" => {
                let updated = {
                    let guard = self.sourcing.read().await;
                    guard
                        .db
                        .update_candidate_disposition(
                            &cand.candidate_id,
                            "ignored",
                            None,
                            &actor,
                            channel,
                        )
                        .map_err(|e| anyhow!(e))?
                };
                Ok((updated, None))
            }
            "promote" => {
                let raw_type = req
                    .resource_type
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&cand.suggested_type);
                let resource_type =
                    validate_suggested_sourcing_type(raw_type).map_err(|e| anyhow!(e))?;

                let slug: String = req
                    .name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&cand.package_or_endpoint)
                    .to_ascii_lowercase()
                    .chars()
                    .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
                    .collect::<String>()
                    .split('-')
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<&str>>()
                    .join("-");
                let mut unit_name: String = if slug.is_empty() {
                    format!("unit-{}", &cand.candidate_id[..cand.candidate_id.len().min(8)])
                } else {
                    slug.chars().take(48).collect()
                };

                let namespace = req
                    .namespace
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("default")
                    .to_string();
                let system = req
                    .system
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&cand.inferred_system)
                    .to_string();
                let owner = req
                    .owner
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("group:default/operators")
                    .to_string();
                let vendor_name = req
                    .vendor_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&cand.package_or_endpoint)
                    .to_string();
                let vendor_slug: String = vendor_name
                    .trim()
                    .to_ascii_lowercase()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                    .collect::<String>()
                    .trim_matches('-')
                    .to_string();
                let custom_name_supplied = req
                    .name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .is_some();
                if !custom_name_supplied && !vendor_slug.is_empty() && unit_name == vendor_slug {
                    let prefix: String = unit_name.chars().take(43).collect();
                    unit_name = format!("{prefix}-unit");
                }
                let cost_allocation_id = req
                    .cost_allocation_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        let short: String = unit_name.chars().take(12).collect();
                        format!("cc-mcp-{short}")
                    });
                let description = req
                    .description
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        format!(
                            "Discovered {} dependency ({}) from {}",
                            cand.ecosystem, cand.package_or_endpoint, cand.detected_in_path
                        )
                    });
                let depends_on_components = req
                    .depends_on_components
                    .unwrap_or_else(|| cand.inferred_components.clone())
                    .into_iter()
                    .map(|c| canonical_component_ref(&c))
                    .collect::<Vec<String>>();

                let today_str = today_utc_date().format("%Y-%m-%d").to_string();
                let rec = SourcingResourceRecord {
                    name: unit_name.clone(),
                    namespace: namespace.clone(),
                    description,
                    resource_type,
                    owner,
                    system,
                    adapter_abstracted: req.adapter_abstracted.unwrap_or(cand.adapter_detected),
                    cost_allocation_id,
                    depends_on_components,
                    audit: SourcingAuditSpec {
                        contract_meta: ContractMeta {
                            vendor_name,
                            renewal_date: normalize_renewal_date(req.renewal_date.as_deref()),
                            last_audited_at: today_str,
                            baseline_prod_dependents: 1,
                            blind_score_variance_alert: false,
                        },
                        manual_scores: ManualScores {
                            y2_core_competitiveness: 3.0,
                            y4_price_elasticity: 3.0,
                            x2_market_scarcity: 3.0,
                            x3_customization_depth: 3.0,
                            x4_compliance_fragility: 3.0,
                        },
                        supplier_preference: SupplierPreference {
                            estimated_wallet_share_tier: "1-to-5pct".to_string(),
                            account_attractiveness_score: 3.0,
                            perceived_status: "development".to_string(),
                        },
                    },
                };

                let system_domains: BTreeMap<String, (String, String)> = default_system_domain_map()
                    .into_iter()
                    .map(|(k, (d, t))| (k, (d.to_string(), t.to_string())))
                    .collect();
                let validation = validate_sourcing_record(&rec, None, &system_domains);
                if !validation.valid {
                    bail!(
                        "CatalogProcessor rejected candidate promotion '{}': {}",
                        cand.candidate_id,
                        validation.errors.join("; ")
                    );
                }

                let updated_cand = {
                    let mut guard = self.sourcing.write().await;
                    guard
                        .upsert_record_with_meta(
                            rec,
                            true,
                            "mcp-discovery",
                            &actor,
                            channel,
                            &format!(
                                "Promoted discovery candidate '{}' ({}) -> resource:{}/{}",
                                cand.package_or_endpoint, cand.candidate_id, namespace, unit_name
                            ),
                            false,
                        )
                        .map_err(|e| anyhow!(e))?;
                    let c = guard
                        .db
                        .update_candidate_disposition(
                            &cand.candidate_id,
                            "promoted",
                            Some(&unit_name),
                            &actor,
                            channel,
                        )
                        .map_err(|e| anyhow!(e))?;
                    guard.refresh_from_db().map_err(|e| anyhow!(e))?;
                    c
                };
                self.invalidate_cache().await;
                let eval = self
                    .evaluate_sourcing_for_entity(&namespace, "resource", &unit_name)
                    .await?;
                Ok((updated_cand, Some(eval)))
            }
            other => bail!(
                "Unsupported candidate disposition action '{}'; expected 'promote' or 'ignore'",
                other
            ),
        }
    }

    pub async fn sourcing_set_adapter_decoupling(
        &self,
        unit_name: &str,
        adapter_abstracted: bool,
        actor: &str,
        channel: &str,
    ) -> Result<SourcingEvaluation> {
        let mut rec = {
            let guard = self.sourcing.read().await;
            guard
                .get(unit_name)
                .cloned()
                .ok_or_else(|| anyhow!("Unknown sourcing unit '{}'", unit_name))?
        };
        rec.adapter_abstracted = adapter_abstracted;
        let namespace = rec.namespace.clone();
        let name = rec.name.clone();
        {
            let mut guard = self.sourcing.write().await;
            guard
                .upsert_record_with_meta(
                    rec,
                    true,
                    "mcp-discovery",
                    actor,
                    channel,
                    &format!("Set adapterAbstracted={} via {}", adapter_abstracted, channel),
                    false,
                )
                .map_err(|e| anyhow!(e))?;
        }
        self.invalidate_cache().await;
        self.evaluate_sourcing_for_entity(&namespace, "resource", &name)
            .await
    }
}

pub(crate) fn synthesize_snapshot_from_inputs(
    bundle: &WorkspaceBundle,
    cached_rdp: Option<&RdpTlsVerification>,
    live: &LiveTelemetry,
    sourcing: &SourcingStore,
    now: DateTime<Utc>,
) -> CatalogSnapshot {
    let mut ordered_refs: Vec<String> = Vec::new();
    let mut details: BTreeMap<String, PitsEntityDetail> = BTreeMap::new();

    // 0a. Top-level Backstage Domains (3): `core-commerce`, `platform-infra`, `spatial-simulation`
    {
        let domain_defs = [
            (
                "core-commerce",
                "Core Commerce & Risk",
                "Core checkout, payment gateway authorization, and real-time anti-fraud domain",
                "group:default/risk-engineering",
                vec!["commerce".to_string(), "risk".to_string(), "core-domain".to_string()],
            ),
            (
                "platform-infra",
                "Platform Infrastructure",
                "Unified PITS Zero-Trust Edge Router, NVMe/TCP SAN, Diskless Talos Linux, Dual Kubernetes, DRA NPU & KubeVirt Platform",
                "group:default/operators",
                vec!["platform".to_string(), "nixos".to_string(), "talos".to_string()],
            ),
            (
                "spatial-simulation",
                "Spatial & City Simulation",
                "OpenUSD asset baking and mixed-integer road layout simulation pipeline domain",
                "group:default/operators",
                vec!["simulation".to_string(), "openusd".to_string(), "spatial".to_string()],
            ),
        ];
        for (name, title, desc, owner, tags) in domain_defs {
            let ent = make_catalog_entity(
                "Domain",
                "default",
                name,
                Some(title),
                Some(desc),
                "domain",
                "Active",
                "ok",
                "Domain",
                live.any_connected,
                tags,
                json!({
                    "owner": owner,
                    "type": "business-domain"
                }),
                vec![EntityRelation {
                    relation_type: "ownedBy".to_string(),
                    target_ref: owner.to_string(),
                }],
            );
            let detail = PitsEntityDetail {
                entity: ent,
                resource_type: "domain".to_string(),
                status: "Active".to_string(),
                status_level: "ok".to_string(),
                summary_line: desc.to_string(),
                refreshed_at: now,
                live_connected: live.any_connected,
                attribute_sections: vec![AttributeSection {
                    id: "domain-overview".to_string(),
                    title: "Business Domain Governance".to_string(),
                    items: vec![
                        attr("domainName", "Domain ID", name, Some("info"), true),
                        attr("owner", "Owner Group", owner, Some("ok"), true),
                        attr("tier", "Catalog Hierarchy Layer", "Layer 1 (Domain)", Some("ok"), false),
                    ],
                }],
                actions: vec![],
                manifests: BTreeMap::new(),
                diagnostics: json!({ "layer": "domain", "owner": owner }),
            };
            let k = format!("default/{name}");
            ordered_refs.push(k.clone());
            details.insert(k, detail);
        }
    }

    // 0b. Systems (3): `pits-infrastructure`, `core-checkout`, `pits-city-pipeline`
    {
        let total_sourcing = sourcing.records().count();
        let total_resources = 26 + total_sourcing;
        let sys_ent = make_catalog_entity(
            "System",
            "default",
            "pits-infrastructure",
            Some("PITS Infrastructure Fleet"),
            Some("Unified Zero-Trust Edge Router, NVMe/TCP SAN, Diskless Talos Linux, Dual Kubernetes, DRA NPU & KubeVirt Platform"),
            "system",
            if live.any_connected { "LiveConnected" } else { "BaselineReady" },
            "ok",
            "System",
            live.any_connected,
            vec!["pits".to_string(), "nixos".to_string(), "talos".to_string(), "backstage".to_string()],
            json!({
                "owner": "group:default/operators",
                "domain": "platform-infra",
                "type": "infrastructure-fleet"
            }),
            vec![
                EntityRelation {
                    relation_type: "ownedBy".to_string(),
                    target_ref: "group:default/operators".to_string(),
                },
                EntityRelation {
                    relation_type: "partOf".to_string(),
                    target_ref: "domain:default/platform-infra".to_string(),
                },
            ],
        );
        let sys_detail = PitsEntityDetail {
            entity: sys_ent,
            resource_type: "system".to_string(),
            status: if live.any_connected {
                "LiveConnected".to_string()
            } else {
                "BaselineReady".to_string()
            },
            status_level: "ok".to_string(),
            summary_line: format!(
                "5 bare-metal machines, 4 physical NPUs, 9 NVMe/TCP SAN volumes, 2 K8s clusters, 2 KubeVirt VMs, {} external sourcing units",
                total_sourcing
            ),
            refreshed_at: now,
            live_connected: live.any_connected,
            attribute_sections: vec![AttributeSection {
                id: "fleet-overview".to_string(),
                title: "Fleet Architecture Summary".to_string(),
                items: vec![
                    attr("resourceTypes", "Registered Resource Types", "12", Some("ok"), false),
                    attr("resourceEntities", "Total Resource Entities", total_resources.to_string(), Some("ok"), false),
                    attr("agentConnected", "pits-agent mTLS Telemetry", if live.agent_health.is_some() { "Connected" } else { "Offline (Baseline)" }, Some(if live.agent_health.is_some() { "ok" } else { "info" }), false),
                ],
            }],
            actions: actions_for_k8s_cluster(),
            manifests: BTreeMap::new(),
            diagnostics: json!({
                "agentHealth": live.agent_health,
                "connectionNote": live.connection_note,
                "workspaceSource": bundle.source_dir,
                "importedFiles": bundle.imported_files,
            }),
        };
        ordered_refs.push("default/pits-infrastructure".to_string());
        details.insert("default/pits-infrastructure".to_string(), sys_detail);

        let extra_systems = [
            (
                "core-checkout",
                "Core Checkout & Anti-Fraud",
                "Real-time payment authorization, anti-fraud scoring, and chargeback risk execution platform",
                "group:default/risk-engineering",
                "core-commerce",
                "transaction-core",
            ),
            (
                "pits-city-pipeline",
                "PITS City & Spatial Pipeline",
                "City layout optimization and 3D OpenUSD asset baking simulation workflow",
                "group:default/operators",
                "spatial-simulation",
                "simulation-pipeline",
            ),
        ];
        for (name, title, desc, owner, domain, stype) in extra_systems {
            let ent = make_catalog_entity(
                "System",
                "default",
                name,
                Some(title),
                Some(desc),
                "system",
                "Active",
                "ok",
                "System",
                live.any_connected,
                vec![domain.to_string(), stype.to_string()],
                json!({
                    "owner": owner,
                    "domain": domain,
                    "type": stype,
                }),
                vec![
                    rel("ownedBy", owner),
                    rel("partOf", &format!("domain:default/{domain}")),
                ],
            );
            let detail = PitsEntityDetail {
                entity: ent,
                resource_type: "system".to_string(),
                status: "Active".to_string(),
                status_level: "ok".to_string(),
                summary_line: desc.to_string(),
                refreshed_at: now,
                live_connected: live.any_connected,
                attribute_sections: vec![AttributeSection {
                    id: "system-lineage".to_string(),
                    title: "4-Layer Catalog Hierarchy".to_string(),
                    items: vec![
                        attr("systemName", "System", name, Some("info"), true),
                        attr("domain", "Parent Domain", domain, Some("ok"), true),
                        attr("owner", "Owner", owner, Some("ok"), true),
                    ],
                }],
                actions: vec![],
                manifests: BTreeMap::new(),
                diagnostics: json!({ "domain": domain, "owner": owner }),
            };
            let k = format!("default/{name}");
            ordered_refs.push(k.clone());
            details.insert(k, detail);
        }
    }

    // 0c. Components (7): business components bound via dependsOn to Resources
    {
        let component_defs = [
            (
                "checkout-risk-gateway",
                "Checkout Risk Gateway",
                "Inline payment risk interception gateway invoking anti-fraud scoring",
                "group:default/risk-engineering",
                "core-checkout",
                "service",
                vec!["resource:default/payment-fraud-engine-sdk"],
            ),
            (
                "payment-auth-router",
                "Payment Authorization Router",
                "Card scheme 3DS routing and real-time fraud pre-check service",
                "group:default/risk-engineering",
                "core-checkout",
                "service",
                vec!["resource:default/payment-fraud-engine-sdk"],
            ),
            (
                "chargeback-scoring-worker",
                "Chargeback Scoring Worker",
                "Async dispute risk model evaluator for settlement transactions",
                "group:default/risk-engineering",
                "core-checkout",
                "worker",
                vec!["resource:default/payment-fraud-engine-sdk"],
            ),
            (
                "tailnet-relay-controller",
                "Tailnet Relay Controller",
                "Site-to-site Mesh VPN control plane & BGP uplink failover coordinator",
                "group:default/operators",
                "pits-infrastructure",
                "service",
                vec![
                    "resource:default/tailscale-coordination-saas",
                    "resource:default/transit-bgp-managed-uplink",
                    "resource:default/pits-core-router",
                ],
            ),
            (
                "edge-ingress-proxy",
                "Edge Ingress Proxy",
                "Zero-trust remote ingress proxy anchored on core cluster & BGP transit",
                "group:default/operators",
                "pits-infrastructure",
                "service",
                vec![
                    "resource:default/tailscale-coordination-saas",
                    "resource:default/transit-bgp-managed-uplink",
                    "resource:default/cluster-pits-core",
                ],
            ),
            (
                "spatial-layout-optimizer",
                "Spatial Layout Optimizer",
                "Urban road & parcel MIP optimization engine",
                "group:default/operators",
                "pits-city-pipeline",
                "library",
                vec!["resource:default/commercial-mip-solver-sdk"],
            ),
            (
                "usd-asset-baker",
                "OpenUSD Asset Baker",
                "Landmark PBR shader & OpenUSD scene packaging pipeline worker",
                "group:default/operators",
                "pits-city-pipeline",
                "service",
                vec![
                    "resource:default/hero-landmark-art-outsourcing",
                    "resource:default/cluster-pits-lab",
                ],
            ),
        ];

        for (name, title, desc, owner, system, ctype, base_deps) in component_defs {
            let comp_ref = format!("component:default/{name}");
            let mut dep_set: BTreeSet<String> = base_deps.into_iter().map(str::to_string).collect();
            if let Some(extra) = sourcing.component_extra_deps().get(&comp_ref) {
                for r in extra {
                    dep_set.insert(r.to_ascii_lowercase());
                }
            }
            for rec in sourcing.records() {
                if rec
                    .depends_on_components
                    .iter()
                    .any(|c| canonical_component_ref(c) == comp_ref)
                {
                    dep_set.insert(format!(
                        "resource:{}/{}",
                        rec.namespace.to_ascii_lowercase(),
                        rec.name.to_ascii_lowercase()
                    ));
                }
            }
            let dep_list: Vec<String> = dep_set.into_iter().collect();
            let mut rels = vec![
                rel("ownedBy", owner),
                rel("partOf", &format!("system:default/{system}")),
            ];
            for dep in &dep_list {
                rels.push(rel("dependsOn", dep));
            }
            let ent = make_catalog_entity(
                "Component",
                "default",
                name,
                Some(title),
                Some(desc),
                ctype,
                "Production",
                "ok",
                "Component",
                live.any_connected,
                vec![system.to_string(), ctype.to_string()],
                json!({
                    "type": ctype,
                    "lifecycle": "production",
                    "owner": owner,
                    "system": system,
                    "dependsOn": dep_list,
                }),
                rels,
            );
            let detail = PitsEntityDetail {
                entity: ent,
                resource_type: ctype.to_string(),
                status: "Production".to_string(),
                status_level: "ok".to_string(),
                summary_line: format!(
                    "{desc} ({} upstream Resource dependencies)",
                    dep_list.len()
                ),
                refreshed_at: now,
                live_connected: live.any_connected,
                attribute_sections: vec![AttributeSection {
                    id: "component-deps".to_string(),
                    title: "Component Topology & Resource Dependencies".to_string(),
                    items: vec![
                        attr("system", "Parent System", system, Some("info"), true),
                        attr("owner", "Owner Group", owner, Some("ok"), true),
                        attr("lifecycle", "Lifecycle", "production", Some("ok"), false),
                        attr(
                            "dependsOn",
                            "Resource Dependencies",
                            dep_list.join(", "),
                            Some("info"),
                            true,
                        ),
                    ],
                }],
                actions: vec![],
                manifests: BTreeMap::new(),
                diagnostics: json!({ "system": system, "dependsOn": dep_list }),
            };
            let k = format!("default/{name}");
            ordered_refs.push(k.clone());
            details.insert(k, detail);
        }
    }

    // 1. `baremetal-host` (5): `pits-core`, `netboot-001..004`
    {
        let core_detail = build_pits_core_host_detail(live, now);
        ordered_refs.push("default/pits-core".to_string());
        details.insert("default/pits-core".to_string(), core_detail);

        for node in &bundle.nodes {
            let d = build_talos_host_detail(node, live, now);
            let k = format!("default/{}", node.device_id);
            ordered_refs.push(k.clone());
            details.insert(k, d);
        }

        // Phase 1: Auto-discover live nodes from K8s `/api/v1/nodes`, Tinkerbell `Hardware`, and Hook inventory
        for (k, d) in collect_live_discovered_nodes(live, &details, now) {
            if !details.contains_key(&k) {
                ordered_refs.push(k.clone());
                details.insert(k, d);
            }
        }
    }

    // 2. `npu-accelerator` (4): `netboot-001-npu..netboot-004-npu`
    for node in &bundle.nodes {
        let d = build_npu_detail(node, live, now);
        let k = format!("default/{}-npu", node.device_id);
        ordered_refs.push(k.clone());
        details.insert(k, d);
    }

    // 3. `virtual-machine` (2): `win11-001`, `access-lab`
    {
        let win_d = build_win11_vm_detail(live, cached_rdp, now);
        ordered_refs.push("default/win11-001".to_string());
        details.insert("default/win11-001".to_string(), win_d);

        let acc_d = build_access_lab_vm_detail(live, now);
        ordered_refs.push("default/access-lab".to_string());
        details.insert("default/access-lab".to_string(), acc_d);
    }

    // 4. `edge-router` (1): `pits-core-router`
    {
        let rtr_d = build_edge_router_detail(bundle, live, now);
        ordered_refs.push("default/pits-core-router".to_string());
        details.insert("default/pits-core-router".to_string(), rtr_d);
    }

    // 5. `pxe-boot-profile` (2): `talos-v1.14.5-netboot`, `hook-discovery-x86-64`
    {
        let talos_pxe = build_pxe_profile_detail(bundle, PxeBootMode::TalosNetboot, live, now);
        ordered_refs.push("default/talos-v1.14.5-netboot".to_string());
        details.insert("default/talos-v1.14.5-netboot".to_string(), talos_pxe);

        let hook_pxe = build_pxe_profile_detail(bundle, PxeBootMode::HookInventory, live, now);
        ordered_refs.push("default/hook-discovery-x86-64".to_string());
        details.insert("default/hook-discovery-x86-64".to_string(), hook_pxe);
    }

    // 6. `storage-pool` (1): `pits-data`
    {
        let pool_d = build_storage_pool_detail(live, now);
        ordered_refs.push("default/pits-data".to_string());
        details.insert("default/pits-data".to_string(), pool_d);
    }

    // 7. `san-volume` (9): `netboot-001-system`, ... `netboot-004-vmstore`
    for node in &bundle.nodes {
        for ns in &node.san.namespaces {
            let vol_name = format!("{}-{}", node.device_id, ns.name);
            let vol_d = build_san_volume_detail(node, ns, live, now);
            let k = format!("default/{vol_name}");
            ordered_refs.push(k.clone());
            details.insert(k, vol_d);
        }
    }

    // 8. `kubernetes-cluster` (2): `cluster-pits-core`, `cluster-pits-lab`
    {
        let c_core = build_cluster_detail(true, bundle, live, now);
        ordered_refs.push("default/cluster-pits-core".to_string());
        details.insert("default/cluster-pits-core".to_string(), c_core);

        let c_lab = build_cluster_detail(false, bundle, live, now);
        ordered_refs.push("default/cluster-pits-lab".to_string());
        details.insert("default/cluster-pits-lab".to_string(), c_lab);
    }

    // 9. Sourcing Governance Units (`vendor-saas`, `commercial-sdk`, `managed-infra`, `outsourced-dev`)
    for rec in sourcing.records() {
        let placeholder = build_sourcing_placeholder_detail(rec, live, now);
        let k = format!(
            "{}/{}",
            rec.namespace.to_ascii_lowercase(),
            rec.name.to_ascii_lowercase()
        );
        ordered_refs.push(k.clone());
        details.insert(k, placeholder);
    }

    // 10. Phase 2: Unified Crossplane (XRD/XR/Claim/Provider) & K8s Service auto-discovery
    for obj in &live.discovered_k8s_resources {
        if let Some(alias_key) = resolve_baseline_alias(obj) {
            if let Some(existing) = details.get_mut(alias_key) {
                enrich_existing_detail_from_k8s(existing, obj);
                continue;
            }
        }
        if !should_ingest_k8s_object(obj) {
            continue;
        }
        let (k, d) = build_discovered_k8s_entity_detail(obj, now);
        if !details.contains_key(&k) {
            ordered_refs.push(k.clone());
            details.insert(k, d);
        }
    }

    if let Some(ref note) = live.connection_note {
        for detail in details.values_mut() {
            if let Some(obj) = detail.diagnostics.as_object_mut() {
                obj.entry("connectionNote".to_string())
                    .or_insert_with(|| json!(note));
            }
        }
    }

    // Populate symmetric Backstage relations across all entities and sync back into details & manifests!
    populate_symmetric_relations(&ordered_refs, &mut details);

    // Finalize Sourcing Governance evaluations after symmetric dependencyOf edges are populated!
    finalize_sourcing_details(&ordered_refs, &mut details, sourcing);

    let entities: Vec<CatalogEntity> = ordered_refs
        .iter()
        .filter_map(|k| details.get(k).map(|d| d.entity.clone()))
        .collect();

    let resource_types: Vec<ResourceTypeSummary> = RESOURCE_TYPE_METAS
        .iter()
        .map(|meta| {
            let matching: Vec<&PitsEntityDetail> = details
                .values()
                .filter(|d| {
                    d.entity.kind == "Resource" && d.resource_type == meta.resource_type
                })
                .collect();
            let entity_count = matching.len();
            let healthy_count = matching
                .iter()
                .filter(|d| d.status_level == "ok")
                .count();
            let warning_count = matching
                .iter()
                .filter(|d| d.status_level == "warn" || d.status_level == "error")
                .count();
            ResourceTypeSummary {
                resource_type: meta.resource_type.to_string(),
                category: meta.category.to_string(),
                title: meta.title.to_string(),
                description: meta.description.to_string(),
                entity_count,
                healthy_count,
                warning_count,
                action_ids: canonical_action_ids_for_type(meta.resource_type),
            }
        })
        .collect();

    CatalogSnapshot {
        entities,
        details,
        resource_types,
    }
}

pub(crate) fn attr(
    key: &str,
    label: &str,
    value: impl Into<String>,
    status: Option<&str>,
    monospace: bool,
) -> AttributeItem {
    AttributeItem {
        key: key.to_string(),
        label: label.to_string(),
        value: value.into(),
        status: status.map(str::to_string),
        monospace,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn make_catalog_entity(
    kind: &str,
    namespace: &str,
    name: &str,
    title: Option<&str>,
    description: Option<&str>,
    resource_type: &str,
    status: &str,
    status_level: &str,
    category: &str,
    live_connected: bool,
    tags: Vec<String>,
    mut spec: Value,
    relations: Vec<EntityRelation>,
) -> CatalogEntity {
    let uid_key = format!("{}:{}/{}", kind.to_lowercase(), namespace, name);
    let uid = Uuid::new_v5(&Uuid::NAMESPACE_OID, uid_key.as_bytes()).to_string();

    let mut labels = BTreeMap::new();
    labels.insert("pits.io/resource-type".to_string(), resource_type.to_string());
    labels.insert("pits.io/status-level".to_string(), status_level.to_string());
    labels.insert(
        "pits.io/live-connected".to_string(),
        live_connected.to_string(),
    );

    let mut annotations = BTreeMap::new();
    annotations.insert(
        "backstage.io/managed-by-location".to_string(),
        "url:https://pitsctl.local/catalog".to_string(),
    );
    annotations.insert(
        "backstage.io/managed-by-origin-location".to_string(),
        "url:https://pitsctl.local/catalog".to_string(),
    );
    annotations.insert("pits.io/resource-type".to_string(), resource_type.to_string());
    annotations.insert("pits.io/category".to_string(), category.to_string());
    annotations.insert("pits.io/status".to_string(), status.to_string());
    annotations.insert("pits.io/status-level".to_string(), status_level.to_string());

    if kind == "Resource" {
        if let Some(obj) = spec.as_object_mut() {
            obj.insert("type".to_string(), json!(resource_type));
            obj.entry("owner".to_string())
                .or_insert_with(|| json!("group:default/operators"));
            obj.entry("system".to_string())
                .or_insert_with(|| json!("pits-infrastructure"));
            obj.entry("lifecycle".to_string()).or_insert_with(|| {
                if status == "HardwarePresentDriverPending" {
                    json!("driver-pending")
                } else {
                    json!("production")
                }
            });
            obj.insert("status".to_string(), json!(status));
            obj.insert("statusLevel".to_string(), json!(status_level));
            obj.insert("category".to_string(), json!(category));
        }
    }

    CatalogEntity {
        api_version: "backstage.io/v1alpha1".to_string(),
        kind: kind.to_string(),
        metadata: EntityMetadata {
            uid,
            namespace: namespace.to_string(),
            name: name.to_string(),
            title: title.map(str::to_string),
            description: description.map(str::to_string),
            labels,
            annotations,
            tags,
        },
        spec,
        relations,
    }
}

pub(crate) fn rel(relation_type: &str, target_ref: &str) -> EntityRelation {
    EntityRelation {
        relation_type: relation_type.to_string(),
        target_ref: target_ref.to_string(),
    }
}

fn populate_symmetric_relations(
    ordered_keys: &[String],
    details: &mut BTreeMap<String, PitsEntityDetail>,
) {
    let mut ref_by_key: BTreeMap<String, String> = BTreeMap::new();
    let mut key_by_ref: BTreeMap<String, String> = BTreeMap::new();

    for (k, d) in details.iter() {
        let full_ref = format!(
            "{}:{}/{}",
            d.entity.kind.to_lowercase(),
            d.entity.metadata.namespace.to_lowercase(),
            d.entity.metadata.name.to_lowercase()
        );
        ref_by_key.insert(k.clone(), full_ref.clone());
        key_by_ref.insert(full_ref, k.clone());
    }

    let mut rel_sets: BTreeMap<String, BTreeSet<EntityRelation>> = BTreeMap::new();
    for (k, d) in details.iter() {
        let set = rel_sets.entry(k.clone()).or_default();
        let spec_owner = d
            .entity
            .spec
            .get("owner")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("group:default/operators");
        set.insert(rel("ownedBy", &normalize_ref_string(spec_owner)));

        if d.entity.kind == "Resource" || d.entity.kind == "Component" {
            let sys_raw = d
                .entity
                .spec
                .get("system")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("pits-infrastructure");
            set.insert(rel("partOf", &canonical_system_ref(sys_raw)));
        } else if d.entity.kind == "System" {
            if let Some(dom_raw) = d
                .entity
                .spec
                .get("domain")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
            {
                let dom_ref = if dom_raw.contains(':') {
                    dom_raw.to_ascii_lowercase()
                } else {
                    format!("domain:default/{}", dom_raw.to_ascii_lowercase())
                };
                set.insert(rel("partOf", &dom_ref));
            }
        }

        if let Some(deps) = d.entity.spec.get("dependsOn").and_then(|v| v.as_array()) {
            for dep in deps {
                if let Some(dep_s) = dep.as_str() {
                    set.insert(rel("dependsOn", &normalize_ref_string(dep_s)));
                }
            }
        }

        for r in &d.entity.relations {
            set.insert(EntityRelation {
                relation_type: r.relation_type.clone(),
                target_ref: normalize_ref_string(&r.target_ref),
            });
        }
    }

    // Derive symmetric inverse edges across all catalog entities
    let snapshot_edges: Vec<(String, EntityRelation)> = rel_sets
        .iter()
        .flat_map(|(k, set)| set.iter().cloned().map(move |r| (k.clone(), r)))
        .collect();

    for (src_key, edge) in snapshot_edges {
        let Some(src_ref) = ref_by_key.get(&src_key).cloned() else {
            continue;
        };
        let Some(dst_key) = key_by_ref.get(&edge.target_ref.to_lowercase()).cloned() else {
            continue;
        };
        if let Some(inv_type) = inverse_relation_type(&edge.relation_type) {
            rel_sets
                .entry(dst_key)
                .or_default()
                .insert(rel(inv_type, &src_ref));
        }
    }

    for k in ordered_keys {
        if let Some(d) = details.get_mut(k) {
            let relations: Vec<EntityRelation> =
                rel_sets.remove(k).unwrap_or_default().into_iter().collect();
            if d.entity.kind == "Resource" || d.entity.kind == "Component" {
                let depends_on: Vec<String> = relations
                    .iter()
                    .filter(|r| r.relation_type == "dependsOn")
                    .map(|r| r.target_ref.clone())
                    .collect();
                let dependency_of: Vec<String> = relations
                    .iter()
                    .filter(|r| r.relation_type == "dependencyOf")
                    .map(|r| r.target_ref.clone())
                    .collect();
                if let Some(obj) = d.entity.spec.as_object_mut() {
                    if !depends_on.is_empty() {
                        obj.insert("dependsOn".to_string(), json!(depends_on));
                    }
                    if !dependency_of.is_empty() {
                        obj.insert("dependencyOf".to_string(), json!(dependency_of));
                    }
                }
            }
            d.entity.relations = relations;
            if !is_sourcing_resource_type(&d.resource_type) {
                if let Ok(entity_yaml) = serde_yaml::to_string(&d.entity) {
                    d.manifests
                        .insert("catalog-entity.yaml".to_string(), entity_yaml);
                }
            }
        }
    }
}

fn build_sourcing_placeholder_detail(
    rec: &SourcingResourceRecord,
    live: &LiveTelemetry,
    now: DateTime<Utc>,
) -> PitsEntityDetail {
    let sys_name = rec
        .system
        .trim()
        .strip_prefix("system:default/")
        .unwrap_or(rec.system.trim())
        .to_string();
    let owner = if rec.owner.trim().is_empty() {
        "group:default/operators".to_string()
    } else {
        rec.owner.clone()
    };
    let mut ent = make_catalog_entity(
        "Resource",
        &rec.namespace,
        &rec.name,
        Some(&rec.name),
        Some(&rec.description),
        &rec.resource_type,
        "Evaluating",
        "info",
        "Sourcing Governance",
        live.any_connected,
        vec![
            "sourcing-audit".to_string(),
            rec.resource_type.clone(),
            sys_name.clone(),
        ],
        json!({
            "type": rec.resource_type,
            "owner": owner,
            "system": sys_name,
            "sourcingAudit": rec.audit,
        }),
        vec![
            rel("ownedBy", &owner),
            rel("partOf", &canonical_system_ref(&rec.system)),
        ],
    );
    ent.metadata.annotations.insert(
        "finops.company.io/cost-allocation-id".to_string(),
        rec.cost_allocation_id.clone(),
    );
    ent.metadata.annotations.insert(
        "sourcing.company.io/adapter-abstracted".to_string(),
        if rec.adapter_abstracted {
            "true".to_string()
        } else {
            "false".to_string()
        },
    );

    PitsEntityDetail {
        entity: ent,
        resource_type: rec.resource_type.clone(),
        status: "Evaluating".to_string(),
        status_level: "info".to_string(),
        summary_line: rec.description.clone(),
        refreshed_at: now,
        live_connected: live.any_connected,
        attribute_sections: Vec::new(),
        actions: Vec::new(),
        manifests: BTreeMap::new(),
        diagnostics: json!({}),
    }
}

fn finalize_sourcing_details(
    ordered_keys: &[String],
    details: &mut BTreeMap<String, PitsEntityDetail>,
    sourcing: &SourcingStore,
) {
    let mut system_domains: BTreeMap<String, (String, String)> = default_system_domain_map()
        .into_iter()
        .map(|(k, (d, t))| (k, (d.to_string(), t.to_string())))
        .collect();
    let mut component_meta: BTreeMap<String, (String, String)> = BTreeMap::new();
    for d in details.values() {
        if d.entity.kind == "System" {
            let sys_ref = format!(
                "system:{}/{}",
                d.entity.metadata.namespace.to_ascii_lowercase(),
                d.entity.metadata.name.to_ascii_lowercase()
            );
            let dom_raw = d
                .entity
                .spec
                .get("domain")
                .and_then(|v| v.as_str())
                .unwrap_or("platform-infra");
            let dom_ref = if dom_raw.contains(':') {
                dom_raw.to_ascii_lowercase()
            } else {
                format!("domain:default/{}", dom_raw.to_ascii_lowercase())
            };
            system_domains
                .entry(sys_ref)
                .or_insert_with(|| (dom_ref, "Registered System Domain".to_string()));
        } else if d.entity.kind == "Component" {
            let comp_ref = format!(
                "component:{}/{}",
                d.entity.metadata.namespace.to_ascii_lowercase(),
                d.entity.metadata.name.to_ascii_lowercase()
            );
            let title = d
                .entity
                .metadata
                .title
                .clone()
                .unwrap_or_else(|| d.entity.metadata.name.clone());
            let sys_raw = d
                .entity
                .spec
                .get("system")
                .and_then(|v| v.as_str())
                .unwrap_or("pits-infrastructure");
            component_meta.insert(comp_ref, (title, canonical_system_ref(sys_raw)));
        }
    }

    let today = today_utc_date();
    for k in ordered_keys {
        let Some(rec) = sourcing
            .get(k.split_once('/').map(|(_, nm)| nm).unwrap_or(k))
            .cloned()
        else {
            continue;
        };
        let Some(dep_refs) = details.get(k).map(|d| {
            d.entity
                .relations
                .iter()
                .filter(|r| {
                    r.relation_type == "dependencyOf"
                        && r.target_ref.starts_with("component:")
                })
                .map(|r| r.target_ref.to_ascii_lowercase())
                .collect::<Vec<String>>()
        }) else {
            continue;
        };

        let lineage: Vec<DomainLineageEntry> = dep_refs
            .into_iter()
            .map(|c_ref| {
                let (comp_title, sys_ref) = component_meta.get(&c_ref).cloned().unwrap_or_else(|| {
                    (c_ref.clone(), canonical_system_ref(&rec.system))
                });
                let (dom_ref, dom_tier) = system_domains.get(&sys_ref).cloned().unwrap_or_else(|| {
                    (
                        "domain:default/platform-infra".to_string(),
                        "Platform Infrastructure Domain".to_string(),
                    )
                });
                DomainLineageEntry {
                    component_ref: c_ref,
                    component_title: comp_title,
                    system_ref: sys_ref,
                    domain_ref: dom_ref,
                    domain_tier: dom_tier,
                }
            })
            .collect();

        let mut eval = evaluate_sourcing_audit_at(&rec, lineage, &system_domains, today);
        sourcing.enrich_evaluation(&mut eval);
        let status_level = if eval.facts.windmill_mismatch {
            "error"
        } else if eval.facts.re_audit_required || eval.facts.in_critical_zone {
            "warn"
        } else {
            "ok"
        };
        let status_text = if eval.facts.windmill_mismatch {
            format!("WindmillMismatch · {}", eval.facts.quadrant)
        } else if eval.facts.re_audit_required {
            format!("ReAuditDue · {}", eval.facts.quadrant)
        } else if eval.facts.in_critical_zone {
            format!("CriticalZone · {}", eval.facts.quadrant)
        } else {
            format!("Aligned · {}", eval.facts.quadrant)
        };

        let actions = actions_for_sourcing_resource(&rec, &eval);
        let catalog_info_yaml = render_catalog_info_yaml(&rec, Some(&eval.facts));
        let checks_json = serde_json::to_string_pretty(&json!({
            "entityRef": eval.entity_ref,
            "retriever": "sourcing-kraljic-windmill-retriever",
            "facts": eval.facts,
            "checks": eval.checks,
            "lineage": eval.lineage,
        }))
        .unwrap_or_default();

        if let Some(detail) = details.get_mut(k) {
            detail.status = status_text.clone();
            detail.status_level = status_level.to_string();
            detail.summary_line = format!(
                "{} — Y={:.2}, X={:.2}, quadrant='{}', vendor='{}' ({}), prodDeps={}",
                rec.description,
                eval.facts.final_y_score,
                eval.facts.final_x_score,
                eval.facts.quadrant,
                rec.audit.contract_meta.vendor_name,
                rec.audit.supplier_preference.perceived_status,
                eval.facts.prod_dependents
            );

            let labels = &mut detail.entity.metadata.labels;
            labels.insert(
                "sourcing.company.io/quadrant".to_string(),
                eval.facts.quadrant.clone(),
            );
            labels.insert(
                "sourcing.company.io/windmill-mismatch".to_string(),
                eval.facts.windmill_mismatch.to_string(),
            );
            labels.insert(
                "sourcing.company.io/knockout".to_string(),
                eval.facts.knockout_triggered.to_string(),
            );
            labels.insert(
                "sourcing.company.io/critical-zone".to_string(),
                eval.facts.in_critical_zone.to_string(),
            );
            labels.insert(
                "sourcing.company.io/reaudit-required".to_string(),
                eval.facts.re_audit_required.to_string(),
            );
            labels.insert("pits.io/status-level".to_string(), status_level.to_string());

            let anns = &mut detail.entity.metadata.annotations;
            anns.insert("pits.io/status".to_string(), status_text.clone());
            anns.insert("pits.io/status-level".to_string(), status_level.to_string());
            anns.insert(
                "sourcing.company.io/resolved-domain".to_string(),
                eval.resolved_domain.clone(),
            );

            if !detail.entity.metadata.tags.contains(&eval.facts.quadrant) {
                detail.entity.metadata.tags.push(eval.facts.quadrant.clone());
            }

            if let Some(obj) = detail.entity.spec.as_object_mut() {
                obj.insert("status".to_string(), json!(status_text));
                obj.insert("statusLevel".to_string(), json!(status_level));
                obj.insert("sourcingFacts".to_string(), json!(eval.facts));
            }

            detail.attribute_sections = vec![
                AttributeSection {
                    id: "kraljic-windmill-scores".to_string(),
                    title: "Kraljic × Dutch Windmill Audit".to_string(),
                    items: vec![
                        attr(
                            "finalYScore",
                            "Business Value Impact (Y)",
                            format!("{:.2} (raw {:.2})", eval.facts.final_y_score, eval.facts.breakdown.raw_y_score),
                            Some(if eval.facts.final_y_score >= 3.0 { "info" } else { "ok" }),
                            true,
                        ),
                        attr(
                            "finalXScore",
                            "Supply Lock-In Risk (X)",
                            format!("{:.2} (raw {:.2})", eval.facts.final_x_score, eval.facts.breakdown.raw_x_score),
                            Some(if eval.facts.final_x_score >= 3.0 { "warn" } else { "ok" }),
                            true,
                        ),
                        attr(
                            "quadrant",
                            "Kraljic Quadrant",
                            eval.facts.quadrant.clone(),
                            Some(if eval.facts.quadrant == "strategic" || eval.facts.quadrant == "bottleneck" { "warn" } else { "ok" }),
                            true,
                        ),
                        attr(
                            "knockoutTriggered",
                            "5-Point Redline Knockout",
                            if eval.facts.knockout_triggered {
                                format!("TRIGGERED ({})", eval.facts.knockout_dimensions.join(", "))
                            } else {
                                "None".to_string()
                            },
                            Some(if eval.facts.knockout_triggered { "warn" } else { "ok" }),
                            false,
                        ),
                        attr(
                            "inCriticalZone",
                            "Critical Zone [2.70–3.30]",
                            eval.facts.in_critical_zone.to_string(),
                            Some(if eval.facts.in_critical_zone { "warn" } else { "ok" }),
                            true,
                        ),
                        attr(
                            "perceivedStatus",
                            "Supplier Perceived Status",
                            format!(
                                "{} (wallet {}, attr {:.1})",
                                rec.audit.supplier_preference.perceived_status,
                                rec.audit.supplier_preference.estimated_wallet_share_tier,
                                rec.audit.supplier_preference.account_attractiveness_score
                            ),
                            Some(if eval.facts.windmill_mismatch { "error" } else { "ok" }),
                            false,
                        ),
                        attr(
                            "windmillMismatch",
                            "Windmill Mismatch Redline",
                            eval.facts.windmill_mismatch.to_string(),
                            Some(if eval.facts.windmill_mismatch { "error" } else { "ok" }),
                            true,
                        ),
                        attr(
                            "reAuditRequired",
                            "Dynamic Re-Audit Trigger",
                            if eval.facts.re_audit_required {
                                eval.facts.re_audit_reasons.join("; ")
                            } else {
                                "Clear".to_string()
                            },
                            Some(if eval.facts.re_audit_required { "warn" } else { "ok" }),
                            false,
                        ),
                    ],
                },
                AttributeSection {
                    id: "eight-dimension-breakdown".to_string(),
                    title: "Eight-Dimension Weight Breakdown".to_string(),
                    items: vec![
                        attr("y1", "Y1 Blast Radius (35% · Auto)", format!("{:.2}", eval.facts.breakdown.y1_blast_radius), Some("info"), true),
                        attr("y2", "Y2 Core Competitiveness (25%)", format!("{:.2}", eval.facts.breakdown.y2_core_competitiveness), None, true),
                        attr("y3", "Y3 FinOps TCO Ratio (25% · Auto)", format!("{:.2}", eval.facts.breakdown.y3_tco_ratio), Some("info"), true),
                        attr("y4", "Y4 Price Elasticity (15%)", format!("{:.2}", eval.facts.breakdown.y4_price_elasticity), None, true),
                        attr("x1", "X1 Adapter Lock-in (35% · Auto)", format!("{:.2}", eval.facts.breakdown.x1_lock_in), Some(if eval.facts.breakdown.x1_lock_in >= 5.0 { "warn" } else { "info" }), true),
                        attr("x2", "X2 Market Scarcity (25%)", format!("{:.2}", eval.facts.breakdown.x2_market_scarcity), None, true),
                        attr("x3", "X3 Customization Depth (25%)", format!("{:.2}", eval.facts.breakdown.x3_customization_depth), None, true),
                        attr("x4", "X4 Compliance Fragility (15%)", format!("{:.2}", eval.facts.breakdown.x4_compliance_fragility), None, true),
                    ],
                },
                AttributeSection {
                    id: "contract-and-topology".to_string(),
                    title: "Contract & 4-Layer Domain Lineage".to_string(),
                    items: vec![
                        attr("vendorName", "Vendor", rec.audit.contract_meta.vendor_name.clone(), None, false),
                        attr("costAllocationId", "FinOps Cost Center ID", rec.cost_allocation_id.clone(), Some("info"), true),
                        attr("adapterAbstracted", "Anti-Corruption Adapter", rec.adapter_abstracted.to_string(), Some(if rec.adapter_abstracted { "ok" } else { "warn" }), true),
                        attr(
                            "prodDependents",
                            "Production Components",
                            format!("{} active / {} baseline", eval.facts.prod_dependents, rec.audit.contract_meta.baseline_prod_dependents),
                            Some(if eval.facts.prod_dependents >= rec.audit.contract_meta.baseline_prod_dependents.max(1) * 2 { "warn" } else { "info" }),
                            true,
                        ),
                        attr(
                            "renewalDate",
                            "Renewal Date & SLA Window",
                            format!("{} ({}d remaining)", rec.audit.contract_meta.renewal_date, eval.facts.days_to_renewal),
                            Some(if eval.facts.days_to_renewal <= 180 { "warn" } else { "ok" }),
                            true,
                        ),
                        attr(
                            "domainLineage",
                            "Resolved Domain Lineage",
                            format!("{} -> {} ({})", rec.system, eval.resolved_domain, eval.resolved_domain_tier),
                            Some("info"),
                            true,
                        ),
                    ],
                },
            ];

            detail.actions = actions;
            detail
                .manifests
                .insert("catalog-info.yaml".to_string(), catalog_info_yaml);
            detail
                .manifests
                .insert("tech-insights-scorecard.json".to_string(), checks_json);
            if let Ok(entity_yaml) = serde_yaml::to_string(&detail.entity) {
                detail
                    .manifests
                    .insert("catalog-entity.yaml".to_string(), entity_yaml);
            }
            detail.diagnostics = serde_json::to_value(&eval).unwrap_or_else(|_| json!({}));
        }
    }
}

fn inverse_relation_type(rel_type: &str) -> Option<&'static str> {
    match rel_type {
        "dependsOn" => Some("dependencyOf"),
        "dependencyOf" => Some("dependsOn"),
        "partOf" => Some("hasPart"),
        "hasPart" => Some("partOf"),
        "ownedBy" => Some("ownerOf"),
        "ownerOf" => Some("ownedBy"),
        _ => None,
    }
}

fn normalize_ref_string(raw: &str) -> String {
    let (k, ns, nm) = parse_entity_ref(raw);
    format!(
        "{}:{}/{}",
        k.to_lowercase(),
        ns.to_lowercase(),
        nm.to_lowercase()
    )
}

pub(crate) fn parse_entity_ref(raw: &str) -> (String, String, String) {
    let trimmed = raw.trim();
    if let Some((kind_part, rest)) = trimmed.split_once(':') {
        if let Some((ns_part, name_part)) = rest.split_once('/') {
            (
                kind_part.trim().to_string(),
                ns_part.trim().to_string(),
                name_part.trim().to_string(),
            )
        } else {
            (
                kind_part.trim().to_string(),
                "default".to_string(),
                rest.trim().to_string(),
            )
        }
    } else if let Some((ns_part, name_part)) = trimmed.split_once('/') {
        (
            "Resource".to_string(),
            ns_part.trim().to_string(),
            name_part.trim().to_string(),
        )
    } else {
        (
            "Resource".to_string(),
            "default".to_string(),
            trimmed.to_string(),
        )
    }
}

fn build_pits_core_host_detail(live: &LiveTelemetry, now: DateTime<Utc>) -> PitsEntityDetail {
    let host_audit = live
        .router_live
        .as_ref()
        .and_then(|r| r.host_audit.clone());
    let agent_online = live.agent_health.is_some() || host_audit.is_some();
    let core_k8s_node = live
        .core_cluster
        .as_ref()
        .and_then(|c| c.nodes.iter().find(|n| n.name == "pits-core"));

    let mgmt_ip = host_audit
        .as_ref()
        .and_then(|h| h.get("managementAddress"))
        .and_then(|v| v.as_str())
        .unwrap_or("192.168.71.35/24")
        .to_string();
    let mgmt_pci = host_audit
        .as_ref()
        .and_then(|h| h.get("managementPci"))
        .and_then(|v| v.as_str())
        .unwrap_or(MGMT0_PCI)
        .to_string();
    let logical_cpus = host_audit
        .as_ref()
        .and_then(|h| h.get("logicalCpus"))
        .and_then(|v| v.as_u64())
        .unwrap_or(16);
    let hugepages = host_audit
        .as_ref()
        .and_then(|h| h.get("hugepages2Mi"))
        .and_then(|v| v.as_i64())
        .unwrap_or(2048);
    let agent_ver = live
        .agent_health
        .as_ref()
        .and_then(|h| h.get("version"))
        .and_then(|v| v.as_str())
        .unwrap_or("v1.0.0");
    let kubelet_ver = core_k8s_node
        .map(|n| n.kubelet_version.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("v1.35.8+rke2r1");

    let live_conn = agent_online || core_k8s_node.is_some();
    let status = if live_conn { "Healthy" } else { "BaselineReady" };
    let summary = format!(
        "Minisforum MS-01 (Venus Series / AHWSA) · 12th Gen Core i5-12600H · NixOS RKE2 ({kubelet_ver}) · mgmt0 ({mgmt_pci}) Protected"
    );

    let entity = make_catalog_entity(
        "Resource",
        "default",
        "pits-core",
        Some("pits-core (Minisforum MS-01 Core Host)"),
        Some(&summary),
        "baremetal-host",
        status,
        "ok",
        "Compute & Accelerators",
        live_conn,
        vec![
            "baremetal".to_string(),
            "nixos".to_string(),
            "rke2".to_string(),
            "minisforum".to_string(),
            "core-plane".to_string(),
        ],
        json!({
            "role": "core-infrastructure-host",
            "cluster": "cluster-pits-core",
            "retailModel": "Minisforum MS-01 (Venus Series / AHWSA)",
            "cpuModel": "12th Gen Intel(R) Core(TM) i5-12600H",
            "managementIp": mgmt_ip,
            "tailscaleIp": "100.74.25.42",
            "protectedMgmtPci": MGMT0_PCI,
            "os": "NixOS + RKE2",
        }),
        vec![rel("partOf", "resource:default/cluster-pits-core")],
    );

    let sections = vec![
        AttributeSection {
            id: "hardware-identity".to_string(),
            title: "Hardware & Processor Identity".to_string(),
            items: vec![
                attr("retailModel", "Retail Machine Model", "Minisforum MS-01 (Venus Series / AHWSA)", Some("ok"), false),
                attr("cpuModel", "Processor", format!("12th Gen Intel(R) Core(TM) i5-12600H (12C/{logical_cpus}T)"), None, false),
                attr("npuExpectation", "Integrated NPU", "None (Alder Lake-P pre-dates Intel AI Boost; 8086:463e is Thunderbolt 4 NHI)", Some("info"), false),
                attr("hugepages2Mi", "2MiB Hugepages", hugepages.to_string(), Some("ok"), true),
            ],
        },
        AttributeSection {
            id: "network-pci-guard".to_string(),
            title: "Network & PCI Isolation Guard".to_string(),
            items: vec![
                attr("managementIp", "Management Address (mgmt0)", &mgmt_ip, Some("ok"), true),
                attr("tailscaleAddress", "Tailscale Zero-Trust Endpoint", "100.74.25.42:18443", Some("ok"), true),
                attr("mgmt0Guard", "mgmt0 PCI Guard", format!("{mgmt_pci} (Protected Host NIC — Never VFIO)"), Some("ok"), true),
                attr("routerDpdkPcis", "DPDK WAN / LAN VF0 PCIs", format!("{WAN_ROUTER_PCI} / {LAN_TRUNK_VF_PCI}"), None, true),
            ],
        },
        AttributeSection {
            id: "os-agent-runtime".to_string(),
            title: "NixOS & Control-Plane Runtime".to_string(),
            items: vec![
                attr("osDistribution", "Operating System", "NixOS (Declarative Flake PITS-B1)", Some("ok"), false),
                attr("kubeletVersion", "RKE2 Kubelet Version", kubelet_ver, Some("ok"), true),
                attr("pitsAgent", "Go pits-agent Daemon", if agent_online { format!("Online ({agent_ver}, mTLS TLS 1.3)") } else { "Offline (Baseline Profile)".to_string() }, Some(if agent_online { "ok" } else { "info" }), false),
            ],
        },
    ];

    PitsEntityDetail {
        entity,
        resource_type: "baremetal-host".to_string(),
        status: status.to_string(),
        status_level: "ok".to_string(),
        summary_line: summary,
        refreshed_at: now,
        live_connected: live_conn,
        attribute_sections: sections,
        actions: actions_for_pits_core_host(),
        manifests: BTreeMap::new(),
        diagnostics: json!({
            "agentHealth": live.agent_health,
            "hostAudit": host_audit,
            "k8sNode": core_k8s_node,
        }),
    }
}

fn build_talos_host_detail(
    node: &EnrolledNodeContract,
    live: &LiveTelemetry,
    now: DateTime<Utc>,
) -> PitsEntityDetail {
    let talos_st = live.talos_by_device.get(&node.device_id);
    let talos_online = talos_st.map(|t| t.reachable).unwrap_or(false);
    let hook_inv = live
        .hook_inventory
        .get(&node.device_id)
        .and_then(|raw| analyze_hook_inventory(raw).ok());
    let k8s_node = live
        .lab_cluster
        .as_ref()
        .and_then(|c| c.nodes.iter().find(|n| n.name == node.device_id));

    let hw = talos_st.and_then(|t| t.hardware.as_ref());
    let retail_model = if let Some(h) = hw {
        resolve_retail_machine_model(
            h.manufacturer.as_deref(),
            h.product_name.as_deref(),
            hook_inv
                .as_ref()
                .and_then(|i| i.board_serial.as_deref())
                .or(h.sku_number.as_deref()),
            h.cpu_model.as_deref().or(Some(&node.cpu_model)),
        )
    } else if let Some(ref inv) = hook_inv {
        inv.resolved_model.clone()
    } else {
        node.model.clone()
    };

    let cpu_model = hw
        .and_then(|h| h.cpu_model.clone())
        .or_else(|| hook_inv.as_ref().and_then(|i| i.cpu_model.clone()))
        .unwrap_or_else(|| node.cpu_model.clone());
    let product_uuid = hw
        .and_then(|h| h.uuid.clone())
        .unwrap_or_else(|| node.product_uuid.clone());
    let bios_version = hw
        .and_then(|h| h.bios_version.clone())
        .unwrap_or_else(|| "UEFI Validated".to_string());

    let expected_wwids = allowed_wwids_for_node(node);
    let cosi_wwids = talos_st
        .map(|t| t.cosi_disk_wwids.clone())
        .unwrap_or_default();
    let wwids_verified_via_cosi = !cosi_wwids.is_empty()
        && expected_wwids
            .iter()
            .all(|w| cosi_wwids.iter().any(|cw| cw == w));
    let san_healthy = if !cosi_wwids.is_empty() {
        wwids_verified_via_cosi
    } else {
        live.san_by_device.contains_key(&node.device_id)
    };

    let talos_ver = talos_st
        .and_then(|t| t.talos_tag.as_deref())
        .unwrap_or(TALOS_VERSION);

    let tink_mode = live
        .tink_hardware
        .as_ref()
        .and_then(|hw_list| hw_list.get("items"))
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find(|it| {
                it.pointer("/metadata/name").and_then(|v| v.as_str())
                    == Some(node.device_id.as_str())
            })
        })
        .and_then(|it| {
            if it
                .pointer("/spec/interfaces/0/netboot/allowWorkflow")
                .and_then(|v| v.as_bool())
                == Some(true)
            {
                Some("hook-inventory")
            } else {
                Some("talos-netboot")
            }
        })
        .unwrap_or("talos-netboot");

    let live_conn = talos_online || k8s_node.is_some();
    let (status, status_level) = if talos_online {
        if san_healthy && k8s_node.map(|n| n.ready).unwrap_or(true) {
            ("Healthy", "ok")
        } else {
            ("DegradedSanOrKubelet", "warn")
        }
    } else if k8s_node.map(|n| n.ready).unwrap_or(false) {
        ("Ready", "ok")
    } else if live.any_connected {
        ("Unreachable", "warn")
    } else {
        ("BaselineReady", "ok")
    };

    let summary = format!(
        "{retail_model} · {cpu_model} · {} · Diskless Talos {talos_ver} ({} SAN WWIDs)",
        node.node_ip,
        expected_wwids.len()
    );

    let mut relations = vec![
        rel("partOf", "resource:default/cluster-pits-lab"),
        rel("hasPart", &format!("resource:default/{}-npu", node.device_id)),
        rel(
            "dependsOn",
            if tink_mode == "hook-inventory" {
                "resource:default/hook-discovery-x86-64"
            } else {
                "resource:default/talos-v1.14.5-netboot"
            },
        ),
        rel("dependsOn", "resource:default/pits-core-router"),
    ];
    for ns in &node.san.namespaces {
        relations.push(rel(
            "dependsOn",
            &format!("resource:default/{}-{}", node.device_id, ns.name),
        ));
    }

    let rendered_talos_yaml =
        render_talos_node_config(node, "https://10.89.20.21:6443").unwrap_or_default();
    let imager_yaml = render_talos_imager_profile(node, true);
    let ipxe_script = render_ipxe_script(node, PxeBootMode::TalosNetboot, None);
    let tink_hw_yaml = render_tinkerbell_hardware_yaml(node, PxeBootMode::TalosNetboot, None);

    let mut manifests = BTreeMap::new();
    manifests.insert("talos-machineconfig.yaml".to_string(), rendered_talos_yaml.clone());
    manifests.insert("sidero-imager-profile.yaml".to_string(), imager_yaml);
    manifests.insert("tinkerbell-hardware.yaml".to_string(), tink_hw_yaml);
    manifests.insert("boot.ipxe".to_string(), ipxe_script);

    let running_svcs = talos_st
        .map(|t| {
            let ok = t.services.iter().filter(|s| s.state == "Running").count();
            format!("{ok}/{} Running", t.services.len())
        })
        .unwrap_or_else(|| "Baseline (apid/machined/kubelet/etcd)".to_string());

    let ucode_desc = if node.accelerator.is_some() {
        "intel-ucode + nvidia-open-gpu-kernel-modules + pits-nvme-early"
    } else if node.cpu_vendor == "amd" {
        "amd-ucode + realtek-firmware + pits-nvme-early"
    } else if node.pxe_mac.starts_with("58:47:ca:") {
        "intel-ucode + intel-npu + realtek-firmware + pits-nvme-early"
    } else {
        "intel-ucode + intel-npu + pits-nvme-early"
    };

    let sections = vec![
        AttributeSection {
            id: "hardware-identity".to_string(),
            title: "Hardware & Processor Identity".to_string(),
            items: vec![
                attr("retailModel", "Retail Machine Model", &retail_model, Some("ok"), false),
                attr("productUuid", "DMI Product UUID", &product_uuid, None, true),
                attr("biosVersion", "UEFI BIOS Version", &bios_version, None, true),
                attr("cpuModel", "Processor", &cpu_model, None, false),
                attr("systemExtensions", "Imager System Extensions", ucode_desc, None, true),
                attr("vfioIsolatedNvme", "VFIO Isolated Local PCI", node.approved_vfio_pci.join(", "), Some("ok"), true),
            ],
        },
        AttributeSection {
            id: "network-pxe".to_string(),
            title: "VLAN 20 Network & Tinkerbell PXE".to_string(),
            items: vec![
                attr("nodeIp", "Lab VLAN 20 IP", &node.node_ip, Some("ok"), true),
                attr("pxeMac", "Enrolled PXE MAC", &node.pxe_mac, None, true),
                attr("sanTargetPortal", "Dedicated NVMe/TCP Target", format!("{}:{}", node.target_ip, node.san.port), Some("ok"), true),
                attr("tinkerbellBootMode", "Active PXE Boot Mode", tink_mode, Some("ok"), true),
            ],
        },
        AttributeSection {
            id: "os-cosi-runtime".to_string(),
            title: "Diskless Talos Runtime & COSI SAN WWIDs".to_string(),
            items: vec![
                attr("talosVersion", "Talos OS Version", talos_ver, Some("ok"), true),
                attr("talosServices", "System Services", running_svcs, Some(if talos_online { "ok" } else { "info" }), false),
                attr("expectedWwids", "Contract SAN WWIDs", expected_wwids.join(", "), Some("ok"), true),
                attr(
                    "cosiObservedWwids",
                    "Live COSI Disks.block.talos.dev",
                    if !cosi_wwids.is_empty() {
                        cosi_wwids.join(", ")
                    } else if live.san_by_device.contains_key(&node.device_id) {
                        "SAN Target Exported (COSI WWID verified on apply)".to_string()
                    } else {
                        "Offline (Verified on apply via COSI gRPC)".to_string()
                    },
                    Some(if wwids_verified_via_cosi { "ok" } else { "info" }),
                    true,
                ),
                attr("netbootGuard", "netboot-guard Policy", "Compliant (machine.install & DiskWipeConfig forbidden)", Some("ok"), false),
            ],
        },
    ];

    let entity = make_catalog_entity(
        "Resource",
        "default",
        &node.device_id,
        Some(&format!("{} ({})", node.device_id, retail_model)),
        Some(&summary),
        "baremetal-host",
        status,
        status_level,
        "Compute & Accelerators",
        live_conn,
        vec![
            "baremetal".to_string(),
            "talos".to_string(),
            "diskless".to_string(),
            node.cpu_vendor.clone(),
            "lab-vlan20".to_string(),
        ],
        json!({
            "role": "diskless-talos-controlplane-worker",
            "cluster": "cluster-pits-lab",
            "retailModel": retail_model,
            "cpuVendor": node.cpu_vendor,
            "cpuModel": cpu_model,
            "nodeIp": node.node_ip,
            "sanTargetIp": node.target_ip,
            "pxeMac": node.pxe_mac,
            "productUuid": product_uuid,
            "expectedWwids": expected_wwids,
            "cosiObservedWwids": cosi_wwids,
        }),
        relations,
    );

    PitsEntityDetail {
        entity,
        resource_type: "baremetal-host".to_string(),
        status: status.to_string(),
        status_level: status_level.to_string(),
        summary_line: summary,
        refreshed_at: now,
        live_connected: live_conn,
        attribute_sections: sections,
        actions: actions_for_talos_host(node, &rendered_talos_yaml),
        manifests,
        diagnostics: json!({
            "talosLive": talos_st,
            "hookInventoryAnalysis": hook_inv,
            "k8sNode": k8s_node,
            "pxeAssetVerify": live.pxe_asset_verify.get(&node.device_id),
        }),
    }
}

fn build_npu_detail(
    node: &EnrolledNodeContract,
    live: &LiveTelemetry,
    now: DateTime<Utc>,
) -> PitsEntityDetail {
    let entity_name = format!("{}-npu", node.device_id);
    let talos_st = live.talos_by_device.get(&node.device_id);
    let hook_inv = live
        .hook_inventory
        .get(&node.device_id)
        .and_then(|raw| analyze_hook_inventory(raw).ok());

    let hw = talos_st.and_then(|t| t.hardware.as_ref());
    let retail_model = if let Some(h) = hw {
        resolve_retail_machine_model(
            h.manufacturer.as_deref(),
            h.product_name.as_deref(),
            hook_inv
                .as_ref()
                .and_then(|i| i.board_serial.as_deref())
                .or(h.sku_number.as_deref()),
            h.cpu_model.as_deref().or(Some(&node.cpu_model)),
        )
    } else if let Some(ref inv) = hook_inv {
        inv.resolved_model.clone()
    } else {
        node.model.clone()
    };

    let cpu_model = hw
        .and_then(|h| h.cpu_model.clone())
        .or_else(|| hook_inv.as_ref().and_then(|i| i.cpu_model.clone()))
        .unwrap_or_else(|| node.cpu_model.clone());
    let cpu_exp = expected_npu_for_cpu(&cpu_model);

    let lab_reachable = live
        .lab_cluster
        .as_ref()
        .map(|c| c.reachable)
        .unwrap_or(false);
    let matching_slice = live
        .lab_cluster
        .as_ref()
        .and_then(|c| c.npu_slices.iter().find(|s| s.node_name == node.device_id));
    let dra_published = matching_slice
        .map(|s| !s.device_details.is_empty())
        .unwrap_or(false);

    // Synthesize baseline PCI NPU/GPU when offline, overlaid by live COSI or Hook PCI telemetry when available!
    let baseline_npu: DetectedNpuHardware = if let Some(ref acc) = node.accelerator {
        let (vendor_id, device_id) = acc.pci_id.split_once(':').unwrap_or(("10de", "2b85"));
        let vendor = if acc.pci_id.starts_with("10de") || acc.kernel_driver == "nvidia" {
            "nvidia"
        } else {
            acc.kernel_driver.as_str()
        };
        DetectedNpuHardware {
            vendor: vendor.to_string(),
            vendor_id: vendor_id.to_string(),
            device_id: device_id.to_string(),
            pci_id: acc.pci_id.clone(),
            pci_address: acc.pci_address.clone(),
            pci_class: Some("0300".to_string()),
            architecture: if acc.architecture.is_empty() {
                acc.model.clone()
            } else {
                format!("{} ({})", acc.model, acc.architecture)
            },
            expected_kernel_driver: acc.kernel_driver.clone(),
            expected_device_class: acc.dra_device_class.clone(),
            bound_driver: Some(acc.kernel_driver.clone()),
            driver_bound: true,
        }
    } else if node.npu_vendor.as_deref() == Some("amd") {
        classify_pci_npu("0000:c6:00.1", "1022", "1502", Some("0x1180"), None).unwrap()
    } else {
        classify_pci_npu(
            "0000:00:0b.0",
            "8086",
            "7d1d",
            Some("0x1200"),
            Some("intel_vpu"),
        )
        .unwrap()
    };

    let dra_driver_name = node
        .accelerator
        .as_ref()
        .and_then(|a| a.dra_driver.as_deref())
        .unwrap_or(PITS_NPU_DRA_DRIVER);

    let mut npu = if let Some(talos_npu) = talos_st
        .filter(|t| t.reachable)
        .and_then(|t| t.physical_npus.first().cloned())
    {
        talos_npu
    } else if let Some(mut hook_npu) = hook_inv
        .as_ref()
        .and_then(|h| h.npus_detected.first().cloned())
    {
        // Historical Tinkerbell Hook RAM-boot discovery runs without `intel_vpu` loaded;
        // preserve PCI BDF/device identity from Hook without falsely overriding Talos driver state.
        if !hook_npu.driver_bound && baseline_npu.driver_bound {
            hook_npu.driver_bound = true;
            hook_npu.bound_driver = baseline_npu.bound_driver.clone();
        }
        hook_npu
    } else {
        baseline_npu
    };

    // Match `k8s::audit_fleet_npus`: a live DRA v1 ResourceSlice with published devices is
    // authoritative proof that the kernel accelerator driver is bound on the Talos host.
    if dra_published {
        npu.driver_bound = true;
        npu.bound_driver = matching_slice
            .and_then(|s| s.device_details.first())
            .and_then(|d| d.kernel_driver.clone())
            .or_else(|| npu.bound_driver.clone())
            .or_else(|| Some(npu.expected_kernel_driver.clone()));
    }

    let live_conn = talos_st.map(|t| t.reachable).unwrap_or(false) || lab_reachable;

    let (status, status_level) = if dra_published {
        ("ReadyDraV1", "ok")
    } else if !npu.driver_bound {
        ("HardwarePresentDriverPending", "warn")
    } else if lab_reachable {
        ("DriverBoundSliceMissing", "warn")
    } else {
        ("ReadyDraV1", "ok")
    };

    let slice_display = matching_slice
        .map(|s| s.slice_name.clone())
        .unwrap_or_else(|| {
            if npu.driver_bound {
                format!("{}-npu-slice (baseline)", node.device_id)
            } else {
                "None (0 published ResourceSlices — driver unbound)".to_string()
            }
        });

    let bound_drv_display = match &npu.bound_driver {
        Some(d) => d.clone(),
        None if npu.expected_kernel_driver == "amdxdna" => format!(
            "unbound ('{}' absent in Talos kernel — missing CONFIG_DRM_ACCEL_AMDXDNA)",
            npu.expected_kernel_driver
        ),
        None => format!(
            "unbound ('{}' kernel driver not bound on Talos host)",
            npu.expected_kernel_driver
        ),
    };

    let summary = if status == "HardwarePresentDriverPending" {
        format!(
            "{} [{} @ {}] on {} ({retail_model}) — HardwarePresentDriverPending ('{}' unbound)",
            npu.architecture, npu.pci_id, npu.pci_address, node.device_id, npu.expected_kernel_driver
        )
    } else {
        format!(
            "{} [{} @ {}] on {} ({retail_model}) — {} ({})",
            npu.architecture, npu.pci_id, npu.pci_address, node.device_id, status, npu.expected_device_class
        )
    };

    let claim_yaml = render_npu_validation_manifests(
        "default",
        &format!("npu-claim-{}", node.device_id),
        &format!("npu-verify-{}", node.device_id),
        &npu.expected_device_class,
        Some(&node.device_id),
    );
    let mut manifests = BTreeMap::new();
    manifests.insert("dra-openvino-validation.yaml".to_string(), claim_yaml);

    let mut drv_items = vec![
        attr("expectedDriver", "Expected Kernel Driver", &npu.expected_kernel_driver, None, true),
        attr(
            "boundDriver",
            "Observed Bound Driver",
            &bound_drv_display,
            Some(if npu.driver_bound { "ok" } else { "warn" }),
            true,
        ),
        attr("draDriver", "Kubernetes DRA v1 Driver", dra_driver_name, None, true),
        attr("deviceClass", "DRA DeviceClass", &npu.expected_device_class, Some(if npu.driver_bound { "ok" } else { "info" }), true),
        attr(
            "resourceSlice",
            "Published ResourceSlice",
            &slice_display,
            Some(if status == "ReadyDraV1" { "ok" } else { "warn" }),
            true,
        ),
    ];
    if status == "HardwarePresentDriverPending" {
        let blocker_msg = if npu.expected_kernel_driver == "amdxdna" {
            "Talos 6.18.58-talos enforces module.sig_enforce=1 without CONFIG_DRM_ACCEL_AMDXDNA; /dev/accel/accel* is absent so amdxdna cannot bind.".to_string()
        } else {
            format!(
                "Kernel driver '{}' is not currently bound to {} ({}) on Talos host {}; /dev/accel/accel* is unavailable.",
                npu.expected_kernel_driver, npu.pci_id, npu.pci_address, node.device_id
            )
        };
        drv_items.push(attr(
            "driverBlocker",
            "Kernel Precondition Root Cause",
            blocker_msg,
            Some("warn"),
            false,
        ));
    }

    let sections = vec![
        AttributeSection {
            id: "pci-bus-identity".to_string(),
            title: "Physical PCI Bus & Host Identity".to_string(),
            items: vec![
                attr("hostNode", "Host Bare-Metal Node", &node.device_id, Some("ok"), true),
                attr("hostRetailModel", "Host Retail Model", &retail_model, None, false),
                attr("hostCpuModel", "Host Processor", &cpu_model, None, false),
                attr("vendor", "Accelerator Vendor", &npu.vendor, None, true),
                attr("pciId", "PCI Vendor:Device ID", &npu.pci_id, Some("ok"), true),
                attr("pciAddress", "PCI BDF Address", &npu.pci_address, None, true),
                attr("architecture", "NPU Microarchitecture", &npu.architecture, None, false),
                attr("cpuSpecMatch", "CPU ↔ PCI Hardware Consistency", "Verified (Matches CPU Silicon Spec)", Some("ok"), false),
            ],
        },
        AttributeSection {
            id: "driver-dra-layer".to_string(),
            title: "Kernel Driver & Kubernetes DRA v1 Layer".to_string(),
            items: drv_items,
        },
    ];

    let entity = make_catalog_entity(
        "Resource",
        "default",
        &entity_name,
        Some(&format!("{} ({})", entity_name, npu.architecture)),
        Some(&summary),
        "npu-accelerator",
        status,
        status_level,
        "Compute & Accelerators",
        live_conn,
        vec![
            "npu".to_string(),
            "dra-v1".to_string(),
            npu.vendor.clone(),
            if npu.vendor == "nvidia" {
                "nvidia-dra".to_string()
            } else if npu.vendor == "amd" {
                "amd-xdna".to_string()
            } else {
                "intel-vpu".to_string()
            },
            node.device_id.clone(),
        ],
        json!({
            "hostNode": node.device_id,
            "hostRetailModel": retail_model,
            "hostCpuModel": cpu_model,
            "vendor": npu.vendor,
            "pciId": npu.pci_id,
            "pciAddress": npu.pci_address,
            "architecture": npu.architecture,
            "expectedKernelDriver": npu.expected_kernel_driver,
            "boundDriver": npu.bound_driver,
            "draDriver": dra_driver_name,
            "deviceClass": npu.expected_device_class,
            "draPublished": dra_published || (npu.driver_bound && (!lab_reachable || node.accelerator.as_ref().map(|a| a.dra_validated).unwrap_or(false))),
        }),
        vec![
            rel("partOf", &format!("resource:default/{}", node.device_id)),
            rel("partOf", "resource:default/cluster-pits-lab"),
        ],
    );

    PitsEntityDetail {
        entity,
        resource_type: "npu-accelerator".to_string(),
        status: status.to_string(),
        status_level: status_level.to_string(),
        summary_line: summary,
        refreshed_at: now,
        live_connected: live_conn,
        attribute_sections: sections,
        actions: actions_for_npu(&node.device_id, &npu.expected_device_class, status),
        manifests,
        diagnostics: json!({
            "detectedHardware": npu,
            "cpuExpectation": cpu_exp,
            "resourceSlice": matching_slice,
            "registeredDeviceClasses": live.lab_device_classes,
        }),
    }
}

fn build_win11_vm_detail(
    live: &LiveTelemetry,
    cached_rdp: Option<&RdpTlsVerification>,
    now: DateTime<Utc>,
) -> PitsEntityDetail {
    let vm_probe_ok = live.lab_vms.is_some();
    let live_vm = live
        .lab_vms
        .as_deref()
        .and_then(|vms| vms.iter().find(|v| v.name == "win11-001"));
    let lab_conn = live
        .lab_cluster
        .as_ref()
        .map(|c| c.reachable)
        .unwrap_or(false);

    let (status, status_level) = match live_vm {
        Some(v) if v.ready || v.printable_status == "Running" => ("Running", "ok"),
        Some(v) => (v.printable_status.as_str(), "warn"),
        None if lab_conn && vm_probe_ok => ("NotFound", "warn"),
        None => ("Running", "ok"),
    };

    let install_iso_detached = live_vm
        .map(|v| !v.attached_disks.iter().any(|d| d.contains("iso")))
        .unwrap_or(true);
    let rdp_sha1 = cached_rdp
        .map(|r| format!("{} (NLA={})", r.cert_sha1_hex, r.nla_hybrid_enforced))
        .unwrap_or_else(|| "Run 'verify-rdp-tls' to inspect live X.509 thumbprint".to_string());

    let summary = format!(
        "Windows 11 24H2 Workstation (pits-vms/win11-001) on netboot-004 · 8 vCPU / 16 GiB · SAN vmstore · RDP {}:{}",
        WIN11_DEFAULT_HOST_IP, WIN11_DEFAULT_RDP_NODEPORT
    );

    let vm_yaml = render_windows11_vm_manifest(false);
    let mut manifests = BTreeMap::new();
    manifests.insert("kubevirt-win11-001.yaml".to_string(), vm_yaml);

    let sections = vec![
        AttributeSection {
            id: "vm-compute-schedule".to_string(),
            title: "KubeVirt Instance & Scheduling".to_string(),
            items: vec![
                attr("cluster", "Cluster / Namespace", "cluster-pits-lab / pits-vms", Some("ok"), true),
                attr("hostNode", "Pinned Bare-Metal Host", live_vm.and_then(|v| v.node_name.as_deref()).unwrap_or("netboot-004"), Some("ok"), true),
                attr("runStrategy", "RunStrategy / Printable Status", format!("{} / {}", live_vm.map(|v| v.run_strategy.as_str()).unwrap_or("Always"), status), Some(status_level), false),
                attr("computeSpec", "vCPU / Memory / Firmware", "8 Cores (host-passthrough) · 16 GiB · q35 UEFI SecureBoot + Persistent vTPM", None, false),
                attr("backingVolume", "Primary Disk Backing", "san-volume/netboot-004-vmstore (/var/mnt/vmstore/win11-001/system.img)", Some("ok"), true),
                attr("installMedia", "Install ISO Media Detached", if install_iso_detached { "Clean (Post-Install Finalized)" } else { "ISO Attached (Run finalize-install-media)" }, Some(if install_iso_detached { "ok" } else { "warn" }), false),
            ],
        },
        AttributeSection {
            id: "rdp-security".to_string(),
            title: "Remote Desktop X.224 + TLS Security".to_string(),
            items: vec![
                attr("rdpEndpoint", "RDP NodePort Endpoint", format!("{WIN11_DEFAULT_HOST_IP}:{WIN11_DEFAULT_RDP_NODEPORT}"), Some("ok"), true),
                attr("resolution", "Pinned Display Profile", "3072×1920 @ 32bpp (CredSSP/NLA Level 2)", None, false),
                attr("certSha1", "Live X.509 SHA-1 Thumbprint", rdp_sha1, Some(if cached_rdp.is_some() { "ok" } else { "info" }), true),
                attr("selfSessionGuard", "Self-Session Stop Lock", "Enabled (confirmSelfSessionStop == true required)", Some("ok"), false),
            ],
        },
    ];

    let entity = make_catalog_entity(
        "Resource",
        "default",
        "win11-001",
        Some("win11-001 (Windows 11 Operator Workstation VM)"),
        Some(&summary),
        "virtual-machine",
        status,
        status_level,
        "Compute & Accelerators",
        lab_conn,
        vec![
            "kubevirt".to_string(),
            "windows-11".to_string(),
            "rdp".to_string(),
            "san-backed".to_string(),
        ],
        json!({
            "cluster": "cluster-pits-lab",
            "vmNamespace": "pits-vms",
            "hostNode": "netboot-004",
            "vcpus": 8,
            "memoryGib": 16,
            "rdpEndpoint": format!("{WIN11_DEFAULT_HOST_IP}:{WIN11_DEFAULT_RDP_NODEPORT}"),
        }),
        vec![
            rel("dependsOn", "resource:default/netboot-004"),
            rel("dependsOn", "resource:default/netboot-004-vmstore"),
            rel("partOf", "resource:default/cluster-pits-lab"),
        ],
    );

    PitsEntityDetail {
        entity,
        resource_type: "virtual-machine".to_string(),
        status: status.to_string(),
        status_level: status_level.to_string(),
        summary_line: summary,
        refreshed_at: now,
        live_connected: lab_conn,
        attribute_sections: sections,
        actions: actions_for_vm("win11-001"),
        manifests,
        diagnostics: json!({
            "liveVm": live_vm,
            "lastVerifiedRdpTls": cached_rdp,
        }),
    }
}

fn build_access_lab_vm_detail(live: &LiveTelemetry, now: DateTime<Utc>) -> PitsEntityDetail {
    let live_vm = live
        .core_vms
        .as_deref()
        .and_then(|vms| vms.iter().find(|v| v.name == "access-lab"));
    let core_conn = live
        .core_cluster
        .as_ref()
        .map(|c| c.reachable)
        .unwrap_or(false);

    let (status, status_level) = match live_vm {
        Some(v) if v.ready || v.printable_status == "Running" => ("Running", "ok"),
        Some(v) => (v.printable_status.as_str(), "info"),
        None => ("Ready", "ok"),
    };

    let summary = format!(
        "Core VLAN 10 Access Lab VM (edge-access-lab/access-lab) on pits-core · 2 vCPU / 4 GiB · Fixed MAC {ACCESS_LAB_FIXED_MAC}"
    );
    let vm_yaml = render_access_lab_vm_manifest();
    let mut manifests = BTreeMap::new();
    manifests.insert("kubevirt-access-lab.yaml".to_string(), vm_yaml);

    let sections = vec![AttributeSection {
        id: "vm-compute-network".to_string(),
        title: "KubeVirt Instance & Multus VLAN 10 Bridge".to_string(),
        items: vec![
            attr("cluster", "Cluster / Namespace", "cluster-pits-core / edge-access-lab", Some("ok"), true),
            attr("hostNode", "Pinned Bare-Metal Host", "pits-core", Some("ok"), true),
            attr("status", "Printable Status", status, Some(status_level), false),
            attr("computeSpec", "vCPU / Memory", "2 Cores · 4 GiB RAM", None, false),
            attr("multusNetwork", "Multus Attachment", "edge-access-lab/core-vlan10 (VLAN 10 Core LAN)", Some("ok"), true),
            attr("fixedMac", "Enrolled VirtIO MAC", ACCESS_LAB_FIXED_MAC, Some("ok"), true),
        ],
    }];

    let entity = make_catalog_entity(
        "Resource",
        "default",
        "access-lab",
        Some("access-lab (Core VLAN 10 Access Appliance VM)"),
        Some(&summary),
        "virtual-machine",
        status,
        status_level,
        "Compute & Accelerators",
        core_conn,
        vec![
            "kubevirt".to_string(),
            "vlan10".to_string(),
            "multus".to_string(),
            "core-plane".to_string(),
        ],
        json!({
            "cluster": "cluster-pits-core",
            "vmNamespace": "edge-access-lab",
            "hostNode": "pits-core",
            "vcpus": 2,
            "memoryGib": 4,
            "macAddress": ACCESS_LAB_FIXED_MAC,
        }),
        vec![
            rel("dependsOn", "resource:default/pits-core"),
            rel("dependsOn", "resource:default/pits-core-router"),
            rel("dependsOn", "resource:default/pits-data"),
            rel("partOf", "resource:default/cluster-pits-core"),
        ],
    );

    PitsEntityDetail {
        entity,
        resource_type: "virtual-machine".to_string(),
        status: status.to_string(),
        status_level: status_level.to_string(),
        summary_line: summary,
        refreshed_at: now,
        live_connected: core_conn,
        attribute_sections: sections,
        actions: actions_for_vm("access-lab"),
        manifests,
        diagnostics: json!({ "liveVm": live_vm }),
    }
}

fn build_edge_router_detail(
    bundle: &WorkspaceBundle,
    live: &LiveTelemetry,
    now: DateTime<Utc>,
) -> PitsEntityDetail {
    let default_er = bundle
        .edge_router_yaml
        .as_deref()
        .and_then(|y| serde_yaml::from_str::<EdgeRouterManifest>(y).ok())
        .unwrap_or_else(pits_core_default_edge_router);
    let val_rep = validate_edge_router(&default_er);
    let default_yaml = serde_yaml::to_string(&default_er).unwrap_or_default();

    let rtr_live = live.router_live.as_ref();
    let live_conn = rtr_live
        .map(|r| r.edge_router_present || r.vpp_report.is_some())
        .unwrap_or(false);

    let (status, status_level) = if !val_rep.valid {
        ("PolicyViolation", "error")
    } else if let Some(r) = rtr_live.filter(|r| r.edge_router_present) {
        if r.crossplane_paused {
            ("Paused", "warn")
        } else if r.ready {
            ("Applied", "ok")
        } else {
            (r.phase.as_str(), "warn")
        }
    } else {
        ("PolicyValidated", "ok")
    };

    let vpp_ver = rtr_live
        .and_then(|r| r.vpp_report.as_ref())
        .and_then(|v| v.get("version"))
        .and_then(|v| v.as_str())
        .unwrap_or("FD.io VPP (DPDK + NAT44-ED + ACL)");
    let vpp_if_count = rtr_live
        .and_then(|r| r.vpp_report.as_ref())
        .and_then(|v| v.get("interfaces"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(4);

    let freshness_str = match rtr_live.and_then(|r| r.verification_age_secs) {
        Some(age) => format!(
            "{age}s ago ({})",
            if rtr_live.map(|r| r.verification_fresh).unwrap_or(false) {
                "<=30s Fresh"
            } else {
                "Stale"
            }
        ),
        None => "30s SLA Configured".to_string(),
    };

    let summary = format!(
        "Crossplane EdgeRouter (edge/pits-core) · VLAN 10/20 Isolated · mgmt0 ({MGMT0_PCI}) Protected · sha256={}",
        &val_rep.desired_spec_sha256[..12]
    );

    let mut manifests = BTreeMap::new();
    manifests.insert("edgerouter-pits-core.yaml".to_string(), default_yaml.clone());

    let sections = vec![
        AttributeSection {
            id: "policy-compliance".to_string(),
            title: "Policy Compliance & Isolation Gates".to_string(),
            items: vec![
                attr("desiredSpecSha256", "Desired Spec SHA-256", &val_rep.desired_spec_sha256, Some("ok"), true),
                attr("coreLabIsolation", "VLAN 10 (Core) ↔ VLAN 20 (Lab) Isolation", if val_rep.core_lab_isolated { "Enforced (Bidirectional Block)" } else { "VIOLATION" }, Some(if val_rep.core_lab_isolated { "ok" } else { "error" }), false),
                attr("labMgmtBlock", "Lab → Management-Net Block", if val_rep.lab_mgmt_blocked { "Enforced (4 Protected CIDRs)" } else { "VIOLATION" }, Some(if val_rep.lab_mgmt_blocked { "ok" } else { "error" }), false),
                attr("mgmt0Guard", "mgmt0 Hardware PCI Guard", format!("{MGMT0_PCI} Isolated (Safe={})", val_rep.mgmt0_pci_safe), Some(if val_rep.mgmt0_pci_safe { "ok" } else { "error" }), true),
                attr("crossplanePaused", "Crossplane Reconciliation", if rtr_live.map(|r| r.crossplane_paused).unwrap_or(false) { "Paused (crossplane.io/paused=true)" } else { "Active" }, Some("ok"), false),
                attr("verificationFreshness", "Live Controller Verification Age", freshness_str, None, true),
            ],
        },
        AttributeSection {
            id: "dpdk-vpp-dataplane".to_string(),
            title: "DPDK & FD.io GoVPP Dataplane".to_string(),
            items: vec![
                attr("wanPci", "WAN DPDK Interface PCI", &default_er.spec.wan.pci_address, Some("ok"), true),
                attr("lanTrunkVfPci", "LAN Trunk SR-IOV VF0 PCI", &default_er.spec.lan_trunk_pci, Some("ok"), true),
                attr("vlans", "Routed Gateways", "VLAN 10: 10.88.0.1/16 · VLAN 20: 10.89.20.1/24", None, true),
                attr("vppVersion", "GoVPP Runtime", format!("{vpp_ver} ({vpp_if_count} interfaces)"), Some("ok"), false),
            ],
        },
    ];

    let entity = make_catalog_entity(
        "Resource",
        "default",
        "pits-core-router",
        Some("pits-core-router (Crossplane DPDK/VPP Edge Gateway)"),
        Some(&summary),
        "edge-router",
        status,
        status_level,
        "Network & Boot",
        live_conn,
        vec![
            "router".to_string(),
            "vpp".to_string(),
            "dpdk".to_string(),
            "crossplane".to_string(),
        ],
        json!({
            "hostNode": "pits-core",
            "k8sRef": "edge/pits-core",
            "wanPci": default_er.spec.wan.pci_address,
            "lanTrunkPci": default_er.spec.lan_trunk_pci,
            "protectedMgmtPci": MGMT0_PCI,
            "desiredSpecSha256": val_rep.desired_spec_sha256,
        }),
        vec![
            rel("dependsOn", "resource:default/pits-core"),
            rel("partOf", "resource:default/cluster-pits-core"),
        ],
    );

    PitsEntityDetail {
        entity,
        resource_type: "edge-router".to_string(),
        status: status.to_string(),
        status_level: status_level.to_string(),
        summary_line: summary,
        refreshed_at: now,
        live_connected: live_conn,
        attribute_sections: sections,
        actions: actions_for_edge_router(&default_yaml),
        manifests,
        diagnostics: json!({
            "policyValidation": val_rep,
            "liveRouter": rtr_live,
        }),
    }
}

fn build_pxe_profile_detail(
    bundle: &WorkspaceBundle,
    mode: PxeBootMode,
    live: &LiveTelemetry,
    now: DateTime<Utc>,
) -> PitsEntityDetail {
    let is_talos = matches!(mode, PxeBootMode::TalosNetboot);
    let entity_name = if is_talos {
        "talos-v1.14.5-netboot"
    } else {
        "hook-discovery-x86-64"
    };
    let title = if is_talos {
        "talos-v1.14.5-netboot (Diskless Talos UEFI iPXE Profile)"
    } else {
        "hook-discovery-x86-64 (Non-Destructive Hook Hardware Discovery)"
    };

    let node_ids: Vec<String> = bundle.nodes.iter().map(|n| n.device_id.clone()).collect();
    let sample_node = bundle.nodes.last().or_else(|| bundle.nodes.first());
    let sample_ipxe = sample_node
        .map(|n| render_ipxe_script(n, mode, None))
        .unwrap_or_default();
    let sample_hw = sample_node
        .map(|n| render_tinkerbell_hardware_yaml(n, mode, None))
        .unwrap_or_default();

    let verified_count = live
        .pxe_asset_verify
        .values()
        .filter(|v| v.get("verified").and_then(|b| b.as_bool()) == Some(true))
        .count();
    let live_conn = !live.pxe_asset_verify.is_empty() || live.tink_hardware.is_some();

    let status = if live_conn { "AssetsVerified" } else { "Ready" };
    let summary = if is_talos {
        format!(
            "Diskless Talos {TALOS_VERSION} HTTP/iPXE netboot profile · selinux=1 module.sig_enforce=1 iommu=pt · Bound to 4 nodes"
        )
    } else {
        "Tinkerbell Hook x86_64 RAM-only hardware discovery profile (pits_disk_mutation=forbidden)"
            .to_string()
    };

    let mut manifests = BTreeMap::new();
    manifests.insert("sample-node.ipxe".to_string(), sample_ipxe);
    manifests.insert("sample-tinkerbell-hardware.yaml".to_string(), sample_hw);

    let sections = vec![
        AttributeSection {
            id: "boot-assets".to_string(),
            title: "Boot Kernel, Initramfs & Kernel Cmdline Policy".to_string(),
            items: vec![
                attr("httpBaseUrl", "HTTP Asset Server (pits-core)", ASSET_HTTP_BASE_DEFAULT, Some("ok"), true),
                attr(
                    "assetPaths",
                    "Kernel / Initramfs Paths",
                    if is_talos {
                        "/talos/<netboot-001..004>/{vmlinuz, initramfs.xz}"
                    } else {
                        "/hook/{vmlinuz-x86_64, initramfs-x86_64}"
                    },
                    None,
                    true,
                ),
                attr(
                    "sha256Verify",
                    "Staged Asset SHA-256 Verification",
                    if !live.pxe_asset_verify.is_empty() {
                        format!("{verified_count}/{} node asset sets verified on pits-core", bundle.nodes.len())
                    } else {
                        "Run 'verify-boot-assets' to check live SHA-256 digests".to_string()
                    },
                    Some("ok"),
                    false,
                ),
                attr(
                    "diskSafetyPolicy",
                    "Local Disk Mutation Policy",
                    if is_talos {
                        "Diskless SAN-only (no talos.config.wipe / machine.install)"
                    } else {
                        "Strictly Forbidden (pits_disk_mutation=forbidden)"
                    },
                    Some("ok"),
                    false,
                ),
            ],
        },
        AttributeSection {
            id: "tinkerbell-bindings".to_string(),
            title: "Tinkerbell Hardware & OUI Handling".to_string(),
            items: vec![
                attr("enrolledNodes", "Supported Enrolled Nodes", node_ids.join(", "), None, true),
                attr("minisforumOui", "Minisforum OUI Direct-iPXE", "58:47:ca:* skips redundant UEFI DHCP re-chain", Some("ok"), true),
                attr("allowWorkflow", "Tinkerbell allowWorkflow", if is_talos { "false (Direct Netboot)" } else { "true (Inventory Report)" }, None, true),
            ],
        },
    ];

    let entity = make_catalog_entity(
        "Resource",
        "default",
        entity_name,
        Some(title),
        Some(&summary),
        "pxe-boot-profile",
        status,
        "ok",
        "Network & Boot",
        live_conn,
        vec![
            "pxe".to_string(),
            "ipxe".to_string(),
            "tinkerbell".to_string(),
            if is_talos { "talos" } else { "hook" }.to_string(),
        ],
        json!({
            "bootMode": if is_talos { "talos-netboot" } else { "hook-inventory" },
            "assetHttpBase": ASSET_HTTP_BASE_DEFAULT,
            "enrolledNodes": node_ids,
        }),
        vec![
            rel("dependsOn", "resource:default/pits-core"),
            rel("partOf", "resource:default/cluster-pits-core"),
        ],
    );

    PitsEntityDetail {
        entity,
        resource_type: "pxe-boot-profile".to_string(),
        status: status.to_string(),
        status_level: "ok".to_string(),
        summary_line: summary,
        refreshed_at: now,
        live_connected: live_conn,
        attribute_sections: sections,
        actions: actions_for_pxe_profile(&node_ids),
        manifests,
        diagnostics: json!({
            "assetVerification": live.pxe_asset_verify,
            "tinkerbellHardware": live.tink_hardware,
            "tinkerbellWorkflows": live.tink_workflows,
        }),
    }
}

fn build_storage_pool_detail(live: &LiveTelemetry, now: DateTime<Utc>) -> PitsEntityDetail {
    let btrfs = live.btrfs.as_ref();
    let live_conn = btrfs.is_some();

    let mount_path = btrfs
        .and_then(|b| b.get("mountPath"))
        .and_then(|v| v.as_str())
        .unwrap_or("/srv/pits-storage/tank");
    let fs_uuid = btrfs
        .and_then(|b| b.get("filesystemUuid"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(EXPECTED_BTRFS_UUID);
    let healthy = btrfs
        .and_then(|b| b.get("healthy"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let total_bytes = btrfs
        .and_then(|b| b.get("totalBytes"))
        .and_then(|v| v.as_u64());
    let used_bytes = btrfs
        .and_then(|b| b.get("usedBytes"))
        .and_then(|v| v.as_u64());
    let free_bytes = btrfs
        .and_then(|b| b.get("freeBytes"))
        .and_then(|v| v.as_u64());

    let cap_str = match (total_bytes, used_bytes, free_bytes) {
        (Some(t), Some(u), Some(f)) => format!(
            "Total {:.1} GiB · Used {:.1} GiB · Free {:.1} GiB",
            (t as f64) / 1_073_741_824.0,
            (u as f64) / 1_073_741_824.0,
            (f as f64) / 1_073_741_824.0
        ),
        _ => "Enterprise NVMe Pool (9 SAN Volumes Provisioned)".to_string(),
    };

    let subvols = btrfs
        .and_then(|b| b.get("subvolumes"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "@data, @archive, @snapshots".to_string());

    let (status, status_level) = if !healthy {
        ("Degraded", "warn")
    } else if live_conn {
        ("Healthy", "ok")
    } else {
        ("BaselineReady", "ok")
    };

    let summary = format!(
        "Btrfs Storage Pool pits-data ({fs_uuid}) on pits-core · {cap_str} · Subvolumes: {subvols}"
    );

    let sections = vec![
        AttributeSection {
            id: "btrfs-topology".to_string(),
            title: "Btrfs Pool Capacity & Subvolume Topology".to_string(),
            items: vec![
                attr("filesystemUuid", "Filesystem UUID", fs_uuid, Some(if fs_uuid == EXPECTED_BTRFS_UUID { "ok" } else { "error" }), true),
                attr("mountPath", "Host Mountpoint", mount_path, Some("ok"), true),
                attr("capacity", "Capacity Utilization", &cap_str, Some("ok"), false),
                attr("subvolumes", "Active Subvolumes", &subvols, Some("ok"), true),
                attr("sanExports", "Backed NVMe/TCP SAN Volumes", "9 deterministic NOCOW extents (@data/san/netboot-001..004)", None, false),
            ],
        },
        AttributeSection {
            id: "media-error-telemetry".to_string(),
            title: "Sysfs I/O Counters & SMART Baseline".to_string(),
            items: vec![
                attr("sysfsErrorCounters", "Read / Write / Flush / Corruption / Generation Errors", if healthy { "0 / 0 / 0 / 0 / 0 (All Zero)" } else { "Non-zero counter detected — inspect diagnostics" }, Some(status_level), true),
                attr("smartBaseline", "Samsung NVMe SMART Baseline", "1 historical SMART media error baseline (monitored, non-increasing)", Some("info"), false),
                attr("lastTrimWeek", "Last Serialized FITRIM", btrfs.and_then(|b| b.get("lastTrimWeek")).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("Scheduled Weekly"), None, true),
                attr("lastScrubMonth", "Last Scrub Verification", btrfs.and_then(|b| b.get("lastScrubMonth")).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("Scheduled Monthly"), None, true),
            ],
        },
    ];

    let entity = make_catalog_entity(
        "Resource",
        "default",
        "pits-data",
        Some("pits-data (Enterprise NVMe Btrfs Storage Pool)"),
        Some(&summary),
        "storage-pool",
        status,
        status_level,
        "Storage Fabric",
        live_conn,
        vec![
            "btrfs".to_string(),
            "storage-pool".to_string(),
            "nvme".to_string(),
            "nocow".to_string(),
        ],
        json!({
            "hostNode": "pits-core",
            "filesystemUuid": fs_uuid,
            "mountPath": mount_path,
            "subvolumes": ["@data", "@archive", "@snapshots"],
        }),
        vec![rel("dependsOn", "resource:default/pits-core")],
    );

    PitsEntityDetail {
        entity,
        resource_type: "storage-pool".to_string(),
        status: status.to_string(),
        status_level: status_level.to_string(),
        summary_line: summary,
        refreshed_at: now,
        live_connected: live_conn,
        attribute_sections: sections,
        actions: actions_for_storage_pool(),
        manifests: BTreeMap::new(),
        diagnostics: json!({ "btrfsStatus": btrfs }),
    }
}

fn build_san_volume_detail(
    node: &EnrolledNodeContract,
    ns: &crate::san::SANNamespaceSpec,
    live: &LiveTelemetry,
    now: DateTime<Utc>,
) -> PitsEntityDetail {
    let entity_name = format!("{}-{}", node.device_id, ns.name);
    let wwid = expected_wwid(&node.san.subsystem_nqn, &ns.name);

    let san_target = live.san_by_device.get(&node.device_id);
    let live_ns = san_target
        .and_then(|t| t.get("namespaces"))
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|it| it.get("id").and_then(|v| v.as_i64()) == Some(ns.id as i64))
        });

    let nocow_ok = san_target
        .and_then(|t| t.get("nocowVerified"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let configfs_enabled = live_ns
        .and_then(|r| r.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let actual_bytes = live_ns
        .and_then(|r| r.get("actualBytes"))
        .and_then(|v| v.as_i64())
        .filter(|b| *b > 0)
        .unwrap_or(ns.bytes);
    let backing_file = live_ns
        .and_then(|r| r.get("backingFile"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("/srv/pits-storage/tank/@data/san/{}/{}.raw", node.device_id, ns.name));
    let gpt_verified = live_ns
        .and_then(|r| r.get("gptVerified"))
        .and_then(|v| v.as_bool())
        .unwrap_or(ns.id == 1);

    let talos_mounted = live
        .talos_by_device
        .get(&node.device_id)
        .map(|t| t.cosi_disk_wwids.iter().any(|w| w == &wwid))
        .unwrap_or(false);

    let live_conn = san_target.is_some() || talos_mounted;
    let (status, status_level) = if san_target.is_some() && (!configfs_enabled || !nocow_ok) {
        ("Degraded", "warn")
    } else if live_conn {
        ("Exported", "ok")
    } else {
        ("Provisioned", "ok")
    };

    let gib = (actual_bytes as f64) / 1_073_741_824.0;
    let summary = format!(
        "NVMe/TCP NSID {} ({gib:.2} GiB) · {wwid} · {}:{} -> {}",
        ns.id, node.san.address, node.san.port, node.device_id
    );

    let nvmet_yaml = render_nvmet_k8s_manifests(&node.san).unwrap_or_default();
    let mut manifests = BTreeMap::new();
    manifests.insert(
        format!("nvmet-{}-deployment.yaml", node.device_id),
        nvmet_yaml,
    );
    if ns.name != "system" {
        let cap_gib = (gib.ceil() as u32).max(1);
        let (sc, hp) = if ns.name == "vmstore" {
            ("pits-vm-local", "/var/mnt/vmstore/win11-001")
        } else {
            ("san-node-local", "/var/mnt/userdata/workloads")
        };
        manifests.insert(
            "static-local-pv-pvc.yaml".to_string(),
            render_san_local_pv_pvc(
                &format!("pv-{entity_name}"),
                if ns.name == "vmstore" { "pits-vms" } else { "default" },
                &format!("{entity_name}-pvc"),
                &node.hostname,
                sc,
                hp,
                cap_gib,
            ),
        );
    }

    let gpt_label = if ns.id == 1 {
        if gpt_verified {
            "Verified 4KiB GPT (META + STATE)"
        } else {
            "Pending 4KiB GPT Check"
        }
    } else {
        "Talos UserVolumeConfig Managed (XFS)"
    };

    let sections = vec![
        AttributeSection {
            id: "nvme-tcp-identity".to_string(),
            title: "NVMe/TCP Subsystem & Deterministic WWID".to_string(),
            items: vec![
                attr("subsystemNqn", "Subsystem NQN", &node.san.subsystem_nqn, None, true),
                attr("hostNqn", "Allowed Host NQN (ACL)", &node.san.host_nqn, Some("ok"), true),
                attr("nsid", "Namespace ID (NSID)", ns.id.to_string(), None, true),
                attr("wwid", "Deterministic UUIDv5 WWID", &wwid, Some("ok"), true),
                attr("targetPortal", "VLAN 20 Target Portal", format!("{}:{}", node.san.address, node.san.port), Some("ok"), true),
            ],
        },
        AttributeSection {
            id: "backing-extent-health".to_string(),
            title: "Backing Extent, NOCOW & Block Verification".to_string(),
            items: vec![
                attr("backingFile", "Btrfs Backing Image Path", &backing_file, None, true),
                attr("alignedBytes", "4KiB-Aligned Capacity", format!("{actual_bytes} bytes ({gib:.2} GiB)"), Some("ok"), true),
                attr("nocow", "FS_NOCOW_FL (+C) Attribute", if nocow_ok { "Verified (+C)" } else { "Missing NOCOW" }, Some(if nocow_ok { "ok" } else { "warn" }), true),
                attr("configfsEnabled", "nvmet configfs Namespace", if configfs_enabled { "Enabled (1)" } else { "Disabled (0)" }, Some(if configfs_enabled { "ok" } else { "warn" }), true),
                attr("gptLayout", "Block Partition Geometry", gpt_label, Some("ok"), false),
                attr("initiatorVisible", "Talos COSI Initiator Visibility", if talos_mounted { "Attached & Verified via COSI" } else { "Pinned in Talos VolumeConfig" }, Some(if talos_mounted { "ok" } else { "info" }), false),
            ],
        },
    ];

    let entity = make_catalog_entity(
        "Resource",
        "default",
        &entity_name,
        Some(&format!("{entity_name} (NSID {} · {gib:.1} GiB)", ns.id)),
        Some(&summary),
        "san-volume",
        status,
        status_level,
        "Storage Fabric",
        live_conn,
        vec![
            "nvme-tcp".to_string(),
            "san".to_string(),
            ns.name.clone(),
            node.device_id.clone(),
        ],
        json!({
            "deviceId": node.device_id,
            "namespaceId": ns.id,
            "volumeRole": ns.name,
            "subsystemNqn": node.san.subsystem_nqn,
            "targetPortal": format!("{}:{}", node.san.address, node.san.port),
            "wwid": wwid,
            "bytes": actual_bytes,
        }),
        vec![
            rel("dependsOn", "resource:default/pits-data"),
            rel("dependsOn", "resource:default/cluster-pits-core"),
        ],
    );

    PitsEntityDetail {
        entity,
        resource_type: "san-volume".to_string(),
        status: status.to_string(),
        status_level: status_level.to_string(),
        summary_line: summary,
        refreshed_at: now,
        live_connected: live_conn,
        attribute_sections: sections,
        actions: actions_for_san_volume(node, ns),
        manifests,
        diagnostics: json!({
            "sanNamespaceLive": live_ns,
            "sanTargetLive": san_target,
        }),
    }
}

fn build_cluster_detail(
    is_core: bool,
    bundle: &WorkspaceBundle,
    live: &LiveTelemetry,
    now: DateTime<Utc>,
) -> PitsEntityDetail {
    let entity_name = if is_core {
        "cluster-pits-core"
    } else {
        "cluster-pits-lab"
    };
    let cp = if is_core {
        live.core_cluster.as_ref()
    } else {
        live.lab_cluster.as_ref()
    };
    let reachable = cp.map(|c| c.reachable).unwrap_or(false);

    let git_ver = cp
        .and_then(|c| c.git_version.as_deref())
        .unwrap_or(if is_core { "v1.35.8+rke2r1" } else { LAB_K8S_VERSION });
    let server_url = cp
        .map(|c| c.server_url.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(if is_core {
            "https://100.74.25.42:6443"
        } else {
            "https://10.89.20.21:6443"
        });

    let expected_nodes = if is_core { 1 } else { bundle.nodes.len() };
    let live_nodes = cp.filter(|c| !c.nodes.is_empty());
    let ready_nodes = live_nodes
        .map(|c| c.nodes.iter().filter(|n| n.ready).count())
        .unwrap_or(expected_nodes);
    let total_nodes = live_nodes
        .map(|c| c.nodes.len())
        .unwrap_or(expected_nodes);

    let pods_str = match cp.filter(|c| c.reachable) {
        Some(c) => format!("{}/{} Running", c.running_pods, c.total_pods),
        None => "Baseline Control-Plane Workloads".to_string(),
    };
    let bound_pvs = cp.map(|c| c.bound_pvs).unwrap_or(if is_core { 2 } else { 3 });

    let (status, status_level) = if reachable {
        if ready_nodes == total_nodes && total_nodes > 0 {
            ("Healthy", "ok")
        } else {
            ("DegradedNodes", "warn")
        }
    } else if live.any_connected {
        ("Unreachable", "warn")
    } else {
        ("BaselineReady", "ok")
    };

    let dcs = if is_core {
        "N/A (Core Infrastructure Cluster)".to_string()
    } else if !live.lab_device_classes.is_empty() {
        live.lab_device_classes.join(", ")
    } else {
        "pits-intel-npu, pits-amd-npu".to_string()
    };

    let transport_desc = if is_core {
        "Direct TLS 1.3 (:6443 via Tailscale / Management LAN)"
    } else {
        "Zero-Trust mTLS HTTP CONNECT Tunnel via pits-agent (:18443 -> 10.89.20.21:6443)"
    };

    let summary = format!(
        "{} ({git_ver}) · {ready_nodes}/{total_nodes} Ready Nodes · {pods_str}",
        if is_core {
            "Core RKE2 Control Plane"
        } else {
            "Lab Diskless Talos K8s + DRA v1 Control Plane"
        }
    );

    let relations = if is_core {
        vec![rel("hasPart", "resource:default/pits-core")]
    } else {
        vec![rel("dependsOn", "resource:default/pits-core-router")]
    };

    let sections = vec![
        AttributeSection {
            id: "control-plane".to_string(),
            title: "Kubernetes API Server & Transport".to_string(),
            items: vec![
                attr("apiServer", "API Server Endpoint", server_url, Some("ok"), true),
                attr("transportMode", "Client Transport", transport_desc, Some("ok"), false),
                attr("kubernetesVersion", "Kubernetes Version", git_ver, Some("ok"), true),
                attr("etcdQuorum", "Etcd Quorum Topology", if is_core { "1-Node RKE2 Embedded Etcd" } else { "3+1 Diskless SAN-Backed Etcd Quorum (10.89.20.0/24)" }, Some("ok"), false),
            ],
        },
        AttributeSection {
            id: "workloads-dra".to_string(),
            title: "Nodes, Workloads & DRA ResourceSlices".to_string(),
            items: vec![
                attr("readyNodes", "Ready Nodes", format!("{ready_nodes}/{total_nodes}"), Some(status_level), true),
                attr("pods", "Pod Health", &pods_str, Some("ok"), false),
                attr("boundPvs", "Bound PersistentVolumes", bound_pvs.to_string(), None, true),
                attr("deviceClasses", "DRA v1 DeviceClasses", &dcs, None, true),
            ],
        },
    ];

    let entity = make_catalog_entity(
        "Resource",
        "default",
        entity_name,
        Some(if is_core {
            "cluster-pits-core (Core NixOS RKE2 Cluster)"
        } else {
            "cluster-pits-lab (Lab Diskless Talos K8s v1.37 Cluster)"
        }),
        Some(&summary),
        "kubernetes-cluster",
        status,
        status_level,
        "Control Planes",
        reachable,
        vec![
            "kubernetes".to_string(),
            if is_core { "rke2" } else { "talos-k8s" }.to_string(),
            if is_core { "core-plane" } else { "dra-v1" }.to_string(),
        ],
        json!({
            "clusterRole": if is_core { "core-infrastructure" } else { "lab-workload-dra" },
            "apiServer": server_url,
            "gitVersion": git_ver,
            "viaAgentBridge": !is_core,
            "readyNodes": ready_nodes,
            "totalNodes": total_nodes,
        }),
        relations,
    );

    PitsEntityDetail {
        entity,
        resource_type: "kubernetes-cluster".to_string(),
        status: status.to_string(),
        status_level: status_level.to_string(),
        summary_line: summary,
        refreshed_at: now,
        live_connected: reachable,
        attribute_sections: sections,
        actions: actions_for_k8s_cluster(),
        manifests: BTreeMap::new(),
        diagnostics: json!({
            "clusterSummary": cp,
            "registeredDeviceClasses": if is_core { &[][..] } else { live.lab_device_classes.as_slice() },
        }),
    }
}

/// Evaluates Backstage Catalog filter expressions:
/// - Empty `filters` matches all entities.
/// - Multiple entries in `filters` are combined with logical **OR**.
/// - Comma-separated clauses within a single filter string (`kind=resource,spec.type=san-volume`)
///   are combined with logical **AND**, compared case-insensitively across nested JSON objects
///   and arrays (e.g. `relations.targetRef`, `metadata.tags`, `metadata.annotations.pits.io/status`).
fn entity_matches_filters(entity_val: &Value, filters: &[String]) -> bool {
    let non_empty: Vec<&str> = filters
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if non_empty.is_empty() {
        return true;
    }

    non_empty.iter().any(|filter_group| {
        filter_group
            .split(',')
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .all(|clause| entity_matches_clause(entity_val, clause))
    })
}

fn entity_matches_clause(entity_val: &Value, clause: &str) -> bool {
    if let Some((key, want)) = clause.split_once("!=") {
        let vals = resolve_scalar_values_at_path(entity_val, key.trim());
        let want_trimmed = want.trim();
        !vals.iter().any(|v| v.eq_ignore_ascii_case(want_trimmed))
    } else if let Some((key, want)) = clause.split_once('=') {
        let vals = resolve_scalar_values_at_path(entity_val, key.trim());
        let want_trimmed = want.trim();
        vals.iter().any(|v| v.eq_ignore_ascii_case(want_trimmed))
    } else {
        path_exists_non_null(entity_val, clause.trim())
    }
}

fn resolve_scalar_values_at_path(val: &Value, path: &str) -> Vec<String> {
    let mut out = Vec::new();
    collect_scalars_at_path(val, path.trim(), &mut out);
    out
}

fn collect_scalars_at_path(val: &Value, path: &str, out: &mut Vec<String>) {
    if path.is_empty() {
        match val {
            Value::String(s) => out.push(s.clone()),
            Value::Number(n) => out.push(n.to_string()),
            Value::Bool(b) => out.push(b.to_string()),
            Value::Array(arr) => {
                for item in arr {
                    collect_scalars_at_path(item, "", out);
                }
            }
            _ => {}
        }
        return;
    }

    match val {
        Value::Object(map) => {
            // 1. Direct literal key match (supports annotation/label keys containing dots, e.g. `pits.io/status`)
            if let Some(child) = get_object_key_ci(map, path) {
                collect_scalars_at_path(child, "", out);
            }
            // 2. Segment split on first `.`
            if let Some((head, tail)) = path.split_once('.') {
                if let Some(child) = get_object_key_ci(map, head) {
                    collect_scalars_at_path(child, tail, out);
                }
            }
        }
        Value::Array(arr) => {
            for elem in arr {
                collect_scalars_at_path(elem, path, out);
            }
        }
        _ => {}
    }
}

fn path_exists_non_null(val: &Value, path: &str) -> bool {
    if path.is_empty() {
        return !val.is_null();
    }
    match val {
        Value::Object(map) => {
            if let Some(child) = get_object_key_ci(map, path) {
                if !child.is_null() {
                    return true;
                }
            }
            if let Some((head, tail)) = path.split_once('.') {
                if let Some(child) = get_object_key_ci(map, head) {
                    return path_exists_non_null(child, tail);
                }
            }
            false
        }
        Value::Array(arr) => arr.iter().any(|e| path_exists_non_null(e, path)),
        _ => false,
    }
}

fn get_object_key_ci<'a>(
    map: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Option<&'a Value> {
    if let Some(v) = map.get(key) {
        return Some(v);
    }
    map.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

fn entity_matches_full_text(val: &Value, lower_tokens: &[String]) -> bool {
    let mut buf = String::new();
    collect_all_leaf_text(val, &mut buf);
    let haystack = buf.to_lowercase();
    lower_tokens.iter().all(|tok| haystack.contains(tok))
}

fn collect_all_leaf_text(val: &Value, out: &mut String) {
    match val {
        Value::String(s) => {
            out.push_str(s);
            out.push(' ');
        }
        Value::Number(n) => {
            out.push_str(&n.to_string());
            out.push(' ');
        }
        Value::Bool(b) => {
            out.push_str(&b.to_string());
            out.push(' ');
        }
        Value::Array(arr) => {
            for v in arr {
                collect_all_leaf_text(v, out);
            }
        }
        Value::Object(map) => {
            for v in map.values() {
                collect_all_leaf_text(v, out);
            }
        }
        Value::Null => {}
    }
}

fn project_entity_fields(val: &Value, field_paths: &[String]) -> Value {
    let mut dst = Value::Object(serde_json::Map::new());
    for fp in field_paths {
        copy_field_path(val, &mut dst, fp.trim());
    }
    dst
}

fn copy_field_path(src: &Value, dst: &mut Value, path: &str) {
    if path.is_empty() {
        *dst = src.clone();
        return;
    }
    if let Value::Object(src_map) = src {
        if let Some((canon_key, child_val)) = src_map
            .get_key_value(path)
            .or_else(|| src_map.iter().find(|(k, _)| k.eq_ignore_ascii_case(path)))
        {
            if let Value::Object(dst_map) = dst {
                dst_map.insert(canon_key.clone(), child_val.clone());
            }
            return;
        }
        if let Some((head, tail)) = path.split_once('.') {
            if let Some((canon_key, child_src)) = src_map
                .get_key_value(head)
                .or_else(|| src_map.iter().find(|(k, _)| k.eq_ignore_ascii_case(head)))
            {
                if let Value::Object(dst_map) = dst {
                    let entry = dst_map
                        .entry(canon_key.clone())
                        .or_insert_with(|| Value::Object(serde_json::Map::new()));
                    copy_field_path(child_src, entry, tail);
                }
            }
        }
    }
}

fn merge_talos_status(
    a: Option<TalosNodeLiveStatus>,
    b: Option<TalosNodeLiveStatus>,
) -> Option<TalosNodeLiveStatus> {
    match (a, b) {
        (None, x) => x,
        (x, None) => x,
        (Some(st1), Some(st2)) => {
            if !st1.reachable {
                return Some(st2);
            }
            if !st2.reachable {
                return Some(st1);
            }
            Some(TalosNodeLiveStatus {
                node: st2.node,
                reachable: true,
                talos_tag: st2.talos_tag.or(st1.talos_tag),
                hostname_reported: st2.hostname_reported.or(st1.hostname_reported),
                hardware: st2.hardware.or(st1.hardware),
                physical_npus: if !st2.physical_npus.is_empty() {
                    st2.physical_npus
                } else {
                    st1.physical_npus
                },
                services: if !st2.services.is_empty() {
                    st2.services
                } else {
                    st1.services
                },
                etcd_members: if !st2.etcd_members.is_empty() {
                    st2.etcd_members
                } else {
                    st1.etcd_members
                },
                cosi_disk_wwids: if !st2.cosi_disk_wwids.is_empty() {
                    st2.cosi_disk_wwids
                } else {
                    st1.cosi_disk_wwids
                },
                error: None,
            })
        }
    }
}

pub(crate) async fn kube_get_retry(
    kc: &KubeClient,
    path: &str,
    extra_headers: Option<&[(&str, &str)]>,
    sem: Option<&Semaphore>,
) -> Result<Value> {
    kube_get_retry_with_timeout(kc, path, extra_headers, sem, PROBE_TIMEOUT).await
}

async fn kube_get_retry_with_timeout(
    kc: &KubeClient,
    path: &str,
    extra_headers: Option<&[(&str, &str)]>,
    sem: Option<&Semaphore>,
    timeout: Duration,
) -> Result<Value> {
    let retry_cutoff = timeout.saturating_sub(Duration::from_secs(2));
    for attempt in 0..2 {
        let (res, attempt_elapsed) = {
            let _permit = match sem {
                Some(s) => s.acquire().await.ok(),
                None => None,
            };
            let attempt_start = Instant::now();
            let r = match extra_headers {
                Some(hdrs) => {
                    tokio::time::timeout(timeout, kc.get_json_with_headers::<Value>(path, hdrs))
                        .await
                }
                None => tokio::time::timeout(timeout, kc.get_json::<Value>(path)).await,
            };
            (r, attempt_start.elapsed())
        };
        match res {
            Ok(Ok(val)) => return Ok(val),
            Ok(Err(e)) if attempt == 1 || attempt_elapsed >= retry_cutoff => return Err(e),
            Err(_) if attempt == 1 || attempt_elapsed >= retry_cutoff => {
                bail!("timeout querying {} on {}", path, kc.cluster_label)
            }
            _ => tokio::time::sleep(Duration::from_millis(150)).await,
        }
    }
    bail!("exhausted retries querying {} on {}", path, kc.cluster_label)
}

/// Concurrent Kubernetes cluster summary inspector (`tokio::join!` across all 5 REST endpoints,
/// gated by optional semaphore + 1x retry, and resilient to single-endpoint transient resets).
pub(crate) async fn inspect_cluster_fast(
    kc: &KubeClient,
    sem: Option<&Semaphore>,
) -> ClusterControlPlaneSummary {
    let mut out = ClusterControlPlaneSummary {
        cluster: kc.cluster_label.clone(),
        server_url: kc.endpoint.server_url.clone(),
        via_agent_bridge: kc.bridge_proxy_url.is_some(),
        reachable: false,
        checked_at: Utc::now(),
        git_version: None,
        nodes: Vec::new(),
        running_pods: 0,
        total_pods: 0,
        bound_pvs: 0,
        npu_slices: Vec::new(),
        error: None,
    };

    let accept_table_hdr = [(
        "Accept",
        "application/json;as=Table;v=v1;g=meta.k8s.io,application/json",
    )];

    let (ver_res, nodes_res, pods_res, pvs_res, slices_res) = tokio::join!(
        kube_get_retry(kc, "/version", None, sem),
        kube_get_retry_with_timeout(kc, "/api/v1/nodes", None, sem, NODES_PROBE_TIMEOUT),
        kube_get_retry(
            kc,
            "/api/v1/pods?includeObject=None",
            Some(&accept_table_hdr),
            sem
        ),
        kube_get_retry(kc, "/api/v1/persistentvolumes", None, sem),
        kube_get_retry(kc, "/apis/resource.k8s.io/v1/resourceslices", None, sem),
    );

    let nodes_res = if nodes_res.is_err() && (ver_res.is_ok() || pods_res.is_ok()) {
        kube_get_retry_with_timeout(kc, "/api/v1/nodes", None, sem, NODES_PROBE_TIMEOUT).await
    } else {
        nodes_res
    };

    if let Ok(ref node_list) = nodes_res {
        if let Some(items) = node_list.get("items").and_then(|v| v.as_array()) {
            for item in items {
                let name = item
                    .pointer("/metadata/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let kubelet_version = item
                    .pointer("/status/nodeInfo/kubeletVersion")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut internal_ip = String::new();
                if let Some(addrs) = item.pointer("/status/addresses").and_then(|v| v.as_array()) {
                    for a in addrs {
                        if a.get("type").and_then(|v| v.as_str()) == Some("InternalIP") {
                            internal_ip = a
                                .get("address")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            break;
                        }
                    }
                }
                let mut ready = false;
                if let Some(conds) = item
                    .pointer("/status/conditions")
                    .and_then(|v| v.as_array())
                {
                    for c in conds {
                        if c.get("type").and_then(|v| v.as_str()) == Some("Ready")
                            && c.get("status").and_then(|v| v.as_str()) == Some("True")
                        {
                            ready = true;
                        }
                    }
                }
                let mut roles = Vec::new();
                if let Some(labels) = item
                    .pointer("/metadata/labels")
                    .and_then(|v| v.as_object())
                {
                    for k in labels.keys() {
                        if let Some(role) = k.strip_prefix("node-role.kubernetes.io/") {
                            roles.push(role.to_string());
                        }
                    }
                }
                let mut custom_allocatable = BTreeMap::new();
                let mut retired_zero_resources = Vec::new();
                if let Some(alloc) = item
                    .pointer("/status/allocatable")
                    .and_then(|v| v.as_object())
                {
                    for (k, v) in alloc {
                        if k.starts_with("pits.io/")
                            || k.starts_with("hugepages-")
                            || k.contains("intel")
                            || k.contains("amd")
                            || k.contains("kubevirt.io/")
                        {
                            if let Some(s) = v.as_str() {
                                if k == "npu.intel.com/accel" && s == "0" {
                                    retired_zero_resources.push(format!(
                                        "{k}=0 (retired Phase-1 DevicePlugin; superseded by DRA v1)"
                                    ));
                                } else {
                                    custom_allocatable.insert(k.clone(), s.to_string());
                                }
                            }
                        }
                    }
                }
                out.nodes.push(KubeNodeSummary {
                    name,
                    ready,
                    kubelet_version,
                    internal_ip,
                    roles,
                    custom_allocatable,
                    retired_zero_resources,
                });
            }
        }
    }

    out.reachable = ver_res.is_ok() || nodes_res.is_ok();
    if !out.reachable {
        out.error = ver_res
            .err()
            .or_else(|| nodes_res.err())
            .map(|e| e.to_string());
        return out;
    }

    out.git_version = ver_res
        .ok()
        .and_then(|ver| {
            ver.get("gitVersion")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            out.nodes
                .iter()
                .find_map(|n| (!n.kubelet_version.is_empty()).then(|| n.kubelet_version.clone()))
        });

    if let Ok(pods) = pods_res {
        if let Some(rows) = pods.get("rows").and_then(|v| v.as_array()) {
            let status_idx = pods
                .get("columnDefinitions")
                .and_then(|v| v.as_array())
                .and_then(|cols| {
                    cols.iter().position(|c| {
                        c.get("name")
                            .and_then(|n| n.as_str())
                            .map(|n| n.eq_ignore_ascii_case("Status"))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(2);
            out.total_pods = rows.len();
            out.running_pods = rows
                .iter()
                .filter(|r| {
                    r.get("cells")
                        .and_then(|c| c.as_array())
                        .and_then(|c| c.get(status_idx))
                        .and_then(|v| v.as_str())
                        == Some("Running")
                })
                .count();
        } else if let Some(items) = pods.get("items").and_then(|v| v.as_array()) {
            out.total_pods = items.len();
            out.running_pods = items
                .iter()
                .filter(|p| p.pointer("/status/phase").and_then(|v| v.as_str()) == Some("Running"))
                .count();
        }
    }

    if let Ok(pvs) = pvs_res {
        if let Some(items) = pvs.get("items").and_then(|v| v.as_array()) {
            out.bound_pvs = items
                .iter()
                .filter(|p| p.pointer("/status/phase").and_then(|v| v.as_str()) == Some("Bound"))
                .count();
        }
    }

    if let Ok(slices) = slices_res {
        if let Some(items) = slices.get("items").and_then(|v| v.as_array()) {
            for item in items {
                let driver = item
                    .pointer("/spec/driver")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if driver != PITS_NPU_DRA_DRIVER && !driver.to_lowercase().contains("npu") {
                    continue;
                }
                let slice_name = item
                    .pointer("/metadata/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let node_name = item
                    .pointer("/spec/nodeName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("cluster")
                    .to_string();
                let mut devices = Vec::new();
                let mut device_details = Vec::new();
                if let Some(devs) = item.pointer("/spec/devices").and_then(|v| v.as_array()) {
                    for d in devs {
                        let dn = d
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unnamed")
                            .to_string();
                        devices.push(dn.clone());
                        let attr_str = |key: &str| -> Option<String> {
                            d.pointer(&format!("/attributes/{key}/string"))
                                .or_else(|| d.pointer(&format!("/attributes/{key}/stringValue")))
                                .and_then(|v| v.as_str())
                                .map(str::to_string)
                        };
                        device_details.push(NpuDraDeviceSummary {
                            name: dn,
                            vendor: attr_str("vendor"),
                            model: attr_str("model"),
                            pci_address: attr_str("pciAddress"),
                            kernel_driver: attr_str("kernelDriver"),
                        });
                    }
                }
                out.npu_slices.push(NpuSliceSummary {
                    slice_name,
                    node_name,
                    driver,
                    devices,
                    device_details,
                });
            }
        }
    }

    out
}

/// Concurrent KubeVirt `VirtualMachine` + `VirtualMachineInstance` inspector (`tokio::join!`).
async fn list_cluster_vms_fast(
    kc: &KubeClient,
    sem: Option<&Semaphore>,
) -> Result<Vec<VirtualMachineSummary>> {
    let (vms_res, vmis_res) = tokio::join!(
        kube_get_retry(kc, "/apis/kubevirt.io/v1/virtualmachines", None, sem),
        kube_get_retry(
            kc,
            "/apis/kubevirt.io/v1/virtualmachineinstances",
            None,
            sem
        ),
    );
    let vms_val = vms_res?;
    let vmis_val = vmis_res.unwrap_or(Value::Null);

    let mut out = Vec::new();
    let Some(items) = vms_val.get("items").and_then(|v| v.as_array()) else {
        return Ok(out);
    };

    for vm in items {
        let ns = vm
            .pointer("/metadata/namespace")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();
        let name = vm
            .pointer("/metadata/name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let printable_status = vm
            .pointer("/status/printableStatus")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string();
        let ready = vm
            .pointer("/status/ready")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let run_strategy = vm
            .pointer("/spec/runStrategy")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| {
                if vm.pointer("/spec/running").and_then(|v| v.as_bool()) == Some(true) {
                    "Running".to_string()
                } else {
                    "Halted".to_string()
                }
            });

        let mut attached_disks = Vec::new();
        if let Some(disks) = vm
            .pointer("/spec/template/spec/domain/devices/disks")
            .and_then(|v| v.as_array())
        {
            for d in disks {
                if let Some(dn) = d.get("name").and_then(|v| v.as_str()) {
                    attached_disks.push(dn.to_string());
                }
            }
        }

        let mut node_name = None;
        let mut guest_os = None;
        let mut interfaces = Vec::new();

        if let Some(vmi_items) = vmis_val.get("items").and_then(|v| v.as_array()) {
            if let Some(vmi) = vmi_items.iter().find(|i| {
                i.pointer("/metadata/namespace").and_then(|v| v.as_str()) == Some(ns.as_str())
                    && i.pointer("/metadata/name").and_then(|v| v.as_str()) == Some(name.as_str())
            }) {
                node_name = vmi
                    .pointer("/status/nodeName")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                guest_os = vmi
                    .pointer("/status/guestOSInfo/prettyName")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                if let Some(ifaces) = vmi
                    .pointer("/status/interfaces")
                    .and_then(|v| v.as_array())
                {
                    for iface in ifaces {
                        interfaces.push(VmInterfaceInfo {
                            name: iface
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("default")
                                .to_string(),
                            mac: iface
                                .get("mac")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            ip_address: iface
                                .get("ipAddress")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                        });
                    }
                }
            }
        }

        out.push(VirtualMachineSummary {
            cluster: kc.cluster_label.clone(),
            namespace: ns,
            name,
            printable_status,
            ready,
            run_strategy,
            node_name,
            guest_os,
            interfaces,
            attached_disks,
        });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::generate_agent_mtls_pki;

    #[tokio::test]
    async fn cold_configured_profile_get_snapshot_returns_under_50ms_with_full_catalog() {
        let pki = generate_agent_mtls_pki("127.0.0.1").expect("in-memory test PKI");

        // 1. Unreachable https://127.0.0.1:19999 endpoint with configured mTLS credentials
        let mut prof = ClientProfile::default();
        prof.agent_endpoint = "https://127.0.0.1:19999".to_string();
        prof.agent_ca_pem = Some(pki.ca_cert_pem.clone());
        prof.agent_client_cert_pem = Some(pki.client_cert_pem.clone());
        prof.agent_client_key_pem = Some(pki.client_key_pem.clone());

        let reg = CatalogRegistry::new(
            Arc::new(RwLock::new(prof)),
            Arc::new(RwLock::new(WorkspaceBundle::builtin_pits_baseline())),
        );
        assert!(
            reg.is_profile_configured().await,
            "profile must satisfy is_profile_configured"
        );

        let start = Instant::now();
        let snap = reg.get_snapshot(false).await;
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(50),
            "cold get_snapshot(false) took {:?}, expected < 50ms",
            elapsed
        );
        assert_eq!(snap.resource_types.len(), 12);
        let resource_count = snap.entities.iter().filter(|e| e.kind == "Resource").count();
        let system_count = snap.entities.iter().filter(|e| e.kind == "System").count();
        let domain_count = snap.entities.iter().filter(|e| e.kind == "Domain").count();
        let component_count = snap.entities.iter().filter(|e| e.kind == "Component").count();
        assert_eq!(resource_count, 31);
        assert_eq!(system_count, 3);
        assert_eq!(domain_count, 3);
        assert_eq!(component_count, 7);

        let sys_detail = snap
            .details
            .get("default/pits-infrastructure")
            .expect("system entity detail");
        assert!(!sys_detail.live_connected);
        assert_eq!(
            sys_detail
                .diagnostics
                .get("connectionNote")
                .and_then(|v| v.as_str()),
            Some("Baseline catalog loaded instantly; syncing live telemetry in background...")
        );

        // 2. Concurrent cold stampede against a blackhole TCP listener that stalls TLS handshakes
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind blackhole listener");
        let blackhole_addr = listener.local_addr().expect("local addr");
        let blackhole_task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });

        let mut blackhole_prof = ClientProfile::default();
        blackhole_prof.agent_endpoint = format!("https://{blackhole_addr}");
        blackhole_prof.agent_ca_pem = Some(pki.ca_cert_pem);
        blackhole_prof.agent_client_cert_pem = Some(pki.client_cert_pem);
        blackhole_prof.agent_client_key_pem = Some(pki.client_key_pem);

        let reg_bh = CatalogRegistry::new(
            Arc::new(RwLock::new(blackhole_prof)),
            Arc::new(RwLock::new(WorkspaceBundle::builtin_pits_baseline())),
        );
        assert!(reg_bh.is_profile_configured().await);

        let facet_keys = vec!["kind".to_string()];
        let burst_start = Instant::now();
        let (r1, r2, r3, r4, r5, r6) = tokio::join!(
            reg_bh.list_resource_types(false),
            reg_bh.list_entities(false),
            reg_bh.get_pits_entity_detail("default", "pits-core", false),
            reg_bh.get_pits_entity_detail("default", "netboot-003-npu", false),
            reg_bh.compute_facets(&facet_keys, &[], false),
            reg_bh.get_snapshot(false),
        );
        let burst_elapsed = burst_start.elapsed();

        assert!(
            burst_elapsed < Duration::from_millis(50),
            "6-way concurrent cold burst took {:?}, expected < 50ms",
            burst_elapsed
        );
        assert_eq!(r1.len(), 12);
        assert_eq!(r2.len(), 44);
        assert!(r3.is_ok());
        assert!(r4.is_ok());
        assert_eq!(r5.facets.get("kind").map(|v| v.len()), Some(4));
        assert_eq!(r6.entities.len(), 44);

        blackhole_task.abort();
    }

    #[test]
    fn cluster_detail_guards_against_empty_nodes_false_positive_and_detects_unready_node() {
        let bundle = WorkspaceBundle::builtin_pits_baseline();
        let now = Utc::now();

        // 1. Reachable cluster (`40/52 Running`) whose `/api/v1/nodes` returned empty must not report `0/4 DegradedNodes`
        let mut live_empty_nodes = LiveTelemetry::default();
        live_empty_nodes.any_connected = true;
        live_empty_nodes.lab_cluster = Some(ClusterControlPlaneSummary {
            cluster: "pits-lab".to_string(),
            server_url: "https://10.89.20.21:6443".to_string(),
            via_agent_bridge: true,
            reachable: true,
            checked_at: now,
            git_version: Some("v1.37.1".to_string()),
            nodes: Vec::new(),
            running_pods: 40,
            total_pods: 52,
            bound_pvs: 3,
            npu_slices: Vec::new(),
            error: None,
        });
        let detail_ok = build_cluster_detail(false, &bundle, &live_empty_nodes, now);
        assert_eq!(detail_ok.status, "Healthy");
        assert_eq!(detail_ok.status_level, "ok");
        assert!(detail_ok.summary_line.contains("4/4 Ready Nodes · 40/52 Running"));

        // 2. Authoritative non-empty node list with an unready node still reports `DegradedNodes`
        let mut live_degraded = LiveTelemetry::default();
        live_degraded.any_connected = true;
        live_degraded.lab_cluster = Some(ClusterControlPlaneSummary {
            cluster: "pits-lab".to_string(),
            server_url: "https://10.89.20.21:6443".to_string(),
            via_agent_bridge: true,
            reachable: true,
            checked_at: now,
            git_version: Some("v1.37.1".to_string()),
            nodes: bundle
                .nodes
                .iter()
                .enumerate()
                .map(|(i, n)| KubeNodeSummary {
                    name: n.device_id.clone(),
                    ready: i < 3,
                    kubelet_version: "v1.37.1".to_string(),
                    internal_ip: n.node_ip.clone(),
                    roles: vec!["control-plane".to_string()],
                    custom_allocatable: BTreeMap::new(),
                    retired_zero_resources: Vec::new(),
                })
                .collect(),
            running_pods: 40,
            total_pods: 52,
            bound_pvs: 3,
            npu_slices: Vec::new(),
            error: None,
        });
        let detail_warn = build_cluster_detail(false, &bundle, &live_degraded, now);
        assert_eq!(detail_warn.status, "DegradedNodes");
        assert_eq!(detail_warn.status_level, "warn");
        assert!(detail_warn.summary_line.contains("3/4 Ready Nodes · 40/52 Running"));
    }

    #[test]
    fn win11_vm_detail_avoids_false_not_found_when_vm_list_probe_failed() {
        let now = Utc::now();
        let reachable_lab = ClusterControlPlaneSummary {
            cluster: "pits-lab".to_string(),
            server_url: "https://10.89.20.21:6443".to_string(),
            via_agent_bridge: true,
            reachable: true,
            checked_at: now,
            git_version: Some("v1.37.1".to_string()),
            nodes: vec![],
            running_pods: 0,
            total_pods: 0,
            bound_pvs: 0,
            npu_slices: vec![],
            error: None,
        };
        let mut live_probe_err = LiveTelemetry::default();
        live_probe_err.lab_cluster = Some(reachable_lab.clone());
        live_probe_err.lab_vms = None;
        let detail_err = build_win11_vm_detail(&live_probe_err, None, now);
        assert_eq!(detail_err.status, "Running");
        assert_eq!(detail_err.status_level, "ok");

        let mut live_confirmed_empty = LiveTelemetry::default();
        live_confirmed_empty.lab_cluster = Some(reachable_lab);
        live_confirmed_empty.lab_vms = Some(vec![]);
        let detail_empty = build_win11_vm_detail(&live_confirmed_empty, None, now);
        assert_eq!(detail_empty.status, "NotFound");
        assert_eq!(detail_empty.status_level, "warn");
    }
}
