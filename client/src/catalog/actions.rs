//! Contextual Entity Action specifications, parameter schemas, safety precondition guards,
//! and execution dispatcher across all 8 PITS Resource Types.

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::catalog::model::{
    ActionExecutionResult, ActionInvokeRequest, ActionParamSpec, EntityActionSpec,
};
use crate::catalog::sourcing::{
    compute_unified_diff, default_system_domain_map, evaluate_sourcing_audit_at,
    render_catalog_info_yaml, today_utc_date, validate_sourcing_record, DomainLineageEntry,
    SourcingEvaluation, SourcingResourceRecord, PERCEIVED_STATUSES, WALLET_SHARE_TIERS,
};
use crate::catalog::sourcing_db::AssessWindmillRequest;
use crate::catalog::synthesizer::{inspect_cluster_fast, CatalogRegistry};
use crate::config::{local_state_dir, EnrolledNodeContract};
use crate::deploy::{deploy_to_nixos_pits_core, NixosDeployMode, NixosDeployOptions};
use crate::k8s::{
    apply_san_local_pv_pvc, audit_fleet_npus, launch_npu_validation_job,
    render_npu_validation_manifests, render_san_local_pv_pvc,
};
use crate::pxe::{
    apply_tinkerbell_hardware, pxe_verify_assets, render_ipxe_script,
    render_tinkerbell_hardware_yaml, PxeBootMode,
};
use crate::router::{
    apply_edge_router, pits_core_default_edge_router, recover_router_runtime,
    set_crossplane_paused, validate_edge_router, EdgeRouterManifest, MGMT0_PCI,
};
use crate::san::{
    align_4k, apply_nvmet_k8s, btrfs_maintain, expected_wwid, render_nvmet_k8s_manifests,
    san_expand, san_provision, SANNamespaceSpec,
};
use crate::talos::{
    allowed_wwids_for_node, render_talos_imager_profile, render_talos_node_config,
    render_talos_thunderbolt_stream_docs, validate_talos_netboot_config,
    TalosHighSpeedFabricOptions,
};
use crate::vm::{
    finalize_windows11_vm, launch_mstsc, render_pinned_rdp_profile,
    render_vm_live_migration_manifest, render_win11_pool_placement_patch,
    render_windows11_vm_manifest, trigger_vm_live_migration, validate_live_migration_target,
    verify_rdp_tls_endpoint, vm_lifecycle_action, write_pinned_rdp_profile, VmLifecycleOp,
    WIN11_DEFAULT_HOST_IP, WIN11_DEFAULT_RDP_NODEPORT, WIN11_FAILOVER_HOST_IP,
};

pub(crate) fn canonical_action_ids_for_type(resource_type: &str) -> Vec<String> {
    let ids: &[&str] = match resource_type {
        "baremetal-host" => &[
            "verify-pxe-assets",
            "apply-talos-config",
            "configure-thunderbolt-busypoll",
            "switch-pxe-mode",
            "reboot-node",
            "render-imager-profile",
            "sync-nixos-agent",
        ],
        "npu-accelerator" => &["launch-openvino-verify", "preview-claim-yaml"],
        "virtual-machine" => &[
            "start",
            "restart",
            "pause",
            "unpause",
            "stop",
            "live-migrate-vm",
            "switch-win11-pool-host",
            "verify-rdp-tls",
            "connect-pinned-rdp",
            "finalize-install-media",
        ],
        "edge-router" => &[
            "apply-router-spec",
            "pause-reconcile",
            "resume-reconcile",
            "recover-dataplane",
            "capture-vpp-trace",
        ],
        "pxe-boot-profile" => &["verify-boot-assets", "render-node-ipxe"],
        "storage-pool" => &["trim-pool", "scrub-verify"],
        "san-volume" => &[
            "reconcile-volume",
            "expand-volume",
            "apply-nvmet-deployment",
            "bind-static-pv",
        ],
        "kubernetes-cluster" => &["audit-fleet-npus", "refresh-cluster-state"],
        "vendor-saas" | "commercial-sdk" | "managed-infra" | "outsourced-dev" => &[
            "calibrate-sourcing-scores",
            "calibrate-supplier-windmill",
            "toggle-adapter-abstraction",
            "acknowledge-reaudit-baseline",
            "sync-workspace-yaml",
        ],
        _ => &[],
    };
    ids.iter().map(|s| (*s).to_string()).collect()
}

pub(crate) fn actions_for_pits_core_host() -> Vec<EntityActionSpec> {
    vec![
        EntityActionSpec {
            id: "audit-host-pci".to_string(),
            title: "Audit Host PCI & mgmt0 Guard".to_string(),
            description: format!(
                "Inspect pits-core PCI driver bindings and verify management NIC ({MGMT0_PCI}) is strictly isolated from VFIO/DPDK."
            ),
            category: "Verification".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![],
        },
        EntityActionSpec {
            id: "sync-nixos-agent".to_string(),
            title: "Sync Declarative NixOS Agent".to_string(),
            description: "Declaratively reconcile pits-agent onto NixOS pits-core (PITS-B1 flake module or RKE2 DaemonSet) and sync mTLS PKI.".to_string(),
            category: "Host Lifecycle".to_string(),
            dangerous: true,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![
                ActionParamSpec {
                    name: "sshTarget".to_string(),
                    label: "SSH Target".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                    default_value: Some(json!("pits-host01")),
                    options: vec![],
                    description: Some("Tailscale SSH host alias for NixOS pits-core".to_string()),
                },
                ActionParamSpec {
                    name: "mode".to_string(),
                    label: "Declarative Deployment Mode".to_string(),
                    param_type: "select".to_string(),
                    required: false,
                    default_value: Some(json!("nixos-module")),
                    options: vec!["nixos-module".to_string(), "rke2-daemonset".to_string()],
                    description: Some("NixOS flake module or RKE2 DaemonSet".to_string()),
                },
                ActionParamSpec {
                    name: "syncCredentials".to_string(),
                    label: "Sync Cluster Credentials to Local Profile".to_string(),
                    param_type: "boolean".to_string(),
                    required: false,
                    default_value: Some(json!(true)),
                    options: vec![],
                    description: Some("Pull RKE2 & Lab kubeconfig/talosconfig into %LOCALAPPDATA%\\PITS\\Client".to_string()),
                },
            ],
        },
    ]
}

pub(crate) fn actions_for_talos_host(
    node: &EnrolledNodeContract,
    default_talos_yaml: &str,
) -> Vec<EntityActionSpec> {
    vec![
        EntityActionSpec {
            id: "verify-pxe-assets".to_string(),
            title: "Verify PXE Boot Assets".to_string(),
            description: format!(
                "Verify staged Talos v1.14.5 vmlinuz & initramfs.xz SHA-256 digests for {} on pits-core.",
                node.device_id
            ),
            category: "Verification".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![],
        },
        EntityActionSpec {
            id: "apply-talos-config".to_string(),
            title: "Apply Talos Machine Config".to_string(),
            description: "Validate multi-document Talos config via netboot-guard + COSI SAN WWID preflight, then apply over HTTP/2 mTLS gRPC.".to_string(),
            category: "Configuration".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![
                ActionParamSpec {
                    name: "yaml".to_string(),
                    label: "Talos Multi-Document YAML".to_string(),
                    param_type: "yaml".to_string(),
                    required: false,
                    default_value: Some(json!(default_talos_yaml)),
                    options: vec![],
                    description: Some("Must forbid machine.install/machine.disks and pin deterministic SAN WWIDs".to_string()),
                },
                ActionParamSpec {
                    name: "mode".to_string(),
                    label: "Apply Mode".to_string(),
                    param_type: "select".to_string(),
                    required: false,
                    default_value: Some(json!("no-reboot")),
                    options: vec![
                        "no-reboot".to_string(),
                        "auto".to_string(),
                        "reboot".to_string(),
                        "try".to_string(),
                    ],
                    description: Some("Talos MachineService ApplyConfiguration mode".to_string()),
                },
                ActionParamSpec {
                    name: "verifyLiveWwids".to_string(),
                    label: "Verify Live COSI SAN WWIDs Before Apply".to_string(),
                    param_type: "boolean".to_string(),
                    required: false,
                    default_value: Some(json!(true)),
                    options: vec![],
                    description: Some("Abort if COSI Disks.block.talos.dev is missing any required SAN WWID".to_string()),
                },
            ],
        },
        EntityActionSpec {
            id: "switch-pxe-mode".to_string(),
            title: "Switch Tinkerbell PXE Mode".to_string(),
            description: "Reconcile Tinkerbell Hardware CR between diskless Talos netboot and non-destructive Hook inventory.".to_string(),
            category: "Boot & Provisioning".to_string(),
            dangerous: true,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![ActionParamSpec {
                name: "mode".to_string(),
                label: "PXE Boot Mode".to_string(),
                param_type: "select".to_string(),
                required: true,
                default_value: Some(json!("talos-netboot")),
                options: vec!["talos-netboot".to_string(), "hook-inventory".to_string()],
                description: Some("Target iPXE & Tinkerbell workflow mode".to_string()),
            }],
        },
        EntityActionSpec {
            id: "render-imager-profile".to_string(),
            title: "Render Sidero Imager Profile".to_string(),
            description: "Export deterministic Sidero Imager profile with CPU microcode, Realtek firmware, and pits-nvme-early extensions.".to_string(),
            category: "Boot & Provisioning".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![ActionParamSpec {
                name: "includeRealtekFirmware".to_string(),
                label: "Include Realtek NIC Firmware".to_string(),
                param_type: "boolean".to_string(),
                required: false,
                default_value: Some(json!(true)),
                options: vec![],
                description: None,
            }],
        },
        EntityActionSpec {
            id: "configure-thunderbolt-busypoll".to_string(),
            title: "Configure Thunderbolt Stream & Busy-Poll".to_string(),
            description: "Render/validate parameterized Talos KernelModuleConfig, SysctlConfig (net.core.busy_poll/busy_read), and pits-tbstream ExtensionServiceConfig for Linux 7.3+ thunderbolt_stream ConfigFS busy_poll.".to_string(),
            category: "Networking & Fabric".to_string(),
            dangerous: true,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![
                ActionParamSpec {
                    name: "socketBusyPollUs".to_string(),
                    label: "net.core.busy_poll (us)".to_string(),
                    param_type: "number".to_string(),
                    required: false,
                    default_value: Some(json!(50)),
                    options: vec![],
                    description: Some("Socket SO_BUSY_POLL sysctl budget in microseconds".to_string()),
                },
                ActionParamSpec {
                    name: "socketBusyReadUs".to_string(),
                    label: "net.core.busy_read (us)".to_string(),
                    param_type: "number".to_string(),
                    required: false,
                    default_value: Some(json!(50)),
                    options: vec![],
                    description: Some("Default per-socket sk_busy_poll microseconds for busy_read".to_string()),
                },
                ActionParamSpec {
                    name: "napiDeferHardIrqs".to_string(),
                    label: "tb* NAPI napi_defer_hard_irqs".to_string(),
                    param_type: "number".to_string(),
                    required: false,
                    default_value: Some(json!(2)),
                    options: vec![],
                    description: Some("Defer hard IRQs on thunderbolt-net interfaces".to_string()),
                },
                ActionParamSpec {
                    name: "groFlushTimeoutNs".to_string(),
                    label: "tb* NAPI gro_flush_timeout (ns)".to_string(),
                    param_type: "number".to_string(),
                    required: false,
                    default_value: Some(json!(200000)),
                    options: vec![],
                    description: Some("NAPI flush timeout in nanoseconds for busy_poll pacing".to_string()),
                },
                ActionParamSpec {
                    name: "busyPollIfaces".to_string(),
                    label: "Busy-Poll Interfaces".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                    default_value: Some(json!("tb0,tb1,enx02*")),
                    options: vec![],
                    description: Some("Comma-separated thunderbolt-net interfaces or sysfs globs for NAPI tuning".to_string()),
                },
                ActionParamSpec {
                    name: "streamEnabled".to_string(),
                    label: "Enable thunderbolt_stream".to_string(),
                    param_type: "boolean".to_string(),
                    required: false,
                    default_value: Some(json!(true)),
                    options: vec![],
                    description: Some("Provision ConfigFS stream channel in /sys/kernel/config/thunderbolt/stream".to_string()),
                },
                ActionParamSpec {
                    name: "streamName".to_string(),
                    label: "ConfigFS Stream Group Name".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                    default_value: Some(json!("pits0")),
                    options: vec![],
                    description: Some("ConfigFS child group under <xdomain>.<service>/<streamName>".to_string()),
                },
                ActionParamSpec {
                    name: "streamBusyPoll".to_string(),
                    label: "Stream ConfigFS busy_poll".to_string(),
                    param_type: "boolean".to_string(),
                    required: false,
                    default_value: Some(json!(true)),
                    options: vec![],
                    description: Some("Enable Linux 7.3 thunderbolt_stream ConfigFS busy_poll attribute".to_string()),
                },
                ActionParamSpec {
                    name: "streamRingSize".to_string(),
                    label: "Stream Ring Size".to_string(),
                    param_type: "number".to_string(),
                    required: false,
                    default_value: Some(json!(256)),
                    options: vec![],
                    description: Some("ConfigFS ring_size (32..=4096)".to_string()),
                },
                ActionParamSpec {
                    name: "streamThrottlingNs".to_string(),
                    label: "Stream Throttling (ns)".to_string(),
                    param_type: "number".to_string(),
                    required: false,
                    default_value: Some(json!(0)),
                    options: vec![],
                    description: Some("ConfigFS throttling attribute in nanoseconds (0 = lowest latency)".to_string()),
                },
                ActionParamSpec {
                    name: "inHopid".to_string(),
                    label: "Input HopID (-1 = auto)".to_string(),
                    param_type: "number".to_string(),
                    required: false,
                    default_value: Some(json!(-1)),
                    options: vec![],
                    description: Some("-1 auto-allocates >= 8, 0 de-allocates, or explicit >= 8".to_string()),
                },
                ActionParamSpec {
                    name: "outHopid".to_string(),
                    label: "Output HopID (-1 = auto)".to_string(),
                    param_type: "number".to_string(),
                    required: false,
                    default_value: Some(json!(-1)),
                    options: vec![],
                    description: Some("-1 auto-allocates >= 8, 0 de-allocates, or explicit >= 8".to_string()),
                },
                ActionParamSpec {
                    name: "preserveBpfJitHarden".to_string(),
                    label: "Preserve bpf_jit_harden=1".to_string(),
                    param_type: "boolean".to_string(),
                    required: false,
                    default_value: Some(json!(false)),
                    options: vec![],
                    description: Some("Retain net.core.bpf_jit_harden=1 sysctl (e.g. netboot-005 GPU node)".to_string()),
                },
                ActionParamSpec {
                    name: "mode".to_string(),
                    label: "Apply Mode".to_string(),
                    param_type: "select".to_string(),
                    required: false,
                    default_value: Some(json!("no-reboot")),
                    options: vec![
                        "no-reboot".to_string(),
                        "auto".to_string(),
                        "reboot".to_string(),
                        "try".to_string(),
                    ],
                    description: Some("Talos apply/patch mode (use no-reboot for active VM host netboot-004)".to_string()),
                },
            ],
        },
        EntityActionSpec {
            id: "reboot-node".to_string(),
            title: "Reboot Diskless Talos Node".to_string(),
            description: "Trigger gRPC MachineService/Reboot (POWERCYCLE forces full UEFI PXE netboot re-entry).".to_string(),
            category: "Lifecycle".to_string(),
            dangerous: true,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![ActionParamSpec {
                name: "powercycle".to_string(),
                label: "UEFI Powercycle Mode".to_string(),
                param_type: "boolean".to_string(),
                required: false,
                default_value: Some(json!(true)),
                options: vec![],
                description: Some("Re-enter UEFI PXE boot flow on next restart".to_string()),
            }],
        },
    ]
}

