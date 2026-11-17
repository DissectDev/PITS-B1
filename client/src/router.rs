//! Domain 1: RKE2 Router / VPP Gateway / Crossplane `EdgeRouter` & `Router` controller.
//!
//! Enforces `pits-core` PCI safety (`0000:58:00.0` `mgmt0` guard), `core` (VLAN 10) <-> `lab`
//! (VLAN 20) mutual isolation, `lab -> management-net` block policy, Crossplane outer-object
//! reconciliation rules, 30s live verification freshness, safe VFIO recovery, and GoVPP trace.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use crate::transport::{AgentClient, KubeClient};

pub const MGMT0_PCI: &str = "0000:58:00.0";
pub const WAN_ROUTER_PCI: &str = "0000:5b:00.0";
pub const LAN_TRUNK_VF_PCI: &str = "0000:03:02.0";
pub const MAX_VERIFICATION_AGE_SECS: i64 = 30;

/// Outer Crossplane composite resource `platform.edge.router/v1alpha1 EdgeRouter`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeRouterManifest {
    pub api_version: String,
    pub kind: String,
    pub metadata: ObjectMeta,
    pub spec: EdgeRouterSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectMeta {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeRouterSpec {
    pub host_node: String,
    pub wan: WanSpec,
    pub lan_trunk_pci: String,
    pub networks: Vec<LanNetworkSpec>,
    #[serde(default)]
    pub aliases: Vec<AliasSpec>,
    #[serde(default)]
    pub dhcp_pools: Vec<DhcpPoolSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns: Option<DnsSpec>,
    #[serde(default)]
    pub firewall_rules: Vec<FirewallRuleSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nat: Option<NatSpec>,
    #[serde(default)]
    pub static_routes: Vec<StaticRouteSpec>,
    // Unimplemented fields explicitly checked & rejected if populated:
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vpn: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ips: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub one_to_one_nat: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WanSpec {
    pub pci_address: String,
    pub mode: String, // "dhcp" | "static"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cidr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    #[serde(default = "default_mtu")]
    pub mtu: u16,
}

fn default_mtu() -> u16 {
    1500
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanNetworkSpec {
    pub name: String,
    pub vlan_id: u16,
    pub gateway_cidr: String,
    pub subnet_cidr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AliasSpec {
    pub name: String,
    pub cidrs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DhcpPoolSpec {
    pub network: String,
    pub range_start: String,
    pub range_end: String,
    pub gateway: String,
    pub dns_servers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_class: Option<String>, // DHCP Option 60
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capwap_ac: Option<Vec<String>>, // DHCP Option 138 (Huawei AP AC discovery)
    #[serde(default)]
    pub reservations: Vec<DhcpReservation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DhcpReservation {
    pub hostname: String,
    pub mac: String,
    pub ip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DnsSpec {
    pub upstream_servers: Vec<String>,
    #[serde(default)]
    pub local_records: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FirewallRuleSpec {
    pub name: String,
    pub action: String, // "allow" | "block"
    pub source: String,
    pub destination: String,
    #[serde(default = "default_proto_any")]
    pub protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_port: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn default_proto_any() -> String {
    "any".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NatSpec {
    pub masquerade_wan: bool,
    #[serde(default)]
    pub port_forwards: Vec<PortForwardSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortForwardSpec {
    pub name: String,
    pub protocol: String,
    pub external_port: u16,
    pub internal_ip: String,
    pub internal_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticRouteSpec {
    pub destination: String,
    pub next_hop: String,
}

/// Policy validation report for an `EdgeRouter` manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouterValidationReport {
    pub valid: bool,
    pub desired_spec_sha256: String,
    pub core_lab_isolated: bool,
    pub lab_mgmt_blocked: bool,
    pub mgmt0_pci_safe: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Evaluates all PITS router hardware & security isolation invariants.
pub fn validate_edge_router(manifest: &EdgeRouterManifest) -> RouterValidationReport {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if manifest.api_version != "platform.edge.router/v1alpha1" || manifest.kind != "EdgeRouter" {
        errors.push(format!(
            "expected platform.edge.router/v1alpha1 EdgeRouter, got {} {}",
            manifest.api_version, manifest.kind
        ));
    }

    let spec = &manifest.spec;

    // 1. Reject unimplemented features per edge-router contract.
    if spec.vpn.is_some() {
        errors.push("spec.vpn is unimplemented and rejected by PITS gateway contract".to_string());
    }
    if spec.ips.is_some() {
        errors.push("spec.ips is unimplemented and rejected by PITS gateway contract".to_string());
    }
    if spec.one_to_one_nat.is_some() {
        errors.push("spec.oneToOneNat is unimplemented and rejected by PITS gateway contract".to_string());
    }

    // 2. Hardware PCI guard (`mgmt0` 0000:58:00.0 must NEVER be touched!).
    let mgmt0_pci_safe =
        spec.wan.pci_address != MGMT0_PCI && spec.lan_trunk_pci != MGMT0_PCI;
    if !mgmt0_pci_safe {
        errors.push(format!(
            "CRITICAL: management NIC PCI {MGMT0_PCI} cannot be assigned to WAN or LAN DPDK"
        ));
    }
    if spec.host_node == "pits-core" {
        if spec.wan.pci_address != WAN_ROUTER_PCI {
            warnings.push(format!(
                "WAN PCI {} differs from canonical pits-core WAN {}",
                spec.wan.pci_address, WAN_ROUTER_PCI
            ));
        }
        if spec.lan_trunk_pci != LAN_TRUNK_VF_PCI {
            warnings.push(format!(
                "LAN trunk VF PCI {} differs from canonical sfp0 VF0 {}",
                spec.lan_trunk_pci, LAN_TRUNK_VF_PCI
            ));
        }
    }

    // 3. Verify VLAN 10 (core) and VLAN 20 (lab) definitions.
    let has_core = spec
        .networks
        .iter()
        .any(|n| n.name == "core" && n.vlan_id == 10 && n.subnet_cidr == "10.88.0.0/16");
    let has_lab = spec
        .networks
        .iter()
        .any(|n| n.name == "lab" && n.vlan_id == 20 && n.subnet_cidr == "10.89.20.0/24");
    if !has_core || !has_lab {
        errors.push(
            "networks must define VLAN 10 ('core', 10.88.0.0/16) and VLAN 20 ('lab', 10.89.20.0/24)"
                .to_string(),
        );
    }

    // 4. Verify `management-net` alias covers host + RKE2 pod/service/CGNAT CIDRs.
    let required_mgmt_cidrs = [
        "192.168.71.0/24",
        "10.42.0.0/16",
        "10.43.0.0/16",
        "100.64.0.0/10",
    ];
    let mgmt_alias_complete = spec
        .aliases
        .iter()
        .find(|a| a.name == "management-net")
        .map(|a| {
            required_mgmt_cidrs
                .iter()
                .all(|req| a.cidrs.iter().any(|c| c == req))
        })
        .unwrap_or(false);
    if !mgmt_alias_complete {
        errors.push(format!(
            "alias 'management-net' must include all 4 protected CIDRs: {:?}",
            required_mgmt_cidrs
        ));
    }

    // 5. Verify ordered firewall ACLs enforce core<->lab isolation & lab->management-net block
    // BEFORE any broad allow rule from core or lab.
    let mut saw_core_to_lab_block = false;
    let mut saw_lab_to_core_block = false;
    let mut saw_lab_to_mgmt_block = false;
    let mut premature_allow = false;

    for rule in &spec.firewall_rules {
        let act = rule.action.to_lowercase();
        let src = rule.source.as_str();
        let dst = rule.destination.as_str();
        if act == "block" || act == "drop" || act == "deny" {
            if (src == "core" || src == "10.88.0.0/16") && (dst == "lab" || dst == "10.89.20.0/24")
                && rule.protocol == "any"
            {
                saw_core_to_lab_block = true;
            }
            if (src == "lab" || src == "10.89.20.0/24")
                && (dst == "core" || dst == "10.88.0.0/16")
                && rule.protocol == "any"
            {
                saw_lab_to_core_block = true;
            }
            if (src == "lab" || src == "10.89.20.0/24")
                && dst == "management-net"
                && rule.protocol == "any"
            {
                saw_lab_to_mgmt_block = true;
            }
        } else if act == "allow" && (dst == "any" || dst == "0.0.0.0/0") {
            if (src == "core" && !saw_core_to_lab_block)
                || (src == "lab" && (!saw_lab_to_core_block || !saw_lab_to_mgmt_block))
            {
                premature_allow = true;
            }
        }
    }

    let core_lab_isolated = saw_core_to_lab_block && saw_lab_to_core_block && !premature_allow;
    let lab_mgmt_blocked = mgmt_alias_complete && saw_lab_to_mgmt_block && !premature_allow;

    if !core_lab_isolated {
        errors.push(
            "ordered firewallRules must block core->lab and lab->core (protocol: any) before any broad allow"
                .to_string(),
        );
    }
    if !lab_mgmt_blocked {
        errors.push(
            "ordered firewallRules must block lab->management-net (protocol: any) before any broad allow"
                .to_string(),
        );
    }

    let desired_spec_sha256 = compute_desired_router_hash(&manifest.spec);

    RouterValidationReport {
        valid: errors.is_empty(),
        desired_spec_sha256,
        core_lab_isolated,
        lab_mgmt_blocked,
        mgmt0_pci_safe,
        errors,
        warnings,
    }
}

pub fn compute_desired_router_hash(spec: &EdgeRouterSpec) -> String {
    let canonical = serde_json::to_vec(spec).unwrap_or_default();
    let digest = Sha256::digest(&canonical);
    hex::encode(digest)
}

/// Canonical `EdgeRouter` (`edge/pits-core`) matching the live PITS deployment.
pub fn pits_core_default_edge_router() -> EdgeRouterManifest {
    let mut labels = BTreeMap::new();
    labels.insert("app.kubernetes.io/part-of".to_string(), "pits-edge-router".to_string());
    EdgeRouterManifest {
        api_version: "platform.edge.router/v1alpha1".to_string(),
        kind: "EdgeRouter".to_string(),
        metadata: ObjectMeta {
            name: "pits-core".to_string(),
            namespace: Some("edge".to_string()),
            labels,
            annotations: BTreeMap::new(),
            generation: None,
        },
        spec: EdgeRouterSpec {
            host_node: "pits-core".to_string(),
            wan: WanSpec {
                pci_address: WAN_ROUTER_PCI.to_string(),
                mode: "dhcp".to_string(),
                cidr: None,
                gateway: None,
                mtu: 1500,
            },
            lan_trunk_pci: LAN_TRUNK_VF_PCI.to_string(),
            networks: vec![
                LanNetworkSpec {
                    name: "core".to_string(),
                    vlan_id: 10,
                    gateway_cidr: "10.88.0.1/16".to_string(),
                    subnet_cidr: "10.88.0.0/16".to_string(),
                },
                LanNetworkSpec {
                    name: "lab".to_string(),
                    vlan_id: 20,
                    gateway_cidr: "10.89.20.1/24".to_string(),
                    subnet_cidr: "10.89.20.0/24".to_string(),
                },
            ],
            aliases: vec![AliasSpec {
                name: "management-net".to_string(),
                cidrs: vec![
                    "192.168.71.0/24".to_string(),
                    "10.42.0.0/16".to_string(),
                    "10.43.0.0/16".to_string(),
                    "100.64.0.0/10".to_string(),
                ],
            }],
            dhcp_pools: vec![
                DhcpPoolSpec {
                    network: "core".to_string(),
                    range_start: "10.88.1.100".to_string(),
                    range_end: "10.88.1.240".to_string(),
                    gateway: "10.88.0.1".to_string(),
                    dns_servers: vec!["10.88.0.1".to_string()],
                    vendor_class: Some("Huawei AP".to_string()),
                    capwap_ac: Some(vec!["10.88.0.10".to_string()]),
                    reservations: vec![],
                },
                DhcpPoolSpec {
                    network: "lab".to_string(),
                    range_start: "10.89.20.100".to_string(),
                    range_end: "10.89.20.199".to_string(),
                    gateway: "10.89.20.1".to_string(),
                    dns_servers: vec!["10.89.20.1".to_string()],
                    vendor_class: None,
                    capwap_ac: None,
                    reservations: vec![
                        DhcpReservation {
                            hostname: "netboot-001".to_string(),
                            mac: "58:47:ca:75:e4:11".to_string(),
                            ip: "10.89.20.21".to_string(),
                        },
                        DhcpReservation {
                            hostname: "netboot-002".to_string(),
                            mac: "58:47:ca:77:d9:de".to_string(),
                            ip: "10.89.20.22".to_string(),
                        },
                        DhcpReservation {
                            hostname: "netboot-003".to_string(),
                            mac: "58:47:ca:77:da:02".to_string(),
                            ip: "10.89.20.23".to_string(),
                        },
                        DhcpReservation {
                            hostname: "netboot-004".to_string(),
                            mac: "58:47:ca:75:f6:9a".to_string(),
                            ip: "10.89.20.24".to_string(),
                        },
                    ],
                },
            ],
            dns: Some(DnsSpec {
                upstream_servers: vec!["223.5.5.5".to_string(), "1.1.1.1".to_string()],
                local_records: BTreeMap::from([
                    ("pits-core.home.arpa".to_string(), "192.168.71.50".to_string()),
                    ("lab-k8s.home.arpa".to_string(), "10.89.20.21".to_string()),
                ]),
            }),
            firewall_rules: vec![
                FirewallRuleSpec {
                    name: "block-core-to-lab".to_string(),
                    action: "block".to_string(),
                    source: "core".to_string(),
                    destination: "lab".to_string(),
                    protocol: "any".to_string(),
                    dest_port: None,
                    description: Some("Strict core -> lab VLAN isolation".to_string()),
                },
                FirewallRuleSpec {
                    name: "block-lab-to-core".to_string(),
                    action: "block".to_string(),
                    source: "lab".to_string(),
                    destination: "core".to_string(),
                    protocol: "any".to_string(),
                    dest_port: None,
                    description: Some("Strict lab -> core VLAN isolation".to_string()),
                },
                FirewallRuleSpec {
                    name: "block-lab-to-mgmt".to_string(),
                    action: "block".to_string(),
                    source: "lab".to_string(),
                    destination: "management-net".to_string(),
                    protocol: "any".to_string(),
                    dest_port: None,
                    description: Some("Prevent lab workloads from reaching host/RKE2 management planes".to_string()),
                },
                FirewallRuleSpec {
                    name: "allow-core-egress".to_string(),
                    action: "allow".to_string(),
                    source: "core".to_string(),
                    destination: "any".to_string(),
                    protocol: "any".to_string(),
                    dest_port: None,
                    description: Some("Core LAN internet egress".to_string()),
                },
                FirewallRuleSpec {
                    name: "allow-lab-egress".to_string(),
                    action: "allow".to_string(),
                    source: "lab".to_string(),
                    destination: "any".to_string(),
                    protocol: "any".to_string(),
                    dest_port: None,
                    description: Some("Lab VLAN internet egress".to_string()),
                },
            ],
            nat: Some(NatSpec {
                masquerade_wan: true,
                port_forwards: vec![],
            }),
            static_routes: vec![],
            vpn: None,
            ips: None,
            one_to_one_nat: None,
        },
        status: None,
    }
}

/// Live reconciliation & freshness evaluation of the RKE2 `Router` CR + VPP dataplane.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouterLiveSummary {
    pub checked_at: DateTime<Utc>,
    pub edge_router_present: bool,
    pub crossplane_paused: bool,
    pub phase: String,
    pub ready: bool,
    pub generation: i64,
    pub observed_generation: i64,
    pub last_verified_time: Option<String>,
    pub verification_age_secs: Option<i64>,
    pub verification_fresh: bool,
    pub host_audit: Option<serde_json::Value>,
    pub vpp_report: Option<serde_json::Value>,
}

pub fn evaluate_verification_freshness(
    last_verified_rfc3339: Option<&str>,
    now: DateTime<Utc>,
) -> (Option<i64>, bool) {
    let Some(ts_str) = last_verified_rfc3339 else {
        return (None, false);
    };
    let Ok(parsed) = DateTime::parse_from_rfc3339(ts_str) else {
        return (None, false);
    };
    let age = (now - parsed.with_timezone(&Utc)).num_seconds().max(0);
    (Some(age), age <= MAX_VERIFICATION_AGE_SECS)
}

/// Queries live RKE2 `EdgeRouter` + `Router` + `pits-agent` host/VPP status.
pub async fn inspect_router_live(
    core_k8s: Option<&KubeClient>,
    agent: Option<&AgentClient>,
) -> RouterLiveSummary {
    let now = Utc::now();
    let mut summary = RouterLiveSummary {
        checked_at: now,
        edge_router_present: false,
        crossplane_paused: false,
        phase: "Unknown".to_string(),
        ready: false,
        generation: 0,
        observed_generation: 0,
        last_verified_time: None,
        verification_age_secs: None,
        verification_fresh: false,
        host_audit: None,
        vpp_report: None,
    };

    if let Some(kc) = core_k8s {
        if let Ok(er) = kc
            .get_json::<serde_json::Value>(
                "/apis/platform.edge.router/v1alpha1/namespaces/edge/edgerouters/pits-core",
            )
            .await
        {
            summary.edge_router_present = true;
            summary.crossplane_paused = er
                .pointer("/metadata/annotations/crossplane.io~1paused")
                .and_then(|v| v.as_str())
                == Some("true");
        }
        if let Ok(rtr) = kc
            .get_json::<serde_json::Value>(
                "/apis/edge.router/v1alpha1/namespaces/edge/routers/pits-core",
            )
            .await
        {
            summary.generation = rtr
                .pointer("/metadata/generation")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            summary.observed_generation = rtr
                .pointer("/status/observedGeneration")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            summary.phase = rtr
                .pointer("/status/phase")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown")
                .to_string();
            summary.ready = rtr
                .pointer("/status/ready")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            summary.last_verified_time = rtr
                .pointer("/status/lastVerifiedAt")
                .or_else(|| rtr.pointer("/status/lastVerifiedTime"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let (age, fresh) =
                evaluate_verification_freshness(summary.last_verified_time.as_deref(), now);
            summary.verification_age_secs = age;
            summary.verification_fresh = fresh
                && summary.ready
                && summary.phase == "Applied"
                && summary.observed_generation == summary.generation;
        }
    }

    if let Some(ag) = agent {
        if let Ok(audit) = ag.get_json::<serde_json::Value>("/v1/host/audit").await {
            summary.host_audit = Some(audit);
        }
        if let Ok(vpp) = ag.get_json::<serde_json::Value>("/v1/vpp/inspect").await {
            summary.vpp_report = Some(vpp);
        }
    }

    summary
}

/// Validates all safety invariants and reconciles the outer `EdgeRouter` (`edge/pits-core`)
/// via Kubernetes Server-Side Apply (never mutating Crossplane-owned child resources directly).
pub async fn apply_edge_router(
    core_k8s: &KubeClient,
    manifest: &EdgeRouterManifest,
) -> Result<serde_json::Value> {
    let report = validate_edge_router(manifest);
    if !report.valid {
        bail!(
            "refusing to apply invalid EdgeRouter: {}",
            report.errors.join("; ")
        );
    }
    let ns = manifest.metadata.namespace.as_deref().unwrap_or("edge");
    let name = &manifest.metadata.name;
    let yaml_bytes = serde_yaml::to_string(manifest).context("serialize EdgeRouter YAML")?;
    let path = format!(
        "/apis/platform.edge.router/v1alpha1/namespaces/{ns}/edgerouters/{name}"
    );
    core_k8s
        .server_side_apply(&path, "pitsctl-router", yaml_bytes.as_bytes(), true)
        .await
}

/// Pauses or resumes Crossplane reconciliation (`crossplane.io/paused`) on `EdgeRouter` & `Router`.
pub async fn set_crossplane_paused(
    core_k8s: &KubeClient,
    namespace: &str,
    name: &str,
    paused: bool,
) -> Result<()> {
    let patch_val = if paused {
        serde_json::json!({
            "metadata": { "annotations": { "crossplane.io/paused": "true" } }
        })
    } else {
        serde_json::json!({
            "metadata": { "annotations": { "crossplane.io/paused": null } }
        })
    };
    let er_path =
        format!("/apis/platform.edge.router/v1alpha1/namespaces/{namespace}/edgerouters/{name}");
    core_k8s.merge_patch(&er_path, &patch_val).await?;
    let rtr_path = format!("/apis/edge.router/v1alpha1/namespaces/{namespace}/routers/{name}");
    let _ = core_k8s.merge_patch(&rtr_path, &patch_val).await;
    Ok(())
}

/// Triggers guarded VFIO restoration (`0000:5b:00.0` + `0000:03:02.0`) and `edge/gateway`
/// rollout verification through Go `pits-agent` (replacing `scripts/restore-router.sh`).
pub async fn recover_router_runtime(
    agent: &AgentClient,
    cleanup_perf_netns: bool,
    refresh_device_plugins: bool,
    wait_ready_seconds: i32,
) -> Result<serde_json::Value> {
    let req = serde_json::json!({
        "cleanupPerfNetns": cleanup_perf_netns,
        "refreshDevicePlugins": refresh_device_plugins,
        "waitReadySeconds": wait_ready_seconds,
    });
    agent.post_json("/v1/router/recover", &req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_pits_core_router_passes_strict_validation() {
        let er = pits_core_default_edge_router();
        let rep = validate_edge_router(&er);
        assert!(rep.valid, "errors: {:?}", rep.errors);
        assert!(rep.core_lab_isolated);
        assert!(rep.lab_mgmt_blocked);
        assert!(rep.mgmt0_pci_safe);

        // Mutating WAN PCI to mgmt0 (0000:58:00.0) MUST fail validation!
        let mut broken = er.clone();
        broken.spec.wan.pci_address = MGMT0_PCI.to_string();
        let rep2 = validate_edge_router(&broken);
        assert!(!rep2.valid);
        assert!(!rep2.mgmt0_pci_safe);

        // Removing core->lab isolation rule MUST fail validation!
        let mut leaky = er;
        leaky.spec.firewall_rules.retain(|r| r.name != "block-core-to-lab");
        let rep3 = validate_edge_router(&leaky);
        assert!(!rep3.valid);
        assert!(!rep3.core_lab_isolated);
    }
}
