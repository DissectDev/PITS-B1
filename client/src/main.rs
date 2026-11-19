//! `pitsctl` — Resource-Centric PITS Infrastructure Client & Embedded Backstage v1.56 Portal.
//!
//! Primary Resource-Type & Entity-Centric CLI surface:
//! - `pitsctl types`    — List all 13 infrastructure & sourcing Resource Types, entity health counts & actions
//! - `pitsctl get`      — List synthesized Backstage `Resource` entities by Resource Type (`all` or specific)
//! - `pitsctl describe` — Inspect an entity's structured Attributes, Relations & Contextual Actions
//! - `pitsctl action`   — Execute a guard-checked contextual Action against a specific entity
//! - `pitsctl catalog`  — Export multi-document `catalog-info.yaml` or inspect catalog types
//! - `pitsctl serve`    — Run embedded Backstage v1.56 Portal + Catalog & Action REST APIs

pub mod catalog;
pub mod config;
pub mod db_common;
pub mod deploy;
pub mod grpc_wire;
pub mod hub;
pub mod k8s;
pub mod mcp;
pub mod pxe;
pub mod router;
pub mod san;
pub mod server;
pub mod talos;
pub mod transport;
pub mod vm;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::catalog::{
    ActionExecutionResult, ActionInvokeRequest, CatalogRegistry, PitsEntityDetail,
    ResourceTypeSummary,
};
use crate::config::{
    default_profile_path, effective_hub_db_url, effective_hub_mcp_token, effective_sourcing_db_url,
    generate_agent_mtls_pki, load_kubeconfig_file, load_talosconfig_file, local_state_dir,
    ClientProfile, WorkspaceBundle,
};
use crate::hub::HubRegistry;
use crate::deploy::{deploy_to_nixos_pits_core, NixosDeployMode, NixosDeployOptions};
use crate::k8s::{
    apply_san_local_pv_pvc, audit_fleet_npus, inspect_cluster_summary, launch_npu_validation_job,
    render_npu_validation_manifests, render_san_local_pv_pvc,
};
use crate::pxe::{
    analyze_hook_inventory, apply_tinkerbell_hardware, list_tinkerbell_hardware,
    list_tinkerbell_workflows, pxe_list_inventory, pxe_verify_assets, render_ipxe_script,
    render_tinkerbell_hardware_yaml, PxeBootMode,
};
use crate::router::{
    apply_edge_router, inspect_router_live, pits_core_default_edge_router, recover_router_runtime,
    set_crossplane_paused, validate_edge_router, EdgeRouterManifest,
};
use crate::san::{
    apply_nvmet_k8s, btrfs_maintain, btrfs_status, expected_wwid, render_nvmet_k8s_manifests,
    san_expand, san_inspect, san_provision,
};
use crate::server::{run_console_server, ConsoleState};
use crate::talos::{
    allowed_wwids_for_node, render_talos_imager_profile, render_talos_node_config,
    validate_talos_netboot_config, TalosClient,
};
use crate::transport::{AgentClient, KubeClient};
use crate::vm::{
    finalize_windows11_vm, generate_csprng_admin_password, launch_mstsc, list_cluster_vms,
    render_access_lab_vm_manifest, render_windows11_vm_manifest, verify_rdp_tls_endpoint,
    vm_lifecycle_action, write_pinned_rdp_profile, VmLifecycleOp,
};

#[derive(Parser, Debug)]
#[command(
    name = "pitsctl",
    version = "1.1.0",
    about = "Resource-Centric PITS Infrastructure Client & Embedded Backstage v1.56 Portal (paired with Go pits-agent)"
)]
struct Cli {
    /// Custom profile YAML path (defaults to `%LOCALAPPDATA%\PITS\Client\profile.yaml`)
    #[arg(long, global = true)]
    profile: Option<PathBuf>,

    /// Source `edge-router` repository path for live contract overlay
    #[arg(long, global = true)]
    workspace_source: Option<PathBuf>,

    /// PostgreSQL connection string (`postgres://...`) for sourcing DB sync (defaults to `PITS_SOURCING_DB_URL` or profile)
    #[arg(long, global = true)]
    sourcing_db_url: Option<String>,

    /// PostgreSQL connection string (`postgres://...`) for Project Hub DB sync (defaults to `PITS_HUB_DB_URL` or profile)
    #[arg(long, global = true)]
    hub_db_url: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// List all registered PITS Resource Types, entity health counts, and supported contextual actions
    Types {
        /// Output format: `table`, `wide`, `json`, or `yaml`
        #[arg(
            short = 'o',
            long = "output",
            value_parser = ["table", "wide", "json", "yaml"],
            default_value = "table"
        )]
        output: String,
        /// Force live telemetry refresh across cluster & agent endpoints
        #[arg(long)]
        live: bool,
    },

    /// List infrastructure entities aggregated by Resource Type (`all`, `baremetal-host`, `npu-accelerator`, `san-volume`, `cicd-pipeline`, ...)
    Get {
        /// Resource Type name (`all`, `baremetal-host`, `npu-accelerator`, `virtual-machine`, `edge-router`, `pxe-boot-profile`, `storage-pool`, `san-volume`, `kubernetes-cluster`, `cicd-pipeline`, ...)
        #[arg(default_value = "all")]
        resource_type: String,
        /// Dual-track summary view: `auto`, `policy`, `live`, or `drift`
        #[arg(
            long = "view",
            value_parser = ["auto", "policy", "live", "drift"],
            default_value = "auto"
        )]
        view: String,
        /// Output format: `table`, `wide`, `json`, or `yaml`
        #[arg(
            short = 'o',
            long = "output",
            value_parser = ["table", "wide", "json", "yaml"],
            default_value = "table"
        )]
        output: String,
        /// Force live telemetry refresh across cluster & agent endpoints
        #[arg(long)]
        live: bool,
    },

    /// Inspect full landing details for a specific entity (Identity, Dual-Track Summary, Attributes, Relations & Actions)
    #[command(visible_alias = "inspect")]
    Describe {
        /// Target entity (`<resource_type>/<name>`, `<name>`, or `resource:default/<name>`)
        entity: String,
        /// Output format: `text`, `json`, or `yaml`
        #[arg(
            short = 'o',
            long = "output",
            value_parser = ["text", "json", "yaml"],
            default_value = "text"
        )]
        output: String,
        /// Force live telemetry refresh across cluster & agent endpoints
        #[arg(long)]
        live: bool,
    },

    /// Execute a precondition-guarded contextual Action on a specific entity
    Action {
        /// Target entity (`<resource_type>/<name>`, `<name>`, or `resource:default/<name>`)
        entity: String,
        /// Contextual action identifier (e.g. `verify-pxe-assets`, `expand-volume`, `launch-openvino-verify`)
        action_id: String,
        /// Action parameter overrides (`--set key=value`, smartly parsed as bool/int/JSON/string)
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
        /// Optional JSON object payload of parameters (merged before `--set` overrides)
        #[arg(long = "input-json")]
        input_json: Option<String>,
        /// Preview safety guards & rendered changes without mutating live infrastructure
        #[arg(long)]
        dry_run: bool,
        /// Output format: `text` or `json`
        #[arg(
            short = 'o',
            long = "output",
            value_parser = ["text", "json"],
            default_value = "text"
        )]
        output: String,
    },

    /// Export standard Backstage `catalog-info.yaml` or inspect catalog Resource Types
    Catalog {
        #[command(subcommand)]
        action: CatalogAction,
    },

    /// Start the embedded Backstage v1.56 Resource Portal & Catalog server (`http://127.0.0.1:18090`)
    Serve {
        /// One or more socket addresses to bind (comma-separated or repeated `--listen`)
        #[arg(long, default_value = "127.0.0.1:18090", value_delimiter = ',')]
        listen: Vec<SocketAddr>,
    },

    /// Start the DB-first Sourcing Governance MCP Server over Stdio (NDJSON / Content-Length)
    Mcp {
        /// Trigger an immediate PostgreSQL bi-directional sync on startup
        #[arg(long)]
        sync_on_start: bool,
    },

    /// Manage Self-Managed Project Hub (SQLite WAL + CNPG PostgreSQL sync, Markdown export, Stdio MCP)
    Hub {
        #[command(subcommand)]
        action: HubAction,
    },

    /// Manage local operator profile & zero-trust Ed25519 mTLS PKI
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },

    /// Declaratively deploy `pits-agent` onto NixOS `pits-core` (`PITS-B1` module or RKE2 DaemonSet)
    DeployNixos {
        /// Tailscale SSH target (`pits-host01` or `root@100.95.95.88`)
        #[arg(long, default_value = "pits-host01")]
        ssh_target: String,
        /// Reachable `pits-core` Tailscale or management address
        #[arg(long, default_value = "100.95.95.88")]
        agent_host: String,
        #[arg(long, default_value_t = 18443)]
        agent_port: u16,
        /// Declarative mode: `nixos-module` (`/home/libraskywalker/PITS-B1/Base`) or `rke2-daemonset`
        #[arg(long, value_parser = ["nixos-module", "rke2-daemonset"], default_value = "nixos-module")]
        mode: String,
        /// NixOS flake directory on `pits-core`
        #[arg(long, default_value = "/home/libraskywalker/PITS-B1/Base")]
        flake_dir: String,
        /// NixOS configuration attribute name in flake (`nixosConfigurations.core`)
        #[arg(long, default_value = "core")]
        flake_attr: String,
        /// Persist as boot default (`nixos-rebuild switch`); defaults to `true`
        #[arg(long, default_value_t = true)]
        switch: bool,
        /// Pull RKE2 `rke2.yaml`, Lab `kubeconfig` & `talosconfig` into `%LOCALAPPDATA%\PITS\Client`
        #[arg(long, default_value_t = true)]
        sync_credentials: bool,
        /// Force-regenerate Ed25519 mTLS PKI certificates
        #[arg(long)]
        rotate_pki: bool,
        /// Path to built Linux amd64 `pits-agent` binary
        #[arg(long, default_value = r"c:\Project\PITS\Infra\server\bin\pits-agent-linux-amd64")]
        binary: PathBuf,
        /// Preview declarative plan and verify ELF SHA-256 without connecting over SSH
        #[arg(long)]
        dry_run: bool,
    },

    // -----------------------------------------------------------------------
    // Hidden Legacy Imperative Commands (preserved for backward compatibility)
    // -----------------------------------------------------------------------
    #[command(hide = true)]
    Status,

    #[command(hide = true)]
    Import {
        #[arg(long, default_value = r"c:\Users\pitsadmin\edge-router")]
        source: PathBuf,
    },

    #[command(hide = true)]
    Router {
        #[command(subcommand)]
        action: RouterAction,
    },

    #[command(hide = true)]
    San {
        #[command(subcommand)]
        action: SanAction,
    },

    #[command(hide = true)]
    Pxe {
        #[command(subcommand)]
        action: PxeAction,
    },

    #[command(hide = true)]
    Talos {
        #[command(subcommand)]
        action: TalosAction,
    },

    #[command(hide = true)]
    K8s {
        #[command(subcommand)]
        action: K8sAction,
    },

    #[command(hide = true)]
    Vm {
        #[command(subcommand)]
        action: VmAction,
    },
}

