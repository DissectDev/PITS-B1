//! Domain 2: NVMe/TCP SAN targets & `pits-data` Btrfs storage pool controller.
//!
//! Computes exact RFC 4122 UUIDv5 namespace WWIDs (`nqn + "/" + name`), enforces 4KiB extent
//! alignment and additive-only growth, renders/reconciles Kubernetes `nvmet-<node>` target
//! manifests (`sfp0.20` VLAN 20 Multus NAD), and drives Go `pits-agent` configfs/Btrfs RPCs.

use anyhow::{bail, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::transport::{AgentClient, KubeClient};

pub const NQN_PREFIX: &str = "nqn.2026-11.arpa.home.pits:";
pub const HOST_NQN_PREFIX: &str = "nqn.2026-11.arpa.home.pits:host-";
pub const EXPECTED_BTRFS_UUID: &str = "777b7660-ef8e-421a-ac60-d6c4cb2fea39";
pub const SECTOR_4K: i64 = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SANNamespaceSpec {
    pub id: i32,
    pub name: String,
    pub bytes: i64,
    #[serde(default)]
    pub init_system_gpt: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SANDeviceSpec {
    pub device_id: String,
    pub pxe_mac: String,
    pub product_uuid: String,
    pub subsystem_nqn: String,
    pub host_nqn: String,
    pub address: String,
    pub port: i32,
    pub configfs_port: i32,
    #[serde(default)]
    pub vm_store: bool,
    pub namespaces: Vec<SANNamespaceSpec>,
}

/// Computes deterministic RFC 4122 UUIDv5(NAMESPACE_DNS, `<subsystem_nqn>/<namespace_name>`).
pub fn namespace_uuid(subsystem_nqn: &str, namespace_name: &str) -> String {
    let key = format!("{subsystem_nqn}/{namespace_name}");
    Uuid::new_v5(&Uuid::NAMESPACE_DNS, key.as_bytes()).to_string()
}

/// Computes Talos Linux NVMe WWID (`uuid.<uuid5>`).
pub fn expected_wwid(subsystem_nqn: &str, namespace_name: &str) -> String {
    format!("uuid.{}", namespace_uuid(subsystem_nqn, namespace_name))
}

/// Aligns byte capacity upward to a 4096-byte logical block boundary.
pub fn align_4k(bytes: i64) -> i64 {
    if bytes <= 0 {
        return 0;
    }
    let rem = bytes % SECTOR_4K;
    if rem == 0 {
        bytes
    } else {
        bytes + (SECTOR_4K - rem)
    }
}

/// Validates enrolled PITS SAN contract invariants on the client before any RPC or K8s apply.
pub fn validate_san_device_spec(spec: &SANDeviceSpec) -> Result<()> {
    let dev_re = Regex::new(r"^netboot-\d{3}$").unwrap();
    let mac_re = Regex::new(r"^(?:[0-9a-f]{2}:){5}[0-9a-f]{2}$").unwrap();
    let ns_re = Regex::new(r"^[a-z][a-z0-9-]{0,31}$").unwrap();

    if !dev_re.is_match(&spec.device_id) {
        bail!("invalid SAN deviceId {:?} (expected netboot-NNN)", spec.device_id);
    }
    if !mac_re.is_match(&spec.pxe_mac.to_lowercase()) {
        bail!("invalid PXE MAC {:?}", spec.pxe_mac);
    }
    Uuid::parse_str(&spec.product_uuid)
        .map_err(|e| anyhow::anyhow!("invalid DMI productUUID {:?}: {e}", spec.product_uuid))?;

    let want_nqn = format!("{NQN_PREFIX}{}", spec.device_id);
    let want_host = format!("{HOST_NQN_PREFIX}{}", spec.device_id);
    if spec.subsystem_nqn != want_nqn {
        bail!(
            "subsystemNQN {:?} must equal {:?}",
            spec.subsystem_nqn,
            want_nqn
        );
    }
    if spec.host_nqn != want_host {
        bail!("hostNQN {:?} must equal {:?}", spec.host_nqn, want_host);
    }
    if spec.port != 4420 {
        bail!("NVMe/TCP port must be 4420, got {}", spec.port);
    }
    if spec.namespaces.len() < 2 {
        bail!("at least NSID 1 (system) and NSID 2 (userdata) are required");
    }
    let mut has_system = false;
    let mut has_userdata = false;
    for ns in &spec.namespaces {
        if ns.id <= 0 || ns.id > 32 || !ns_re.is_match(&ns.name) {
            bail!("invalid namespace id={} name={:?}", ns.id, ns.name);
        }
        if ns.bytes < 16 * 1024 * 1024 {
            bail!("namespace {} bytes {} below 16 MiB minimum", ns.name, ns.bytes);
        }
        if ns.id == 1 && ns.name == "system" {
            has_system = true;
        }
        if ns.id == 2 && ns.name == "userdata" {
            has_userdata = true;
        }
    }
    if !has_system || !has_userdata {
        bail!("NSID 1 must be 'system' and NSID 2 must be 'userdata'");
    }
    Ok(())
}

/// Renders the Multus `NetworkAttachmentDefinition` (`sfp0.20` Lab VLAN 20) and `nvmet-<device>`
/// Kubernetes `Deployment` for `pits-core` (`tink-system` namespace) — replacing `nvmet-manager.py`.
pub fn render_nvmet_k8s_manifests(spec: &SANDeviceSpec) -> Result<String> {
    validate_san_device_spec(spec)?;
    let app_name = format!("nvmet-{}", spec.device_id);
    let spec_json = serde_json::to_string_pretty(spec)?;
    let indented_json: String = spec_json
        .lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n");

    Ok(format!(
        r#"apiVersion: k8s.cni.cncf.io/v1
kind: NetworkAttachmentDefinition
metadata:
  name: lab-vlan20-san
  namespace: tink-system
spec:
  config: |
    {{
      "cniVersion": "0.3.1",
      "name": "lab-vlan20-san",
      "type": "macvlan",
      "master": "sfp0.20",
      "mode": "bridge",
      "ipam": {{
        "type": "static"
      }}
    }}
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: {app_name}-contract
  namespace: tink-system
  labels:
    app.kubernetes.io/name: {app_name}
    pits.io/san-device: {dev}
data:
  san-contract.json: |
{indented_json}
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: {app_name}
  namespace: tink-system
  labels:
    app.kubernetes.io/name: {app_name}
    pits.io/san-device: {dev}
spec:
  replicas: 1
  strategy:
    type: Recreate
  selector:
    matchLabels:
      app.kubernetes.io/name: {app_name}
  template:
    metadata:
      labels:
        app.kubernetes.io/name: {app_name}
        pits.io/san-device: {dev}
      annotations:
        k8s.v1.cni.cncf.io/networks: '[
          {{"name": "lab-vlan20-san", "ips": ["{addr}/24"]}}
        ]'
    spec:
      nodeSelector:
        kubernetes.io/hostname: pits-core
      containers:
        - name: nvmet-holder
          image: ghcr.io/siderolabs/tools:v1.14.5
          command: ["/bin/sh", "-c", "trap : TERM INT; sleep infinity & wait"]
          securityContext:
            privileged: true
          volumeMounts:
            - name: configfs
              mountPath: /sys/kernel/config
            - name: san-backing
              mountPath: /var/mnt/tank/@data/pits/san/{dev}
            - name: contract
              mountPath: /etc/pits-san
              readOnly: true
      volumes:
        - name: configfs
          hostPath:
            path: /sys/kernel/config
            type: Directory
        - name: san-backing
          hostPath:
            path: /var/mnt/tank/@data/pits/san/{dev}
            type: DirectoryOrCreate
        - name: contract
          configMap:
            name: {app_name}-contract
"#,
        app_name = app_name,
        dev = spec.device_id,
        addr = spec.address,
        indented_json = indented_json,
    ))
}

/// Applies `nvmet-<device>` Kubernetes resources on `pits-core` (`tink-system`).
pub async fn apply_nvmet_k8s(core_k8s: &KubeClient, spec: &SANDeviceSpec) -> Result<()> {
    let rendered = render_nvmet_k8s_manifests(spec)?;
    let docs: Vec<&str> = rendered.split("\n---\n").collect();
    if docs.len() >= 3 {
        core_k8s
            .server_side_apply(
                "/apis/k8s.cni.cncf.io/v1/namespaces/tink-system/network-attachment-definitions/lab-vlan20-san",
                "pitsctl-san",
                docs[0].as_bytes(),
                true,
            )
            .await?;
        let cm_path = format!(
            "/api/v1/namespaces/tink-system/configmaps/nvmet-{}-contract",
            spec.device_id
        );
        core_k8s
            .server_side_apply(&cm_path, "pitsctl-san", docs[1].as_bytes(), true)
            .await?;
        let dep_path = format!(
            "/apis/apps/v1/namespaces/tink-system/deployments/nvmet-{}",
            spec.device_id
        );
        core_k8s
            .server_side_apply(&dep_path, "pitsctl-san", docs[2].as_bytes(), true)
            .await?;
    }
    Ok(())
}

/// Calls Go `pits-agent` to inspect live NVMe/TCP configfs + 4KiB GPT + NOCOW state.
pub async fn san_inspect(agent: &AgentClient, spec: &SANDeviceSpec) -> Result<serde_json::Value> {
    agent.post_json("/v1/san/inspect", spec).await
}

/// Calls Go `pits-agent` to provision backing extents, 4KiB META+STATE GPT, and configfs target.
pub async fn san_provision(
    agent: &AgentClient,
    spec: &SANDeviceSpec,
    hot_add_only: bool,
    configure_listener: bool,
    dry_run: bool,
) -> Result<serde_json::Value> {
    validate_san_device_spec(spec)?;
    let req = serde_json::json!({
        "spec": spec,
        "hotAddOnly": hot_add_only,
        "configureListener": configure_listener,
        "dryRun": dry_run,
    });
    agent.post_json("/v1/san/provision", &req).await
}

/// Calls Go `pits-agent` for additive live namespace expansion + `revalidate_size`.
pub async fn san_expand(
    agent: &AgentClient,
    device_id: &str,
    namespace_id: i32,
    new_bytes: i64,
    dry_run: bool,
) -> Result<serde_json::Value> {
    let req = serde_json::json!({
        "deviceID": device_id,
        "namespaceID": namespace_id,
        "newBytes": align_4k(new_bytes),
        "dryRun": dry_run,
    });
    agent.post_json("/v1/san/expand", &req).await
}

/// Queries `pits-data` Btrfs filesystem health, UUID (`777b7660-...`), subvolumes, and device errors.
pub async fn btrfs_status(agent: &AgentClient) -> Result<serde_json::Value> {
    agent.get_json("/v1/btrfs/status").await
}

/// Runs serialized `trim` (`FITRIM` ioctl) or `scrub` check on `pits-data` via `pits-agent`.
pub async fn btrfs_maintain(agent: &AgentClient, operation: &str) -> Result<serde_json::Value> {
    let req = serde_json::json!({ "operation": operation });
    agent.post_json("/v1/btrfs/maintain", &req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_wwid_matches_all_9_enrolled_pits_namespaces() {
        let cases = [
            ("nqn.2026-11.arpa.home.pits:netboot-001", "system", "uuid.6f58a394-206a-5991-8e37-929f2aed7ecd"),
            ("nqn.2026-11.arpa.home.pits:netboot-001", "userdata", "uuid.d5d12205-1537-5ddd-96d5-8e829540b651"),
            ("nqn.2026-11.arpa.home.pits:netboot-002", "system", "uuid.0db2c830-a3a9-50e5-8ea8-3ea90aafa2bd"),
            ("nqn.2026-11.arpa.home.pits:netboot-002", "userdata", "uuid.fef249f2-87c7-51e1-8b6a-2a3bac123fa2"),
            ("nqn.2026-11.arpa.home.pits:netboot-003", "system", "uuid.4d307d50-3a36-50ac-be22-c39af6337c5f"),
            ("nqn.2026-11.arpa.home.pits:netboot-003", "userdata", "uuid.29bba904-a2e8-54ae-9918-b8c351851c36"),
            ("nqn.2026-11.arpa.home.pits:netboot-004", "system", "uuid.2bca88f0-1844-572a-8f32-ef2b1e227333"),
            ("nqn.2026-11.arpa.home.pits:netboot-004", "userdata", "uuid.18268b6c-597c-5c8e-abb5-2ceccee6e445"),
            ("nqn.2026-11.arpa.home.pits:netboot-004", "vmstore", "uuid.c55a459d-a8cb-5177-b2ad-f47d603ee6ed"),
        ];
        for (nqn, name, want) in cases {
            assert_eq!(expected_wwid(nqn, name), want);
        }
        assert_eq!(align_4k(4097), 8192);
    }
}
