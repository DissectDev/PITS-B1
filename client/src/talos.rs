//! Domain 4: Talos Linux diskless node lifecycle (`netboot-001..004`), native `netboot-guard`
//! policy engine, multi-document machine config + `pits-nvme-early` extension generator,
//! and native HTTP/2 mTLS gRPC client (`MachineService` & COSI `State`).

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_yaml::Deserializer;
use std::collections::BTreeSet;
use std::sync::Arc;

use crate::config::{ClientProfile, EnrolledNodeContract, ResolvedTalosContext};
use crate::grpc_wire::{
    decode_fields, encode_bool_field, encode_bytes_field, encode_string_field, encode_varint_field,
    invoke_grpc, GrpcTarget,
};
use crate::pxe::{classify_pci_npu, resolve_retail_machine_model, DetectedNpuHardware};
use crate::san::expected_wwid;
use crate::transport::build_rustls_client_config;

pub const TALOS_VERSION: &str = "v1.14.5";
pub const LAB_K8S_VERSION: &str = "v1.37.1";
pub const TALOS_APID_PORT: u16 = 50000;

/// Result of running the native Rust `netboot-guard` policy validator on a Talos YAML bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetbootGuardReport {
    pub passed: bool,
    pub documents_inspected: usize,
    pub matched_wwids: Vec<String>,
    pub pci_rebinds: Vec<String>,
    pub violations: Vec<String>,
}

/// Enforces all PITS diskless Talos safety rules (replacing `cmd/talos-netboot-guard` & shell wrappers):
/// - Forbids `machine.install` and `machine.disks`
/// - Forbids `UnattendedInstallConfig`, `DiskWipeConfig`, `RawVolumeConfig`
/// - Requires every `VolumeConfig` / `UserVolumeConfig` with `diskSelector` to match an exact
///   `disk.wwid == "uuid.<deterministic-wwid>"` belonging to the node's NVMe/TCP SAN contract
/// - Rejects `PCIDriverRebindConfig` unless explicitly approved (e.g. isolated `0000:01:00.0`).
pub fn validate_talos_netboot_config(
    multi_doc_yaml: &str,
    allowed_wwids: &[String],
    approved_vfio_pci: &[String],
) -> NetbootGuardReport {
    let mut violations = Vec::new();
    let mut matched_wwids = BTreeSet::new();
    let mut pci_rebinds = Vec::new();
    let mut docs_count = 0usize;

    let allowed_wwid_set: BTreeSet<&str> = allowed_wwids.iter().map(String::as_str).collect();
    let approved_pci_set: BTreeSet<&str> = approved_vfio_pci.iter().map(String::as_str).collect();

    for doc in Deserializer::from_str(multi_doc_yaml) {
        let val: serde_json::Value = match serde::Deserialize::deserialize(doc) {
            Ok(v) => v,
            Err(e) => {
                violations.push(format!("YAML parse error: {e}"));
                continue;
            }
        };
        if val.is_null() {
            continue;
        }
        docs_count += 1;

        let kind = val.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "UnattendedInstallConfig" | "DiskWipeConfig" | "RawVolumeConfig" => {
                violations.push(format!(
                    "forbidden destructive Talos document kind {kind:?} in diskless netboot config"
                ));
                continue;
            }
            "VolumeConfig" | "UserVolumeConfig" => {
                let vol_name = val.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");
                let match_expr = val
                    .pointer("/provisioning/diskSelector/match")
                    .and_then(|v| v.as_str());
                match match_expr {
                    None => {
                        // In-memory / system non-provisioned EPHEMERAL overrides without diskSelector are checked:
                        if kind == "UserVolumeConfig"
                            || vol_name == "STATE"
                            || vol_name == "META"
                            || vol_name == "EPHEMERAL"
                        {
                            violations.push(format!(
                                "{kind} ({vol_name}) missing provisioning.diskSelector.match WWID pin"
                            ));
                        }
                    }
                    Some(expr) => {
                        if let Some(wwid) = extract_wwid_from_cel(expr) {
                            if !allowed_wwid_set.is_empty()
                                && !allowed_wwid_set.contains(wwid.as_str())
                            {
                                violations.push(format!(
                                    "{kind} ({vol_name}) references unauthorized WWID {wwid:?} (allowed: {allowed_wwids:?})"
                                ));
                            } else {
                                matched_wwids.insert(wwid);
                            }
                        } else {
                            violations.push(format!(
                                "{kind} ({vol_name}) uses non-WWID diskSelector {expr:?}; must be 'disk.wwid == \"uuid.<uuid>\"'"
                            ));
                        }
                    }
                }
            }
            "PCIDriverRebindConfig" => {
                let pci_addr = val.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let target_drv = val
                    .get("targetDriver")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                pci_rebinds.push(format!("{pci_addr}->{target_drv}"));
                if target_drv != "vfio-pci" || !approved_pci_set.contains(pci_addr) {
                    violations.push(format!(
                        "unapproved PCIDriverRebindConfig {pci_addr:?} -> {target_drv:?} (approved IOMMU-isolated PCI: {approved_vfio_pci:?})"
                    ));
                }
            }
            _ => {
                // v1alpha1 MachineConfig document (which omits `kind` in Talos v1alpha1)
                if val.get("version").and_then(|v| v.as_str()) == Some("v1alpha1")
                    || val.get("machine").is_some()
                {
                    if let Some(install) = val.pointer("/machine/install") {
                        if !install.is_null() {
                            violations.push(
                                "machine.install is strictly forbidden on diskless PITS Talos nodes"
                                    .to_string(),
                            );
                        }
                    }
                    if let Some(disks) = val.pointer("/machine/disks") {
                        if !disks.is_null()
                            && disks.as_array().map(|a| !a.is_empty()).unwrap_or(true)
                        {
                            violations.push(
                                "machine.disks is strictly forbidden; use UserVolumeConfig with deterministic SAN WWIDs"
                                    .to_string(),
                            );
                        }
                    }
                }
            }
        }
    }

    if docs_count == 0 {
        violations.push("Talos configuration contains zero YAML documents".to_string());
    }

    NetbootGuardReport {
        passed: violations.is_empty(),
        documents_inspected: docs_count,
        matched_wwids: matched_wwids.into_iter().collect(),
        pci_rebinds,
        violations,
    }
}