#[derive(Subcommand, Debug)]
enum CatalogAction {
    /// Export all synthesized Backstage `System` and `Resource` entities as multi-document YAML
    Export {
        /// Optional output file path (prints to stdout if omitted)
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
        /// Force live telemetry refresh before exporting
        #[arg(long)]
        live: bool,
    },
    /// List registered Resource Types (alias for `pitsctl types`)
    Types {
        #[arg(
            short = 'o',
            long = "output",
            value_parser = ["table", "wide", "json", "yaml"],
            default_value = "table"
        )]
        output: String,
        #[arg(long)]
        live: bool,
    },
}

#[derive(Subcommand, Debug)]
enum HubAction {
    /// Inspect Project Hub SQLite WAL / FTS5 + CNPG PostgreSQL sync status
    Status,
    /// Trigger immediate SQLite WAL <-> CNPG PostgreSQL bi-directional outbox sync
    Sync {
        /// Optional postgres:// URL overriding PITS_HUB_DB_URL / profile
        #[arg(long)]
        pg_url: Option<String>,
    },
    /// Export projects, AGY-<n> tasks, AC checklists, and session logs as Markdown
    ExportMd {
        /// Optional output file path (prints to stdout if omitted)
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
    },
    /// Start the Self-Managed Project Hub MCP Server over Stdio (9 tools, human-gated)
    Mcp {
        /// Trigger an immediate PostgreSQL bi-directional sync on startup
        #[arg(long)]
        sync_on_start: bool,
    },
}

#[derive(Subcommand, Debug)]
enum ProfileAction {
    /// Show active profile paths and local state directory
    Show,
    /// Configure endpoints and kubeconfig/talosconfig paths
    Set {
        #[arg(long)]
        agent_endpoint: Option<String>,
        #[arg(long)]
        agent_token: Option<String>,
        #[arg(long)]
        core_kubeconfig: Option<String>,
        #[arg(long)]
        lab_kubeconfig: Option<String>,
        #[arg(long)]
        talosconfig: Option<String>,
        #[arg(long)]
        lab_bridge: Option<bool>,
    },
    /// Generate Ed25519 CA + Server + Client mTLS certificates and save client identity to profile
    GeneratePki {
        #[arg(long, default_value = "100.95.95.88")]
        server_san: String,
        /// Optional directory outside Syncthing to export server certs for `pits-agent`
        #[arg(long)]
        export_server_dir: Option<PathBuf>,
    },
    /// Register or update an additional Kubernetes / vCluster endpoint in profile
    AddCluster {
        #[arg(long)]
        name: String,
        #[arg(long)]
        kubeconfig: PathBuf,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        context: Option<String>,
        #[arg(long, default_value_t = false)]
        via_agent_bridge: bool,
    },
    /// Remove an additional Kubernetes cluster from profile
    RemoveCluster {
        #[arg(long)]
        name: String,
    },
}