pub(crate) fn actions_for_npu(
    host_node: &str,
    default_device_class: &str,
    status: &str,
) -> Vec<EntityActionSpec> {
    let driver_pending = status == "HardwarePresentDriverPending";
    let enabled = !driver_pending && status != "NoNpuHardware";
    let disabled_reason = if driver_pending {
        if default_device_class.contains("amd") {
            Some(format!(
                "Blocked by HardwarePresentDriverPending on {host_node}: physical AMD Ryzen AI XDNA NPU (1022:1502) is present on PCI bus, but kernel driver 'amdxdna' is unbound in Talos Linux (missing CONFIG_DRM_ACCEL_AMDXDNA with module.sig_enforce=1), so /dev/accel/accel* and DRA ResourceSlice '{default_device_class}' are unavailable."
            ))
        } else {
            Some(format!(
                "Blocked by HardwarePresentDriverPending on {host_node}: physical NPU accelerator is present on PCI bus, but its expected kernel driver is unbound on Talos host {host_node}, so /dev/accel/accel* and DRA ResourceSlice '{default_device_class}' are unavailable."
            ))
        }
    } else if status == "NoNpuHardware" {
        Some("No physical NPU accelerator detected on host node.".to_string())
    } else {
        None
    };

    vec![
        EntityActionSpec {
            id: "launch-openvino-verify".to_string(),
            title: "Launch OpenVINO DRA Verification Job".to_string(),
            description: "Apply resource.k8s.io/v1 ResourceClaim and rootless UID 1000 OpenVINO NPU probe Job pinned to this host.".to_string(),
            category: "DRA Workload".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled,
            disabled_reason,
            parameters: vec![
                ActionParamSpec {
                    name: "deviceClass".to_string(),
                    label: "DRA DeviceClass".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                    default_value: Some(json!(default_device_class)),
                    options: vec![],
                    description: Some("Kubernetes resource.k8s.io/v1 DeviceClass name".to_string()),
                },
                ActionParamSpec {
                    name: "namespace".to_string(),
                    label: "Target Namespace".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                    default_value: Some(json!("default")),
                    options: vec![],
                    description: Some("Namespace in cluster-pits-lab for ResourceClaim & Job".to_string()),
                },
            ],
        },
        EntityActionSpec {
            id: "preview-claim-yaml".to_string(),
            title: "Preview DRA v1 ResourceClaim & Job YAML".to_string(),
            description: "Render resource.k8s.io/v1 ResourceClaim and rootless OpenVINO verification Job manifest.".to_string(),
            category: "Manifests".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![
                ActionParamSpec {
                    name: "deviceClass".to_string(),
                    label: "DRA DeviceClass".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                    default_value: Some(json!(default_device_class)),
                    options: vec![],
                    description: None,
                },
                ActionParamSpec {
                    name: "namespace".to_string(),
                    label: "Target Namespace".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                    default_value: Some(json!("default")),
                    options: vec![],
                    description: None,
                },
            ],
        },
    ]
}

pub(crate) fn actions_for_vm(vm_name: &str) -> Vec<EntityActionSpec> {
    let is_win11 = vm_name == "win11-001";
    let stop_params = if is_win11 {
        vec![ActionParamSpec {
            name: "confirmSelfSessionStop".to_string(),
            label: "Confirm Stopping Active Operator Workstation (win11-001)".to_string(),
            param_type: "boolean".to_string(),
            required: true,
            default_value: Some(json!(false)),
            options: vec![],
            description: Some(
                "Safety lock: win11-001 hosts the active Windows 11 operator session. Set confirmSelfSessionStop=true to authorize shutdown."
                    .to_string(),
            ),
        }]
    } else {
        vec![]
    };

    let mut list = vec![
        EntityActionSpec {
            id: "start".to_string(),
            title: "Start Virtual Machine".to_string(),
            description: "Invoke KubeVirt subresource start on this VirtualMachine.".to_string(),
            category: "Lifecycle".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![],
        },
        EntityActionSpec {
            id: "restart".to_string(),
            title: "Restart Virtual Machine".to_string(),
            description: "Invoke KubeVirt subresource restart on this VirtualMachine.".to_string(),
            category: "Lifecycle".to_string(),
            dangerous: is_win11,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![],
        },
        EntityActionSpec {
            id: "pause".to_string(),
            title: "Pause VMI".to_string(),
            description: "Pause guest vCPU execution via KubeVirt VirtualMachineInstance subresource.".to_string(),
            category: "Lifecycle".to_string(),
            dangerous: is_win11,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![],
        },
        EntityActionSpec {
            id: "unpause".to_string(),
            title: "Unpause VMI".to_string(),
            description: "Resume guest vCPU execution via KubeVirt VirtualMachineInstance subresource.".to_string(),
            category: "Lifecycle".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![],
        },
        EntityActionSpec {
            id: "stop".to_string(),
            title: if is_win11 {
                "Stop Virtual Machine (Self-Session Guarded)".to_string()
            } else {
                "Stop Virtual Machine".to_string()
            },
            description: if is_win11 {
                "Protected by self-session safety lock (requires confirmSelfSessionStop=true).".to_string()
            } else {
                "Invoke KubeVirt subresource stop on this VirtualMachine.".to_string()
            },
            category: "Lifecycle".to_string(),
            dangerous: true,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: stop_params,
        },
    ];

    if is_win11 {
        list.push(EntityActionSpec {
            id: "verify-rdp-tls".to_string(),
            title: "Verify RDP X.224 + TLS Certificate".to_string(),
            description: "Perform native Microsoft RDP X.224 CredSSP/NLA negotiation and TLS 1.3 handshake to inspect live X.509 SHA-1/SHA-256 thumbprints.".to_string(),
            category: "Remote Desktop Security".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![
                ActionParamSpec {
                    name: "host".to_string(),
                    label: "RDP Node Host IP".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                    default_value: Some(json!(WIN11_DEFAULT_HOST_IP)),
                    options: vec![
                        WIN11_DEFAULT_HOST_IP.to_string(),
                        "10.89.20.21".to_string(),
                        "10.89.20.22".to_string(),
                        WIN11_FAILOVER_HOST_IP.to_string(),
                    ],
                    description: Some(format!(
                        "{WIN11_DEFAULT_HOST_IP} (netboot-004), 10.89.20.21 (netboot-001), 10.89.20.22 (netboot-002), or {WIN11_FAILOVER_HOST_IP} (netboot-003)"
                    )),
                },
                ActionParamSpec {
                    name: "port".to_string(),
                    label: "RDP NodePort".to_string(),
                    param_type: "integer".to_string(),
                    required: false,
                    default_value: Some(json!(WIN11_DEFAULT_RDP_NODEPORT)),
                    options: vec![],
                    description: None,
                },
            ],
        });
        list.push(EntityActionSpec {
            id: "connect-pinned-rdp".to_string(),
            title: "Connect Certificate-Pinned RDP (3072×1920)".to_string(),
            description: "Verify live RDP TLS certificate, write thumbprint-pinned .rdp profile in %LOCALAPPDATA%\\PITS\\Client\\rdp, and launch mstsc.exe.".to_string(),
            category: "Remote Desktop Security".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![
                ActionParamSpec {
                    name: "host".to_string(),
                    label: "RDP Node Host IP".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                    default_value: Some(json!(WIN11_DEFAULT_HOST_IP)),
                    options: vec![
                        WIN11_DEFAULT_HOST_IP.to_string(),
                        "10.89.20.21".to_string(),
                        "10.89.20.22".to_string(),
                        WIN11_FAILOVER_HOST_IP.to_string(),
                    ],
                    description: Some(format!(
                        "{WIN11_DEFAULT_HOST_IP} (netboot-004), 10.89.20.21 (netboot-001), 10.89.20.22 (netboot-002), or {WIN11_FAILOVER_HOST_IP} (netboot-003)"
                    )),
                },
                ActionParamSpec {
                    name: "username".to_string(),
                    label: "Windows Username".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                    default_value: Some(json!("pitsadmin")),
                    options: vec![],
                    description: None,
                },
                ActionParamSpec {
                    name: "width".to_string(),
                    label: "Desktop Width".to_string(),
                    param_type: "integer".to_string(),
                    required: false,
                    default_value: Some(json!(3072)),
                    options: vec![],
                    description: None,
                },
                ActionParamSpec {
                    name: "height".to_string(),
                    label: "Desktop Height".to_string(),
                    param_type: "integer".to_string(),
                    required: false,
                    default_value: Some(json!(1920)),
                    options: vec![],
                    description: None,
                },
                ActionParamSpec {
                    name: "launchClient".to_string(),
                    label: "Launch mstsc.exe Immediately".to_string(),
                    param_type: "boolean".to_string(),
                    required: false,
                    default_value: Some(json!(true)),
                    options: vec![],
                    description: None,
                },
            ],
        });
        list.push(EntityActionSpec {
            id: "finalize-install-media".to_string(),
            title: "Finalize & Detach Install ISO Media".to_string(),
            description: "Reconcile clean post-install VirtualMachine spec removing installation, VirtIO, and sysprep CD-ROM drives.".to_string(),
            category: "Provisioning".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![],
        });
        list.push(EntityActionSpec {
            id: "switch-win11-pool-host".to_string(),
            title: "Switch win11-001 Between 003/004 Host Pool".to_string(),
            description: "Preview/coordinate atomic cold-failover of pits-vms/win11-001 across netboot-003 (14Gi, standby) and netboot-004 (16Gi + RTL8126 VFIO) using SAN-fenced nvmet NSID 3 handoff.".to_string(),
            category: "Host Pool & Failover".to_string(),
            dangerous: true,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![
                ActionParamSpec {
                    name: "targetNode".to_string(),
                    label: "Target Pool Host".to_string(),
                    param_type: "select".to_string(),
                    required: true,
                    default_value: Some(json!("netboot-003")),
                    options: vec![
                        "netboot-003".to_string(),
                        "netboot-004".to_string(),
                    ],
                    description: Some("Target node in the netboot-003/netboot-004 VM host pool".to_string()),
                },
                ActionParamSpec {
                    name: "upgradeNetboot004".to_string(),
                    label: "Upgrade netboot-004 to 7.3.1-talos After Switch".to_string(),
                    param_type: "boolean".to_string(),
                    required: false,
                    default_value: Some(json!(true)),
                    options: vec![],
                    description: Some("When switching to netboot-003, cordon and powercycle netboot-004 into staged 7.3.1-talos".to_string()),
                },
                ActionParamSpec {
                    name: "confirmSelfSessionStop".to_string(),
                    label: "Confirm Stopping Active win11-001 Workstation".to_string(),
                    param_type: "boolean".to_string(),
                    required: true,
                    default_value: Some(json!(false)),
                    options: vec![],
                    description: Some("Safety lock: cold failover stops win11-001 for ~1-2 min while handing off SAN vmstore NSID 3".to_string()),
                },
            ],
        });
        list.push(EntityActionSpec {
            id: "live-migrate-vm".to_string(),
            title: "Live Migrate VM Over 40Gbps Thunderbolt".to_string(),
            description: "Sub-second RWX live migration across the homogeneous Intel Meteor Lake pool (netboot-001, netboot-002, netboot-004) over the dedicated 40Gbps tb-migration network without guest downtime.".to_string(),
            category: "Host Pool & Failover".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![ActionParamSpec {
                name: "targetNode".to_string(),
                label: "Target Intel Pool Host".to_string(),
                param_type: "select".to_string(),
                required: false,
                default_value: Some(json!("auto")),
                options: vec![
                    "auto".to_string(),
                    "netboot-001".to_string(),
                    "netboot-002".to_string(),
                    "netboot-004".to_string(),
                ],
                description: Some("Optional target host in [netboot-001, netboot-002, netboot-004]; 'auto' lets kube-scheduler select another pool host".to_string()),
            }],
        });
    }

    list
}