fn extract_wwid_from_cel(expr: &str) -> Option<String> {
    let compact = expr.trim();
    for quote in ['"', '\''] {
        let prefix = format!("disk.wwid == {quote}uuid.");
        if let Some(rest) = compact.strip_prefix(&prefix) {
            if let Some(uuid_part) = rest.strip_suffix(quote) {
                if !uuid_part.is_empty() && !uuid_part.contains('*') && !uuid_part.contains(' ') {
                    return Some(format!("uuid.{uuid_part}"));
                }
            }
        }
    }
    None
}

/// Returns the exact list of allowed NVMe/TCP SAN WWIDs for an enrolled PITS node.
pub fn allowed_wwids_for_node(node: &EnrolledNodeContract) -> Vec<String> {
    node.san
        .namespaces
        .iter()
        .map(|ns| expected_wwid(&node.san.subsystem_nqn, &ns.name))
        .collect()
}

/// Composes a complete, `netboot-guard`-compliant multi-document Talos v1.14.5 configuration
/// for an enrolled diskless node (`netboot-001..004`), pinning `STATE`, `EPHEMERAL`, `userdata`,
/// and optional `vmstore` (`netboot-004`) exclusively to deterministic NVMe/TCP SAN WWIDs.
pub fn render_talos_node_config(
    node: &EnrolledNodeContract,
    cluster_endpoint: &str,
) -> Result<String> {
    let nqn = &node.san.subsystem_nqn;
    let system_wwid = expected_wwid(nqn, "system");
    let userdata_wwid = expected_wwid(nqn, "userdata");
    let intel_vpu_line = if node.has_intel_npu {
        "\n      - name: intel_vpu"
    } else {
        ""
    };

    let mut docs = Vec::new();
    docs.push(format!(
        r#"version: v1alpha1
debug: false
persist: true
machine:
  type: controlplane
  kubelet:
    image: ghcr.io/siderolabs/kubelet:{k8s_ver}
    nodeIP:
      validSubnets:
        - 10.89.20.0/24
    extraConfig:
      featureGates:
        DynamicResourceAllocation: true
  network:
    hostname: {host}
    interfaces:
      - deviceSelector:
          hardwareAddr: "{mac}"
        dhcp: false
        addresses:
          - {ip}/24
        routes:
          - network: 0.0.0.0/0
            gateway: 10.89.20.1
    nameservers:
      - 10.89.20.1
  sysctls:
    vm.nr_hugepages: "1024"
  kernel:
    modules:
      - name: nvme_tcp
      - name: vfio_pci{intel_vpu_line}
cluster:
  clusterName: pits-lab-talos
  controlPlane:
    endpoint: {cluster_endpoint}
  allowSchedulingOnControlPlanes: true
  network:
    podSubnets:
      - 10.244.0.0/16
    serviceSubnets:
      - 10.96.0.0/12
  etcd:
    advertisedSubnets:
      - 10.89.20.0/24"#,
        k8s_ver = LAB_K8S_VERSION,
        host = node.hostname,
        mac = node.pxe_mac,
        ip = node.node_ip,
        intel_vpu_line = intel_vpu_line,
        cluster_endpoint = cluster_endpoint,
    ));

    // Pin META, STATE, and EPHEMERAL to NSID 1 (`system`) WWID
    for sys_vol in ["META", "STATE", "EPHEMERAL"] {
        docs.push(format!(
            r#"apiVersion: v1alpha1
kind: VolumeConfig
name: {sys_vol}
provisioning:
  diskSelector:
    match: 'disk.wwid == "{system_wwid}"'"#
        ));
    }

    // Pin `userdata` UserVolumeConfig to NSID 2 (`userdata`) WWID
    docs.push(format!(
        r#"apiVersion: v1alpha1
kind: UserVolumeConfig
name: userdata
provisioning:
  diskSelector:
    match: 'disk.wwid == "{userdata_wwid}"'
  minSize: 16GiB"#
    ));

    // On VM host nodes (e.g. `netboot-004`), pin `vmstore` UserVolumeConfig to NSID 3 (`vmstore`) WWID
    if node.san.namespaces.iter().any(|n| n.name == "vmstore") {
        let vmstore_wwid = expected_wwid(nqn, "vmstore");
        docs.push(format!(
            r#"apiVersion: v1alpha1
kind: UserVolumeConfig
name: vmstore
provisioning:
  diskSelector:
    match: 'disk.wwid == "{vmstore_wwid}"'
  minSize: 64GiB"#
        ));
    }

    for pci in &node.approved_vfio_pci {
        docs.push(format!(
            r#"apiVersion: v1alpha1
kind: PCIDriverRebindConfig
name: "{pci}"
targetDriver: vfio-pci"#
        ));
    }

    let full_yaml = docs.join("\n---\n") + "\n";
    let guard = validate_talos_netboot_config(
        &full_yaml,
        &allowed_wwids_for_node(node),
        &node.approved_vfio_pci,
    );
    if !guard.passed {
        bail!(
            "internal netboot-guard self-check failed: {:?}",
            guard.violations
        );
    }
    Ok(full_yaml)
}