#[derive(Subcommand, Debug)]
enum RouterAction {
    /// Render canonical `pits-core` `EdgeRouter` YAML
    Render,
    /// Validate an `EdgeRouter` YAML file against PCI, VLAN 10/20 isolation & mgmt-net rules
    Validate {
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Query live `EdgeRouter`, `Router`, 30s verification freshness, PCI bindings & GoVPP state
    Status,
    /// Validate and Server-Side Apply an `EdgeRouter` manifest (`edge/pits-core`)
    Apply {
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Pause or resume Crossplane reconciliation (`crossplane.io/paused`)
    Pause {
        #[arg(long)]
        resume: bool,
    },
    /// Run `mgmt0`-guarded VFIO restoration + RKE2 `edge/gateway` rollout verification via `pits-agent`
    Recover {
        #[arg(long, default_value_t = true)]
        cleanup_perf_netns: bool,
        #[arg(long, default_value_t = true)]
        refresh_plugins: bool,
        #[arg(long, default_value_t = 60)]
        wait_seconds: i32,
    },
    /// Inspect or capture live VPP `dpdk-input` packet traces via GoVPP binary socket
    VppTrace {
        #[arg(long, default_value_t = 50)]
        count: i32,
        #[arg(long)]
        filter: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum SanAction {
    /// Compute deterministic UUIDv5 WWIDs for all enrolled PITS namespaces
    Wwids,
    /// Render `nvmet-<device>` Kubernetes `Deployment` + `sfp0.20` Multus NAD YAML
    RenderK8s {
        #[arg(long, default_value = "netboot-004")]
        device: String,
        #[arg(long)]
        apply: bool,
    },
    /// Inspect live `configfs` subsystem, 4KiB GPT (`META`+`STATE`) & NOCOW status via `pits-agent`
    Inspect {
        #[arg(long, default_value = "netboot-004")]
        device: String,
    },
    /// Provision SAN backing extents, 4KiB system GPT, and `nvmet` target (`--hot-add` for live NSID 3)
    Provision {
        #[arg(long, default_value = "netboot-004")]
        device: String,
        #[arg(long)]
        hot_add: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Grow an existing SAN namespace additively (4KiB aligned) + trigger `revalidate_size`
    Expand {
        #[arg(long)]
        device: String,
        #[arg(long)]
        nsid: i32,
        #[arg(long)]
        size_gib: u64,
        #[arg(long)]
        dry_run: bool,
    },
    /// Inspect `pits-data` Btrfs pool (`777b7660-...`) capacity, subvolumes & sysfs error stats
    BtrfsStatus,
    /// Execute serialized `trim` (`FITRIM`) or `scrub` check on `pits-data`
    BtrfsMaintain {
        #[arg(value_parser = ["trim", "scrub"])]
        operation: String,
    },
}

#[derive(Subcommand, Debug)]
enum PxeAction {
    /// Compile deterministic iPXE boot script (`talos` or `hook`)
    RenderIpxe {
        #[arg(long, default_value = "netboot-004")]
        device: String,
        #[arg(long)]
        hook: bool,
    },
    /// Render or Server-Side Apply Tinkerbell `Hardware` CR (`tink-system`)
    Hardware {
        #[arg(long, default_value = "netboot-004")]
        device: String,
        #[arg(long)]
        hook: bool,
        #[arg(long)]
        apply: bool,
    },
    /// List live Tinkerbell `Hardware` and `Workflow` CRs on `pits-core`
    ListLive,
    /// Analyze a Hook non-destructive hardware discovery JSON file (IOMMU, NPU, Realtek, disks)
    AnalyzeInventory {
        #[arg(long)]
        file: PathBuf,
    },
    /// List Hook inventory reports stored on `pits-core` via `pits-agent`
    InventoryReports,
    /// Verify `vmlinuz` and `initramfs.xz` SHA-256 digests on `pits-core`
    VerifyAssets {
        #[arg(long, default_value = "netboot-004")]
        device: String,
    },
}

#[derive(Subcommand, Debug)]
enum TalosAction {
    /// Render `netboot-guard`-validated multi-document Talos v1.14.5 config for an enrolled node
    RenderConfig {
        #[arg(long, default_value = "netboot-004")]
        device: String,
        #[arg(long, default_value = "https://10.89.20.21:6443")]
        endpoint: String,
    },
    /// Render Sidero Imager boot profile with `pits-nvme-early` system extension
    RenderImager {
        #[arg(long, default_value = "netboot-004")]
        device: String,
    },
    /// Run native `netboot-guard` policy check on a Talos YAML file
    Guard {
        #[arg(long, default_value = "netboot-004")]
        device: String,
        #[arg(long)]
        file: PathBuf,
    },
    /// Query live Talos node `Version`, `ServiceList`, `EtcdMemberList` & COSI SAN WWIDs via gRPC
    Status {
        #[arg(long, default_value = "netboot-004")]
        device: String,
    },
    /// Run `netboot-guard` + COSI SAN WWID preflight + gRPC `ApplyConfiguration`
    Apply {
        #[arg(long)]
        device: String,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long, default_value_t = 2)]
        mode: u64,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value_t = true)]
        verify_live_wwids: bool,
    },
    /// Reboot a diskless Talos node via gRPC (`--powercycle` forces full UEFI PXE re-entry)
    Reboot {
        #[arg(long)]
        device: String,
        #[arg(long, default_value_t = true)]
        powercycle: bool,
    },
}

#[derive(Subcommand, Debug)]
enum K8sAction {
    /// Inspect both `pits-core` (RKE2 v1.35.8) and `pits-lab` (Talos v1.37.1 + NPU Slices)
    Status,
    /// Render or apply node-pinned static SAN Local `PersistentVolume` + `PersistentVolumeClaim`
    SanPv {
        #[arg(long, default_value = "pv-win11-vmstore")]
        pv_name: String,
        #[arg(long, default_value = "pits-vms")]
        namespace: String,
        #[arg(long, default_value = "win11-vmstore-pvc")]
        pvc_name: String,
        #[arg(long, default_value = "netboot-004")]
        node: String,
        #[arg(long, default_value = "pits-vm-local")]
        storage_class: String,
        #[arg(long, default_value = "/var/mnt/vmstore/win11-001")]
        host_path: String,
        #[arg(long, default_value_t = 320)]
        size_gib: u32,
        #[arg(long)]
        apply: bool,
    },
    /// Cross-verify machine models, CPUs, physical PCI NPUs, kernel drivers & DRA ResourceSlices across all 5 machines
    NpuAudit,
    /// Render or launch Intel NPU DRA v1 (`resource.k8s.io/v1`) `ResourceClaim` + UID 1000 OpenVINO `Job`
    NpuVerify {
        #[arg(long, default_value = "netboot-002")]
        node: String,
        #[arg(long, default_value = "pits-intel-npu")]
        device_class: String,
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand, Debug)]
enum VmAction {
    /// List KubeVirt `VirtualMachine` & `VirtualMachineInstance` states across `core` and `lab`
    List,
    /// Render enrolled KubeVirt VM manifests (`win11-001` or `access-lab`)
    Render {
        #[arg(value_parser = ["win11-install", "win11-final", "access-lab"])]
        template: String,
    },
    /// Generate a CSPRNG 20-char Administrator password for Windows 11 unattended sysprep
    GenAdminPassword,
    /// Start, stop, restart, pause, or unpause a KubeVirt VM
    Lifecycle {
        #[arg(long, value_parser = ["core", "lab"], default_value = "lab")]
        cluster: String,
        #[arg(long, default_value = "pits-vms")]
        namespace: String,
        #[arg(long, default_value = "win11-001")]
        name: String,
        #[arg(value_parser = ["start", "stop", "restart", "pause", "unpause"])]
        op: String,
    },
    /// Detach installation/virtio/sysprep CD-ROM ISOs from `pits-vms/win11-001` post-install
    FinalizeWin11,
    /// Perform native RDP X.224 + TLS handshake, verify X.509 thumbprint, write 3072×1920 `.rdp`, and optionally launch `mstsc.exe`
    Rdp {
        #[arg(long, default_value = "10.89.20.24")]
        host: String,
        #[arg(long, default_value_t = 30389)]
        port: u16,
        #[arg(long, default_value = "pitsadmin")]
        username: String,
        #[arg(long)]
        launch: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut profile = ClientProfile::load_or_default(cli.profile.as_deref())?;
    let resolved_sourcing_db_url = effective_sourcing_db_url(cli.sourcing_db_url.as_deref(), &profile);
    let resolved_hub_db_url = effective_hub_db_url(cli.hub_db_url.as_deref(), &profile);
    let resolved_hub_mcp_token = effective_hub_mcp_token(&profile);
    let runtime_profile = |base: &ClientProfile| -> ClientProfile {
        let mut out = base.clone();
        out.sourcing_db_url = resolved_sourcing_db_url.clone();
        out.hub_db_url = resolved_hub_db_url.clone();
        out.hub_mcp_token = resolved_hub_mcp_token.clone();
        out
    };

    let ws_path = cli
        .workspace_source
        .clone()
        .or_else(|| profile.active_workspace_source.as_ref().map(PathBuf::from));
    let bundle = match ws_path {
        Some(ref p) => WorkspaceBundle::load_from_edge_router(p)?,
        None => WorkspaceBundle::builtin_pits_baseline(),
    };

    match cli.command {
        Commands::Types { output, live } => {
            let registry = build_registry(runtime_profile(&profile), bundle);
            run_types_command(&registry, &output, live).await?;
        }

        Commands::Get {
            resource_type,
            view,
            output,
            live,
        } => {
            let registry = build_registry(runtime_profile(&profile), bundle);
            run_get_command(&registry, &resource_type, &view, &output, live).await?;
        }

        Commands::Describe {
            entity,
            output,
            live,
        } => {
            let registry = build_registry(runtime_profile(&profile), bundle);
            let (namespace, target) = parse_entity_arg(&entity);
            let detail = registry
                .get_pits_entity_detail(&namespace, &target, live)
                .await?;
            match output.as_str() {
                "json" => println!("{}", serde_json::to_string_pretty(&detail)?),
                "yaml" => print!("{}", serde_yaml::to_string(&detail)?),
                _ => print_entity_detail_text(&detail),
            }
        }

        Commands::Action {
            entity,
            action_id,
            set,
            input_json,
            dry_run,
            output,
        } => {
            let registry = build_registry(runtime_profile(&profile), bundle);
            let (namespace, target) = parse_entity_arg(&entity);
            let parameters = build_action_parameters(input_json.as_deref(), &set)?;
            let req = ActionInvokeRequest {
                parameters,
                dry_run,
            };
            let result = registry
                .execute_entity_action(&namespace, &target, &action_id, req)
                .await?;
            match output.as_str() {
                "json" => println!("{}", serde_json::to_string_pretty(&result)?),
                _ => print_action_result_text(&result),
            }
            if !result.success {
                std::process::exit(1);
            }
        }

        Commands::Catalog { action } => {
            let registry = build_registry(runtime_profile(&profile), bundle);
            match action {
                CatalogAction::Export { output, live } => {
                    let yaml = registry.export_catalog_yaml(live).await?;
                    if let Some(dest) = output {
                        if let Some(parent) = dest.parent() {
                            if !parent.as_os_str().is_empty() {
                                fs::create_dir_all(parent)?;
                            }
                        }
                        fs::write(&dest, &yaml)
                            .with_context(|| format!("write catalog YAML {}", dest.display()))?;
                        println!("Exported Backstage catalog YAML to {}", dest.display());
                    } else {
                        print!("{yaml}");
                    }
                }
                CatalogAction::Types { output, live } => {
                    run_types_command(&registry, &output, live).await?;
                }
            }
        }

        Commands::Serve { listen } => {
            let state = ConsoleState::new(runtime_profile(&profile), bundle);
            run_console_server(&listen, state).await?;
        }

        Commands::Mcp { sync_on_start } => {
            let registry = build_registry(runtime_profile(&profile), bundle);
            if sync_on_start {
                let _ = registry.sourcing_sync_db(None).await;
            }
            crate::mcp::run_mcp_stdio(registry).await?;
        }

        Commands::Hub { action } => {
            let hub = HubRegistry::new(Arc::new(RwLock::new(runtime_profile(&profile))));
            match action {
                HubAction::Status => {
                    let st = hub
                        .db
                        .status()
                        .map_err(|e| anyhow!("hub status failed: {e}"))?;
                    println!("{}", serde_json::to_string_pretty(&st)?);
                }
                HubAction::Sync { pg_url } => {
                    let st = hub.sync_db(pg_url.as_deref()).await;
                    println!("{}", serde_json::to_string_pretty(&st)?);
                }
                HubAction::ExportMd { output } => {
                    let md = hub
                        .db
                        .export_markdown()
                        .map_err(|e| anyhow!("hub export-md failed: {e}"))?;
                    if let Some(path) = output {
                        fs::write(&path, &md)
                            .with_context(|| format!("write {}", path.display()))?;
                        println!("Exported Project Hub Markdown to {}", path.display());
                    } else {
                        print!("{md}");
                    }
                }
                HubAction::Mcp { sync_on_start } => {
                    if sync_on_start {
                        let _ = hub.sync_db(None).await;
                    }
                    crate::hub::run_hub_mcp_stdio(hub).await?;
                }
            }
        }

        Commands::Profile { action } => match action {
            ProfileAction::Show => {
                let has_pki = profile.agent_ca_pem.is_some()
                    && profile.agent_client_cert_pem.is_some()
                    && profile.agent_client_key_pem.is_some();
                let view = serde_json::json!({
                    "profilePath": cli.profile.unwrap_or_else(default_profile_path),
                    "localStateDir": local_state_dir(),
                    "activeWorkspaceSource": profile.active_workspace_source,
                    "agentEndpoint": profile.agent_endpoint,
                    "hasBearerToken": profile.agent_bearer_token.is_some(),
                    "hasClientPki": has_pki,
                    "sourcingDbConfigured": resolved_sourcing_db_url.is_some(),
                    "sourcingDbEndpointRedacted": resolved_sourcing_db_url.as_deref().map(crate::catalog::redact_db_url),
                    "coreKubeconfigPath": profile.core_kubeconfig_path,
                    "coreContext": profile.core_context,
                    "labKubeconfigPath": profile.lab_kubeconfig_path,
                    "labContext": profile.lab_context,
                    "labViaAgentBridge": profile.lab_via_agent_bridge,
                    "talosconfigPath": profile.talosconfig_path,
                    "talosContext": profile.talos_context,
                    "talosViaAgentBridge": profile.talos_via_agent_bridge,
                    "extraClusters": profile.extra_clusters,
                });
                print!("{}", serde_yaml::to_string(&view)?);
            }
            ProfileAction::Set {
                agent_endpoint,
                agent_token,
                core_kubeconfig,
                lab_kubeconfig,
                talosconfig,
                lab_bridge,
            } => {
                if let Some(v) = agent_endpoint {
                    profile.agent_endpoint = v;
                }
                if let Some(v) = agent_token {
                    profile.agent_bearer_token = Some(v);
                }
                if let Some(v) = core_kubeconfig {
                    profile.core_kubeconfig_path = Some(v);
                }
                if let Some(v) = lab_kubeconfig {
                    profile.lab_kubeconfig_path = Some(v);
                }
                if let Some(v) = talosconfig {
                    profile.talosconfig_path = Some(v);
                }
                if let Some(v) = lab_bridge {
                    profile.lab_via_agent_bridge = v;
                    profile.talos_via_agent_bridge = v;
                }
                let p = profile.save(cli.profile.as_deref())?;
                println!("Saved profile to {}", p.display());
            }
            ProfileAction::GeneratePki {
                server_san,
                export_server_dir,
            } => {
                let pki = generate_agent_mtls_pki(&server_san)?;
                profile.agent_ca_pem = Some(pki.ca_cert_pem.clone());
                profile.agent_client_cert_pem = Some(pki.client_cert_pem.clone());
                profile.agent_client_key_pem = Some(pki.client_key_pem.clone());
                let saved = profile.save(cli.profile.as_deref())?;

                let out_dir =
                    export_server_dir.unwrap_or_else(|| local_state_dir().join("agent-pki"));
                fs::create_dir_all(&out_dir)?;
                fs::write(out_dir.join("ca.crt"), &pki.ca_cert_pem)?;
                fs::write(out_dir.join("server.crt"), &pki.server_cert_pem)?;
                fs::write(out_dir.join("server.key"), &pki.server_key_pem)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "profileUpdated": saved,
                        "exportedAgentServerCertsDir": out_dir,
                    }))?
                );
            }
            ProfileAction::AddCluster {
                name,
                kubeconfig,
                title,
                role,
                context,
                via_agent_bridge,
            } => {
                let entry = crate::config::ExtraClusterConfig {
                    name: name.trim().to_string(),
                    title,
                    role,
                    kubeconfig_path: kubeconfig.display().to_string(),
                    context,
                    via_agent_bridge,
                };
                if let Some(existing) = profile
                    .extra_clusters
                    .iter_mut()
                    .find(|c| c.name.eq_ignore_ascii_case(&entry.name))
                {
                    *existing = entry;
                } else {
                    profile.extra_clusters.push(entry);
                }
                let p = profile.save(cli.profile.as_deref())?;
                println!("Saved cluster {} to {}", name, p.display());
            }
            ProfileAction::RemoveCluster { name } => {
                profile
                    .extra_clusters
                    .retain(|c| !c.name.eq_ignore_ascii_case(name.trim()));
                let p = profile.save(cli.profile.as_deref())?;
                println!("Updated profile {}", p.display());
            }
        },

        Commands::DeployNixos {
            ssh_target,
            agent_host,
            agent_port,
            mode,
            flake_dir,
            flake_attr,
            switch,
            sync_credentials,
            rotate_pki,
            binary,
            dry_run,
        } => {
            let deploy_mode = match mode.as_str() {
                "rke2-daemonset" => NixosDeployMode::Rke2Daemonset,
                _ => NixosDeployMode::NixosModule,
            };
            let opts = NixosDeployOptions {
                ssh_target,
                agent_host,
                agent_port,
                flake_dir,
                flake_attr,
                mode: deploy_mode,
                switch_generation: switch,
                sync_credentials,
                rotate_pki,
                binary_path: binary,
                dry_run,
            };
            let report =
                deploy_to_nixos_pits_core(&mut profile, cli.profile.as_deref(), &opts).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }

        // -------------------------------------------------------------------
        // Hidden Legacy Commands
        // -------------------------------------------------------------------
        Commands::Status => {
            let agent_health = match AgentClient::from_profile(&profile) {
                Ok(ag) => ag.get_json::<serde_json::Value>("/v1/health").await.ok(),
                Err(_) => None,
            };
            let er_rep = validate_edge_router(&pits_core_default_edge_router());
            let out = serde_json::json!({
                "client": "pitsctl (Rust)",
                "server": "pits-agent (Go)",
                "localStateDir": local_state_dir(),
                "profilePath": cli.profile.unwrap_or_else(default_profile_path),
                "workspaceSource": bundle.source_dir,
                "importedFiles": bundle.imported_files,
                "agentOnline": agent_health.is_some(),
                "agentHealth": agent_health,
                "routerBaselinePolicyValid": er_rep.valid,
                "enrolledNodes": bundle.nodes.iter().map(|n| serde_json::json!({
                    "deviceId": n.device_id,
                    "model": n.model,
                    "cpuModel": n.cpu_model,
                    "npuVendor": n.npu_vendor,
                    "npuPciId": n.npu_pci_id,
                    "hasIntelNpu": n.has_intel_npu,
                    "nodeIp": n.node_ip,
                    "sanTarget": format!("{}:{}", n.target_ip, n.san.port),
                    "pxeMac": n.pxe_mac,
                    "wwids": allowed_wwids_for_node(n),
                })).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }

        Commands::Import { source } => {
            let imported = WorkspaceBundle::load_from_edge_router(&source)?;
            profile.active_workspace_source = Some(source.display().to_string());
            let saved = profile.save(cli.profile.as_deref())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "savedProfile": saved,
                    "sourceDir": imported.source_dir,
                    "importedFiles": imported.imported_files,
                    "nodes": imported.nodes,
                }))?
            );
        }

        Commands::Router { action } => match action {
            RouterAction::Render => {
                println!("{}", serde_yaml::to_string(&pits_core_default_edge_router())?);
            }
            RouterAction::Validate { file } => {
                let er = load_or_default_edge_router(file.as_deref())?;
                let rep = validate_edge_router(&er);
                println!("{}", serde_json::to_string_pretty(&rep)?);
                if !rep.valid {
                    std::process::exit(1);
                }
            }
            RouterAction::Status => {
                let kc = build_core_k8s_opt(&profile);
                let ag = AgentClient::from_profile(&profile).ok();
                let st = inspect_router_live(kc.as_ref(), ag.as_ref()).await;
                println!("{}", serde_json::to_string_pretty(&st)?);
            }
            RouterAction::Apply { file } => {
                let er = load_or_default_edge_router(file.as_deref())?;
                let kc = build_core_k8s_req(&profile)?;
                let out = apply_edge_router(&kc, &er).await?;
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
            RouterAction::Pause { resume } => {
                let kc = build_core_k8s_req(&profile)?;
                set_crossplane_paused(&kc, "edge", "pits-core", !resume).await?;
                println!("{}", serde_json::json!({ "crossplanePaused": !resume }));
            }
            RouterAction::Recover {
                cleanup_perf_netns,
                refresh_plugins,
                wait_seconds,
            } => {
                let ag = AgentClient::from_profile(&profile)?;
                let out =
                    recover_router_runtime(&ag, cleanup_perf_netns, refresh_plugins, wait_seconds)
                        .await?;
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
            RouterAction::VppTrace { count, filter } => {
                let ag = AgentClient::from_profile(&profile)?;
                let req = serde_json::json!({
                    "packetCount": count,
                    "filterText": filter.unwrap_or_default(),
                });
                let out: serde_json::Value = ag.post_json("/v1/vpp/trace", &req).await?;
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
        },

        Commands::San { action } => match action {
            SanAction::Wwids => {
                let mut rows = Vec::new();
                for n in &bundle.nodes {
                    for ns in &n.san.namespaces {
                        rows.push(serde_json::json!({
                            "device": n.device_id,
                            "nsid": ns.id,
                            "name": ns.name,
                            "nqn": n.san.subsystem_nqn,
                            "wwid": expected_wwid(&n.san.subsystem_nqn, &ns.name),
                        }));
                    }
                }
                println!("{}", serde_json::to_string_pretty(&rows)?);
            }
            SanAction::RenderK8s { device, apply } => {
                let node = bundle
                    .find_node(&device)
                    .ok_or_else(|| anyhow!("unknown device {device}"))?;
                let yaml = render_nvmet_k8s_manifests(&node.san)?;
                if apply {
                    let kc = build_core_k8s_req(&profile)?;
                    apply_nvmet_k8s(&kc, &node.san).await?;
                    println!("Applied nvmet-{device} manifests to tink-system");
                } else {
                    println!("{yaml}");
                }
            }
            SanAction::Inspect { device } => {
                let node = bundle
                    .find_node(&device)
                    .ok_or_else(|| anyhow!("unknown device {device}"))?;
                let ag = AgentClient::from_profile(&profile)?;
                let out = san_inspect(&ag, &node.san).await?;
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
            SanAction::Provision {
                device,
                hot_add,
                dry_run,
            } => {
                let node = bundle
                    .find_node(&device)
                    .ok_or_else(|| anyhow!("unknown device {device}"))?;
                let ag = AgentClient::from_profile(&profile)?;
                let out = san_provision(&ag, &node.san, hot_add, !hot_add, dry_run).await?;
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
            SanAction::Expand {
                device,
                nsid,
                size_gib,
                dry_run,
            } => {
                let ag = AgentClient::from_profile(&profile)?;
                let bytes = (size_gib as i64) * 1024 * 1024 * 1024;
                let out = san_expand(&ag, &device, nsid, bytes, dry_run).await?;
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
            SanAction::BtrfsStatus => {
                let ag = AgentClient::from_profile(&profile)?;
                let out = btrfs_status(&ag).await?;
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
            SanAction::BtrfsMaintain { operation } => {
                let ag = AgentClient::from_profile(&profile)?;
                let out = btrfs_maintain(&ag, &operation).await?;
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
        },

        Commands::Pxe { action } => match action {
            PxeAction::RenderIpxe { device, hook } => {
                let node = bundle
                    .find_node(&device)
                    .ok_or_else(|| anyhow!("unknown device {device}"))?;
                let mode = if hook {
                    PxeBootMode::HookInventory
                } else {
                    PxeBootMode::TalosNetboot
                };
                print!("{}", render_ipxe_script(node, mode, None));
            }
            PxeAction::Hardware {
                device,
                hook,
                apply,
            } => {
                let node = bundle
                    .find_node(&device)
                    .ok_or_else(|| anyhow!("unknown device {device}"))?;
                let mode = if hook {
                    PxeBootMode::HookInventory
                } else {
                    PxeBootMode::TalosNetboot
                };
                if apply {
                    let kc = build_core_k8s_req(&profile)?;
                    let out = apply_tinkerbell_hardware(&kc, node, mode, None).await?;
                    println!("{}", serde_json::to_string_pretty(&out)?);
                } else {
                    print!("{}", render_tinkerbell_hardware_yaml(node, mode, None));
                }
            }
            PxeAction::ListLive => {
                let kc = build_core_k8s_req(&profile)?;
                let hw = list_tinkerbell_hardware(&kc).await?;
                let wf = list_tinkerbell_workflows(&kc).await?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "hardware": hw,
                        "workflows": wf,
                    }))?
                );
            }
            PxeAction::AnalyzeInventory { file } => {
                let raw = fs::read_to_string(&file)
                    .with_context(|| format!("read {}", file.display()))?;
                let val: serde_json::Value = serde_json::from_str(&raw)?;
                let rep = analyze_hook_inventory(&val)?;
                println!("{}", serde_json::to_string_pretty(&rep)?);
            }
            PxeAction::InventoryReports => {
                let ag = AgentClient::from_profile(&profile)?;
                let out = pxe_list_inventory(&ag).await?;
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
            PxeAction::VerifyAssets { device } => {
                let ag = AgentClient::from_profile(&profile)?;
                let out = pxe_verify_assets(&ag, &device, BTreeMap::new()).await?;
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
        },

        Commands::Talos { action } => match action {
            TalosAction::RenderConfig { device, endpoint } => {
                let node = bundle
                    .find_node(&device)
                    .ok_or_else(|| anyhow!("unknown device {device}"))?;
                print!("{}", render_talos_node_config(node, &endpoint)?);
            }
            TalosAction::RenderImager { device } => {
                let node = bundle
                    .find_node(&device)
                    .ok_or_else(|| anyhow!("unknown device {device}"))?;
                print!("{}", render_talos_imager_profile(node, true));
            }
            TalosAction::Guard { device, file } => {
                let node = bundle
                    .find_node(&device)
                    .ok_or_else(|| anyhow!("unknown device {device}"))?;
                let yaml = fs::read_to_string(&file)?;
                let rep = validate_talos_netboot_config(
                    &yaml,
                    &allowed_wwids_for_node(node),
                    &node.approved_vfio_pci,
                );
                println!("{}", serde_json::to_string_pretty(&rep)?);
                if !rep.passed {
                    std::process::exit(1);
                }
            }
            TalosAction::Status { device } => {
                let node_ip = bundle
                    .find_node(&device)
                    .map(|n| n.node_ip.clone())
                    .unwrap_or(device);
                let tc = build_talos_req(&profile)?;
                let out = tc.inspect_node(&node_ip).await;
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
            TalosAction::Apply {
                device,
                file,
                mode,
                dry_run,
                verify_live_wwids,
            } => {
                let node = bundle
                    .find_node(&device)
                    .ok_or_else(|| anyhow!("unknown device {device}"))?;
                let yaml = match file {
                    Some(p) => fs::read_to_string(p)?,
                    None => render_talos_node_config(node, "https://10.89.20.21:6443")?,
                };
                let tc = build_talos_req(&profile)?;
                let msgs = tc
                    .apply_configuration(node, &yaml, mode, dry_run, verify_live_wwids)
                    .await?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "device": device,
                        "applied": true,
                        "dryRun": dry_run,
                        "messages": msgs,
                    }))?
                );
            }
            TalosAction::Reboot { device, powercycle } => {
                let node_ip = bundle
                    .find_node(&device)
                    .map(|n| n.node_ip.clone())
                    .unwrap_or(device);
                let tc = build_talos_req(&profile)?;
                tc.reboot_node(&node_ip, powercycle).await?;
                println!(
                    "{}",
                    serde_json::json!({ "rebooted": node_ip, "powercycle": powercycle })
                );
            }
        },

        Commands::K8s { action } => match action {
            K8sAction::Status => {
                let core = match build_core_k8s_req(&profile) {
                    Ok(kc) => serde_json::to_value(inspect_cluster_summary(&kc).await)?,
                    Err(e) => serde_json::json!({ "reachable": false, "error": e.to_string() }),
                };
                let lab = match build_lab_k8s_req(&profile) {
                    Ok(kc) => serde_json::to_value(inspect_cluster_summary(&kc).await)?,
                    Err(e) => serde_json::json!({ "reachable": false, "error": e.to_string() }),
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "pitsCoreRke2": core,
                        "pitsLabTalos": lab,
                    }))?
                );
            }
            K8sAction::SanPv {
                pv_name,
                namespace,
                pvc_name,
                node,
                storage_class,
                host_path,
                size_gib,
                apply,
            } => {
                if apply {
                    let kc = build_lab_k8s_req(&profile)?;
                    apply_san_local_pv_pvc(
                        &kc,
                        &pv_name,
                        &namespace,
                        &pvc_name,
                        &node,
                        &storage_class,
                        &host_path,
                        size_gib,
                    )
                    .await?;
                    println!("Applied SAN PV {pv_name} and PVC {namespace}/{pvc_name}");
                } else {
                    print!(
                        "{}",
                        render_san_local_pv_pvc(
                            &pv_name,
                            &namespace,
                            &pvc_name,
                            &node,
                            &storage_class,
                            &host_path,
                            size_gib
                        )
                    );
                }
            }
            K8sAction::NpuAudit => {
                let ag = AgentClient::from_profile(&profile).ok();
                let core = build_core_k8s_opt(&profile);
                let lab = build_lab_k8s_req(&profile).ok();
                let tc = build_talos_req(&profile).ok();
                let report = audit_fleet_npus(
                    &bundle,
                    ag.as_ref(),
                    core.as_ref(),
                    lab.as_ref(),
                    tc.as_ref(),
                )
                .await;
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
            K8sAction::NpuVerify {
                node,
                device_class,
                apply,
            } => {
                let claim = format!("npu-claim-{node}");
                let job = format!("npu-verify-{node}");
                if apply {
                    let kc = build_lab_k8s_req(&profile)?;
                    launch_npu_validation_job(
                        &kc,
                        "default",
                        &claim,
                        &job,
                        &device_class,
                        Some(&node),
                    )
                    .await?;
                    println!("Launched Intel NPU validation job default/{job}");
                } else {
                    print!(
                        "{}",
                        render_npu_validation_manifests(
                            "default",
                            &claim,
                            &job,
                            &device_class,
                            Some(&node)
                        )
                    );
                }
            }
        },

        Commands::Vm { action } => match action {
            VmAction::List => {
                let mut vms = Vec::new();
                if let Ok(kc) = build_core_k8s_req(&profile) {
                    if let Ok(v) = list_cluster_vms(&kc).await {
                        vms.extend(v);
                    }
                }
                if let Ok(kc) = build_lab_k8s_req(&profile) {
                    if let Ok(v) = list_cluster_vms(&kc).await {
                        vms.extend(v);
                    }
                }
                println!("{}", serde_json::to_string_pretty(&vms)?);
            }
            VmAction::Render { template } => match template.as_str() {
                "win11-install" => print!("{}", render_windows11_vm_manifest(true)),
                "win11-final" => print!("{}", render_windows11_vm_manifest(false)),
                "access-lab" => print!("{}", render_access_lab_vm_manifest()),
                _ => unreachable!(),
            },
            VmAction::GenAdminPassword => {
                println!("{}", generate_csprng_admin_password());
            }
            VmAction::Lifecycle {
                cluster,
                namespace,
                name,
                op,
            } => {
                let kc = if cluster == "core" {
                    build_core_k8s_req(&profile)?
                } else {
                    build_lab_k8s_req(&profile)?
                };
                let parsed_op = match op.as_str() {
                    "start" => VmLifecycleOp::Start,
                    "stop" => VmLifecycleOp::Stop,
                    "restart" => VmLifecycleOp::Restart,
                    "pause" => VmLifecycleOp::Pause,
                    "unpause" => VmLifecycleOp::Unpause,
                    _ => unreachable!(),
                };
                vm_lifecycle_action(&kc, &namespace, &name, parsed_op).await?;
                println!("Executed {op} on {cluster}:{namespace}/{name}");
            }
            VmAction::FinalizeWin11 => {
                let kc = build_lab_k8s_req(&profile)?;
                finalize_windows11_vm(&kc).await?;
                println!("Finalized pits-vms/win11-001 (installation CD-ROM media detached)");
            }
            VmAction::Rdp {
                host,
                port,
                username,
                launch,
            } => {
                let bridge_ag = if profile.lab_via_agent_bridge {
                    AgentClient::from_profile(&profile).ok()
                } else {
                    None
                };
                let bridge_pair = bridge_ag
                    .as_ref()
                    .map(|ag| (&profile, ag.tls_config.clone()));
                let tls_info = verify_rdp_tls_endpoint(&host, port, bridge_pair).await?;
                let rdp_path = write_pinned_rdp_profile(
                    "win11-001",
                    &host,
                    port,
                    &username,
                    Some(&tls_info.cert_sha1_hex),
                    3072,
                    1920,
                )?;
                if launch {
                    launch_mstsc(&rdp_path)?;
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "rdpTls": tls_info,
                        "rdpFile": rdp_path,
                        "launchedMstsc": launch,
                    }))?
                );
            }
        },
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Resource-Centric CLI Helpers & Formatters
// ---------------------------------------------------------------------------