pub(crate) fn actions_for_edge_router(default_router_yaml: &str) -> Vec<EntityActionSpec> {
    vec![
        EntityActionSpec {
            id: "apply-router-spec".to_string(),
            title: "Validate & Apply EdgeRouter Spec".to_string(),
            description: format!(
                "Enforce mgmt0 ({MGMT0_PCI}) PCI safety, VLAN 10/20 mutual isolation, and lab->management-net block before Server-Side Apply."
            ),
            category: "Policy & Reconciliation".to_string(),
            dangerous: true,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![ActionParamSpec {
                name: "yaml".to_string(),
                label: "EdgeRouter Manifest YAML".to_string(),
                param_type: "yaml".to_string(),
                required: true,
                default_value: Some(json!(default_router_yaml)),
                options: vec![],
                description: Some("platform.edge.router/v1alpha1 EdgeRouter manifest".to_string()),
            }],
        },
        EntityActionSpec {
            id: "pause-reconcile".to_string(),
            title: "Pause Crossplane Reconciliation".to_string(),
            description: "Set crossplane.io/paused=true on EdgeRouter and child Router CRs.".to_string(),
            category: "Policy & Reconciliation".to_string(),
            dangerous: true,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![],
        },
        EntityActionSpec {
            id: "resume-reconcile".to_string(),
            title: "Resume Crossplane Reconciliation".to_string(),
            description: "Remove crossplane.io/paused annotation on EdgeRouter and child Router CRs.".to_string(),
            category: "Policy & Reconciliation".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![],
        },
        EntityActionSpec {
            id: "recover-dataplane".to_string(),
            title: "Recover VFIO & VPP Gateway Dataplane".to_string(),
            description: format!(
                "Restore WAN/LAN VFIO bindings under mgmt0 ({MGMT0_PCI}) hardware guard and verify edge/gateway rollout."
            ),
            category: "Dataplane Operations".to_string(),
            dangerous: true,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![
                ActionParamSpec {
                    name: "cleanupPerfNetns".to_string(),
                    label: "Clean Up Stale Test Netns".to_string(),
                    param_type: "boolean".to_string(),
                    required: false,
                    default_value: Some(json!(true)),
                    options: vec![],
                    description: None,
                },
                ActionParamSpec {
                    name: "refreshDevicePlugins".to_string(),
                    label: "Refresh K8s SR-IOV/VFIO Device Plugins".to_string(),
                    param_type: "boolean".to_string(),
                    required: false,
                    default_value: Some(json!(true)),
                    options: vec![],
                    description: None,
                },
                ActionParamSpec {
                    name: "waitSeconds".to_string(),
                    label: "Gateway Ready Timeout (s)".to_string(),
                    param_type: "integer".to_string(),
                    required: false,
                    default_value: Some(json!(60)),
                    options: vec![],
                    description: None,
                },
            ],
        },
        EntityActionSpec {
            id: "capture-vpp-trace".to_string(),
            title: "Capture GoVPP dpdk-input Packet Trace".to_string(),
            description: "Capture live packet trace from VPP dpdk-input graph node via GoVPP socket.".to_string(),
            category: "Diagnostics".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![
                ActionParamSpec {
                    name: "count".to_string(),
                    label: "Packet Count".to_string(),
                    param_type: "integer".to_string(),
                    required: false,
                    default_value: Some(json!(50)),
                    options: vec![],
                    description: Some("Number of packets to capture on dpdk-input".to_string()),
                },
                ActionParamSpec {
                    name: "filter".to_string(),
                    label: "Substring Filter".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                    default_value: Some(json!("")),
                    options: vec![],
                    description: Some("Optional substring filter applied to VPP trace output".to_string()),
                },
            ],
        },
    ]
}

pub(crate) fn actions_for_pxe_profile(node_ids: &[String]) -> Vec<EntityActionSpec> {
    let opts = if node_ids.is_empty() {
        vec![
            "netboot-001".to_string(),
            "netboot-002".to_string(),
            "netboot-003".to_string(),
            "netboot-004".to_string(),
        ]
    } else {
        node_ids.to_vec()
    };
    let default_dev = opts
        .last()
        .cloned()
        .unwrap_or_else(|| "netboot-004".to_string());

    vec![
        EntityActionSpec {
            id: "verify-boot-assets".to_string(),
            title: "Verify Staged Boot Assets (SHA-256)".to_string(),
            description: "Verify HTTP boot kernel & initramfs SHA-256 digests on pits-core.".to_string(),
            category: "Verification".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![ActionParamSpec {
                name: "device".to_string(),
                label: "Target Enrolled Node".to_string(),
                param_type: "select".to_string(),
                required: false,
                default_value: Some(json!(default_dev.clone())),
                options: opts.clone(),
                description: None,
            }],
        },
        EntityActionSpec {
            id: "render-node-ipxe".to_string(),
            title: "Render Node iPXE & Tinkerbell Hardware YAML".to_string(),
            description: "Compile deterministic iPXE boot script and Tinkerbell Hardware manifest for an enrolled node.".to_string(),
            category: "Manifests".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![ActionParamSpec {
                name: "device".to_string(),
                label: "Target Enrolled Node".to_string(),
                param_type: "select".to_string(),
                required: false,
                default_value: Some(json!(default_dev)),
                options: opts,
                description: None,
            }],
        },
    ]
}

pub(crate) fn actions_for_storage_pool() -> Vec<EntityActionSpec> {
    vec![
        EntityActionSpec {
            id: "trim-pool".to_string(),
            title: "Run Serialized FITRIM Reclamation".to_string(),
            description: "Execute serialized FITRIM ioctl reclamation on pits-data Btrfs pool (/var/mnt/tank).".to_string(),
            category: "Maintenance".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![],
        },
        EntityActionSpec {
            id: "scrub-verify".to_string(),
            title: "Verify Btrfs Scrub Zero-Error State".to_string(),
            description: "Verify Btrfs scrub status and 5 sysfs device I/O & checksum error counters.".to_string(),
            category: "Verification".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![],
        },
    ]
}

pub(crate) fn actions_for_san_volume(
    node: &EnrolledNodeContract,
    ns: &SANNamespaceSpec,
) -> Vec<EntityActionSpec> {
    let is_system = ns.name == "system";
    let current_gib = ((ns.bytes as f64) / (1024.0 * 1024.0 * 1024.0)).ceil() as u64;
    let suggested_expand_gib = if ns.name == "vmstore" {
        current_gib.max(160) + 40
    } else {
        160u64.max(current_gib + 10)
    };
    let expand_disabled_reason = if is_system {
        Some(
            "System boot volumes (NSID 1, 4KiB META+STATE GPT) use fixed 66 GiB geometry; online additive expansion is restricted to userdata and vmstore volumes."
                .to_string(),
        )
    } else {
        None
    };
    let pv_disabled_reason = if is_system {
        Some(
            "NSID 1 system volumes back Talos OS META/STATE/EPHEMERAL partitions directly and must not be bound as workload PersistentVolumes."
                .to_string(),
        )
    } else {
        None
    };

    let vol_entity_name = format!("{}-{}", node.device_id, ns.name);
    let default_k8s_ns = if ns.name == "vmstore" {
        "pits-vms"
    } else {
        "default"
    };

    vec![
        EntityActionSpec {
            id: "reconcile-volume".to_string(),
            title: "Reconcile NVMe/TCP Backing & configfs".to_string(),
            description: "Ensure 4KiB-aligned NOCOW backing file, GPT headers (if NSID 1), and nvmet configfs namespace export.".to_string(),
            category: "Provisioning".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![ActionParamSpec {
                name: "hotAddOnly".to_string(),
                label: "Hot-Add Namespace Only (Preserve Active Listener)".to_string(),
                param_type: "boolean".to_string(),
                required: false,
                default_value: Some(json!(ns.id == 3)),
                options: vec![],
                description: None,
            }],
        },
        EntityActionSpec {
            id: "expand-volume".to_string(),
            title: "Expand Volume Online (4KiB-Aligned)".to_string(),
            description: "Monotonically grow backing extent on pits-data and trigger nvmet configfs revalidate_size without unmounting.".to_string(),
            category: "Capacity".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: !is_system,
            disabled_reason: expand_disabled_reason,
            parameters: vec![ActionParamSpec {
                name: "sizeGib".to_string(),
                label: "Target Capacity (GiB)".to_string(),
                param_type: "integer".to_string(),
                required: true,
                default_value: Some(json!(suggested_expand_gib)),
                options: vec![],
                description: Some("Target capacity in GiB (must be >= current size; rounded to 4KiB boundary)".to_string()),
            }],
        },
        EntityActionSpec {
            id: "apply-nvmet-deployment".to_string(),
            title: "Apply nvmet K8s Deployment & Multus NAD".to_string(),
            description: format!(
                "Render and Server-Side Apply tink-system/nvmet-{} Deployment + lab-vlan20-san Multus NAD.",
                node.device_id
            ),
            category: "Kubernetes Target".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![],
        },
        EntityActionSpec {
            id: "bind-static-pv".to_string(),
            title: "Bind Node-Pinned Static PV & PVC".to_string(),
            description: "Render and Server-Side Apply node-affiliated Local PersistentVolume and PersistentVolumeClaim in Lab K8s.".to_string(),
            category: "Kubernetes Storage".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: !is_system,
            disabled_reason: pv_disabled_reason,
            parameters: vec![
                ActionParamSpec {
                    name: "pvName".to_string(),
                    label: "PersistentVolume Name".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                    default_value: Some(json!(format!("pv-{vol_entity_name}"))),
                    options: vec![],
                    description: None,
                },
                ActionParamSpec {
                    name: "namespace".to_string(),
                    label: "PVC Namespace".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                    default_value: Some(json!(default_k8s_ns)),
                    options: vec![],
                    description: None,
                },
                ActionParamSpec {
                    name: "pvcName".to_string(),
                    label: "PersistentVolumeClaim Name".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                    default_value: Some(json!(format!("{vol_entity_name}-pvc"))),
                    options: vec![],
                    description: None,
                },
                ActionParamSpec {
                    name: "sizeGib".to_string(),
                    label: "Claim Capacity (GiB)".to_string(),
                    param_type: "integer".to_string(),
                    required: false,
                    default_value: Some(json!(current_gib.max(1))),
                    options: vec![],
                    description: None,
                },
            ],
        },
    ]
}

pub(crate) fn actions_for_k8s_cluster() -> Vec<EntityActionSpec> {
    vec![
        EntityActionSpec {
            id: "audit-fleet-npus".to_string(),
            title: "Run 5-Machine Hardware ↔ DRA NPU Audit".to_string(),
            description: "Cross-verify DMI retail models, CPU microarchitectures, PCI NPUs, kernel drivers, and Kubernetes v1.37 DRA ResourceSlices across all 5 machines.".to_string(),
            category: "Audit & Compliance".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![],
        },
        EntityActionSpec {
            id: "refresh-cluster-state".to_string(),
            title: "Refresh Control-Plane Telemetry".to_string(),
            description: "Invalidate 15s catalog cache and re-probe live Kubernetes API server, nodes, pods, PVs, and DRA ResourceSlices.".to_string(),
            category: "Diagnostics".to_string(),
            dangerous: false,
            supports_dry_run: false,
            enabled: true,
            disabled_reason: None,
            parameters: vec![],
        },
    ]
}