/// Declarative parameters for Talos `7.3.1-talos` `thunderbolt_stream` (`USB4STREAM` ConfigFS)
/// and kernel/NAPI `busy_poll` low-latency tuning across high-speed `thunderbolt-net` interfaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TalosHighSpeedFabricOptions {
    pub socket_busy_poll_us: u32,
    pub socket_busy_read_us: u32,
    pub napi_defer_hard_irqs: u32,
    pub gro_flush_timeout_ns: u64,
    pub busy_poll_ifaces: String,
    pub stream_enabled: bool,
    pub stream_name: String,
    pub stream_busy_poll: bool,
    pub stream_ring_size: u32,
    pub stream_throttling_ns: u32,
    pub in_hopid: i32,
    pub out_hopid: i32,
    pub preserve_hugepages: bool,
    pub preserve_ip_forward: bool,
    pub preserve_bpf_jit_harden: bool,
}

impl Default for TalosHighSpeedFabricOptions {
    fn default() -> Self {
        Self {
            socket_busy_poll_us: 50,
            socket_busy_read_us: 50,
            napi_defer_hard_irqs: 2,
            gro_flush_timeout_ns: 200_000,
            busy_poll_ifaces: "tb0,tb1,enx02*".to_string(),
            stream_enabled: true,
            stream_name: "pits0".to_string(),
            stream_busy_poll: true,
            stream_ring_size: 256,
            stream_throttling_ns: 0,
            in_hopid: -1,
            out_hopid: -1,
            preserve_hugepages: true,
            preserve_ip_forward: true,
            preserve_bpf_jit_harden: false,
        }
    }
}

fn is_valid_thunderbolt_hopid(hopid: i32) -> bool {
    matches!(hopid, -1 | 0 | 8..=65535)
}

/// Validates and renders multi-document Talos `v1alpha1` patch documents (`KernelModuleConfig`,
/// `SysctlConfig`, and `ExtensionServiceConfig` for `pits-tbstream`) for parameterized
/// `thunderbolt_stream` (`busy_poll`) and network stack `busy_poll` / NAPI reconciliation.
pub fn render_talos_thunderbolt_stream_docs(
    node: &EnrolledNodeContract,
    opts: &TalosHighSpeedFabricOptions,
) -> Result<String> {
    let stream_name = opts.stream_name.trim();
    if stream_name.is_empty()
        || !stream_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!(
            "invalid thunderbolt stream name {stream_name:?}: must be non-empty ASCII alphanumeric, '-' or '_'"
        );
    }
    if !(32..=4096).contains(&opts.stream_ring_size) {
        bail!(
            "invalid streamRingSize {}: Linux thunderbolt_stream ConfigFS requires 32..=4096",
            opts.stream_ring_size
        );
    }
    if !is_valid_thunderbolt_hopid(opts.in_hopid) || !is_valid_thunderbolt_hopid(opts.out_hopid) {
        bail!(
            "invalid HopID (in={}, out={}): must be -1 (auto), 0 (unused), or 8..=65535",
            opts.in_hopid,
            opts.out_hopid
        );
    }

    let ifaces = opts.busy_poll_ifaces.trim();
    if ifaces.is_empty()
        || !ifaces
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ',' | '-' | '_' | '*'))
    {
        bail!("invalid busyPollIfaces {ifaces:?}");
    }

    let mut docs = Vec::new();
    for kmod in ["thunderbolt", "thunderbolt_net", "thunderbolt_stream"] {
        docs.push(format!(
            "apiVersion: v1alpha1\nkind: KernelModuleConfig\nname: {kmod}"
        ));
    }

    let mut sysctl_lines = Vec::new();
    sysctl_lines.push(format!(
        "  net.core.busy_poll: \"{}\"",
        opts.socket_busy_poll_us
    ));
    sysctl_lines.push(format!(
        "  net.core.busy_read: \"{}\"",
        opts.socket_busy_read_us
    ));
    if opts.preserve_ip_forward {
        sysctl_lines.push("  net.ipv4.ip_forward: \"1\"".to_string());
    }
    if opts.preserve_bpf_jit_harden {
        sysctl_lines.push("  net.core.bpf_jit_harden: \"1\"".to_string());
    }
    if opts.preserve_hugepages {
        sysctl_lines.push("  vm.nr_hugepages: \"1024\"".to_string());
    }
    docs.push(format!(
        "apiVersion: v1alpha1\nkind: SysctlConfig\nparams:\n{}",
        sysctl_lines.join("\n")
    ));

    let enabled_str = if opts.stream_enabled { "true" } else { "false" };
    let busy_poll_flag = if opts.stream_busy_poll { "1" } else { "0" };
    docs.push(format!(
        r#"apiVersion: v1alpha1
kind: ExtensionServiceConfig
name: pits-tbstream
environment:
  - PITS_NODE_ID={dev}
  - PITS_TBSTREAM_ENABLED={enabled_str}
  - PITS_TBSTREAM_NAME={stream_name}
  - PITS_TBSTREAM_BUSY_POLL={busy_poll_flag}
  - PITS_TBSTREAM_RING_SIZE={ring_size}
  - PITS_TBSTREAM_THROTTLING={throttling}
  - PITS_TBSTREAM_IN_HOPID={in_hop}
  - PITS_TBSTREAM_OUT_HOPID={out_hop}
  - PITS_NET_BUSY_POLL_IFACES={ifaces}
  - PITS_NET_NAPI_DEFER_HARD_IRQS={napi}
  - PITS_NET_GRO_FLUSH_TIMEOUT={gro}"#,
        dev = node.device_id,
        ring_size = opts.stream_ring_size,
        throttling = opts.stream_throttling_ns,
        in_hop = opts.in_hopid,
        out_hop = opts.out_hopid,
        napi = opts.napi_defer_hard_irqs,
        gro = opts.gro_flush_timeout_ns,
    ));

    Ok(docs.join("\n---\n") + "\n")
}