fn build_registry(profile: ClientProfile, bundle: WorkspaceBundle) -> CatalogRegistry {
    CatalogRegistry::new(Arc::new(RwLock::new(profile)), Arc::new(RwLock::new(bundle)))
}

/// Parses user entity target (`netboot-004`, `baremetal-host/netboot-004`, or `resource:default/netboot-004`).
fn parse_entity_arg(raw: &str) -> (String, String) {
    let trimmed = raw.trim();
    if let Some((_kind, rest)) = trimmed.split_once(':') {
        if let Some((ns, name)) = rest.split_once('/') {
            return (ns.to_string(), name.to_string());
        }
        return ("default".to_string(), rest.to_string());
    }
    ("default".to_string(), trimmed.to_string())
}

fn parse_smart_param_value(raw: &str) -> serde_json::Value {
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("true") {
        return serde_json::Value::Bool(true);
    }
    if trimmed.eq_ignore_ascii_case("false") {
        return serde_json::Value::Bool(false);
    }
    if let Ok(i) = trimmed.parse::<i64>() {
        return serde_json::Value::Number(i.into());
    }
    if let Ok(u) = trimmed.parse::<u64>() {
        return serde_json::Value::Number(u.into());
    }
    if trimmed.contains('.') && !trimmed.contains("..") {
        if let Ok(f) = trimmed.parse::<f64>() {
            if let Some(num) = serde_json::Number::from_f64(f) {
                return serde_json::Value::Number(num);
            }
        }
    }
    if (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
    {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            return val;
        }
    }
    serde_json::Value::String(raw.to_string())
}