pub(crate) fn actions_for_sourcing_resource(
    rec: &SourcingResourceRecord,
    eval: &SourcingEvaluation,
) -> Vec<EntityActionSpec> {
    let m = &rec.audit.manual_scores;
    let sp = &rec.audit.supplier_preference;
    let cm = &rec.audit.contract_meta;
    vec![
        EntityActionSpec {
            id: "calibrate-sourcing-scores".to_string(),
            title: "Calibrate Manual Kraljic Scores (Y2/Y4/X2/X3/X4)".to_string(),
            description: "Update subjective business value & supply risk blind-review scores (1.0–5.0) and recompute 5-point knockout escalation.".to_string(),
            category: "Sourcing Audit".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![
                ActionParamSpec {
                    name: "y2_coreCompetitiveness".to_string(),
                    label: "Y2 Core Competitiveness (1-5)".to_string(),
                    param_type: "number".to_string(),
                    required: false,
                    default_value: Some(json!(m.y2_core_competitiveness)),
                    options: vec![],
                    description: Some("Customer-facing differentiation & core moat impact".to_string()),
                },
                ActionParamSpec {
                    name: "y4_priceElasticity".to_string(),
                    label: "Y4 Price Elasticity (1-5)".to_string(),
                    param_type: "number".to_string(),
                    required: false,
                    default_value: Some(json!(m.y4_price_elasticity)),
                    options: vec![],
                    description: Some("Commercial cost-down negotiation leverage".to_string()),
                },
                ActionParamSpec {
                    name: "x2_marketScarcity".to_string(),
                    label: "X2 Market Scarcity (1-5)".to_string(),
                    param_type: "number".to_string(),
                    required: false,
                    default_value: Some(json!(m.x2_market_scarcity)),
                    options: vec![],
                    description: Some("Scarcity of qualified alternative suppliers (5 triggers knockout)".to_string()),
                },
                ActionParamSpec {
                    name: "x3_customizationDepth".to_string(),
                    label: "X3 Customization Depth (1-5)".to_string(),
                    param_type: "number".to_string(),
                    required: false,
                    default_value: Some(json!(m.x3_customization_depth)),
                    options: vec![],
                    description: Some("Bespoke fork depth & switching lead-time penalty".to_string()),
                },
                ActionParamSpec {
                    name: "x4_complianceFragility".to_string(),
                    label: "X4 Compliance Fragility (1-5)".to_string(),
                    param_type: "number".to_string(),
                    required: false,
                    default_value: Some(json!(m.x4_compliance_fragility)),
                    options: vec![],
                    description: Some("Regulatory, licensing & cross-border embargo exposure (5 triggers knockout)".to_string()),
                },
            ],
        },
        EntityActionSpec {
            id: "calibrate-supplier-windmill".to_string(),
            title: "Calibrate Dutch Windmill Status".to_string(),
            description: "Update how the vendor perceives PITS (core | development | exploitable | nuisance) and estimated wallet share tier.".to_string(),
            category: "Dutch Windmill".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![
                ActionParamSpec {
                    name: "perceivedStatus".to_string(),
                    label: "Vendor Perceived Status".to_string(),
                    param_type: "select".to_string(),
                    required: true,
                    default_value: Some(json!(sp.perceived_status)),
                    options: PERCEIVED_STATUSES.iter().map(|s| (*s).to_string()).collect(),
                    description: Some("Supplier's view of our account in their Dutch Windmill matrix".to_string()),
                },
                ActionParamSpec {
                    name: "estimatedWalletShareTier".to_string(),
                    label: "Estimated Wallet Share Tier".to_string(),
                    param_type: "select".to_string(),
                    required: false,
                    default_value: Some(json!(sp.estimated_wallet_share_tier)),
                    options: WALLET_SHARE_TIERS.iter().map(|s| (*s).to_string()).collect(),
                    description: Some("Share of the vendor's matching product-line revenue".to_string()),
                },
                ActionParamSpec {
                    name: "accountAttractivenessScore".to_string(),
                    label: "Account Attractiveness (1-5)".to_string(),
                    param_type: "number".to_string(),
                    required: false,
                    default_value: Some(json!(sp.account_attractiveness_score)),
                    options: vec![],
                    description: Some("Reference value, gross margin & co-innovation appeal".to_string()),
                },
            ],
        },
        EntityActionSpec {
            id: "toggle-adapter-abstraction".to_string(),
            title: "Toggle Anti-Corruption Adapter Flag".to_string(),
            description: format!(
                "Update sourcing.company.io/adapter-abstracted (currently '{}', X1={:.1}) to verify lock-in impact.",
                rec.adapter_abstracted, eval.facts.breakdown.x1_lock_in
            ),
            category: "Architecture Decoupling".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![ActionParamSpec {
                name: "adapterAbstracted".to_string(),
                label: "Anti-Corruption Adapter Implemented".to_string(),
                param_type: "boolean".to_string(),
                required: true,
                default_value: Some(json!(!rec.adapter_abstracted)),
                options: vec![],
                description: Some("Set true once all upstream Components call an internal port/adapter abstraction".to_string()),
            }],
        },
        EntityActionSpec {
            id: "acknowledge-reaudit-baseline".to_string(),
            title: "Complete Dynamic Re-Audit & Reset Baseline".to_string(),
            description: format!(
                "Record fresh re-audit date, optionally extend renewalDate (currently {}), sync baselineProdDependents to {}, and clear variance alert.",
                cm.renewal_date, eval.facts.prod_dependents
            ),
            category: "Sourcing Audit".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![
                ActionParamSpec {
                    name: "renewalDate".to_string(),
                    label: "Next Contract Renewal Date (YYYY-MM-DD)".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                    default_value: Some(json!(cm.renewal_date)),
                    options: vec![],
                    description: Some("ISO contract renewal date".to_string()),
                },
                ActionParamSpec {
                    name: "baselineProdDependents".to_string(),
                    label: "Baseline Production Dependents".to_string(),
                    param_type: "integer".to_string(),
                    required: false,
                    default_value: Some(json!(eval.facts.prod_dependents.max(1))),
                    options: vec![],
                    description: Some("Sets new baseline count for 2x dependency-doubling alert".to_string()),
                },
                ActionParamSpec {
                    name: "clearVarianceAlert".to_string(),
                    label: "Clear Blind-Score Variance Alert".to_string(),
                    param_type: "boolean".to_string(),
                    required: false,
                    default_value: Some(json!(true)),
                    options: vec![],
                    description: None,
                },
            ],
        },
        EntityActionSpec {
            id: "sync-workspace-yaml".to_string(),
            title: "Sync Canonical catalog-info.yaml to Workspace".to_string(),
            description: "Render canonical backstage.io/v1alpha1 Resource YAML with unified diff preview and optionally write to catalog/sourcing/<name>.yaml.".to_string(),
            category: "GitOps Export".to_string(),
            dangerous: false,
            supports_dry_run: true,
            enabled: true,
            disabled_reason: None,
            parameters: vec![],
        },
    ]
}

fn param_str(params: &BTreeMap<String, serde_json::Value>, key: &str) -> Option<String> {
    params.get(key).and_then(|v| match v {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    })
}

fn param_raw_str(params: &BTreeMap<String, serde_json::Value>, key: &str) -> Option<String> {
    params.get(key).and_then(|v| v.as_str().map(str::to_string))
}

fn param_i64(params: &BTreeMap<String, serde_json::Value>, key: &str) -> Option<i64> {
    params.get(key).and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_u64().and_then(|u| i64::try_from(u).ok()))
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
    })
}

fn param_f64(params: &BTreeMap<String, serde_json::Value>, key: &str) -> Option<f64> {
    params.get(key).and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
    })
}

fn param_bool(params: &BTreeMap<String, serde_json::Value>, key: &str, default: bool) -> bool {
    params
        .get(key)
        .and_then(|v| {
            v.as_bool().or_else(|| {
                v.as_str().and_then(|s| match s.trim().to_lowercase().as_str() {
                    "true" | "1" | "yes" => Some(true),
                    "false" | "0" | "no" => Some(false),
                    _ => None,
                })
            })
        })
        .unwrap_or(default)
}

impl CatalogRegistry {
    /// Executes (or dry-runs) a contextual action on a catalog entity, strictly enforcing
    /// all PITS hardware & operational safety preconditions before touching live infrastructure,
    /// and invalidating the 15s catalog cache after non-dry-run mutations.
    pub async fn execute_entity_action(
        &self,
        namespace: &str,
        target: &str,
        action_id: &str,
        req: ActionInvokeRequest,
    ) -> Result<ActionExecutionResult> {
        let ns_norm = if namespace.trim().is_empty() {
            "default"
        } else {
            namespace.trim()
        };
        let detail = self.get_pits_entity_detail(ns_norm, target, false).await?;
        let entity_name = detail.entity.metadata.name.clone();
        let kind_lower = detail.entity.kind.to_lowercase();
        let entity_ref = format!("{kind_lower}:{ns_norm}/{entity_name}");
        let res_type = detail.resource_type.as_str();

        let result = match res_type {
            "baremetal-host" => {
                self.exec_baremetal_host_action(&entity_ref, &entity_name, action_id, &req)
                    .await?
            }
            "npu-accelerator" => {
                self.exec_npu_action(&detail.status, &entity_ref, &entity_name, action_id, &req)
                    .await?
            }
            "virtual-machine" => {
                self.exec_vm_action(&entity_ref, &entity_name, action_id, &req)
                    .await?
            }
            "edge-router" => {
                self.exec_router_action(&entity_ref, &entity_name, action_id, &req)
                    .await?
            }
            "pxe-boot-profile" => {
                self.exec_pxe_profile_action(&entity_ref, &entity_name, action_id, &req)
                    .await?
            }
            "storage-pool" => {
                self.exec_storage_pool_action(&entity_ref, &entity_name, action_id, &req)
                    .await?
            }
            "san-volume" => {
                self.exec_san_volume_action(&entity_ref, &entity_name, action_id, &req)
                    .await?
            }
            "kubernetes-cluster" | "system" => {
                self.exec_cluster_action(&entity_ref, &entity_name, action_id, &req)
                    .await?
            }
            "vendor-saas" | "commercial-sdk" | "managed-infra" | "outsourced-dev" => {
                self.exec_sourcing_action(&entity_ref, &entity_name, action_id, &req, &detail.diagnostics)
                    .await?
            }
            other => bail!("unsupported resource type {other:?} on {entity_ref}"),
        };

        if !req.dry_run {
            self.invalidate_cache().await;
        }

        Ok(result)
    }