/// Renders the Sidero Imager profile + `pits-nvme-early` system extension contract for a node
/// (replacing `stage-additional-talos.py` & shell imager wrappers).
pub fn render_talos_imager_profile(
    node: &EnrolledNodeContract,
    include_realtek_firmware: bool,
) -> String {
    let ucode_ext = if node.cpu_vendor.eq_ignore_ascii_case("amd") {
        "ghcr.io/siderolabs/amd-ucode:20260916"
    } else {
        "ghcr.io/siderolabs/intel-ucode:20260812"
    };
    let mut exts = vec![
        ucode_ext,
        "ghcr.io/siderolabs/iscsi-tools:v0.2.0",
        "ghcr.io/siderolabs/util-linux-tools:2.40.2",
        "registry.pits.home.arpa/talos/pits-nvme-early:v1.14.5",
    ];
    if node.has_intel_npu {
        exts.push("ghcr.io/siderolabs/intel-npu:20260916-v1.14.5");
    }
    if include_realtek_firmware || node.pxe_mac.to_lowercase().starts_with("58:47:ca:") {
        exts.push("ghcr.io/siderolabs/realtek-firmware:20260916");
    }
    let ext_lines: String = exts
        .iter()
        .map(|img| format!("      - imageRef: {img}"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"arch: amd64
platform: metal
secureboot: false
version: {ver}
customization:
  extraKernelArgs:
    - console=tty0
    - console=ttyS0,115200n8
    - selinux=1
    - module.sig_enforce=1
    - intel_iommu=on
    - iommu=pt
    - hugepagesz=2M
    - hugepages=1024
    - nvme_core.multipath=N
    - pits.nvme.target={target_ip}:4420
    - pits.nvme.nqn={nqn}
    - pits.nvme.hostnqn={host_nqn}
input:
  kernel:
    path: /usr/install/amd64/vmlinuz
  initramfs:
    path: /usr/install/amd64/initramfs.xz
  baseInstaller:
    imageRef: ghcr.io/siderolabs/installer:{ver}
  systemExtensions:
{ext_lines}
output:
  kind: boot
  outFormat: raw
"#,
        ver = TALOS_VERSION,
        target_ip = node.target_ip,
        nqn = node.san.subsystem_nqn,
        host_nqn = node.san.host_nqn,
        ext_lines = ext_lines,
    )
}

/// Live DMI chassis & processor identity queried from Talos COSI `hardware` resources.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TalosHardwareIdentity {
    pub manufacturer: Option<String>,
    pub product_name: Option<String>,
    pub version: Option<String>,
    pub sku_number: Option<String>,
    pub bios_version: Option<String>,
    pub serial_number: Option<String>,
    pub uuid: Option<String>,
    pub resolved_model: String,
    pub cpu_manufacturer: Option<String>,
    pub cpu_model: Option<String>,
    pub cpu_cores: Option<u64>,
    pub cpu_threads: Option<u64>,
}

/// Decoded Talos node runtime status via native gRPC (`MachineService` + COSI `State`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TalosNodeLiveStatus {
    pub node: String,
    pub reachable: bool,
    pub talos_tag: Option<String>,
    pub hostname_reported: Option<String>,
    pub hardware: Option<TalosHardwareIdentity>,
    pub physical_npus: Vec<DetectedNpuHardware>,
    pub services: Vec<TalosServiceSummary>,
    pub etcd_members: Vec<String>,
    pub cosi_disk_wwids: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TalosServiceSummary {
    pub id: String,
    pub state: String,
}

/// Native HTTP/2 + mTLS gRPC client for Sidero Talos `apid` (`:50000`).
#[derive(Clone)]
pub struct TalosClient {
    pub ctx: ResolvedTalosContext,
    pub tls_config: Arc<rustls::ClientConfig>,
    pub bridge_proxy_url: Option<String>,
    pub bridge_bearer_token: Option<String>,
    pub bridge_tls_config: Option<Arc<rustls::ClientConfig>>,
}