fn build_action_parameters(
    input_json: Option<&str>,
    sets: &[String],
) -> Result<BTreeMap<String, serde_json::Value>> {
    let mut params = BTreeMap::new();
    if let Some(raw_json) = input_json {
        let content = if let Some(path) = raw_json.strip_prefix('@') {
            fs::read_to_string(path).with_context(|| format!("read --input-json file {path}"))?
        } else {
            raw_json.to_string()
        };
        let val: serde_json::Value =
            serde_json::from_str(&content).context("parse --input-json as JSON")?;
        let obj = val
            .as_object()
            .ok_or_else(|| anyhow!("--input-json must be a JSON object"))?;
        for (k, v) in obj {
            params.insert(k.clone(), v.clone());
        }
    }

    for entry in sets {
        let (k, v_raw) = entry
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid --set {:?}; expected KEY=VALUE", entry))?;
        let key = k.trim();
        if key.is_empty() {
            bail!("invalid --set {:?}; parameter key cannot be empty", entry);
        }
        params.insert(key.to_string(), parse_smart_param_value(v_raw));
    }

    Ok(params)
}

async fn run_types_command(registry: &CatalogRegistry, output: &str, live: bool) -> Result<()> {
    let types = registry.list_resource_types(live).await;
    match output {
        "json" => println!("{}", serde_json::to_string_pretty(&types)?),
        "yaml" => print!("{}", serde_yaml::to_string(&types)?),
        "wide" => print_resource_types_table(&types, true),
        _ => print_resource_types_table(&types, false),
    }
    Ok(())
}