    async fn exec_baremetal_host_action(
        &self,
        entity_ref: &str,
        entity_name: &str,
        action_id: &str,
        req: &ActionInvokeRequest,
    ) -> Result<ActionExecutionResult> {
        if entity_name == "pits-core" {
            return match action_id {
                "audit-host-pci" => {
                    if req.dry_run {
                        return Ok(ActionExecutionResult {
                            success: true,
                            entity_ref: entity_ref.to_string(),
                            action_id: action_id.to_string(),
                            dry_run: true,
                            executed_at: Utc::now(),
                            summary: format!(
                                "Dry-run: would query pits-agent /v1/host/audit and verify {MGMT0_PCI} (mgmt0) isolation"
                            ),
                            changes_or_warnings: vec![],
                            output: json!({ "protectedMgmtPci": MGMT0_PCI }),
                            rendered_artifact: None,
                        });
                    }
                    let ag = self.build_agent_req().await?;
                    let audit: serde_json::Value = ag.get_json("/v1/host/audit").await?;
                    Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: false,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Verified live pits-core host PCI bindings; {MGMT0_PCI} (mgmt0) protected"
                        ),
                        changes_or_warnings: vec![],
                        output: audit,
                        rendered_artifact: None,
                    })
                }
                "sync-nixos-agent" => {
                    let ssh_target = param_str(&req.parameters, "sshTarget")
                        .unwrap_or_else(|| "pits-host01".to_string());
                    let mode_str = param_str(&req.parameters, "mode")
                        .unwrap_or_else(|| "nixos-module".to_string());
                    let mode = if mode_str == "rke2-daemonset" {
                        NixosDeployMode::Rke2Daemonset
                    } else {
                        NixosDeployMode::NixosModule
                    };
                    let sync_creds = param_bool(&req.parameters, "syncCredentials", true);
                    let bin_path =
                        PathBuf::from(r"c:\Project\PITS\Infra\server\bin\pits-agent-linux-amd64");

                    if req.dry_run && !bin_path.exists() {
                        return Ok(ActionExecutionResult {
                            success: true,
                            entity_ref: entity_ref.to_string(),
                            action_id: action_id.to_string(),
                            dry_run: true,
                            executed_at: Utc::now(),
                            summary: format!(
                                "Dry-run: declarative NixOS deployment plan prepared for {ssh_target} ({mode_str})"
                            ),
                            changes_or_warnings: vec![format!(
                                "Note: {} not yet built locally; build Go binary before non-dry-run deploy.",
                                bin_path.display()
                            )],
                            output: json!({
                                "sshTarget": ssh_target,
                                "mode": mode_str,
                                "syncCredentials": sync_creds,
                            }),
                            rendered_artifact: None,
                        });
                    }

                    let opts = NixosDeployOptions {
                        ssh_target,
                        agent_host: "100.74.25.42".to_string(),
                        agent_port: 18443,
                        flake_dir: "/home/libraskywalker/PITS-B1/Base".to_string(),
                        flake_attr: "core".to_string(),
                        mode,
                        switch_generation: true,
                        sync_credentials: sync_creds,
                        rotate_pki: false,
                        binary_path: bin_path,
                        dry_run: req.dry_run,
                    };
                    let mut prof = self.profile.write().await;
                    let rep = deploy_to_nixos_pits_core(&mut prof, None, &opts).await?;
                    Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: req.dry_run,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Declarative NixOS pits-agent sync ({}) completed",
                            rep.activation_action
                        ),
                        changes_or_warnings: rep.planned_steps.clone(),
                        output: serde_json::to_value(&rep)?,
                        rendered_artifact: None,
                    })
                }
                other => bail!("unsupported action {other:?} on baremetal-host/pits-core"),
            };
        }

        let node = {
            let b = self.bundle.read().await;
            b.find_node(entity_name)
                .cloned()
                .ok_or_else(|| anyhow!("unknown enrolled baremetal-host {entity_name:?}"))?
        };

        match action_id {
            "verify-pxe-assets" | "verify-boot-assets" => {
                let ipxe = render_ipxe_script(&node, PxeBootMode::TalosNetboot, None);
                if req.dry_run {
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: would verify staged vmlinuz & initramfs.xz SHA-256 digests for {} on pits-core",
                            node.device_id
                        ),
                        changes_or_warnings: vec![],
                        output: json!({
                            "deviceId": node.device_id,
                            "expectedKernelUrl": format!("http://10.89.20.1:8080/talos/{}/vmlinuz", node.device_id),
                            "expectedInitrdUrl": format!("http://10.89.20.1:8080/talos/{}/initramfs.xz", node.device_id),
                        }),
                        rendered_artifact: Some(ipxe),
                    });
                }
                let ag = self.build_agent_req().await?;
                let verify = pxe_verify_assets(&ag, &node.device_id, BTreeMap::new()).await?;
                {
                    let mut guard = self.last_pxe_verify.write().await;
                    guard.insert(node.device_id.clone(), verify.clone());
                }
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!(
                        "Verified staged Talos PXE boot assets (vmlinuz & initramfs.xz) for {} on pits-core",
                        node.device_id
                    ),
                    changes_or_warnings: vec![],
                    output: verify,
                    rendered_artifact: Some(ipxe),
                })
            }
            "apply-talos-config" => {
                let default_yaml =
                    render_talos_node_config(&node, "https://10.89.20.21:6443")?;
                let yaml = param_raw_str(&req.parameters, "yaml")
                    .or_else(|| param_raw_str(&req.parameters, "configYaml"))
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or(default_yaml);
                let allowed_wwids = allowed_wwids_for_node(&node);
                let guard =
                    validate_talos_netboot_config(&yaml, &allowed_wwids, &node.approved_vfio_pci);
                if !guard.passed {
                    bail!(
                        "netboot-guard safety precondition blocked apply-talos-config on {}: {}",
                        node.device_id,
                        guard.violations.join("; ")
                    );
                }

                let mode_str = param_str(&req.parameters, "mode")
                    .unwrap_or_else(|| "no-reboot".to_string());
                let mode_num = match mode_str.as_str() {
                    "auto" | "1" => 1u64,
                    "no-reboot" | "2" => 2u64,
                    "reboot" | "3" => 3u64,
                    "try" | "5" => 5u64,
                    _ => 2u64,
                };
                let verify_live = param_bool(&req.parameters, "verifyLiveWwids", true);

                if req.dry_run {
                    let mut warnings = vec![format!(
                        "netboot-guard passed ({} YAML documents, pinned WWIDs: {:?})",
                        guard.documents_inspected, guard.matched_wwids
                    )];
                    if let Ok(tc) = self.build_talos_req().await {
                        if let Ok(msgs) = tc
                            .apply_configuration(&node, &yaml, mode_num, true, verify_live)
                            .await
                        {
                            warnings.extend(msgs);
                        }
                    }
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run passed netboot-guard for {} ({} SAN WWIDs verified)",
                            node.device_id,
                            guard.matched_wwids.len()
                        ),
                        changes_or_warnings: warnings,
                        output: serde_json::to_value(&guard)?,
                        rendered_artifact: Some(yaml),
                    });
                }

                let tc = self.build_talos_req().await?;
                let messages = tc
                    .apply_configuration(&node, &yaml, mode_num, false, verify_live)
                    .await?;
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!(
                        "Applied netboot-guard compliant Talos configuration to {} (mode={mode_str})",
                        node.device_id
                    ),
                    changes_or_warnings: messages.clone(),
                    output: json!({
                        "guard": guard,
                        "talosMessages": messages,
                    }),
                    rendered_artifact: Some(yaml),
                })
            }
            "switch-pxe-mode" => {
                let mode_str = param_str(&req.parameters, "mode")
                    .unwrap_or_else(|| "talos-netboot".to_string());
                let pxe_mode = match mode_str.as_str() {
                    "hook-inventory" | "hook" => PxeBootMode::HookInventory,
                    "talos-netboot" | "talos" => PxeBootMode::TalosNetboot,
                    other => bail!(
                        "invalid PXE mode {other:?} (expected 'talos-netboot' or 'hook-inventory')"
                    ),
                };
                let hw_yaml = render_tinkerbell_hardware_yaml(&node, pxe_mode, None);
                if req.dry_run {
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: rendered Tinkerbell Hardware manifest for {} in {mode_str} mode",
                            node.device_id
                        ),
                        changes_or_warnings: vec![],
                        output: json!({ "device": node.device_id, "mode": mode_str }),
                        rendered_artifact: Some(hw_yaml),
                    });
                }
                let kc = self.build_core_k8s_req().await?;
                let applied = apply_tinkerbell_hardware(&kc, &node, pxe_mode, None).await?;
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!(
                        "Switched Tinkerbell Hardware tink-system/{} to {mode_str}",
                        node.device_id
                    ),
                    changes_or_warnings: vec![format!(
                        "Updated allowWorkflow={} on tink-system/{}",
                        matches!(pxe_mode, PxeBootMode::HookInventory),
                        node.device_id
                    )],
                    output: applied,
                    rendered_artifact: Some(hw_yaml),
                })
            }
            "render-imager-profile" => {
                let include_realtek = param_bool(&req.parameters, "includeRealtekFirmware", true);
                let profile_yaml = render_talos_imager_profile(&node, include_realtek);
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: req.dry_run,
                    executed_at: Utc::now(),
                    summary: format!("Rendered Sidero Imager profile for {}", node.device_id),
                    changes_or_warnings: vec![],
                    output: json!({
                        "device": node.device_id,
                        "cpuVendor": node.cpu_vendor,
                        "includeRealtekFirmware": include_realtek,
                    }),
                    rendered_artifact: Some(profile_yaml),
                })
            }
            "reboot-node" => {
                let powercycle = param_bool(&req.parameters, "powercycle", true);
                if req.dry_run {
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: would invoke Talos MachineService/Reboot on {} ({}, powercycle={powercycle})",
                            node.device_id, node.node_ip
                        ),
                        changes_or_warnings: vec![
                            "Node will re-enter UEFI PXE netboot flow upon restart.".to_string(),
                        ],
                        output: json!({
                            "device": node.device_id,
                            "nodeIp": node.node_ip,
                            "powercycle": powercycle,
                        }),
                        rendered_artifact: None,
                    });
                }
                let tc = self.build_talos_req().await?;
                tc.reboot_node(&node.node_ip, powercycle).await?;
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!(
                        "Triggered Talos reboot on {} ({}, powercycle={powercycle})",
                        node.device_id, node.node_ip
                    ),
                    changes_or_warnings: vec![],
                    output: json!({ "rebooted": node.device_id, "powercycle": powercycle }),
                    rendered_artifact: None,
                })
            }
            "configure-thunderbolt-busypoll" => {
                let mut opts = TalosHighSpeedFabricOptions::default();
                let u32_param = |key: &str, default: u32| -> Result<u32> {
                    match param_i64(&req.parameters, key) {
                        Some(v) => u32::try_from(v)
                            .map_err(|_| anyhow!("invalid parameter {key}: {v} out of u32 range")),
                        None => Ok(default),
                    }
                };
                opts.socket_busy_poll_us = u32_param("socketBusyPollUs", opts.socket_busy_poll_us)?;
                opts.socket_busy_read_us = u32_param("socketBusyReadUs", opts.socket_busy_read_us)?;
                opts.napi_defer_hard_irqs = u32_param("napiDeferHardIrqs", opts.napi_defer_hard_irqs)?;
                opts.gro_flush_timeout_ns = match param_i64(&req.parameters, "groFlushTimeoutNs") {
                    Some(v) => u64::try_from(v).map_err(|_| {
                        anyhow!("invalid parameter groFlushTimeoutNs: {v} must be non-negative")
                    })?,
                    None => opts.gro_flush_timeout_ns,
                };
                if let Some(ifaces) = param_str(&req.parameters, "busyPollIfaces") {
                    opts.busy_poll_ifaces = ifaces;
                }
                opts.stream_enabled = param_bool(&req.parameters, "streamEnabled", opts.stream_enabled);
                if let Some(name) = param_str(&req.parameters, "streamName") {
                    opts.stream_name = name;
                }
                opts.stream_busy_poll =
                    param_bool(&req.parameters, "streamBusyPoll", opts.stream_busy_poll);
                opts.stream_ring_size = u32_param("streamRingSize", opts.stream_ring_size)?;
                opts.stream_throttling_ns =
                    u32_param("streamThrottlingNs", opts.stream_throttling_ns)?;
                if let Some(v) = param_i64(&req.parameters, "inHopid") {
                    opts.in_hopid = i32::try_from(v)
                        .map_err(|_| anyhow!("invalid parameter inHopid: {v}"))?;
                }
                if let Some(v) = param_i64(&req.parameters, "outHopid") {
                    opts.out_hopid = i32::try_from(v)
                        .map_err(|_| anyhow!("invalid parameter outHopid: {v}"))?;
                }
                opts.preserve_bpf_jit_harden = param_bool(
                    &req.parameters,
                    "preserveBpfJitHarden",
                    node.device_id == "netboot-005",
                );

                let mode_str = param_str(&req.parameters, "mode")
                    .unwrap_or_else(|| "no-reboot".to_string());
                let mode_num = match mode_str.as_str() {
                    "auto" | "1" => 1u64,
                    "no-reboot" | "2" => 2u64,
                    "reboot" | "3" => 3u64,
                    "try" | "5" => 5u64,
                    other => bail!(
                        "invalid Talos apply mode {other:?} (expected 'no-reboot', 'auto', 'reboot', or 'try')"
                    ),
                };

                let patch_yaml = render_talos_thunderbolt_stream_docs(&node, &opts)?;
                let patch_cmd = format!(
                    "talosctl --nodes {} patch mc --mode={} --patch-file {}-tbstream-busypoll.patch.yaml",
                    node.node_ip, mode_str, node.device_id
                );

                if req.dry_run {
                    let mut warnings = Vec::new();
                    if node.device_id == "netboot-004" && mode_str != "no-reboot" {
                        warnings.push(
                            "netboot-004 hosts active VM win11-001; use mode=no-reboot only".to_string(),
                        );
                    }
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: rendered 7.3.1 thunderbolt_stream & busy_poll patch for {} (stream={} busy_poll={} net.core.busy_poll={}us)",
                            node.device_id,
                            opts.stream_name,
                            opts.stream_busy_poll,
                            opts.socket_busy_poll_us,
                        ),
                        changes_or_warnings: warnings,
                        output: json!({
                            "device": node.device_id,
                            "nodeIp": node.node_ip,
                            "mode": mode_str,
                            "options": opts,
                            "talosctlPatchCmd": patch_cmd,
                        }),
                        rendered_artifact: Some(patch_yaml),
                    });
                }

                if node.device_id == "netboot-004" && matches!(mode_str.as_str(), "reboot" | "auto") {
                    bail!(
                        "safety policy blocks reboot/auto mode on active VM host netboot-004 (win11-001); use mode=no-reboot"
                    );
                }
                let base_yaml = param_raw_str(&req.parameters, "baseConfigYaml")
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| {
                        anyhow!(
                            "live ApplyConfiguration requires baseConfigYaml (full sealed node config) so PKI certs and LinkConfig are preserved; apply rendered patch via: {patch_cmd}"
                        )
                    })?;
                let combined_yaml = format!(
                    "{}\n---\n{}",
                    base_yaml.trim_end(),
                    patch_yaml.trim_start()
                );
                let allowed_wwids = allowed_wwids_for_node(&node);
                let guard = validate_talos_netboot_config(
                    &combined_yaml,
                    &allowed_wwids,
                    &node.approved_vfio_pci,
                );
                if !guard.passed {
                    bail!(
                        "netboot-guard blocked configure-thunderbolt-busypoll on {}: {}",
                        node.device_id,
                        guard.violations.join("; ")
                    );
                }

                let tc = self.build_talos_req().await?;
                let messages = tc
                    .apply_configuration(&node, &combined_yaml, mode_num, false, true)
                    .await?;
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!(
                        "Applied thunderbolt_stream + busy_poll config to {} (mode={mode_str})",
                        node.device_id
                    ),
                    changes_or_warnings: messages.clone(),
                    output: json!({
                        "device": node.device_id,
                        "options": opts,
                        "talosMessages": messages,
                    }),
                    rendered_artifact: Some(patch_yaml),
                })
            }
            other => bail!("unsupported action {other:?} on baremetal-host/{entity_name}"),
        }
    }

    async fn exec_npu_action(
        &self,
        current_status: &str,
        entity_ref: &str,
        entity_name: &str,
        action_id: &str,
        req: &ActionInvokeRequest,
    ) -> Result<ActionExecutionResult> {
        let host_node = entity_name.trim_end_matches("-npu");
        let default_dc = {
            let b = self.bundle.read().await;
            match b
                .find_node(host_node)
                .and_then(|n| n.npu_vendor.as_deref())
            {
                Some("amd") => "pits-amd-npu",
                _ => "pits-intel-npu",
            }
        };
        let device_class = param_str(&req.parameters, "deviceClass")
            .unwrap_or_else(|| default_dc.to_string());
        let k8s_ns =
            param_str(&req.parameters, "namespace").unwrap_or_else(|| "default".to_string());
        let claim_name = format!("npu-claim-{host_node}");
        let job_name = format!("npu-verify-{host_node}");
        let rendered = render_npu_validation_manifests(
            &k8s_ns,
            &claim_name,
            &job_name,
            &device_class,
            Some(host_node),
        );

        match action_id {
            "preview-claim-yaml" => Ok(ActionExecutionResult {
                success: true,
                entity_ref: entity_ref.to_string(),
                action_id: action_id.to_string(),
                dry_run: req.dry_run,
                executed_at: Utc::now(),
                summary: format!(
                    "Rendered DRA v1 ResourceClaim ({claim_name}) & OpenVINO Job ({job_name}) for {host_node}"
                ),
                changes_or_warnings: vec![],
                output: json!({
                    "hostNode": host_node,
                    "namespace": k8s_ns,
                    "deviceClass": device_class,
                    "claimName": claim_name,
                    "jobName": job_name,
                }),
                rendered_artifact: Some(rendered),
            }),
            "launch-openvino-verify" => {
                if current_status == "HardwarePresentDriverPending" && !req.dry_run {
                    if device_class.contains("amd") {
                        bail!(
                            "precondition blocked launch-openvino-verify on {entity_name}: status is HardwarePresentDriverPending (kernel driver 'amdxdna' is unbound on Talos node {host_node}; missing CONFIG_DRM_ACCEL_AMDXDNA, so DRA DeviceClass '{device_class}' has zero ResourceSlices)"
                        );
                    } else {
                        bail!(
                            "precondition blocked launch-openvino-verify on {entity_name}: status is HardwarePresentDriverPending (expected NPU kernel driver is unbound on Talos node {host_node}, so DRA DeviceClass '{device_class}' has zero ResourceSlices)"
                        );
                    }
                }
                if req.dry_run {
                    let mut notes = vec![format!(
                        "Would apply ResourceClaim {k8s_ns}/{claim_name} and Job {k8s_ns}/{job_name} (DeviceClass={device_class})"
                    )];
                    if current_status == "HardwarePresentDriverPending" {
                        if device_class.contains("amd") {
                            notes.push(format!(
                                "WARNING: {entity_name} is currently HardwarePresentDriverPending ('amdxdna' unbound); live execution will remain blocked until kernel driver binds."
                            ));
                        } else {
                            notes.push(format!(
                                "WARNING: {entity_name} is currently HardwarePresentDriverPending (kernel driver unbound); live execution will remain blocked until kernel driver binds."
                            ));
                        }
                    }
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: rendered OpenVINO DRA v1 verification manifests for {entity_name}"
                        ),
                        changes_or_warnings: notes,
                        output: json!({
                            "status": current_status,
                            "claimName": claim_name,
                            "jobName": job_name,
                            "deviceClass": device_class,
                        }),
                        rendered_artifact: Some(rendered),
                    });
                }

                let lab_kc = self.build_lab_k8s_req().await?;
                launch_npu_validation_job(
                    &lab_kc,
                    &k8s_ns,
                    &claim_name,
                    &job_name,
                    &device_class,
                    Some(host_node),
                )
                .await?;
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!(
                        "Launched DRA v1 ResourceClaim {k8s_ns}/{claim_name} and OpenVINO Job {k8s_ns}/{job_name} on {host_node}"
                    ),
                    changes_or_warnings: vec![],
                    output: json!({
                        "namespace": k8s_ns,
                        "claimName": claim_name,
                        "jobName": job_name,
                        "deviceClass": device_class,
                        "targetNode": host_node,
                    }),
                    rendered_artifact: Some(rendered),
                })
            }
            other => bail!("unsupported action {other:?} on npu-accelerator/{entity_name}"),
        }
    }

    async fn exec_vm_action(
        &self,
        entity_ref: &str,
        entity_name: &str,
        action_id: &str,
        req: &ActionInvokeRequest,
    ) -> Result<ActionExecutionResult> {
        let (is_lab, vm_ns) = match entity_name {
            "win11-001" => (true, "pits-vms"),
            "access-lab" => (false, "edge-access-lab"),
            other => bail!("unknown virtual-machine {other:?}"),
        };

        if entity_name == "win11-001" && action_id == "stop" {
            let confirmed = req
                .parameters
                .get("confirmSelfSessionStop")
                .and_then(|v| v.as_bool())
                == Some(true);
            if !confirmed {
                bail!(
                    "safety precondition blocked stop on win11-001: boolean parameter confirmSelfSessionStop == true is required to stop the active Windows 11 operator workstation"
                );
            }
        }

        match action_id {
            "start" | "stop" | "restart" | "pause" | "unpause" => {
                let op = match action_id {
                    "start" => VmLifecycleOp::Start,
                    "stop" => VmLifecycleOp::Stop,
                    "restart" => VmLifecycleOp::Restart,
                    "pause" => VmLifecycleOp::Pause,
                    _ => VmLifecycleOp::Unpause,
                };
                if req.dry_run {
                    let (plural, sub) = op.subresource_info();
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: would invoke KubeVirt PUT /apis/subresources.kubevirt.io/v1/namespaces/{vm_ns}/{plural}/{entity_name}/{sub}"
                        ),
                        changes_or_warnings: vec![],
                        output: json!({
                            "cluster": if is_lab { "cluster-pits-lab" } else { "cluster-pits-core" },
                            "namespace": vm_ns,
                            "vm": entity_name,
                            "operation": action_id,
                        }),
                        rendered_artifact: None,
                    });
                }
                let kc = if is_lab {
                    self.build_lab_k8s_req().await?
                } else {
                    self.build_core_k8s_req().await?
                };
                vm_lifecycle_action(&kc, vm_ns, entity_name, op).await?;
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!(
                        "Executed KubeVirt {action_id} on {vm_ns}/{entity_name}"
                    ),
                    changes_or_warnings: vec![],
                    output: json!({ "vm": entity_name, "namespace": vm_ns, "action": action_id }),
                    rendered_artifact: None,
                })
            }
            "verify-rdp-tls" if entity_name == "win11-001" => {
                let host = param_str(&req.parameters, "host")
                    .unwrap_or_else(|| WIN11_DEFAULT_HOST_IP.to_string());
                let port = param_i64(&req.parameters, "port")
                    .and_then(|p| u16::try_from(p).ok())
                    .unwrap_or(WIN11_DEFAULT_RDP_NODEPORT);
                if req.dry_run {
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: would perform X.224 CredSSP/NLA + TLS 1.3 handshake to {host}:{port}"
                        ),
                        changes_or_warnings: vec![],
                        output: json!({ "host": host, "port": port }),
                        rendered_artifact: None,
                    });
                }

                let prof = self.profile.read().await.clone();
                let rdp_ver = if prof.lab_via_agent_bridge {
                    let ag = self.build_agent_req().await?;
                    match verify_rdp_tls_endpoint(&host, port, Some((&prof, ag.tls_config))).await {
                        Ok(v) => v,
                        Err(_) => verify_rdp_tls_endpoint(&host, port, None).await?,
                    }
                } else {
                    verify_rdp_tls_endpoint(&host, port, None).await?
                };

                {
                    let mut guard = self.last_rdp_tls.write().await;
                    *guard = Some(rdp_ver.clone());
                }

                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!(
                        "Verified live RDP X.224+TLS endpoint {host}:{port} (SHA-1={}, NLA={})",
                        rdp_ver.cert_sha1_hex, rdp_ver.nla_hybrid_enforced
                    ),
                    changes_or_warnings: vec![],
                    output: serde_json::to_value(&rdp_ver)?,
                    rendered_artifact: None,
                })
            }
            "connect-pinned-rdp" if entity_name == "win11-001" => {
                let username = param_str(&req.parameters, "username")
                    .unwrap_or_else(|| "pitsadmin".to_string());
                let width = param_i64(&req.parameters, "width")
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or(3072);
                let height = param_i64(&req.parameters, "height")
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or(1920);
                let launch = param_bool(&req.parameters, "launchClient", true);
                let host = param_str(&req.parameters, "host")
                    .unwrap_or_else(|| WIN11_DEFAULT_HOST_IP.to_string());
                let port = WIN11_DEFAULT_RDP_NODEPORT;

                if req.dry_run {
                    let preview_rdp = format!(
                        "full address:s:{host}:{port}\nusername:s:{username}\ndesktopwidth:i:{width}\ndesktopheight:i:{height}\nauthentication level:i:2\nenablecredsspsupport:i:1\n"
                    );
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: would verify RDP TLS certificate and write pinned .rdp in {}\\rdp",
                            local_state_dir().display()
                        ),
                        changes_or_warnings: vec![],
                        output: json!({
                            "host": host,
                            "port": port,
                            "username": username,
                            "resolution": format!("{width}x{height}"),
                        }),
                        rendered_artifact: Some(preview_rdp),
                    });
                }

                let prof = self.profile.read().await.clone();
                let rdp_ver = if prof.lab_via_agent_bridge {
                    let ag = self.build_agent_req().await?;
                    match verify_rdp_tls_endpoint(&host, port, Some((&prof, ag.tls_config))).await {
                        Ok(v) => v,
                        Err(_) => verify_rdp_tls_endpoint(&host, port, None).await?,
                    }
                } else {
                    verify_rdp_tls_endpoint(&host, port, None).await?
                };
                {
                    let mut guard = self.last_rdp_tls.write().await;
                    *guard = Some(rdp_ver.clone());
                }
                let rdp_content = render_pinned_rdp_profile(
                    &host,
                    port,
                    &username,
                    Some(&rdp_ver.cert_sha1_hex),
                    width,
                    height,
                );
                let rdp_path = write_pinned_rdp_profile(
                    "win11-001",
                    &host,
                    port,
                    &username,
                    Some(&rdp_ver.cert_sha1_hex),
                    width,
                    height,
                )?;
                let mut warnings = Vec::new();
                let mut launched_mstsc = false;
                if launch {
                    if cfg!(target_os = "windows") {
                        launch_mstsc(&rdp_path)?;
                        launched_mstsc = true;
                    } else {
                        warnings.push(
                            "Portal host is non-Windows (pits-core); auto-launch skipped. Download or copy the thumbprint-pinned win11-001.rdp artifact to launch mstsc.exe on your Windows workstation.".to_string(),
                        );
                    }
                }
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: if launched_mstsc {
                        format!(
                            "Wrote & launched certificate-pinned RDP profile ({}) with SHA-1 {}",
                            rdp_path.display(),
                            rdp_ver.cert_sha1_hex
                        )
                    } else {
                        format!(
                            "Generated & verified certificate-pinned RDP profile (SHA-1 {}); ready for client download",
                            rdp_ver.cert_sha1_hex
                        )
                    },
                    changes_or_warnings: warnings,
                    output: json!({
                        "rdpFile": rdp_path.display().to_string(),
                        "certSha1": rdp_ver.cert_sha1_hex,
                        "launchedMstsc": launched_mstsc,
                    }),
                    rendered_artifact: Some(rdp_content),
                })
            }
            "finalize-install-media" if entity_name == "win11-001" => {
                let finalized_yaml = render_windows11_vm_manifest(false);
                if req.dry_run {
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary:
                            "Dry-run: rendered clean post-install pits-vms/win11-001 manifest (install ISOs removed)"
                                .to_string(),
                        changes_or_warnings: vec![],
                        output: json!({ "vm": "pits-vms/win11-001", "installMediaAttached": false }),
                        rendered_artifact: Some(finalized_yaml),
                    });
                }
                let lab_kc = self.build_lab_k8s_req().await?;
                finalize_windows11_vm(&lab_kc).await?;
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: "Finalized pits-vms/win11-001 and detached installation CD-ROM media"
                        .to_string(),
                    changes_or_warnings: vec![],
                    output: json!({ "finalized": "pits-vms/win11-001" }),
                    rendered_artifact: Some(finalized_yaml),
                })
            }
            "switch-win11-pool-host" if entity_name == "win11-001" => {
                let target_node = param_str(&req.parameters, "targetNode")
                    .unwrap_or_else(|| "netboot-003".to_string());
                let upgrade_004 = param_bool(&req.parameters, "upgradeNetboot004", true);
                let confirmed = req
                    .parameters
                    .get("confirmSelfSessionStop")
                    .and_then(|v| v.as_bool())
                    == Some(true);
                let placement_patch = render_win11_pool_placement_patch(&target_node)?;
                let upgrade_flag = if target_node == "netboot-003" && upgrade_004 {
                    " --upgrade-004"
                } else {
                    ""
                };
                let systemd_cmd = format!(
                    "systemd-run --unit=pits-win11-switch --no-block /var/lib/pits/tinkerbell-20261108/scripts/tinkerbell/win11-pool-switch.py switch --target {target_node}{upgrade_flag}"
                );
                let target_rdp_host = if target_node == "netboot-003" {
                    WIN11_FAILOVER_HOST_IP
                } else {
                    WIN11_DEFAULT_HOST_IP
                };
                if req.dry_run {
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: rendered KubeVirt pool placement patch and detached switch command for {target_node}"
                        ),
                        changes_or_warnings: vec![
                            "Run the rendered systemd-run command directly on pits-core so the SAN handoff survives win11-001 shutdown.".to_string(),
                        ],
                        output: json!({
                            "targetNode": target_node,
                            "upgradeNetboot004": target_node == "netboot-003" && upgrade_004,
                            "rdpEndpoint": format!("{target_rdp_host}:{WIN11_DEFAULT_RDP_NODEPORT}"),
                            "orchestratorCommand": systemd_cmd,
                        }),
                        rendered_artifact: Some(format!(
                            "# Orchestrator (run on pits-core):\n# {systemd_cmd}\n{placement_patch}"
                        )),
                    });
                }
                if !confirmed {
                    bail!(
                        "safety precondition blocked switch-win11-pool-host on win11-001: boolean parameter confirmSelfSessionStop == true is required to stop the active Windows 11 workstation during SAN handoff"
                    );
                }
                let lab_kc = self.build_lab_k8s_req().await?;
                let path = "/apis/kubevirt.io/v1/namespaces/pits-vms/virtualmachines/win11-001";
                let patch_val: serde_json::Value = serde_json::from_str(&placement_patch)?;
                lab_kc.merge_patch(path, &patch_val).await?;
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!(
                        "Patched win11-001 pool placement for {target_node}; run win11-pool-switch.py on pits-core for full SAN nvmet handoff"
                    ),
                    changes_or_warnings: vec![format!("pits-core command: {systemd_cmd}")],
                    output: json!({
                        "targetNode": target_node,
                        "rdpEndpoint": format!("{target_rdp_host}:{WIN11_DEFAULT_RDP_NODEPORT}"),
                        "orchestratorCommand": systemd_cmd,
                    }),
                    rendered_artifact: Some(placement_patch),
                })
            }
            "live-migrate-vm" if entity_name == "win11-001" => {
                let raw_target = param_str(&req.parameters, "targetNode");
                let resolved_target = validate_live_migration_target(raw_target.as_deref())?;
                let mig_name = format!("win11-mig-{}", Utc::now().format("%Y%m%d%H%M%S"));
                let manifest = render_vm_live_migration_manifest("pits-vms", "win11-001", &mig_name);
                let cli_cmd = match &resolved_target {
                    Some(t) => format!(
                        "/var/lib/pits/tinkerbell-20261108/scripts/tinkerbell/win11-live-migrate.py migrate {t}"
                    ),
                    None => "/var/lib/pits/tinkerbell-20261108/scripts/tinkerbell/win11-live-migrate.py migrate auto".to_string(),
                };
                if req.dry_run {
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: rendered VirtualMachineInstanceMigration {mig_name} over 40Gbps tb-migration (target={})",
                            resolved_target.as_deref().unwrap_or("auto")
                        ),
                        changes_or_warnings: vec![
                            "Requires win11-001 to be cut over to RWX block+state (LiveMigratable=True).".to_string(),
                        ],
                        output: json!({
                            "migrationName": mig_name,
                            "targetNode": resolved_target.as_deref().unwrap_or("auto"),
                            "migrationNetwork": "tb-migration",
                            "cliCommand": cli_cmd,
                        }),
                        rendered_artifact: Some(manifest),
                    });
                }
                let lab_kc = self.build_lab_k8s_req().await?;
                let applied = trigger_vm_live_migration(
                    &lab_kc,
                    "pits-vms",
                    "win11-001",
                    &mig_name,
                    resolved_target.as_deref(),
                )
                .await?;
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!(
                        "Submitted KubeVirt live migration pits-vms/{mig_name} (target={})",
                        resolved_target.as_deref().unwrap_or("auto")
                    ),
                    changes_or_warnings: vec![],
                    output: json!({
                        "migrationName": mig_name,
                        "targetNode": resolved_target.as_deref().unwrap_or("auto"),
                        "migrationNetwork": "tb-migration",
                        "apiResponse": applied,
                    }),
                    rendered_artifact: Some(manifest),
                })
            }
            other => bail!("unsupported action {other:?} on virtual-machine/{entity_name}"),
        }
    }

    async fn exec_router_action(
        &self,
        entity_ref: &str,
        entity_name: &str,
        action_id: &str,
        req: &ActionInvokeRequest,
    ) -> Result<ActionExecutionResult> {
        match action_id {
            "apply-router-spec" => {
                let default_yaml = {
                    let b = self.bundle.read().await;
                    b.edge_router_yaml
                        .clone()
                        .unwrap_or_else(|| serde_yaml::to_string(&pits_core_default_edge_router()).unwrap_or_default())
                };
                let yaml = param_raw_str(&req.parameters, "yaml")
                    .or_else(|| param_raw_str(&req.parameters, "manifestYaml"))
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or(default_yaml);
                let manifest: EdgeRouterManifest = serde_yaml::from_str(&yaml)
                    .context("parse EdgeRouter YAML for apply-router-spec")?;
                let val_rep = validate_edge_router(&manifest);
                if !val_rep.valid {
                    bail!(
                        "router safety guard blocked apply-router-spec (mgmt0 {} safe={}, core<->lab isolated={}, lab->mgmt blocked={}): {}",
                        MGMT0_PCI,
                        val_rep.mgmt0_pci_safe,
                        val_rep.core_lab_isolated,
                        val_rep.lab_mgmt_blocked,
                        val_rep.errors.join("; ")
                    );
                }

                let canonical_yaml = serde_yaml::to_string(&manifest)?;
                if req.dry_run {
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: EdgeRouter policy guard passed (sha256={})",
                            &val_rep.desired_spec_sha256[..16]
                        ),
                        changes_or_warnings: val_rep.warnings.clone(),
                        output: serde_json::to_value(&val_rep)?,
                        rendered_artifact: Some(canonical_yaml),
                    });
                }

                let kc = self.build_core_k8s_req().await?;
                let applied = apply_edge_router(&kc, &manifest).await?;
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!(
                        "Applied validated EdgeRouter edge/pits-core (sha256={})",
                        &val_rep.desired_spec_sha256[..16]
                    ),
                    changes_or_warnings: val_rep.warnings.clone(),
                    output: json!({
                        "validation": val_rep,
                        "applied": applied,
                    }),
                    rendered_artifact: Some(canonical_yaml),
                })
            }
            "pause-reconcile" | "resume-reconcile" => {
                let paused = action_id == "pause-reconcile";
                if req.dry_run {
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: would set crossplane.io/paused={paused} on edge/pits-core"
                        ),
                        changes_or_warnings: vec![],
                        output: json!({ "crossplanePaused": paused }),
                        rendered_artifact: None,
                    });
                }
                let kc = self.build_core_k8s_req().await?;
                set_crossplane_paused(&kc, "edge", "pits-core", paused).await?;
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!(
                        "{} Crossplane reconciliation on edge/pits-core",
                        if paused { "Paused" } else { "Resumed" }
                    ),
                    changes_or_warnings: vec![],
                    output: json!({ "crossplanePaused": paused }),
                    rendered_artifact: None,
                })
            }
            "recover-dataplane" => {
                let cleanup = param_bool(&req.parameters, "cleanupPerfNetns", true);
                let refresh = param_bool(&req.parameters, "refreshDevicePlugins", true);
                let wait_secs = param_i64(&req.parameters, "waitSeconds").unwrap_or(60) as i32;
                if req.dry_run {
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: would run mgmt0-guarded ({MGMT0_PCI}) VFIO restoration and verify edge/gateway readiness"
                        ),
                        changes_or_warnings: vec![],
                        output: json!({
                            "cleanupPerfNetns": cleanup,
                            "refreshDevicePlugins": refresh,
                            "waitReadySeconds": wait_secs,
                        }),
                        rendered_artifact: None,
                    });
                }
                let ag = self.build_agent_req().await?;
                let out = recover_router_runtime(&ag, cleanup, refresh, wait_secs).await?;
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: "Completed mgmt0-guarded VFIO recovery and verified edge/gateway rollout"
                        .to_string(),
                    changes_or_warnings: vec![],
                    output: out,
                    rendered_artifact: None,
                })
            }
            "capture-vpp-trace" => {
                let count = param_i64(&req.parameters, "count")
                    .or_else(|| param_i64(&req.parameters, "packetCount"))
                    .unwrap_or(50)
                    .clamp(1, 500) as i32;
                let filter = param_str(&req.parameters, "filter")
                    .or_else(|| param_str(&req.parameters, "filterText"))
                    .unwrap_or_default();
                if req.dry_run {
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: would capture {count} dpdk-input packet(s) via GoVPP socket"
                        ),
                        changes_or_warnings: vec![],
                        output: json!({ "packetCount": count, "filterText": filter }),
                        rendered_artifact: None,
                    });
                }
                let ag = self.build_agent_req().await?;
                let trace: serde_json::Value = ag
                    .post_json(
                        "/v1/vpp/trace",
                        &json!({ "packetCount": count, "filterIp": filter, "clearAfter": false }),
                    )
                    .await?;
                let trace_str = trace
                    .get("excerpt")
                    .or_else(|| trace.get("traceOutput"))
                    .or_else(|| trace.get("trace"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!("Captured VPP dpdk-input trace ({count} packets)"),
                    changes_or_warnings: vec![],
                    output: trace,
                    rendered_artifact: trace_str,
                })
            }
            other => bail!("unsupported action {other:?} on edge-router/{entity_name}"),
        }
    }

    async fn exec_pxe_profile_action(
        &self,
        entity_ref: &str,
        entity_name: &str,
        action_id: &str,
        req: &ActionInvokeRequest,
    ) -> Result<ActionExecutionResult> {
        let mode = if entity_name.contains("hook") {
            PxeBootMode::HookInventory
        } else {
            PxeBootMode::TalosNetboot
        };
        let dev_id = param_str(&req.parameters, "device")
            .unwrap_or_else(|| "netboot-004".to_string());
        let node = {
            let b = self.bundle.read().await;
            b.find_node(&dev_id)
                .cloned()
                .ok_or_else(|| anyhow!("unknown enrolled node {dev_id:?}"))?
        };

        match action_id {
            "render-node-ipxe" => {
                let ipxe = render_ipxe_script(&node, mode, None);
                let hw_yaml = render_tinkerbell_hardware_yaml(&node, mode, None);
                let combined = format!("# --- iPXE Script ({dev_id}) ---\n{ipxe}\n---\n{hw_yaml}");
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: req.dry_run,
                    executed_at: Utc::now(),
                    summary: format!("Rendered iPXE script & Tinkerbell Hardware YAML for {dev_id}"),
                    changes_or_warnings: vec![],
                    output: json!({
                        "profile": entity_name,
                        "device": dev_id,
                        "ipxeScript": ipxe,
                    }),
                    rendered_artifact: Some(combined),
                })
            }
            "verify-boot-assets" | "verify-pxe-assets" => {
                if req.dry_run {
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: would verify staged HTTP boot assets for {dev_id} ({entity_name})"
                        ),
                        changes_or_warnings: vec![],
                        output: json!({ "profile": entity_name, "device": dev_id }),
                        rendered_artifact: None,
                    });
                }
                let ag = self.build_agent_req().await?;
                let out = pxe_verify_assets(&ag, &dev_id, BTreeMap::new()).await?;
                {
                    let mut guard = self.last_pxe_verify.write().await;
                    guard.insert(dev_id.clone(), out.clone());
                }
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!(
                        "Verified staged boot assets for {dev_id} under profile {entity_name}"
                    ),
                    changes_or_warnings: vec![],
                    output: out,
                    rendered_artifact: None,
                })
            }
            other => bail!("unsupported action {other:?} on pxe-boot-profile/{entity_name}"),
        }
    }

    async fn exec_storage_pool_action(
        &self,
        entity_ref: &str,
        entity_name: &str,
        action_id: &str,
        req: &ActionInvokeRequest,
    ) -> Result<ActionExecutionResult> {
        let op = match action_id {
            "trim-pool" => "trim",
            "scrub-verify" => "scrub",
            other => bail!("unsupported action {other:?} on storage-pool/{entity_name}"),
        };
        if req.dry_run {
            return Ok(ActionExecutionResult {
                success: true,
                entity_ref: entity_ref.to_string(),
                action_id: action_id.to_string(),
                dry_run: true,
                executed_at: Utc::now(),
                summary: format!(
                    "Dry-run: would invoke serialized Btrfs '{op}' operation on pool {entity_name} (/var/mnt/tank)"
                ),
                changes_or_warnings: vec![],
                output: json!({ "pool": entity_name, "operation": op }),
                rendered_artifact: None,
            });
        }
        let ag = self.build_agent_req().await?;
        let out = btrfs_maintain(&ag, op).await?;
        Ok(ActionExecutionResult {
            success: true,
            entity_ref: entity_ref.to_string(),
            action_id: action_id.to_string(),
            dry_run: false,
            executed_at: Utc::now(),
            summary: format!("Completed Btrfs '{op}' maintenance on pool {entity_name}"),
            changes_or_warnings: vec![],
            output: out,
            rendered_artifact: None,
        })
    }

    async fn exec_san_volume_action(
        &self,
        entity_ref: &str,
        entity_name: &str,
        action_id: &str,
        req: &ActionInvokeRequest,
    ) -> Result<ActionExecutionResult> {
        let (node, ns_spec) = {
            let b = self.bundle.read().await;
            let mut found = None;
            for n in &b.nodes {
                for ns in &n.san.namespaces {
                    if format!("{}-{}", n.device_id, ns.name) == entity_name {
                        found = Some((n.clone(), ns.clone()));
                        break;
                    }
                }
            }
            found.ok_or_else(|| anyhow!("unknown san-volume entity {entity_name:?}"))?
        };
        let wwid = expected_wwid(&node.san.subsystem_nqn, &ns_spec.name);

        match action_id {
            "reconcile-volume" => {
                let hot_add = param_bool(&req.parameters, "hotAddOnly", ns_spec.id == 3);
                if req.dry_run {
                    let mut live_preview = None;
                    if self.is_profile_configured().await {
                        if let Ok(ag) = self.build_agent_req().await {
                            live_preview =
                                san_provision(&ag, &node.san, hot_add, !hot_add, true).await.ok();
                        }
                    }
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: validated NVMe/TCP configfs & 4KiB NOCOW reconciliation plan for {entity_name} ({wwid})"
                        ),
                        changes_or_warnings: vec![],
                        output: json!({
                            "volume": entity_name,
                            "nsid": ns_spec.id,
                            "wwid": wwid,
                            "bytes": ns_spec.bytes,
                            "hotAddOnly": hot_add,
                            "agentPreview": live_preview,
                        }),
                        rendered_artifact: None,
                    });
                }
                let ag = self.build_agent_req().await?;
                let out = san_provision(&ag, &node.san, hot_add, !hot_add, false).await?;
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!(
                        "Reconciled NVMe/TCP SAN volume {entity_name} (NSID {}, {wwid})",
                        ns_spec.id
                    ),
                    changes_or_warnings: vec![],
                    output: out,
                    rendered_artifact: None,
                })
            }
            "expand-volume" => {
                if ns_spec.name == "system" {
                    bail!(
                        "safety guard blocked expand-volume on {entity_name}: NSID 1 system boot volumes use fixed 66 GiB GPT geometry"
                    );
                }
                let raw_bytes = if let Some(gib) = param_i64(&req.parameters, "sizeGib") {
                    if gib <= 0 {
                        bail!("sizeGib must be positive, got {gib}");
                    }
                    gib.checked_mul(1024 * 1024 * 1024)
                        .ok_or_else(|| anyhow!("sizeGib {gib} overflowed i64 bytes"))?
                } else if let Some(b) = param_i64(&req.parameters, "newBytes") {
                    b
                } else {
                    bail!("expand-volume requires integer parameter 'sizeGib'")
                };
                let aligned_bytes = align_4k(raw_bytes);
                if aligned_bytes < ns_spec.bytes {
                    bail!(
                        "safety guard blocked expand-volume on {entity_name}: requested {} bytes is smaller than current {} bytes (SAN expansion is strictly additive)",
                        aligned_bytes,
                        ns_spec.bytes
                    );
                }

                if req.dry_run {
                    let mut agent_dry_run = None;
                    if self.is_profile_configured().await {
                        if let Ok(ag) = self.build_agent_req().await {
                            agent_dry_run = san_expand(
                                &ag,
                                &node.device_id,
                                ns_spec.id,
                                aligned_bytes,
                                true,
                            )
                            .await
                            .ok();
                        }
                    }
                    let delta_bytes = aligned_bytes - ns_spec.bytes;
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: verified 4KiB-aligned online expansion of {entity_name} (NSID {}) to {} bytes (+{} bytes)",
                            ns_spec.id, aligned_bytes, delta_bytes
                        ),
                        changes_or_warnings: vec![
                            format!(
                                "Backing file /var/mnt/tank/@data/pits/san/{}/{}.img will grow to {} bytes (4KiB aligned) and write 1 to configfs revalidate_size",
                                node.device_id, ns_spec.name, aligned_bytes
                            ),
                        ],
                        output: json!({
                            "volume": entity_name,
                            "deviceId": node.device_id,
                            "namespaceId": ns_spec.id,
                            "wwid": wwid,
                            "previousBytes": ns_spec.bytes,
                            "newAlignedBytes": aligned_bytes,
                            "deltaBytes": delta_bytes,
                            "agentDryRun": agent_dry_run,
                        }),
                        rendered_artifact: None,
                    });
                }

                let ag = self.build_agent_req().await?;
                let out = san_expand(&ag, &node.device_id, ns_spec.id, aligned_bytes, false).await?;
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!(
                        "Expanded SAN volume {entity_name} (NSID {}) online to {} bytes",
                        ns_spec.id, aligned_bytes
                    ),
                    changes_or_warnings: vec![],
                    output: out,
                    rendered_artifact: None,
                })
            }
            "apply-nvmet-deployment" => {
                let rendered = render_nvmet_k8s_manifests(&node.san)?;
                if req.dry_run {
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: rendered tink-system/nvmet-{} Deployment + lab-vlan20-san Multus NAD",
                            node.device_id
                        ),
                        changes_or_warnings: vec![],
                        output: json!({
                            "deviceId": node.device_id,
                            "targetAddress": format!("{}:{}", node.san.address, node.san.port),
                        }),
                        rendered_artifact: Some(rendered),
                    });
                }
                let kc = self.build_core_k8s_req().await?;
                apply_nvmet_k8s(&kc, &node.san).await?;
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!(
                        "Applied tink-system/nvmet-{} Deployment and Multus NAD on pits-core",
                        node.device_id
                    ),
                    changes_or_warnings: vec![],
                    output: json!({ "applied": format!("tink-system/nvmet-{}", node.device_id) }),
                    rendered_artifact: Some(rendered),
                })
            }
            "bind-static-pv" => {
                if ns_spec.name == "system" {
                    bail!(
                        "safety guard blocked bind-static-pv on {entity_name}: NSID 1 system volumes cannot be bound as workload PersistentVolumes"
                    );
                }
                let default_gib =
                    (((ns_spec.bytes as f64) / (1024.0 * 1024.0 * 1024.0)).ceil() as u32).max(1);
                let pv_name = param_str(&req.parameters, "pvName")
                    .unwrap_or_else(|| format!("pv-{entity_name}"));
                let k8s_ns = param_str(&req.parameters, "namespace").unwrap_or_else(|| {
                    if ns_spec.name == "vmstore" {
                        "pits-vms".to_string()
                    } else {
                        "default".to_string()
                    }
                });
                let pvc_name = param_str(&req.parameters, "pvcName")
                    .unwrap_or_else(|| format!("{entity_name}-pvc"));
                let size_gib = param_i64(&req.parameters, "sizeGib")
                    .and_then(|g| u32::try_from(g).ok())
                    .unwrap_or(default_gib);
                let (storage_class, host_path) = if ns_spec.name == "vmstore" {
                    ("pits-vm-local", "/var/mnt/vmstore/win11-001")
                } else {
                    ("san-node-local", "/var/mnt/userdata/workloads")
                };

                let yaml = render_san_local_pv_pvc(
                    &pv_name,
                    &k8s_ns,
                    &pvc_name,
                    &node.hostname,
                    storage_class,
                    host_path,
                    size_gib,
                );
                if req.dry_run {
                    return Ok(ActionExecutionResult {
                        success: true,
                        entity_ref: entity_ref.to_string(),
                        action_id: action_id.to_string(),
                        dry_run: true,
                        executed_at: Utc::now(),
                        summary: format!(
                            "Dry-run: rendered static PV {pv_name} & PVC {k8s_ns}/{pvc_name} pinned to {}",
                            node.hostname
                        ),
                        changes_or_warnings: vec![],
                        output: json!({
                            "pvName": pv_name,
                            "namespace": k8s_ns,
                            "pvcName": pvc_name,
                            "node": node.hostname,
                            "sizeGib": size_gib,
                        }),
                        rendered_artifact: Some(yaml),
                    });
                }
                let lab_kc = self.build_lab_k8s_req().await?;
                apply_san_local_pv_pvc(
                    &lab_kc,
                    &pv_name,
                    &k8s_ns,
                    &pvc_name,
                    &node.hostname,
                    storage_class,
                    host_path,
                    size_gib,
                )
                .await?;
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: false,
                    executed_at: Utc::now(),
                    summary: format!(
                        "Bound static PV {pv_name} & PVC {k8s_ns}/{pvc_name} on {}",
                        node.hostname
                    ),
                    changes_or_warnings: vec![],
                    output: json!({ "pvName": pv_name, "pvc": format!("{k8s_ns}/{pvc_name}") }),
                    rendered_artifact: Some(yaml),
                })
            }
            other => bail!("unsupported action {other:?} on san-volume/{entity_name}"),
        }
    }

    async fn exec_cluster_action(
        &self,
        entity_ref: &str,
        entity_name: &str,
        action_id: &str,
        req: &ActionInvokeRequest,
    ) -> Result<ActionExecutionResult> {
        match action_id {
            "audit-fleet-npus" => {
                let bundle = self.bundle.read().await.clone();
                let configured = self.is_profile_configured().await;
                let ag = if configured {
                    self.build_agent_req().await.ok()
                } else {
                    None
                };
                let core = if configured {
                    self.build_core_k8s_req().await.ok()
                } else {
                    None
                };
                let lab = if configured {
                    self.build_lab_k8s_req().await.ok()
                } else {
                    None
                };
                let tc = if configured {
                    self.build_talos_req().await.ok()
                } else {
                    None
                };
                let rep = audit_fleet_npus(
                    &bundle,
                    ag.as_ref(),
                    core.as_ref(),
                    lab.as_ref(),
                    tc.as_ref(),
                )
                .await;
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: req.dry_run,
                    executed_at: Utc::now(),
                    summary: format!(
                        "Audited {} machines: {} physical NPUs ({} DRA v1 ready, {} driver pending)",
                        rep.total_machines,
                        rep.physical_npu_machines,
                        rep.dra_ready_machines,
                        rep.driver_pending_machines
                    ),
                    changes_or_warnings: vec![],
                    output: serde_json::to_value(&rep)?,
                    rendered_artifact: Some(serde_yaml::to_string(&rep)?),
                })
            }
            "refresh-cluster-state" => {
                self.invalidate_cache().await;
                let summary_val = if entity_name == "cluster-pits-core" {
                    match self.build_core_k8s_req().await {
                        Ok(kc) => serde_json::to_value(inspect_cluster_fast(&kc, None).await)?,
                        Err(e) => json!({ "cluster": "pits-core", "reachable": false, "note": e.to_string() }),
                    }
                } else {
                    match self.build_lab_k8s_req().await {
                        Ok(kc) => serde_json::to_value(inspect_cluster_fast(&kc, None).await)?,
                        Err(e) => json!({ "cluster": "pits-lab", "reachable": false, "note": e.to_string() }),
                    }
                };
                Ok(ActionExecutionResult {
                    success: true,
                    entity_ref: entity_ref.to_string(),
                    action_id: action_id.to_string(),
                    dry_run: req.dry_run,
                    executed_at: Utc::now(),
                    summary: format!("Invalidated catalog cache and refreshed {entity_name}"),
                    changes_or_warnings: vec![],
                    output: summary_val,
                    rendered_artifact: None,
                })
            }
            other => bail!("unsupported action {other:?} on {entity_name}"),
        }
    }

    async fn exec_sourcing_action(
        &self,
        entity_ref: &str,
        entity_name: &str,
        action_id: &str,
        req: &ActionInvokeRequest,
        diagnostics: &serde_json::Value,
    ) -> Result<ActionExecutionResult> {
        let lineage: Vec<DomainLineageEntry> = diagnostics
            .get("lineage")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let system_domains: BTreeMap<String, (String, String)> = default_system_domain_map()
            .into_iter()
            .map(|(k, (d, t))| (k, (d.to_string(), t.to_string())))
            .collect();

        let (original_rec, existing_ws_yaml, state_store_path) = {
            let store = self.sourcing.read().await;
            let rec = store
                .get(entity_name)
                .cloned()
                .ok_or_else(|| anyhow!("unknown sourcing entity {entity_name:?}"))?;
            let existing = store.read_existing_workspace_yaml(entity_name);
            let path = store.state_store_path.display().to_string();
            (rec, existing, path)
        };

        let mut updated = original_rec.clone();
        let mut notes = Vec::new();
        let today = today_utc_date();

        match action_id {
            "calibrate-sourcing-scores" => {
                if let Some(v) = param_f64(&req.parameters, "y2_coreCompetitiveness") {
                    updated.audit.manual_scores.y2_core_competitiveness = v;
                }
                if let Some(v) = param_f64(&req.parameters, "y4_priceElasticity") {
                    updated.audit.manual_scores.y4_price_elasticity = v;
                }
                if let Some(v) = param_f64(&req.parameters, "x2_marketScarcity") {
                    updated.audit.manual_scores.x2_market_scarcity = v;
                }
                if let Some(v) = param_f64(&req.parameters, "x3_customizationDepth") {
                    updated.audit.manual_scores.x3_customization_depth = v;
                }
                if let Some(v) = param_f64(&req.parameters, "x4_complianceFragility") {
                    updated.audit.manual_scores.x4_compliance_fragility = v;
                }
                updated.audit.contract_meta.last_audited_at = today.format("%Y-%m-%d").to_string();
                notes.push("Updated manual blind-review dimensions and stamped lastAuditedAt".to_string());
            }
            "calibrate-supplier-windmill" => {
                if let Some(s) = param_str(&req.parameters, "perceivedStatus") {
                    updated.audit.supplier_preference.perceived_status = s.to_ascii_lowercase();
                }
                if let Some(t) = param_str(&req.parameters, "estimatedWalletShareTier") {
                    updated.audit.supplier_preference.estimated_wallet_share_tier = t.to_ascii_lowercase();
                }
                if let Some(v) = param_f64(&req.parameters, "accountAttractivenessScore") {
                    updated.audit.supplier_preference.account_attractiveness_score = v;
                }
                updated.audit.contract_meta.last_audited_at = today.format("%Y-%m-%d").to_string();
                notes.push("Updated Dutch Windmill supplier preference parameters".to_string());
            }
            "toggle-adapter-abstraction" => {
                let target_flag = param_bool(
                    &req.parameters,
                    "adapterAbstracted",
                    !updated.adapter_abstracted,
                );
                updated.adapter_abstracted = target_flag;
                notes.push(format!(
                    "Set sourcing.company.io/adapter-abstracted -> \"{}\"",
                    target_flag
                ));
            }
            "acknowledge-reaudit-baseline" => {
                if let Some(rd) = param_str(&req.parameters, "renewalDate") {
                    updated.audit.contract_meta.renewal_date = rd;
                }
                let default_deps = lineage.len().max(1) as i64;
                let next_base = param_i64(&req.parameters, "baselineProdDependents")
                    .unwrap_or(default_deps)
                    .max(1) as usize;
                updated.audit.contract_meta.baseline_prod_dependents = next_base;
                if param_bool(&req.parameters, "clearVarianceAlert", true) {
                    updated.audit.contract_meta.blind_score_variance_alert = false;
                }
                updated.audit.contract_meta.last_audited_at = today.format("%Y-%m-%d").to_string();
                notes.push(format!(
                    "Reset baselineProdDependents={} and stamped lastAuditedAt={}",
                    next_base, updated.audit.contract_meta.last_audited_at
                ));
            }
            "sync-workspace-yaml" => {
                notes.push("Rendered canonical backstage.io/v1alpha1 catalog-info.yaml".to_string());
            }
            other => bail!("unsupported sourcing action {other:?} on {entity_name}"),
        }

        let report = validate_sourcing_record(&updated, None, &system_domains);
        if !report.valid {
            bail!(
                "CatalogProcessor rejected sourcing mutation on {entity_name}: {}",
                report.errors.join("; ")
            );
        }

        let mut eval = evaluate_sourcing_audit_at(&updated, lineage, &system_domains, today);
        let generated_yaml = render_catalog_info_yaml(&updated, Some(&eval.facts));
        let baseline_yaml = if existing_ws_yaml.is_empty() {
            render_catalog_info_yaml(&original_rec, None)
        } else {
            existing_ws_yaml
        };
        let diff = compute_unified_diff(
            &baseline_yaml,
            &generated_yaml,
            &format!("catalog/sourcing/{}.yaml", entity_name),
        );

        let clear_variance = action_id == "acknowledge-reaudit-baseline"
            && param_bool(&req.parameters, "clearVarianceAlert", true);
        let mut workspace_written_path = None;
        if !req.dry_run {
            let mut store = self.sourcing.write().await;
            if action_id != "sync-workspace-yaml" {
                let source_origin = if action_id == "calibrate-sourcing-scores" {
                    "action-calibrate"
                } else {
                    "preserve"
                };
                let event_summary = format!(
                    "Portal entity action '{}' on '{}': {}",
                    action_id,
                    entity_name,
                    notes.join("; ")
                );
                if action_id == "calibrate-supplier-windmill" {
                    store
                        .db
                        .assess_windmill(
                            &AssessWindmillRequest {
                                unit_name: entity_name.to_string(),
                                estimated_wallet_share_tier: updated
                                    .audit
                                    .supplier_preference
                                    .estimated_wallet_share_tier
                                    .clone(),
                                account_attractiveness_score: updated
                                    .audit
                                    .supplier_preference
                                    .account_attractiveness_score,
                                perceived_status: updated
                                    .audit
                                    .supplier_preference
                                    .perceived_status
                                    .clone(),
                                assessed_by: "portal-action".to_string(),
                                rationale: "Updated via Portal Entity Action".to_string(),
                            },
                            "portal-action",
                        )
                        .map_err(|e| anyhow!(e))?;
                }
                store
                    .upsert_record_with_meta(
                        updated.clone(),
                        true,
                        source_origin,
                        "portal-operator",
                        "portal-action",
                        &event_summary,
                        clear_variance,
                    )
                    .map_err(|e| anyhow!(e))?;
            } else {
                let written = store
                    .write_workspace_yaml(&updated, Some(&eval.facts))
                    .map_err(|e| anyhow!(e))?;
                let written_str = written.display().to_string();
                let _ = store.db.append_event(
                    entity_name,
                    "sync-workspace-yaml",
                    "portal-operator",
                    "portal-action",
                    &format!("Exported catalog-info.yaml to {}", written_str),
                    json!({ "workspaceYamlWritten": written_str }),
                );
                workspace_written_path = Some(written_str);
            }
            store.enrich_evaluation(&mut eval);
        } else {
            let store = self.sourcing.read().await;
            store.enrich_evaluation(&mut eval);
        }

        let summary = if req.dry_run {
            format!(
                "Dry-run '{action_id}' on {entity_name}: Y={:.2}, X={:.2}, quadrant='{}', mismatch={}, reAudit={}, criticalZone={}",
                eval.facts.final_y_score,
                eval.facts.final_x_score,
                eval.facts.quadrant,
                eval.facts.windmill_mismatch,
                eval.facts.re_audit_required,
                eval.facts.in_critical_zone,
            )
        } else {
            format!(
                "Applied '{action_id}' on {entity_name}: Y={:.2}, X={:.2}, quadrant='{}' (persisted to {})",
                eval.facts.final_y_score,
                eval.facts.final_x_score,
                eval.facts.quadrant,
                workspace_written_path.as_deref().unwrap_or(&state_store_path),
            )
        };

        Ok(ActionExecutionResult {
            success: true,
            entity_ref: entity_ref.to_string(),
            action_id: action_id.to_string(),
            dry_run: req.dry_run,
            executed_at: Utc::now(),
            summary,
            changes_or_warnings: notes,
            output: json!({
                "evaluation": eval,
                "stateStorePath": state_store_path,
                "workspaceYamlWritten": workspace_written_path,
                "unifiedDiff": diff,
            }),
            rendered_artifact: Some(format!("{generated_yaml}\n# --- Unified Diff ---\n{diff}")),
        })
    }
}
