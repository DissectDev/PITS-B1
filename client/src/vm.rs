//! Domain 6: Cross-cluster KubeVirt Virtual Machine Manager (`pits-vms/win11-001` on Lab Talos
//! `netboot-004` & `edge-access-lab/access-lab` on `pits-core`), post-install media finalize,
//! QEMU Guest Agent JSON-RPC codec, and native Rust RDP X.224 + TLS X.509 verifier & launcher.

use anyhow::{anyhow, bail, Context, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::TlsConnector;

use crate::config::{local_state_dir, ClientProfile};
use crate::transport::{
    build_rustls_client_config, open_raw_or_tunneled_tcp, server_name_for_host, KubeClient,
};

pub const ACCESS_LAB_FIXED_MAC: &str = "7e:0a:35:9a:5a:d3";
pub const WIN11_DEFAULT_HOST_IP: &str = "10.89.20.24";
pub const WIN11_FAILOVER_HOST_IP: &str = "10.89.20.23";
pub const WIN11_HOST_POOL: [&str; 2] = ["netboot-003", "netboot-004"];
pub const WIN11_LIVE_MIGRATION_POOL: [&str; 3] = ["netboot-001", "netboot-002", "netboot-004"];
pub const WIN11_LIVE_POOL_HOST_IPS: [(&str, &str); 3] = [
    ("netboot-001", "10.89.20.21"),
    ("netboot-002", "10.89.20.22"),
    ("netboot-004", "10.89.20.24"),
];
pub const WIN11_DEFAULT_RDP_NODEPORT: u16 = 30389;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualMachineSummary {
    pub cluster: String,
    pub namespace: String,
    pub name: String,
    pub printable_status: String,
    pub ready: bool,
    pub run_strategy: String,
    pub node_name: Option<String>,
    pub guest_os: Option<String>,
    pub interfaces: Vec<VmInterfaceInfo>,
    pub attached_disks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmInterfaceInfo {
    pub name: String,
    pub mac: String,
    pub ip_address: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VmLifecycleOp {
    Start,
    Stop,
    Restart,
    Pause,
    Unpause,
}

impl VmLifecycleOp {
    pub fn subresource_info(self) -> (&'static str, &'static str) {
        match self {
            VmLifecycleOp::Start => ("virtualmachines", "start"),
            VmLifecycleOp::Stop => ("virtualmachines", "stop"),
            VmLifecycleOp::Restart => ("virtualmachines", "restart"),
            VmLifecycleOp::Pause => ("virtualmachineinstances", "pause"),
            VmLifecycleOp::Unpause => ("virtualmachineinstances", "unpause"),
        }
    }
}

/// Lists all KubeVirt `VirtualMachine` + live `VirtualMachineInstance` states on a cluster.
pub async fn list_cluster_vms(kc: &KubeClient) -> Result<Vec<VirtualMachineSummary>> {
    let vms_val = kc
        .get_json::<serde_json::Value>("/apis/kubevirt.io/v1/virtualmachines")
        .await?;
    let vmis_val = kc
        .get_json::<serde_json::Value>("/apis/kubevirt.io/v1/virtualmachineinstances")
        .await
        .unwrap_or(serde_json::Value::Null);

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

/// Invokes a KubeVirt subresource lifecycle action (`start`, `stop`, `restart`, `pause`, `unpause`).
pub async fn vm_lifecycle_action(
    kc: &KubeClient,
    namespace: &str,
    name: &str,
    op: VmLifecycleOp,
) -> Result<()> {
    let (resource_plural, action) = op.subresource_info();
    let path = format!(
        "/apis/subresources.kubevirt.io/v1/namespaces/{namespace}/{resource_plural}/{name}/{action}"
    );
    kc.put_subresource(&path, Some(b"{}".to_vec())).await
}

/// Generates a cryptographically random 20-character Windows Administrator password via OS CSPRNG.
pub fn generate_csprng_admin_password() -> String {
    const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
    const LOWER: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
    const DIGITS: &[u8] = b"23456789";
    const SYMBOLS: &[u8] = b"!@#%^&*-_=+";
    const ALL: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789!@#%^&*-_=+";

    let mut rng = rand::rngs::OsRng;
    let mut chars = vec![
        UPPER[rng.gen_range(0..UPPER.len())] as char,
        LOWER[rng.gen_range(0..LOWER.len())] as char,
        DIGITS[rng.gen_range(0..DIGITS.len())] as char,
        SYMBOLS[rng.gen_range(0..SYMBOLS.len())] as char,
    ];
    for _ in 0..16 {
        chars.push(ALL[rng.gen_range(0..ALL.len())] as char);
    }
    chars.into_iter().collect()
}

/// Renders the KubeVirt `VirtualMachine` + RDP NodePort `Service` for `pits-vms/win11-001`
/// on `netboot-004` (`q35`, UEFI `SecureBoot`, persistent `vTPM`, `Asia/Shanghai` RTC, SAN `vmstore`)
/// — replacing `render-windows-vm.py` and `finalize-windows-vm.py`.
pub fn render_windows11_vm_manifest(include_install_media: bool) -> String {
    let extra_disks = if include_install_media {
        r#"            - name: win11-iso
              bootOrder: 2
              cdrom:
                bus: sata
            - name: virtio-drivers
              cdrom:
                bus: sata
            - name: sysprep-unattend
              cdrom:
                bus: sata"#
    } else {
        ""
    };

    let extra_volumes = if include_install_media {
        r#"        - name: win11-iso
          hostDisk:
            path: /var/mnt/vmstore/isos/Win11_24H2_x64.iso
            type: Disk
        - name: virtio-drivers
          containerDisk:
            image: quay.io/kubevirt/virtio-container-disk:v1.4.0
        - name: sysprep-unattend
          secret:
            secretName: win11-001-sysprep"#
    } else {
        ""
    };

    let extra_disks_section = if extra_disks.is_empty() {
        String::new()
    } else {
        format!("\n{extra_disks}")
    };
    let extra_vols_section = if extra_volumes.is_empty() {
        String::new()
    } else {
        format!("\n{extra_volumes}")
    };

    format!(
        r#"apiVersion: kubevirt.io/v1
kind: VirtualMachine
metadata:
  name: win11-001
  namespace: pits-vms
  labels:
    app.kubernetes.io/name: win11-001
    pits.io/san-backed: "true"
spec:
  runStrategy: Always
  template:
    metadata:
      labels:
        kubevirt.io/domain: win11-001
        app.kubernetes.io/name: win11-001
    spec:
      nodeSelector:
        kubernetes.io/hostname: netboot-004
      domain:
        machine:
          type: q35
        cpu:
          cores: 8
          sockets: 1
          threads: 1
          model: host-passthrough
        resources:
          requests:
            memory: 16Gi
        clock:
          timezone: Asia/Shanghai
          utc: {{}}
          timer:
            hpet:
              present: false
            hyperv: {{}}
            pit:
              tickPolicy: delay
            rtc:
              tickPolicy: catchup
        features:
          acpi: {{}}
          apic: {{}}
          hyperv:
            relaxed: {{}}
            vapic: {{}}
            spinlocks:
              spinlocks: 8191
          smm:
            enabled: true
        firmware:
          bootloader:
            efi:
              secureBoot: true
              persistent: true
        devices:
          tpm:
            persistent: true
          autoattachGraphicsDevice: true
          inputs:
            - bus: usb
              name: tablet
              type: tablet
          interfaces:
            - name: default
              masquerade: {{}}
              model: virtio
          disks:
            - name: system-disk
              bootOrder: 1
              disk:
                bus: virtio{extra_disks_section}
      networks:
        - name: default
          pod: {{}}
      volumes:
        - name: system-disk
          hostDisk:
            path: /var/mnt/vmstore/win11-001/system.img
            type: DiskOrCreate
            capacity: 320Gi{extra_vols_section}
---
apiVersion: v1
kind: Service
metadata:
  name: win11-001-rdp
  namespace: pits-vms
spec:
  type: NodePort
  selector:
    kubevirt.io/domain: win11-001
  ports:
    - name: rdp-tcp
      protocol: TCP
      port: 3389
      targetPort: 3389
      nodePort: 30389
"#
    )
}

/// Renders the `edge-access-lab/access-lab` KubeVirt VM on `pits-core`, strictly preserving
/// enrolled MAC address `7e:0a:35:9a:5a:d3` (`deploy/access-lab/vm.yaml`).
pub fn render_access_lab_vm_manifest() -> String {
    format!(
        r#"apiVersion: kubevirt.io/v1
kind: VirtualMachine
metadata:
  name: access-lab
  namespace: edge-access-lab
spec:
  runStrategy: Always
  template:
    metadata:
      labels:
        kubevirt.io/domain: access-lab
    spec:
      nodeSelector:
        kubernetes.io/hostname: pits-core
      domain:
        cpu:
          cores: 2
        resources:
          requests:
            memory: 4Gi
        devices:
          interfaces:
            - name: core-lan
              bridge: {{}}
              macAddress: "{mac}"
              model: virtio
          disks:
            - name: rootdisk
              disk:
                bus: virtio
      networks:
        - name: core-lan
          multus:
            networkName: edge-access-lab/core-vlan10
      volumes:
        - name: rootdisk
          persistentVolumeClaim:
            claimName: access-lab-root
"#,
        mac = ACCESS_LAB_FIXED_MAC
    )
}

/// Renders a KubeVirt `VirtualMachine` JSON merge patch to move `pits-vms/win11-001` cleanly
/// across the `netboot-003` / `netboot-004` host pool while preserving firmware UUID/serial and vTPM/EFI state.
pub fn render_win11_pool_placement_patch(target_node: &str) -> Result<String> {
    if !WIN11_HOST_POOL.contains(&target_node) {
        bail!(
            "unsupported win11-001 pool host {target_node:?}: expected one of {:?}",
            WIN11_HOST_POOL
        );
    }
    let (memory_gib, host_devices) = if target_node == "netboot-003" {
        ("14Gi", serde_json::json!([]))
    } else {
        (
            "16Gi",
            serde_json::json!([
                {
                    "deviceName": "pits.io/rtl8126-backup",
                    "name": "ont-lan"
                }
            ]),
        )
    };

    let patch = serde_json::json!({
        "spec": {
            "template": {
                "spec": {
                    "nodeSelector": {
                        "kubernetes.io/hostname": serde_json::Value::Null,
                        "pits.io/win11-pool": "enabled",
                        "pits.io/win11-active-host": "true"
                    },
                    "domain": {
                        "memory": {
                            "guest": memory_gib
                        },
                        "resources": {
                            "requests": {
                                "memory": memory_gib
                            }
                        },
                        "devices": {
                            "hostDevices": host_devices
                        }
                    }
                }
            }
        }
    });
    Ok(serde_json::to_string_pretty(&patch)? + "\n")
}

/// Validates an optional live-migration target host (`auto` / `None` or a homogeneous Intel Meteor Lake pool node).
pub fn validate_live_migration_target(target_node: Option<&str>) -> Result<Option<String>> {
    match target_node.map(str::trim).filter(|s| !s.is_empty() && *s != "auto") {
        None => Ok(None),
        Some(node) if WIN11_LIVE_MIGRATION_POOL.contains(&node) => Ok(Some(node.to_string())),
        Some(other) => bail!(
            "unsupported live-migration target {other:?}: expected 'auto' or one of {:?}",
            WIN11_LIVE_MIGRATION_POOL
        ),
    }
}

/// Renders a KubeVirt `VirtualMachineInstanceMigration` manifest for sub-second RWX live migration.
pub fn render_vm_live_migration_manifest(
    namespace: &str,
    vm_name: &str,
    migration_name: &str,
) -> String {
    format!(
        r#"apiVersion: kubevirt.io/v1
kind: VirtualMachineInstanceMigration
metadata:
  name: {migration_name}
  namespace: {namespace}
spec:
  vmiName: {vm_name}
"#
    )
}

/// Steers optional preferred target node labels across `WIN11_LIVE_MIGRATION_POOL` and submits
/// a KubeVirt `VirtualMachineInstanceMigration` CR via Server-Side Apply.
pub async fn trigger_vm_live_migration(
    kc: &KubeClient,
    namespace: &str,
    vm_name: &str,
    migration_name: &str,
    target_node: Option<&str>,
) -> Result<serde_json::Value> {
    let resolved_target = validate_live_migration_target(target_node)?;
    if let Some(ref target) = resolved_target {
        for node in WIN11_LIVE_MIGRATION_POOL {
            let val = if node == target {
                serde_json::Value::String("true".to_string())
            } else {
                serde_json::Value::Null
            };
            let label_patch = serde_json::json!({
                "metadata": {
                    "labels": {
                        "pits.io/win11-target-host": val
                    }
                }
            });
            kc.merge_patch(&format!("/api/v1/nodes/{node}"), &label_patch)
                .await?;
        }
    }
    let manifest = render_vm_live_migration_manifest(namespace, vm_name, migration_name);
    let path = format!(
        "/apis/kubevirt.io/v1/namespaces/{namespace}/virtualmachineinstancemigrations/{migration_name}"
    );
    kc.server_side_apply(&path, "pitsctl-vm", manifest.as_bytes(), true)
        .await
}

/// Finalizes `pits-vms/win11-001` after Windows 11 installation by applying the clean,
/// install-media-free `VirtualMachine` manifest (`include_install_media = false`).
pub async fn finalize_windows11_vm(lab_k8s: &KubeClient) -> Result<()> {
    let manifest = render_windows11_vm_manifest(false);
    let docs: Vec<&str> = manifest.split("\n---\n").collect();
    lab_k8s
        .server_side_apply(
            "/apis/kubevirt.io/v1/namespaces/pits-vms/virtualmachines/win11-001",
            "pitsctl-vm",
            docs[0].as_bytes(),
            true,
        )
        .await?;
    lab_k8s
        .server_side_apply(
            "/api/v1/namespaces/pits-vms/services/win11-001-rdp",
            "pitsctl-vm",
            docs[1].as_bytes(),
            true,
        )
        .await?;
    Ok(())
}

/// Builds a synchronized QEMU Guest Agent (`qemu-ga`) JSON-RPC wire payload
/// (`guest-sync-delimited` + command, replacing `inspect-windows-guest.py`).
pub fn build_qga_command_payload(command: &str, sync_id: u64) -> Vec<u8> {
    let sync = serde_json::json!({
        "execute": "guest-sync-delimited",
        "arguments": { "id": sync_id }
    });
    let cmd = serde_json::json!({ "execute": command });
    let mut out = vec![0xFFu8];
    out.extend_from_slice(sync.to_string().as_bytes());
    out.push(b'\n');
    out.extend_from_slice(cmd.to_string().as_bytes());
    out.push(b'\n');
    out
}

/// Result of a native Rust RDP X.224 + TLS handshake inspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RdpTlsVerification {
    pub host: String,
    pub port: u16,
    pub selected_protocol: u32,
    pub nla_hybrid_enforced: bool,
    pub cert_sha1_hex: String,
    pub cert_sha256_hex: String,
}

/// Performs an authentic Microsoft RDP X.224 Connection Request (`PROTOCOL_HYBRID | PROTOCOL_SSL`)
/// followed by a TLS 1.2/1.3 handshake (directly or via `pits-agent` `HTTP CONNECT` Lab Bridge),
/// extracting the live RDP server certificate SHA-1 & SHA-256 thumbprints!
pub async fn verify_rdp_tls_endpoint(
    host: &str,
    port: u16,
    bridge: Option<(&ClientProfile, Arc<rustls::ClientConfig>)>,
) -> Result<RdpTlsVerification> {
    let (proxy_url, proxy_token, proxy_tls) = match bridge {
        Some((p, t)) => (
            Some(p.agent_endpoint.as_str()),
            p.agent_bearer_token.as_deref(),
            Some(t),
        ),
        None => (None, None, None),
    };

    let mut stream = open_raw_or_tunneled_tcp(host, port, proxy_url, proxy_token, proxy_tls)
        .await
        .with_context(|| format!("connect RDP endpoint {host}:{port}"))?;

    // TPKT (4 bytes) + X.224 CR TPDU (7 bytes) + RDP Negotiation Request (8 bytes) = 19 bytes.
    // requestedProtocols = 0x00000003 (PROTOCOL_SSL=1 | PROTOCOL_HYBRID=2 [CredSSP/NLA]).
    let x224_cr: [u8; 19] = [
        0x03, 0x00, 0x00, 0x13, // TPKT v3, total length 19
        0x0e, // X.224 length indicator = 14
        0xe0, // X.224 Connection Request (CR)
        0x00, 0x00, // DST-REF
        0x00, 0x00, // SRC-REF
        0x00, // Class 0
        0x01, // TYPE_RDP_NEG_REQ
        0x00, // flags
        0x08, 0x00, // RDP_NEG_REQ length = 8
        0x03, 0x00, 0x00, 0x00, // PROTOCOL_SSL | PROTOCOL_HYBRID
    ];

    stream
        .write_all(&x224_cr)
        .await
        .context("send RDP X.224 Connection Request")?;
    stream.flush().await?;

    // Read TPKT 4-byte header
    let mut tpkt = [0u8; 4];
    stream
        .read_exact(&mut tpkt)
        .await
        .context("read RDP TPKT header")?;
    if tpkt[0] != 0x03 {
        bail!("invalid TPKT version 0x{:02x} from {host}:{port}", tpkt[0]);
    }
    let total_len = u16::from_be_bytes([tpkt[2], tpkt[3]]) as usize;
    if !(11..=256).contains(&total_len) {
        bail!("unexpected RDP X.224 response length {total_len}");
    }
    let mut tpdu = vec![0u8; total_len - 4];
    stream
        .read_exact(&mut tpdu)
        .await
        .context("read RDP X.224 Connection Confirm")?;

    // Check X.224 CC code (0xD0) and optional RDP_NEG_RSP (type 0x02)
    if tpdu.len() < 7 || (tpdu[1] & 0xF0) != 0xD0 {
        bail!("unexpected X.224 TPDU code 0x{:02x}", tpdu.get(1).copied().unwrap_or(0));
    }
    let mut selected_protocol = 0u32;
    if tpdu.len() >= 15 && tpdu[7] == 0x02 {
        selected_protocol = u32::from_le_bytes([tpdu[11], tpdu[12], tpdu[13], tpdu[14]]);
    } else if tpdu.len() >= 15 && tpdu[7] == 0x03 {
        let fail_code = u32::from_le_bytes([tpdu[11], tpdu[12], tpdu[13], tpdu[14]]);
        bail!("RDP Negotiation Failure from server (code=0x{fail_code:08x})");
    }

    // Upgrade the exact same socket to TLS and record the leaf X.509 certificate DER!
    let (tls_cfg, captured_leaf) =
        build_rustls_client_config(None, None, None, true, Vec::new())?;
    let connector = TlsConnector::from(tls_cfg);
    let _tls_stream = connector
        .connect(server_name_for_host(host)?, stream)
        .await
        .context("RDP TLS handshake after X.224 negotiation")?;

    let der = captured_leaf
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .ok_or_else(|| anyhow!("TLS handshake completed without server leaf certificate"))?;

    let sha1_hex = hex::encode_upper(Sha1::digest(&der));
    let sha256_hex = hex::encode_upper(Sha256::digest(&der));

    Ok(RdpTlsVerification {
        host: host.to_string(),
        port,
        selected_protocol,
        nla_hybrid_enforced: (selected_protocol & 0x02) != 0,
        cert_sha1_hex: sha1_hex,
        cert_sha256_hex: sha256_hex,
    })
}

/// Formats a high-DPI (`3072×1920`), NLA-enforced, certificate-pinned `.rdp` file payload.
pub fn render_pinned_rdp_profile(
    host: &str,
    port: u16,
    username: &str,
    cert_sha1_thumbprint: Option<&str>,
    width: u32,
    height: u32,
) -> String {
    let mut lines = vec![
        format!("full address:s:{host}:{port}"),
        format!("username:s:{username}"),
        "screen mode id:i:1".to_string(),
        format!("desktopwidth:i:{width}"),
        format!("desktopheight:i:{height}"),
        "session bpp:i:32".to_string(),
        "smart sizing:i:1".to_string(),
        "dynamic resolution:i:1".to_string(),
        "authentication level:i:2".to_string(),
        "enablecredsspsupport:i:1".to_string(),
        "negotiate security layer:i:1".to_string(),
        "audiomode:i:0".to_string(),
        "redirectclipboard:i:1".to_string(),
    ];
    if let Some(sha1) = cert_sha1_thumbprint.filter(|s| !s.is_empty()) {
        lines.push("use redirection server name:i:1".to_string());
        lines.push(format!("certificatethumbprint:s:{sha1}"));
    }
    lines.join("\r\n") + "\r\n"
}

/// Writes a high-DPI (`3072×1920`), NLA-enforced, certificate-pinned `.rdp` connection profile
/// under `%LOCALAPPDATA%\PITS\Client\rdp\` (outside Syncthing `commonIgnore`!).
pub fn write_pinned_rdp_profile(
    profile_name: &str,
    host: &str,
    port: u16,
    username: &str,
    cert_sha1_thumbprint: Option<&str>,
    width: u32,
    height: u32,
) -> Result<PathBuf> {
    let dir = local_state_dir().join("rdp");
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(format!("{profile_name}.rdp"));
    let content = render_pinned_rdp_profile(host, port, username, cert_sha1_thumbprint, width, height);
    fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// Launches the native Windows Remote Desktop client (`mstsc.exe`) on the generated `.rdp` file.
pub fn launch_mstsc(rdp_file: &PathBuf) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("mstsc.exe")
            .arg(rdp_file)
            .spawn()
            .context("spawn mstsc.exe")?;
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = rdp_file;
        bail!("mstsc.exe is only available on Windows hosts")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vm_templates_finalize_and_qga_framing() {
        let install_vm = render_windows11_vm_manifest(true);
        assert!(install_vm.contains("win11-iso"));
        assert!(install_vm.contains("secureBoot: true"));
        assert!(install_vm.contains("Asia/Shanghai"));

        let finalized_vm = render_windows11_vm_manifest(false);
        assert!(!finalized_vm.contains("win11-iso"));
        assert!(finalized_vm.contains("system-disk"));

        let patch_003 = render_win11_pool_placement_patch("netboot-003").expect("patch 003");
        assert!(patch_003.contains("\"kubernetes.io/hostname\": null"));
        assert!(patch_003.contains("\"14Gi\""));
        assert!(patch_003.contains("\"hostDevices\": []"));
        let patch_004 = render_win11_pool_placement_patch("netboot-004").expect("patch 004");
        assert!(patch_004.contains("\"16Gi\""));
        assert!(patch_004.contains("pits.io/rtl8126-backup"));
        assert!(render_win11_pool_placement_patch("netboot-005").is_err());

        assert_eq!(WIN11_LIVE_MIGRATION_POOL, ["netboot-001", "netboot-002", "netboot-004"]);
        assert_eq!(validate_live_migration_target(Some("auto")).unwrap(), None);
        assert_eq!(
            validate_live_migration_target(Some("netboot-001")).unwrap(),
            Some("netboot-001".to_string())
        );
        assert!(validate_live_migration_target(Some("netboot-003")).is_err());
        let mig_yaml = render_vm_live_migration_manifest("pits-vms", "win11-001", "win11-mig-1");
        assert!(mig_yaml.contains("kind: VirtualMachineInstanceMigration"));
        assert!(mig_yaml.contains("vmiName: win11-001"));

        let access_vm = render_access_lab_vm_manifest();
        assert!(access_vm.contains(ACCESS_LAB_FIXED_MAC));

        let pw = generate_csprng_admin_password();
        assert_eq!(pw.len(), 20);

        let qga = build_qga_command_payload("guest-get-osinfo", 42);
        assert_eq!(qga[0], 0xFF);
        assert!(String::from_utf8_lossy(&qga[1..]).contains("guest-get-osinfo"));
    }
}