fn format_type_highlights(t: &ResourceTypeSummary) -> String {
    if t.highlights.is_empty() {
        return "-".to_string();
    }
    t.highlights
        .iter()
        .map(|p| format!("{}={}", p.label, p.value))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn print_resource_types_table(types: &[ResourceTypeSummary], wide: bool) {
    if wide {
        let headers = [
            "RESOURCE TYPE",
            "CATEGORY",
            "MODE",
            "ENTITIES",
            "HEALTH",
            "DRIFT",
            "HIGHLIGHTS",
            "ACTIONS",
        ];
        let rows: Vec<[String; 8]> = types
            .iter()
            .map(|t| {
                [
                    t.resource_type.clone(),
                    t.category.clone(),
                    t.observation_mode.clone(),
                    t.entity_count.to_string(),
                    format!("{} ok / {} warn", t.healthy_count, t.warning_count),
                    if t.drifted_count > 0 {
                        format!("{} DRIFT", t.drifted_count)
                    } else {
                        "0".to_string()
                    },
                    format_type_highlights(t),
                    t.action_ids.join(", "),
                ]
            })
            .collect();
        render_aligned_table(&headers, &rows);
    } else {
        let headers = [
            "RESOURCE TYPE",
            "MODE",
            "ENTITIES",
            "HEALTH",
            "DRIFT",
            "HIGHLIGHTS",
        ];
        let rows: Vec<[String; 6]> = types
            .iter()
            .map(|t| {
                [
                    t.resource_type.clone(),
                    t.observation_mode.clone(),
                    t.entity_count.to_string(),
                    format!("{} ok / {} warn", t.healthy_count, t.warning_count),
                    if t.drifted_count > 0 {
                        format!("{} DRIFT", t.drifted_count)
                    } else {
                        "0".to_string()
                    },
                    format_type_highlights(t),
                ]
            })
            .collect();
        render_aligned_table(&headers, &rows);
    }

    let total_entities: usize = types.iter().map(|t| t.entity_count).sum();
    let total_drifted: usize = types.iter().map(|t| t.drifted_count).sum();
    println!(
        "\nTotal: {} resource types, {} resource entities, {} drifted",
        types.len(),
        total_entities,
        total_drifted
    );
}

fn format_policy_pills(detail: &PitsEntityDetail, limit: usize) -> String {
    let parts: Vec<String> = detail
        .summary
        .static_policy
        .iter()
        .take(limit)
        .map(|p| format!("{}={}", p.label, p.value))
        .collect();
    if parts.is_empty() {
        detail.summary_line.clone()
    } else {
        parts.join(" · ")
    }
}

fn format_dynamic_signals(detail: &PitsEntityDetail, limit: usize) -> String {
    let parts: Vec<String> = detail
        .summary
        .dynamic_signals
        .iter()
        .take(limit)
        .map(|s| match &s.delta {
            Some(d) if !d.is_empty() => format!("{}={} ({})", s.label, s.value, d),
            _ => format!("{}={}", s.label, s.value),
        })
        .collect();
    if parts.is_empty() {
        detail.summary_line.clone()
    } else {
        parts.join(" · ")
    }
}

pub(crate) fn format_entity_summary_for_view(detail: &PitsEntityDetail, view: &str) -> String {
    match view {
        "policy" => format_policy_pills(detail, usize::MAX),
        "live" => format_dynamic_signals(detail, usize::MAX),
        "drift" => {
            if detail.summary.drift_detected {
                format!(
                    "DRIFT: {}",
                    detail
                        .summary
                        .drift_summary
                        .as_deref()
                        .unwrap_or("policy or runtime drift detected")
                )
            } else {
                "Compliant (No Drift)".to_string()
            }
        }
        _ => {
            use crate::catalog::ObservationMode;
            match detail.summary.observation_mode {
                ObservationMode::PolicyFirst => {
                    let drift_tag = if detail.summary.drift_detected {
                        format!(
                            "DRIFT: {}",
                            detail.summary.drift_summary.as_deref().unwrap_or("yes")
                        )
                    } else {
                        "Compliant".to_string()
                    };
                    format!("[Policy] {} | {}", format_policy_pills(detail, 3), drift_tag)
                }
                ObservationMode::DynamicFirst => {
                    format!(
                        "[Live] {} | [Policy] {}",
                        format_dynamic_signals(detail, 3),
                        format_policy_pills(detail, 2)
                    )
                }
                ObservationMode::Hybrid => {
                    format!(
                        "[Policy] {} | [Live] {}",
                        format_policy_pills(detail, 2),
                        format_dynamic_signals(detail, 2)
                    )
                }
                ObservationMode::GovernanceEvent => {
                    format!(
                        "[Gov] {} | [Events] {}",
                        format_policy_pills(detail, 2),
                        format_dynamic_signals(detail, 2)
                    )
                }
            }
        }
    }
}

async fn run_get_command(
    registry: &CatalogRegistry,
    requested_type: &str,
    view: &str,
    output: &str,
    live: bool,
) -> Result<()> {
    let all_entities = registry.list_entities(live).await;
    let req_lower = requested_type.trim().to_ascii_lowercase();
    let req_norm = match req_lower.as_str() {
        "cicd" | "ci-cd" | "cicd-pipelines" => "cicd-pipeline",
        "aistorage" | "ai-storage" | "aistorageworkspace" | "ai-storage-workspaces" => {
            "ai-storage-workspace"
        }
        "authflow" | "auth-flow" | "authflowbundle" | "auth-flow-bundles" => "auth-flow-bundle",
        _ => req_lower.strip_suffix('s').unwrap_or(&req_lower),
    };

    let filtered: Vec<_> = if req_lower == "all" || req_lower == "*" {
        all_entities
            .into_iter()
            .filter(|e| e.kind.eq_ignore_ascii_case("Resource"))
            .collect()
    } else if req_lower == "crossplane" || req_lower == "xp" {
        all_entities
            .into_iter()
            .filter(|e| {
                e.metadata.tags.iter().any(|t| t == "crossplane")
                    || e.spec
                        .get("type")
                        .and_then(|v| v.as_str())
                        .map(|t| {
                            t.starts_with("crossplane-")
                                || t == "ai-storage-workspace"
                                || t == "auth-flow-bundle"
                        })
                        .unwrap_or(false)
            })
            .collect()
    } else if req_lower == "domain" || req_lower == "domains" {
        all_entities
            .into_iter()
            .filter(|e| e.kind.eq_ignore_ascii_case("Domain"))
            .collect()
    } else if req_lower == "system" || req_lower == "systems" {
        all_entities
            .into_iter()
            .filter(|e| e.kind.eq_ignore_ascii_case("System"))
            .collect()
    } else if req_lower == "component" || req_lower == "components" {
        all_entities
            .into_iter()
            .filter(|e| e.kind.eq_ignore_ascii_case("Component"))
            .collect()
    } else {
        all_entities
            .into_iter()
            .filter(|e| {
                if !e.kind.eq_ignore_ascii_case("Resource")
                    && !e.kind.eq_ignore_ascii_case("Component")
                {
                    return false;
                }
                let st = e
                    .spec
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                st == req_lower || st == req_norm
            })
            .collect()
    };

    if filtered.is_empty() && req_lower != "all" && req_lower != "*" {
        let known: Vec<String> = registry
            .list_resource_types(false)
            .await
            .into_iter()
            .map(|t| t.resource_type)
            .collect();
        bail!(
            "no entities matched resource type {:?} (registered types: all, domains, systems, components, {})",
            requested_type,
            known.join(", ")
        );
    }

    match output {
        "json" => println!("{}", serde_json::to_string_pretty(&filtered)?),
        "yaml" => print!("{}", serde_yaml::to_string(&filtered)?),
        "wide" => {
            let summary_hdr = match view {
                "policy" => "STATIC POLICY",
                "live" => "DYNAMIC SIGNALS",
                "drift" => "DRIFT ASSESSMENT",
                _ => "DUAL-TRACK SUMMARY",
            };
            let headers = [
                "RESOURCE TYPE",
                "NAME",
                "MODE",
                "STATUS",
                "DRIFT",
                "OWNER",
                summary_hdr,
            ];
            let mut entries: Vec<(bool, [String; 7])> = Vec::with_capacity(filtered.len());
            for ent in &filtered {
                let rtype = ent
                    .spec
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or(ent.kind.as_str())
                    .to_string();
                let owner = ent
                    .spec
                    .get("owner")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-")
                    .to_string();
                let (mode, status, drifted, summary) = match registry
                    .get_pits_entity_detail(&ent.metadata.namespace, &ent.metadata.name, false)
                    .await
                {
                    Ok(d) => (
                        d.summary.observation_mode.as_str().to_string(),
                        d.status.clone(),
                        d.summary.drift_detected,
                        format_entity_summary_for_view(&d, view),
                    ),
                    Err(_) => (
                        "-".to_string(),
                        ent.metadata
                            .annotations
                            .get("pits.io/status")
                            .cloned()
                            .unwrap_or_else(|| "Active".to_string()),
                        false,
                        ent.metadata.description.clone().unwrap_or_default(),
                    ),
                };
                entries.push((
                    drifted,
                    [
                        rtype,
                        ent.metadata.name.clone(),
                        mode,
                        status,
                        if drifted { "DRIFT".to_string() } else { "ok".to_string() },
                        owner,
                        summary,
                    ],
                ));
            }
            if view == "drift" {
                entries.sort_by(|a, b| b.0.cmp(&a.0));
            }
            let rows: Vec<[String; 7]> = entries.into_iter().map(|(_, r)| r).collect();
            render_aligned_table(&headers, &rows);
        }
        _ => {
            let summary_hdr = match view {
                "policy" => "STATIC POLICY",
                "live" => "DYNAMIC SIGNALS",
                "drift" => "DRIFT ASSESSMENT",
                _ => "DUAL-TRACK SUMMARY",
            };
            let headers = ["RESOURCE TYPE", "NAME", "STATUS", "DRIFT", summary_hdr];
            let mut entries: Vec<(bool, [String; 5])> = Vec::with_capacity(filtered.len());
            for ent in &filtered {
                let rtype = ent
                    .spec
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or(ent.kind.as_str())
                    .to_string();
                let (status, drifted, summary) = match registry
                    .get_pits_entity_detail(&ent.metadata.namespace, &ent.metadata.name, false)
                    .await
                {
                    Ok(d) => (
                        d.status.clone(),
                        d.summary.drift_detected,
                        format_entity_summary_for_view(&d, view),
                    ),
                    Err(_) => (
                        ent.metadata
                            .annotations
                            .get("pits.io/status")
                            .cloned()
                            .unwrap_or_else(|| "Active".to_string()),
                        false,
                        ent.metadata.description.clone().unwrap_or_default(),
                    ),
                };
                entries.push((
                    drifted,
                    [
                        rtype,
                        ent.metadata.name.clone(),
                        status,
                        if drifted { "DRIFT".to_string() } else { "ok".to_string() },
                        summary,
                    ],
                ));
            }
            if view == "drift" {
                entries.sort_by(|a, b| b.0.cmp(&a.0));
            }
            let drifted_cnt = entries.iter().filter(|(d, _)| *d).count();
            let total_cnt = entries.len();
            let rows: Vec<[String; 5]> = entries.into_iter().map(|(_, r)| r).collect();
            render_aligned_table(&headers, &rows);
            if view == "drift" {
                println!("\nDrift Summary: {drifted_cnt} / {total_cnt} entities drifted");
            }
        }
    }

    Ok(())
}

fn render_aligned_table<const N: usize>(headers: &[&str; N], rows: &[[String; N]]) {
    let mut widths = [0usize; N];
    for (i, h) in headers.iter().enumerate() {
        widths[i] = h.chars().count();
    }
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            let len = cell.chars().count();
            if len > widths[i] {
                widths[i] = len;
            }
        }
    }

    // Header row
    let mut hdr_line = String::new();
    for (i, h) in headers.iter().enumerate() {
        if i + 1 == N {
            hdr_line.push_str(h);
        } else {
            hdr_line.push_str(&format!("{:<width$}  ", h, width = widths[i]));
        }
    }
    println!("{hdr_line}");

    // Data rows
    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i + 1 == N {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{:<width$}  ", cell, width = widths[i]));
            }
        }
        println!("{line}");
    }
}