impl TalosClient {
    pub fn from_context(
        ctx: ResolvedTalosContext,
        bridge: Option<(&ClientProfile, Arc<rustls::ClientConfig>)>,
    ) -> Result<Self> {
        let (tls_config, _) = build_rustls_client_config(
            Some(&ctx.ca_pem),
            Some(&ctx.crt_pem),
            Some(&ctx.key_pem),
            false,
            vec![b"h2".to_vec()],
        )?;
        let (bridge_proxy_url, bridge_bearer_token, bridge_tls_config) = match bridge {
            Some((prof, b_tls)) => (
                Some(prof.agent_endpoint.clone()),
                prof.agent_bearer_token.clone(),
                Some(b_tls),
            ),
            None => (None, None, None),
        };
        Ok(Self {
            ctx,
            tls_config,
            bridge_proxy_url,
            bridge_bearer_token,
            bridge_tls_config,
        })
    }

    fn direct_target_for_node(&self, node_ip: &str) -> GrpcTarget {
        let (host, port) = split_host_port(node_ip, TALOS_APID_PORT);
        GrpcTarget {
            tls_server_name: Some(host.clone()),
            host,
            port,
            tls_config: self.tls_config.clone(),
            bridge_proxy_url: self.bridge_proxy_url.clone(),
            bridge_bearer_token: self.bridge_bearer_token.clone(),
            bridge_tls_config: self.bridge_tls_config.clone(),
        }
    }

    fn target_for_node(&self, node_ip: &str) -> GrpcTarget {
        if node_ip.parse::<std::net::IpAddr>().is_ok() {
            return self.direct_target_for_node(node_ip);
        }
        let endpoint_raw = self
            .ctx
            .endpoints
            .first()
            .map(String::as_str)
            .unwrap_or(node_ip);
        let (host, port) = split_host_port(endpoint_raw, TALOS_APID_PORT);
        GrpcTarget {
            host,
            port,
            tls_server_name: Some(node_ip.to_string()),
            tls_config: self.tls_config.clone(),
            bridge_proxy_url: self.bridge_proxy_url.clone(),
            bridge_bearer_token: self.bridge_bearer_token.clone(),
            bridge_tls_config: self.bridge_tls_config.clone(),
        }
    }

    /// Queries `/machine.MachineService/Version`, `/machine.MachineService/ServiceList`,
    /// `/machine.MachineService/EtcdMemberList`, COSI `SystemInformations`/`Processors`/`PCIDevices`,
    /// and COSI `Disks.block.talos.dev`.
    pub async fn inspect_node(&self, node_ip: &str) -> TalosNodeLiveStatus {
        let mut out = TalosNodeLiveStatus {
            node: node_ip.to_string(),
            reachable: false,
            talos_tag: None,
            hostname_reported: None,
            hardware: None,
            physical_npus: Vec::new(),
            services: Vec::new(),
            etcd_members: Vec::new(),
            cosi_disk_wwids: Vec::new(),
            error: None,
        };
        let target = self.target_for_node(node_ip);
        let node_hdr = if target.host == node_ip {
            None
        } else {
            Some(node_ip)
        };

        match invoke_grpc(&target, "/machine.MachineService/Version", node_hdr, &[]).await {
            Ok(frames) => {
                out.reachable = true;
                for frame in frames {
                    let strings = extract_nested_ascii_strings(&frame);
                    for s in strings {
                        if s.starts_with("v1.") && out.talos_tag.is_none() {
                            out.talos_tag = Some(s);
                        } else if s.starts_with("netboot-") && out.hostname_reported.is_none() {
                            out.hostname_reported = Some(s);
                        }
                    }
                }
            }
            Err(e) => {
                out.error = Some(e.to_string());
                return out;
            }
        }

        let (svc_res, etcd_res, disk_res, (hw_opt, npus)) = tokio::join!(
            invoke_grpc(&target, "/machine.MachineService/ServiceList", node_hdr, &[]),
            invoke_grpc(
                &target,
                "/machine.MachineService/EtcdMemberList",
                node_hdr,
                &[],
            ),
            self.cosi_list_disk_wwids(&target, node_hdr),
            self.cosi_inspect_hardware(&target, node_hdr),
        );

        if let Ok(frames) = svc_res {
            for frame in frames {
                out.services.extend(parse_service_list_proto(&frame));
            }
        }

        if let Ok(frames) = etcd_res {
            for frame in frames {
                for s in extract_nested_ascii_strings(&frame) {
                    if s.starts_with("netboot-") || s.starts_with("https://10.89.20.") {
                        out.etcd_members.push(s);
                    }
                }
            }
        }

        if let Ok(wwids) = disk_res {
            out.cosi_disk_wwids = wwids;
        }
        out.hardware = hw_opt;
        out.physical_npus = npus;

        out
    }