fn print_entity_detail_text(detail: &PitsEntityDetail) {
    let ent = &detail.entity;
    let entity_ref = format!(
        "{}:{}/{}",
        ent.kind.to_ascii_lowercase(),
        ent.metadata.namespace,
        ent.metadata.name
    );

    println!("Entity Ref:      {entity_ref}");
    println!("Kind:            {} ({})", ent.kind, ent.api_version);
    println!("Resource Type:   {}", detail.resource_type);
    if let Some(ref title) = ent.metadata.title {
        println!("Title:           {title}");
    }
    if let Some(ref desc) = ent.metadata.description {
        println!("Description:     {desc}");
    }
    println!("UID:             {}", ent.metadata.uid);
    if !ent.metadata.tags.is_empty() {
        println!("Tags:            {}", ent.metadata.tags.join(", "));
    }
    println!(
        "Status:          {} [{}]",
        detail.status,
        detail.status_level.to_ascii_uppercase()
    );
    println!(
        "Observation Mode: {} ({})",
        detail.summary.observation_mode.as_str(),
        detail.summary.cadence_tier
    );
    if detail.summary.drift_detected {
        println!(
            "Drift:           DRIFTED — {}",
            detail
                .summary
                .drift_summary
                .as_deref()
                .unwrap_or("policy or runtime drift detected")
        );
    } else {
        println!("Drift:           None (Policy Compliant)");
    }
    if let Some(changed) = detail.summary.last_changed_at {
        println!("Last Changed At: {}", changed.to_rfc3339());
    }
    println!("Summary:         {}", detail.summary_line);
    println!("Live Connected:  {}", detail.live_connected);
    println!("Refreshed At:    {}", detail.refreshed_at.to_rfc3339());

    if !detail.summary.static_policy.is_empty() {
        println!("\n=== Static Policy & Contract ===");
        let max_label = detail
            .summary
            .static_policy
            .iter()
            .map(|p| p.label.chars().count())
            .max()
            .unwrap_or(16);
        for p in &detail.summary.static_policy {
            let badge = match p.status.as_deref() {
                Some(st) if !st.is_empty() => format!(" [{}]", st.to_ascii_uppercase()),
                _ => String::new(),
            };
            println!(
                "  {:<width$} : {}{}",
                p.label,
                p.value,
                badge,
                width = max_label
            );
        }
    }

    if !detail.summary.dynamic_signals.is_empty() {
        println!("\n=== Dynamic Signals (Value / Δ / Age) ===");
        let max_label = detail
            .summary
            .dynamic_signals
            .iter()
            .map(|s| s.label.chars().count())
            .max()
            .unwrap_or(16);
        for s in &detail.summary.dynamic_signals {
            let delta_str = s
                .delta
                .as_deref()
                .filter(|d| !d.is_empty())
                .map(|d| format!(" (Δ: {d})"))
                .unwrap_or_default();
            let age_str = s
                .observed_age_secs
                .filter(|a| *a > 0)
                .map(|a| format!(" [age {a}s]"))
                .unwrap_or_default();
            let badge = match s.status.as_deref() {
                Some(st) if !st.is_empty() => format!(" [{}]", st.to_ascii_uppercase()),
                _ => String::new(),
            };
            println!(
                "  {:<width$} : {}{}{}{}",
                s.label,
                s.value,
                delta_str,
                age_str,
                badge,
                width = max_label
            );
        }
    }

    for sec in &detail.attribute_sections {
        println!("\n=== {} ({}) ===", sec.title, sec.id);
        let max_label = sec
            .items
            .iter()
            .map(|it| it.label.chars().count())
            .max()
            .unwrap_or(16);
        for item in &sec.items {
            let badge = match item.status.as_deref() {
                Some(st) if !st.is_empty() => format!(" [{}]", st.to_ascii_uppercase()),
                _ => String::new(),
            };
            println!(
                "  {:<width$} : {}{}",
                item.label,
                item.value,
                badge,
                width = max_label
            );
        }
    }

    println!("\n=== Relations ({}) ===", ent.relations.len());
    if ent.relations.is_empty() {
        println!("  (none)");
    } else {
        for rel in &ent.relations {
            println!("  - {:<14} -> {}", rel.relation_type, rel.target_ref);
        }
    }

    println!("\n=== Contextual Actions ({}) ===", detail.actions.len());
    if detail.actions.is_empty() {
        println!("  (none)");
    } else {
        for act in &detail.actions {
            let state_badge = if act.enabled {
                "[ENABLED]"
            } else {
                "[DISABLED]"
            };
            let mut flags = Vec::new();
            if act.supports_dry_run {
                flags.push("supports-dry-run");
            }
            if act.dangerous {
                flags.push("dangerous");
            }
            let flag_str = if flags.is_empty() {
                String::new()
            } else {
                format!(" ({})", flags.join(", "))
            };
            println!(
                "  * {} — {} {}{}",
                act.id, act.title, state_badge, flag_str
            );
            println!("      {}", act.description);
            if let Some(ref reason) = act.disabled_reason {
                if !reason.is_empty() {
                    println!("      Disabled Reason: {reason}");
                }
            }
            if !act.parameters.is_empty() {
                let param_desc: Vec<String> = act
                    .parameters
                    .iter()
                    .map(|p| {
                        let req_tag = if p.required { "required" } else { "optional" };
                        match &p.default_value {
                            Some(dv) => format!("{}:{} ({}, default={})", p.name, p.param_type, req_tag, dv),
                            None => format!("{}:{} ({})", p.name, p.param_type, req_tag),
                        }
                    })
                    .collect();
                println!("      Parameters:      {}", param_desc.join(", "));
            }
        }
    }

    if !detail.manifests.is_empty() {
        let keys: Vec<&str> = detail.manifests.keys().map(String::as_str).collect();
        println!("\n=== Synthesized Manifests ({}) ===", keys.len());
        println!("  {}", keys.join(", "));
    }
}

fn print_action_result_text(res: &ActionExecutionResult) {
    let outcome = if res.success { "SUCCESS" } else { "FAILED" };
    let mode = if res.dry_run { " [DRY-RUN]" } else { "" };
    println!("Action:       {}", res.action_id);
    println!("Entity:       {}", res.entity_ref);
    println!("Status:       {outcome}{mode}");
    println!("Executed At:  {}", res.executed_at.to_rfc3339());
    println!("Summary:      {}", res.summary);

    if !res.changes_or_warnings.is_empty() {
        println!("\nChanges / Warnings:");
        for item in &res.changes_or_warnings {
            println!("  - {item}");
        }
    }

    if let Some(ref artifact) = res.rendered_artifact {
        if !artifact.trim().is_empty() {
            println!("\nRendered Artifact:\n{artifact}");
        }
    }

    if !res.output.is_null() {
        if let Ok(pretty) = serde_json::to_string_pretty(&res.output) {
            println!("\nOutput:\n{pretty}");
        }
    }
}

// ---------------------------------------------------------------------------
// Legacy Client Construction Helpers
// ---------------------------------------------------------------------------

fn load_or_default_edge_router(file: Option<&Path>) -> Result<EdgeRouterManifest> {
    match file {
        Some(p) => {
            let raw = fs::read_to_string(p)
                .with_context(|| format!("read EdgeRouter file {}", p.display()))?;
            Ok(serde_yaml::from_str(&raw)?)
        }
        None => Ok(pits_core_default_edge_router()),
    }
}

fn build_core_k8s_opt(prof: &ClientProfile) -> Option<KubeClient> {
    build_core_k8s_req(prof).ok()
}

fn build_core_k8s_req(prof: &ClientProfile) -> Result<KubeClient> {
    let path = prof.core_kubeconfig_path.as_deref().ok_or_else(|| {
        anyhow!("coreKubeconfigPath not set; run `pitsctl profile set --core-kubeconfig <path>`")
    })?;
    let ep = load_kubeconfig_file(Path::new(path), prof.core_context.as_deref())?;
    KubeClient::from_endpoint("pits-core", ep, None)
}

fn build_lab_k8s_req(prof: &ClientProfile) -> Result<KubeClient> {
    let path = prof.lab_kubeconfig_path.as_deref().ok_or_else(|| {
        anyhow!("labKubeconfigPath not set; run `pitsctl profile set --lab-kubeconfig <path>`")
    })?;
    let ep = load_kubeconfig_file(Path::new(path), prof.lab_context.as_deref())?;
    if prof.lab_via_agent_bridge {
        let ag = AgentClient::from_profile(prof)?;
        KubeClient::from_endpoint("pits-lab", ep, Some((prof, ag.tls_config)))
    } else {
        KubeClient::from_endpoint("pits-lab", ep, None)
    }
}

fn build_talos_req(prof: &ClientProfile) -> Result<TalosClient> {
    let path = prof.talosconfig_path.as_deref().ok_or_else(|| {
        anyhow!("talosconfigPath not set; run `pitsctl profile set --talosconfig <path>`")
    })?;
    let ctx = load_talosconfig_file(Path::new(path), prof.talos_context.as_deref())?;
    if prof.talos_via_agent_bridge {
        let ag = AgentClient::from_profile(prof)?;
        TalosClient::from_context(ctx, Some((prof, ag.tls_config)))
    } else {
        TalosClient::from_context(ctx, None)
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    #[test]
    fn test_resource_centric_cli_parsing() {
        let c1 = Cli::try_parse_from(["pitsctl", "types", "-o", "wide", "--live"]).unwrap();
        assert!(matches!(c1.command, Commands::Types { live: true, .. }));

        let c2 = Cli::try_parse_from(["pitsctl", "get"]).unwrap();
        match c2.command {
            Commands::Get {
                resource_type,
                view,
                output,
                live,
            } => {
                assert_eq!(resource_type, "all");
                assert_eq!(view, "auto");
                assert_eq!(output, "table");
                assert!(!live);
            }
            _ => panic!("expected Get"),
        }

        let c2_view = Cli::try_parse_from([
            "pitsctl",
            "get",
            "edge-router",
            "--view",
            "drift",
            "-o",
            "wide",
        ])
        .unwrap();
        match c2_view.command {
            Commands::Get {
                resource_type,
                view,
                output,
                ..
            } => {
                assert_eq!(resource_type, "edge-router");
                assert_eq!(view, "drift");
                assert_eq!(output, "wide");
            }
            _ => panic!("expected Get with --view"),
        }

        let c3 = Cli::try_parse_from([
            "pitsctl",
            "describe",
            "npu-accelerator/netboot-003-npu",
            "--live",
        ])
        .unwrap();
        assert!(matches!(c3.command, Commands::Describe { live: true, .. }));

        let c3_inspect = Cli::try_parse_from([
            "pitsctl",
            "inspect",
            "edge-router/pits-core-router",
        ])
        .unwrap();
        assert!(matches!(c3_inspect.command, Commands::Describe { .. }));

        let c4 = Cli::try_parse_from([
            "pitsctl",
            "action",
            "san-volume/netboot-004-userdata",
            "expand-volume",
            "--set",
            "sizeGib=160",
            "--set",
            "confirm=true",
            "--dry-run",
        ])
        .unwrap();
        match c4.command {
            Commands::Action {
                entity,
                action_id,
                set,
                dry_run,
                ..
            } => {
                assert_eq!(entity, "san-volume/netboot-004-userdata");
                assert_eq!(action_id, "expand-volume");
                assert_eq!(set.len(), 2);
                assert!(dry_run);
            }
            _ => panic!("expected Action"),
        }

        // Hidden legacy command still parses cleanly
        let c5 = Cli::try_parse_from(["pitsctl", "san", "wwids"]).unwrap();
        assert!(matches!(c5.command, Commands::San { .. }));
    }

    #[test]
    fn test_smart_param_and_entity_arg_parsing() {
        assert_eq!(parse_smart_param_value("true"), serde_json::json!(true));
        assert_eq!(parse_smart_param_value("false"), serde_json::json!(false));
        assert_eq!(parse_smart_param_value("160"), serde_json::json!(160));
        assert_eq!(
            parse_smart_param_value("pits-intel-npu"),
            serde_json::json!("pits-intel-npu")
        );
        assert_eq!(
            parse_smart_param_value("0000:58:00.0"),
            serde_json::json!("0000:58:00.0")
        );

        let merged = build_action_parameters(
            Some(r#"{"sizeGib": 100, "label": "orig"}"#),
            &["sizeGib=160".to_string(), "hotAdd=true".to_string()],
        )
        .unwrap();
        assert_eq!(merged.get("sizeGib"), Some(&serde_json::json!(160)));
        assert_eq!(merged.get("hotAdd"), Some(&serde_json::json!(true)));
        assert_eq!(merged.get("label"), Some(&serde_json::json!("orig")));

        assert_eq!(
            parse_entity_arg("baremetal-host/netboot-004"),
            ("default".to_string(), "baremetal-host/netboot-004".to_string())
        );
        assert_eq!(
            parse_entity_arg("resource:default/netboot-003-npu"),
            ("default".to_string(), "netboot-003-npu".to_string())
        );
    }

    #[tokio::test]
    async fn test_get_view_formatting_modes_policy_live_drift_auto() {
        let mut prof = ClientProfile::default();
        prof.agent_bearer_token = None;
        prof.agent_ca_pem = None;
        prof.agent_client_cert_pem = None;
        prof.agent_client_key_pem = None;
        prof.core_kubeconfig_path = None;
        prof.lab_kubeconfig_path = None;
        prof.talosconfig_path = None;
        let reg = build_registry(prof, WorkspaceBundle::builtin_pits_baseline());

        let npu3 = reg
            .get_pits_entity_detail("default", "npu-accelerator/netboot-003-npu", false)
            .await
            .expect("netboot-003-npu");
        let v_policy = format_entity_summary_for_view(&npu3, "policy");
        let v_live = format_entity_summary_for_view(&npu3, "live");
        let v_drift = format_entity_summary_for_view(&npu3, "drift");
        let v_auto = format_entity_summary_for_view(&npu3, "auto");

        assert!(v_policy.contains("Expected Driver=amdxdna"), "policy={v_policy}");
        assert!(v_live.contains("Bound Driver=unbound"), "live={v_live}");
        assert!(v_drift.starts_with("DRIFT:"), "drift={v_drift}");
        assert!(v_auto.contains("[Policy]") && v_auto.contains("[Live]"), "auto={v_auto}");

        let router = reg
            .get_pits_entity_detail("default", "edge-router/pits-core-router", false)
            .await
            .expect("pits-core-router");
        assert_eq!(format_entity_summary_for_view(&router, "drift"), "Compliant (No Drift)");
        assert!(format_entity_summary_for_view(&router, "auto").starts_with("[Live]"));
    }
}