    /// Queries COSI `/cosi.resource.State/List` and decodes `(metadata.id, parsed_yaml_spec)` pairs.
    pub async fn cosi_list_yaml_specs(
        &self,
        target: &GrpcTarget,
        node_hdr: Option<&str>,
        namespace: &str,
        resource_type: &str,
    ) -> Result<Vec<(String, serde_json::Value)>> {
        let mut req = Vec::new();
        encode_string_field(1, namespace, &mut req);
        encode_string_field(2, resource_type, &mut req);

        let frames = invoke_grpc(target, "/cosi.resource.State/List", node_hdr, &req).await?;
        let mut items = Vec::new();
        for frame in frames {
            if let Ok(top_fields) = decode_fields(&frame) {
                for rf in top_fields {
                    if rf.number != 1 {
                        continue;
                    }
                    let Some(res_bytes) = rf.as_bytes() else {
                        continue;
                    };
                    let Ok(res_fields) = decode_fields(res_bytes) else {
                        continue;
                    };
                    let mut id = String::new();
                    let mut yaml_val = None;
                    for sub in res_fields {
                        if sub.number == 1 {
                            if let Some(meta_bytes) = sub.as_bytes() {
                                if let Ok(mfields) = decode_fields(meta_bytes) {
                                    if let Some(id_str) =
                                        mfields.iter().find(|x| x.number == 3).and_then(|x| x.as_str())
                                    {
                                        id = id_str.to_string();
                                    }
                                }
                            }
                        } else if sub.number == 2 {
                            if let Some(spec_bytes) = sub.as_bytes() {
                                if let Ok(sfields) = decode_fields(spec_bytes) {
                                    if let Some(yaml_str) =
                                        sfields.iter().find(|x| x.number == 2).and_then(|x| x.as_str())
                                    {
                                        if let Ok(val) =
                                            serde_yaml::from_str::<serde_json::Value>(yaml_str)
                                        {
                                            yaml_val = Some(val);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let Some(val) = yaml_val {
                        items.push((id, val));
                    }
                }
            }
        }
        Ok(items)
    }

    /// Queries COSI `SystemInformations`, `Processors`, and `PCIDevices` on `namespace="hardware"`
    /// to resolve exact machine model, CPU model, and physical PCI NPUs + bound kernel drivers.
    pub async fn cosi_inspect_hardware(
        &self,
        target: &GrpcTarget,
        node_hdr: Option<&str>,
    ) -> (Option<TalosHardwareIdentity>, Vec<DetectedNpuHardware>) {
        let (sys_res, proc_res, pci_res) = tokio::join!(
            self.cosi_list_yaml_specs(
                target,
                node_hdr,
                "hardware",
                "SystemInformations.hardware.talos.dev",
            ),
            self.cosi_list_yaml_specs(
                target,
                node_hdr,
                "hardware",
                "Processors.hardware.talos.dev",
            ),
            self.cosi_list_yaml_specs(
                target,
                node_hdr,
                "hardware",
                "PCIDevices.hardware.talos.dev",
            ),
        );

        let sys_spec = sys_res
            .ok()
            .and_then(|mut v| if v.is_empty() { None } else { Some(v.remove(0).1) });
        let proc_spec = proc_res
            .ok()
            .and_then(|mut v| if v.is_empty() { None } else { Some(v.remove(0).1) });

        let hw = if sys_spec.is_some() || proc_spec.is_some() {
            let manufacturer = sys_spec
                .as_ref()
                .and_then(|s| s.get("manufacturer"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let product_name = sys_spec
                .as_ref()
                .and_then(|s| s.get("productName"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let version = sys_spec
                .as_ref()
                .and_then(|s| s.get("version"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sku_number = sys_spec
                .as_ref()
                .and_then(|s| s.get("skuNumber"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let bios_version = sys_spec
                .as_ref()
                .and_then(|s| s.get("biosVersion"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let serial_number = sys_spec
                .as_ref()
                .and_then(|s| s.get("serialnumber").or_else(|| s.get("serialNumber")))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let uuid = sys_spec
                .as_ref()
                .and_then(|s| s.get("uuid"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_lowercase());

            let cpu_manufacturer = proc_spec
                .as_ref()
                .and_then(|p| p.get("manufacturer"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let cpu_model = proc_spec
                .as_ref()
                .and_then(|p| p.get("productName"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let cpu_cores = proc_spec
                .as_ref()
                .and_then(|p| p.get("coreCount"))
                .and_then(|v| v.as_u64());
            let cpu_threads = proc_spec
                .as_ref()
                .and_then(|p| p.get("threadCount"))
                .and_then(|v| v.as_u64());

            let resolved_model = resolve_retail_machine_model(
                manufacturer.as_deref(),
                product_name.as_deref(),
                sku_number.as_deref(),
                cpu_model.as_deref(),
            );

            Some(TalosHardwareIdentity {
                manufacturer,
                product_name,
                version,
                sku_number,
                bios_version,
                serial_number,
                uuid,
                resolved_model,
                cpu_manufacturer,
                cpu_model,
                cpu_cores,
                cpu_threads,
            })
        } else {
            None
        };

        let mut npus = Vec::new();
        if let Ok(pci_items) = pci_res {
            for (bdf_id, spec) in pci_items {
                let vendor_id = spec
                    .get("vendor_id")
                    .or_else(|| spec.get("vendorId"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let product_id = spec
                    .get("product_id")
                    .or_else(|| spec.get("productId"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let class_id = spec
                    .get("class_id")
                    .or_else(|| spec.get("classId"))
                    .and_then(|v| v.as_str());
                let subclass_id = spec
                    .get("subclass_id")
                    .or_else(|| spec.get("subclassId"))
                    .and_then(|v| v.as_str());
                let full_class = match (class_id, subclass_id) {
                    (Some(c), Some(sc)) => Some(format!(
                        "0x{}{}",
                        c.trim_start_matches("0x"),
                        sc.trim_start_matches("0x")
                    )),
                    (Some(c), None) => Some(c.to_string()),
                    _ => None,
                };
                let driver = spec.get("driver").and_then(|v| v.as_str());
                if let Some(npu) = classify_pci_npu(
                    &bdf_id,
                    vendor_id,
                    product_id,
                    full_class.as_deref(),
                    driver,
                ) {
                    npus.push(npu);
                }
            }
        }

        (hw, npus)
    }

    /// Queries COSI `/cosi.resource.State/List` (`namespace="runtime"`, `type="Disks.block.talos.dev"`)
    /// to verify that deterministic NVMe/TCP SAN WWIDs are attached before applying config!
    pub async fn cosi_list_disk_wwids(
        &self,
        target: &GrpcTarget,
        node_hdr: Option<&str>,
    ) -> Result<Vec<String>> {
        let mut wwids = BTreeSet::new();
        let items = self
            .cosi_list_yaml_specs(target, node_hdr, "runtime", "Disks.block.talos.dev")
            .await?;
        for (_id, spec) in items {
            if let Some(w) = spec.get("wwid").and_then(|v| v.as_str()) {
                if w.starts_with("uuid.") && w.len() == 41 {
                    wwids.insert(w.to_string());
                }
            }
        }
        Ok(wwids.into_iter().collect())
    }

    /// Runs `netboot-guard`, verifies live SAN WWID visibility via COSI when `verify_live_wwids`
    /// is true, and invokes `/machine.MachineService/ApplyConfiguration`.
    /// `mode`: `1` = `AUTO`, `2` = `NO_REBOOT`, `3` = `REBOOT`, `5` = `TRY`.
    pub async fn apply_configuration(
        &self,
        node: &EnrolledNodeContract,
        multi_doc_yaml: &str,
        mode: u64,
        dry_run: bool,
        verify_live_wwids: bool,
    ) -> Result<Vec<String>> {
        let allowed_wwids = allowed_wwids_for_node(node);
        let guard = validate_talos_netboot_config(
            multi_doc_yaml,
            &allowed_wwids,
            &node.approved_vfio_pci,
        );
        if !guard.passed {
            bail!(
                "netboot-guard blocked ApplyConfiguration on {}: {}",
                node.device_id,
                guard.violations.join("; ")
            );
        }

        let target = self.target_for_node(&node.node_ip);
        let node_hdr = if target.host == node.node_ip {
            None
        } else {
            Some(node.node_ip.as_str())
        };
        if verify_live_wwids {
            let live_wwids = self
                .cosi_list_disk_wwids(&target, node_hdr)
                .await
                .context("preflight COSI Disks.block.talos.dev WWID check")?;
            for required in &guard.matched_wwids {
                if !live_wwids.contains(required) {
                    bail!(
                        "preflight aborted: Talos node {} does not currently see required SAN WWID {} (visible: {:?})",
                        node.device_id,
                        required,
                        live_wwids
                    );
                }
            }
        }

        // machine.ApplyConfigurationRequest:
        // field 1: bytes data, field 2: Mode mode, field 4: bool dry_run
        let mut req = Vec::new();
        encode_bytes_field(1, multi_doc_yaml.as_bytes(), &mut req);
        encode_varint_field(2, mode, &mut req);
        encode_bool_field(4, dry_run, &mut req);

        let frames = invoke_grpc(
            &target,
            "/machine.MachineService/ApplyConfiguration",
            node_hdr,
            &req,
        )
        .await?;

        let mut details = Vec::new();
        for f in frames {
            details.extend(extract_nested_ascii_strings(&f));
        }
        Ok(details)
    }

    /// Invokes `/machine.MachineService/Reboot` (`powercycle=true` forces full UEFI PXE netboot re-entry).
    pub async fn reboot_node(&self, node_ip: &str, powercycle: bool) -> Result<()> {
        let target = self.target_for_node(node_ip);
        let node_hdr = if target.host == node_ip {
            None
        } else {
            Some(node_ip)
        };
        // machine.RebootRequest: field 1 = Mode (0 = DEFAULT, 1 = POWERCYCLE)
        let mut req = Vec::new();
        if powercycle {
            encode_varint_field(1, 1, &mut req);
        }
        let _ = invoke_grpc(
            &target,
            "/machine.MachineService/Reboot",
            node_hdr,
            &req,
        )
        .await
        .map_err(|e| anyhow!("Talos Reboot RPC to {node_ip}: {e}"))?;
        Ok(())
    }
}

fn split_host_port(raw: &str, default_port: u16) -> (String, u16) {
    let stripped = raw
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    if let Some((h, p)) = stripped.rsplit_once(':') {
        if let Ok(port) = p.parse::<u16>() {
            return (h.to_string(), port);
        }
    }
    (stripped.to_string(), default_port)
}

fn parse_service_list_proto(buf: &[u8]) -> Vec<TalosServiceSummary> {
    let mut out = Vec::new();
    let Ok(top) = decode_fields(buf) else {
        return out;
    };
    for f in top {
        if let Some(msg_bytes) = f.as_bytes() {
            if let Ok(inner) = decode_fields(msg_bytes) {
                for svc in inner {
                    if svc.number == 2 {
                        if let Some(sb) = svc.as_bytes() {
                            if let Ok(sfields) = decode_fields(sb) {
                                let id = sfields
                                    .iter()
                                    .find(|x| x.number == 1)
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let state = sfields
                                    .iter()
                                    .find(|x| x.number == 2)
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("Unknown")
                                    .to_string();
                                if !id.is_empty() {
                                    out.push(TalosServiceSummary { id, state });
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

fn extract_nested_ascii_strings(buf: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    collect_strings_recursive(buf, 0, &mut out);
    out
}

fn collect_strings_recursive(buf: &[u8], depth: usize, out: &mut Vec<String>) {
    if depth > 6 {
        return;
    }
    let Ok(fields) = decode_fields(buf) else {
        return;
    };
    for f in fields {
        if let Some(bytes) = f.as_bytes() {
            if let Ok(s) = std::str::from_utf8(bytes) {
                if !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_ascii_graphic() || c == ' ' || c == '\n' || c == '\t')
                {
                    out.push(s.to_string());
                    continue;
                }
            }
            collect_strings_recursive(bytes, depth + 1, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WorkspaceBundle;

    #[test]
    fn test_netboot_guard_allows_deterministic_san_and_blocks_disk_mutations() {
        let bundle = WorkspaceBundle::builtin_pits_baseline();
        let node4 = bundle.find_node("netboot-004").unwrap();
        let rendered =
            render_talos_node_config(node4, "https://10.89.20.21:6443").expect("render Talos YAML");
        assert!(rendered.contains("uuid.c55a459d-a8cb-5177-b2ad-f47d603ee6ed"));
        assert!(rendered.contains("intel_vpu"));

        let allowed = allowed_wwids_for_node(node4);
        let ok_rep = validate_talos_netboot_config(&rendered, &allowed, &node4.approved_vfio_pci);
        assert!(ok_rep.passed, "unexpected violations: {:?}", ok_rep.violations);
        assert_eq!(ok_rep.matched_wwids.len(), 3);

        // AMD node netboot-003 must NOT load intel_vpu and must use amd-ucode in Imager!
        let node3 = bundle.find_node("netboot-003").unwrap();
        let rendered3 =
            render_talos_node_config(node3, "https://10.89.20.21:6443").expect("render AMD YAML");
        assert!(!rendered3.contains("intel_vpu"));
        let imager3 = render_talos_imager_profile(node3, false);
        assert!(imager3.contains("amd-ucode"));
        assert!(!imager3.contains("intel-ucode"));
        assert!(!imager3.contains("intel-npu"));

        // Attempt to sneak in `machine.install` or `DiskWipeConfig` -> MUST FAIL!
        let dangerous = format!(
            "{rendered}---\napiVersion: v1alpha1\nkind: DiskWipeConfig\nname: wipe-internal\n"
        );
        let bad_rep = validate_talos_netboot_config(&dangerous, &allowed, &node4.approved_vfio_pci);
        assert!(!bad_rep.passed);
        assert!(bad_rep
            .violations
            .iter()
            .any(|v| v.contains("DiskWipeConfig")));
    }

    #[test]
    fn test_thunderbolt_stream_and_busypoll_patch_rendering() {
        let bundle = WorkspaceBundle::builtin_pits_baseline();
        let node3 = bundle.find_node("netboot-003").unwrap();
        let opts = TalosHighSpeedFabricOptions::default();
        assert_eq!(opts.in_hopid, -1);
        assert_eq!(opts.out_hopid, -1);

        let patch = render_talos_thunderbolt_stream_docs(node3, &opts)
            .expect("render thunderbolt stream + busypoll patch");
        assert!(patch.contains("kind: KernelModuleConfig\nname: thunderbolt_stream"));
        assert!(patch.contains("net.core.busy_poll: \"50\""));
        assert!(patch.contains("net.core.busy_read: \"50\""));
        assert!(patch.contains("vm.nr_hugepages: \"1024\""));
        assert!(patch.contains("kind: ExtensionServiceConfig\nname: pits-tbstream"));
        assert!(patch.contains("PITS_TBSTREAM_BUSY_POLL=1"));
        assert!(patch.contains("PITS_TBSTREAM_IN_HOPID=-1"));
        assert!(patch.contains("PITS_TBSTREAM_OUT_HOPID=-1"));
        assert!(patch.contains("PITS_NET_BUSY_POLL_IFACES=tb0,tb1,enx02*"));
        assert!(patch.contains("PITS_NET_NAPI_DEFER_HARD_IRQS=2"));
        assert!(patch.contains("PITS_NET_GRO_FLUSH_TIMEOUT=200000"));

        let mut custom_iface = opts.clone();
        custom_iface.busy_poll_ifaces = "tb0,enx02*".to_string();
        assert!(render_talos_thunderbolt_stream_docs(node3, &custom_iface).is_ok());

        let mut bad_iface = opts.clone();
        bad_iface.busy_poll_ifaces = "tb0;rm -rf".to_string();
        assert!(render_talos_thunderbolt_stream_docs(node3, &bad_iface).is_err());

        let mut bad_ring = opts.clone();
        bad_ring.stream_ring_size = 16;
        assert!(render_talos_thunderbolt_stream_docs(node3, &bad_ring).is_err());

        let mut bad_hop = opts.clone();
        bad_hop.in_hopid = 3;
        assert!(render_talos_thunderbolt_stream_docs(node3, &bad_hop).is_err());

        let mut bad_name = opts;
        bad_name.stream_name = "bad/name".to_string();
        assert!(render_talos_thunderbolt_stream_docs(node3, &bad_name).is_err());
    }
}
