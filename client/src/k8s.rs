//! Domain 5: Dual Kubernetes Control Planes (`pits-core` RKE2 v1.35.8 & Lab Talos K8s v1.37.1),
//! static NVMe/TCP SAN `PersistentVolume` / `PersistentVolumeClaim` provisioning, and
//! multi-vendor NPU Dynamic Resource Allocation (`resource.k8s.io/v1` DRA) operations & fleet audit.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::config::WorkspaceBundle;
use crate::pxe::{
    analyze_hook_inventory, classify_pci_npu, expected_npu_for_cpu, pxe_list_inventory,
    resolve_retail_machine_model, CpuNpuExpectation, DetectedNpuHardware,
};
use crate::talos::TalosClient;
use crate::transport::{AgentClient, KubeClient};

pub const PITS_NPU_DRA_DRIVER: &str = "npu.pits.home.arpa";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterControlPlaneSummary {
    pub cluster: String,
    pub server_url: String,
    pub via_agent_bridge: bool,
    pub reachable: bool,
    pub checked_at: DateTime<Utc>,
    pub git_version: Option<String>,
    pub nodes: Vec<KubeNodeSummary>,
    pub running_pods: usize,
    pub total_pods: usize,
    pub bound_pvs: usize,
    pub npu_slices: Vec<NpuSliceSummary>,
    #[serde(default)]
    pub dra_slices: Vec<DraSliceSummary>,
    #[serde(default)]
    pub dra_claims: Vec<DraClaimAllocation>,
    #[serde(default)]
    pub device_plugin_gpu_used: BTreeMap<String, usize>,
    #[serde(default)]
    pub device_classes: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DraClaimAllocation {
    pub namespace: String,
    pub name: String,
    pub driver: String,
    pub pool: String,
    pub device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,
    #[serde(default)]
    pub allocated: bool,
    #[serde(default)]
    pub consumer_pods: Vec<String>,
    #[serde(default)]
    pub consumer_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AcceleratorOccupancy {
    pub total: usize,
    pub allocated: usize,
    pub occupancy_pct: f64,
    pub consumer_pods: Vec<String>,
    pub claim_names: Vec<String>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraDeviceSummary {
    pub name: String,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub product_name: Option<String>,
    pub architecture: Option<String>,
    pub uuid: Option<String>,
    pub pci_address: Option<String>,
    pub kernel_driver: Option<String>,
    pub driver_version: Option<String>,
    pub memory: Option<String>,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraSliceSummary {
    pub slice_name: String,
    pub node_name: String,
    pub driver: String,
    pub device_class: String,
    pub devices: Vec<DraDeviceSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KubeNodeSummary {
    pub name: String,
    pub ready: bool,
    pub kubelet_version: String,
    pub internal_ip: String,
    pub roles: Vec<String>,
    pub custom_allocatable: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub retired_zero_resources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NpuDraDeviceSummary {
    pub name: String,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub pci_address: Option<String>,
    pub kernel_driver: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NpuSliceSummary {
    pub slice_name: String,
    pub node_name: String,
    pub driver: String,
    pub devices: Vec<String>,
    pub device_details: Vec<NpuDraDeviceSummary>,
}

/// Inspects live Kubernetes control-plane version, nodes, pods, PVs, and Intel NPU DRA slices.
pub async fn inspect_cluster_summary(kc: &KubeClient) -> ClusterControlPlaneSummary {
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
        dra_slices: Vec::new(),
        dra_claims: Vec::new(),
        device_plugin_gpu_used: BTreeMap::new(),
        device_classes: Vec::new(),
        error: None,
    };

    match kc.get_json::<serde_json::Value>("/version").await {
        Ok(ver) => {
            out.reachable = true;
            out.git_version = ver
                .get("gitVersion")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
        Err(e) => {
            out.error = Some(e.to_string());
            return out;
        }
    }

    if let Ok(node_list) = kc.get_json::<serde_json::Value>("/api/v1/nodes").await {
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
                            || k.contains("nvidia")
                            || k.contains("gpu")
                            || k.contains("kubevirt.io/")
                        {
                            if let Some(s) = v.as_str() {
                                if k == "npu.intel.com/accel" && s == "0" {
                                    retired_zero_resources.push(format!("{k}=0 (retired Phase-1 DevicePlugin; superseded by DRA v1)"));
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

    if let Ok(pods) = kc
        .get_json_with_headers::<serde_json::Value>(
            "/api/v1/pods?includeObject=None",
            &[(
                "Accept",
                "application/json;as=Table;v=v1;g=meta.k8s.io,application/json",
            )],
        )
        .await
    {
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

    if let Ok(pvs) = kc
        .get_json::<serde_json::Value>("/api/v1/persistentvolumes")
        .await
    {
        if let Some(items) = pvs.get("items").and_then(|v| v.as_array()) {
            out.bound_pvs = items
                .iter()
                .filter(|p| p.pointer("/status/phase").and_then(|v| v.as_str()) == Some("Bound"))
                .count();
        }
    }

    // Kubernetes DRA v1 DeviceClasses + ResourceSlice discovery (`resource.k8s.io/v1`)
    if let Ok(dcs) = kc
        .get_json::<serde_json::Value>("/apis/resource.k8s.io/v1/deviceclasses")
        .await
    {
        if let Some(items) = dcs.get("items").and_then(|v| v.as_array()) {
            for item in items {
                if let Some(name) = item.pointer("/metadata/name").and_then(|v| v.as_str()) {
                    out.device_classes.push(name.to_string());
                }
            }
            out.device_classes.sort();
            out.device_classes.dedup();
        }
    }

    if let Ok(slices) = kc
        .get_json::<serde_json::Value>("/apis/resource.k8s.io/v1/resourceslices")
        .await
    {
        if let Some(items) = slices.get("items").and_then(|v| v.as_array()) {
            for item in items {
                let driver = item
                    .pointer("/spec/driver")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                out.dra_slices.push(parse_dra_slice_item(item, &driver));
                if driver == PITS_NPU_DRA_DRIVER || driver.to_lowercase().contains("npu") {
                    out.npu_slices.push(parse_npu_slice_item(item, driver));
                }
            }
        }
    }

    if let Ok(claims) = kc
        .get_json::<serde_json::Value>("/apis/resource.k8s.io/v1/resourceclaims")
        .await
    {
        out.dra_claims = parse_dra_claims_list(&claims);
    }

    if cluster_has_gpu_allocatable(&out.nodes) {
        if let Ok(active_pods) = kc
            .get_json::<serde_json::Value>(
                "/api/v1/pods?fieldSelector=status.phase%21%3DSucceeded,status.phase%21%3DFailed",
            )
            .await
        {
            out.device_plugin_gpu_used = parse_pod_device_plugin_gpu_usage(&active_pods);
        }
    }

    out
}

pub(crate) fn is_gpu_allocatable_key(key: &str) -> bool {
    let kl = key.to_ascii_lowercase();
    kl == "nvidia.com/gpu" || kl == "amd.com/gpu" || kl.starts_with("gpu.intel.com/")
}

pub(crate) fn is_gpu_dra_driver(driver: &str) -> bool {
    let drv = driver.to_ascii_lowercase();
    drv.contains("gpu") || drv.contains("nvidia")
}

pub(crate) fn is_npu_dra_driver(driver: &str) -> bool {
    let drv = driver.to_ascii_lowercase();
    driver == PITS_NPU_DRA_DRIVER || drv.contains("npu")
}

pub(crate) fn cluster_has_gpu_allocatable(nodes: &[KubeNodeSummary]) -> bool {
    nodes.iter().any(|n| {
        n.custom_allocatable
            .iter()
            .any(|(k, v)| is_gpu_allocatable_key(k) && v.trim().parse::<usize>().unwrap_or(0) > 0)
    })
}

/// Parses Kubernetes `/apis/resource.k8s.io/v1/resourceclaims` into allocated device claims.
pub(crate) fn parse_dra_claims_list(val: &serde_json::Value) -> Vec<DraClaimAllocation> {
    let Some(items) = val.get("items").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        let ns = item
            .pointer("/metadata/namespace")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();
        let name = item
            .pointer("/metadata/name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let Some(results) = item
            .pointer("/status/allocation/devices/results")
            .and_then(|v| v.as_array())
        else {
            continue;
        };
        if results.is_empty() {
            continue;
        }
        let mut selector_node: Option<String> = None;
        if let Some(terms) = item
            .pointer("/status/allocation/nodeSelector/nodeSelectorTerms")
            .and_then(|v| v.as_array())
        {
            for term in terms {
                if let Some(fields) = term.get("matchFields").and_then(|v| v.as_array()) {
                    for f in fields {
                        if f.get("key").and_then(|v| v.as_str()) == Some("metadata.name") {
                            if let Some(v) = f
                                .get("values")
                                .and_then(|a| a.as_array())
                                .and_then(|a| a.first())
                                .and_then(|v| v.as_str())
                            {
                                selector_node = Some(v.to_string());
                                break;
                            }
                        }
                    }
                }
                if selector_node.is_none() {
                    if let Some(exprs) = term.get("matchExpressions").and_then(|v| v.as_array()) {
                        for e in exprs {
                            if e.get("key").and_then(|v| v.as_str())
                                == Some("kubernetes.io/hostname")
                            {
                                if let Some(v) = e
                                    .get("values")
                                    .and_then(|a| a.as_array())
                                    .and_then(|a| a.first())
                                    .and_then(|v| v.as_str())
                                {
                                    selector_node = Some(v.to_string());
                                    break;
                                }
                            }
                        }
                    }
                }
                if selector_node.is_some() {
                    break;
                }
            }
        }

        let mut consumers = Vec::new();
        if let Some(reserved) = item.pointer("/status/reservedFor").and_then(|v| v.as_array()) {
            for r in reserved {
                if let Some(rn) = r.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                    consumers.push(format!("{ns}/{rn}"));
                }
            }
        }
        consumers.sort();
        consumers.dedup();

        for res in results {
            let driver = res
                .get("driver")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let pool = res
                .get("pool")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let device = res
                .get("device")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let resolved_node = selector_node
                .clone()
                .or_else(|| (!pool.is_empty()).then(|| pool.clone()));
            out.push(DraClaimAllocation {
                namespace: ns.clone(),
                name: name.clone(),
                driver,
                pool,
                device,
                node_name: resolved_node,
                allocated: true,
                consumer_count: consumers.len(),
                consumer_pods: consumers.clone(),
            });
        }
    }
    out
}

/// Counts non-terminated DevicePlugin GPU requests/limits per node from a Kubernetes `PodList`.
pub(crate) fn parse_pod_device_plugin_gpu_usage(val: &serde_json::Value) -> BTreeMap<String, usize> {
    let mut used = BTreeMap::new();
    let Some(items) = val.get("items").and_then(|v| v.as_array()) else {
        return used;
    };
    for p in items {
        let phase = p
            .pointer("/status/phase")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if phase == "Succeeded" || phase == "Failed" {
            continue;
        }
        let Some(node) = p
            .pointer("/spec/nodeName")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(containers) = p.pointer("/spec/containers").and_then(|v| v.as_array()) else {
            continue;
        };
        for c in containers {
            let mut seen_keys = std::collections::BTreeSet::new();
            for ptr in ["/resources/limits", "/resources/requests"] {
                if let Some(obj) = c.pointer(ptr).and_then(|v| v.as_object()) {
                    for (k, v) in obj {
                        let kl = k.to_ascii_lowercase();
                        if seen_keys.contains(&kl) || !is_gpu_allocatable_key(&kl) {
                            continue;
                        }
                        let cnt = v
                            .as_str()
                            .and_then(|s| s.trim().parse::<usize>().ok())
                            .or_else(|| v.as_u64().map(|u| u as usize))
                            .unwrap_or(0);
                        if cnt > 0 {
                            *used.entry(node.to_string()).or_insert(0) += cnt;
                            seen_keys.insert(kl);
                        }
                    }
                }
            }
        }
    }
    used
}

fn claim_matches_node(claim: &DraClaimAllocation, node_name: &str) -> bool {
    claim
        .node_name
        .as_deref()
        .map(|n| n.eq_ignore_ascii_case(node_name))
        .unwrap_or(false)
        || claim.pool.eq_ignore_ascii_case(node_name)
}

/// Computes single-GPU DRA `ResourceClaim` occupancy (`0%` or `100%`) and consuming pods.
pub(crate) fn compute_gpu_device_occupancy(
    cp: &ClusterControlPlaneSummary,
    node_name: &str,
    driver: &str,
    device_name: &str,
) -> AcceleratorOccupancy {
    let mut pods = Vec::new();
    let mut claims = Vec::new();
    let mut allocated = false;

    for c in &cp.dra_claims {
        if !c.allocated || !is_gpu_dra_driver(&c.driver) {
            continue;
        }
        let driver_ok = c.driver.eq_ignore_ascii_case(driver);
        let device_ok = c.device.eq_ignore_ascii_case(device_name);
        let node_ok = claim_matches_node(c, node_name);
        if driver_ok && device_ok && node_ok {
            allocated = true;
            claims.push(format!("{}/{}", c.namespace, c.name));
            pods.extend(c.consumer_pods.iter().cloned());
        }
    }
    claims.sort();
    claims.dedup();
    pods.sort();
    pods.dedup();

    let alloc_count = if allocated { 1 } else { 0 };
    AcceleratorOccupancy {
        total: 1,
        allocated: alloc_count,
        occupancy_pct: if allocated { 100.0 } else { 0.0 },
        consumer_pods: pods,
        claim_names: claims,
        source: "dra".to_string(),
    }
}

/// Computes node-level DevicePlugin fallback GPU occupancy when DRA slices are absent.
pub(crate) fn compute_gpu_node_device_plugin_occupancy(
    cp: &ClusterControlPlaneSummary,
    node_name: &str,
    total_gpus: usize,
) -> AcceleratorOccupancy {
    let used = cp.device_plugin_gpu_used.get(node_name).copied().unwrap_or(0);
    let clamped = used.min(total_gpus);
    let pct = if total_gpus > 0 {
        ((clamped as f64) * 100.0 / (total_gpus as f64)).min(100.0)
    } else {
        0.0
    };
    AcceleratorOccupancy {
        total: total_gpus,
        allocated: used,
        occupancy_pct: pct,
        consumer_pods: Vec::new(),
        claim_names: Vec::new(),
        source: "device-plugin".to_string(),
    }
}

/// Computes single-node NPU DRA `ResourceClaim` occupancy.
pub(crate) fn compute_npu_node_occupancy(
    cp: &ClusterControlPlaneSummary,
    node_name: &str,
    total_devices: usize,
) -> AcceleratorOccupancy {
    let mut seen_devs = std::collections::BTreeSet::new();
    let mut pods = Vec::new();
    let mut claims = Vec::new();

    for c in &cp.dra_claims {
        if !c.allocated || !is_npu_dra_driver(&c.driver) || !claim_matches_node(c, node_name) {
            continue;
        }
        seen_devs.insert(format!(
            "{}/{}/{}",
            c.driver.to_ascii_lowercase(),
            c.pool.to_ascii_lowercase(),
            c.device.to_ascii_lowercase()
        ));
        claims.push(format!("{}/{}", c.namespace, c.name));
        pods.extend(c.consumer_pods.iter().cloned());
    }
    claims.sort();
    claims.dedup();
    pods.sort();
    pods.dedup();

    let total = total_devices.max(seen_devs.len());
    let allocated = seen_devs.len();
    let pct = if total > 0 {
        ((allocated as f64) * 100.0 / (total as f64)).min(100.0)
    } else {
        0.0
    };
    AcceleratorOccupancy {
        total,
        allocated,
        occupancy_pct: pct,
        consumer_pods: pods,
        claim_names: claims,
        source: "dra".to_string(),
    }
}

/// Computes cluster-wide GPU occupancy (`DRA` primary, `DevicePlugin` fallback).
pub(crate) fn compute_cluster_gpu_occupancy(cp: &ClusterControlPlaneSummary) -> AcceleratorOccupancy {
    let dra_total: usize = cp
        .dra_slices
        .iter()
        .filter(|s| is_gpu_dra_driver(&s.driver))
        .map(|s| s.devices.len())
        .sum();
    let total = if dra_total > 0 {
        dra_total
    } else {
        cp.nodes
            .iter()
            .flat_map(|n| n.custom_allocatable.iter())
            .filter(|(k, _)| is_gpu_allocatable_key(k))
            .filter_map(|(_, v)| v.trim().parse::<usize>().ok())
            .sum()
    };

    let mut dra_seen = std::collections::BTreeSet::new();
    let mut pods = Vec::new();
    let mut claims = Vec::new();
    for c in &cp.dra_claims {
        if !c.allocated || !is_gpu_dra_driver(&c.driver) {
            continue;
        }
        dra_seen.insert(format!(
            "{}/{}/{}",
            c.driver.to_ascii_lowercase(),
            c.pool.to_ascii_lowercase(),
            c.device.to_ascii_lowercase()
        ));
        claims.push(format!("{}/{}", c.namespace, c.name));
        pods.extend(c.consumer_pods.iter().cloned());
    }
    claims.sort();
    claims.dedup();
    pods.sort();
    pods.dedup();

    let (allocated, source) = if !dra_seen.is_empty() {
        (dra_seen.len(), "dra")
    } else {
        let dp_used: usize = cp.device_plugin_gpu_used.values().sum();
        if dp_used > 0 {
            (dp_used, "device-plugin")
        } else if dra_total > 0 {
            (0, "dra")
        } else if total > 0 {
            (0, "device-plugin")
        } else {
            (0, "none")
        }
    };

    let pct = if total > 0 {
        ((allocated as f64) * 100.0 / (total as f64)).min(100.0)
    } else {
        0.0
    };

    AcceleratorOccupancy {
        total,
        allocated,
        occupancy_pct: pct,
        consumer_pods: pods,
        claim_names: claims,
        source: source.to_string(),
    }
}

/// Computes cluster-wide NPU DRA occupancy.
pub(crate) fn compute_cluster_npu_occupancy(cp: &ClusterControlPlaneSummary) -> AcceleratorOccupancy {
    let total_from_npu: usize = cp.npu_slices.iter().map(|s| s.devices.len()).sum();
    let total_from_dra: usize = cp
        .dra_slices
        .iter()
        .filter(|s| is_npu_dra_driver(&s.driver))
        .map(|s| s.devices.len())
        .sum();
    let total = total_from_npu.max(total_from_dra);

    let mut dra_seen = std::collections::BTreeSet::new();
    let mut pods = Vec::new();
    let mut claims = Vec::new();
    for c in &cp.dra_claims {
        if !c.allocated || !is_npu_dra_driver(&c.driver) {
            continue;
        }
        dra_seen.insert(format!(
            "{}/{}/{}",
            c.driver.to_ascii_lowercase(),
            c.pool.to_ascii_lowercase(),
            c.device.to_ascii_lowercase()
        ));
        claims.push(format!("{}/{}", c.namespace, c.name));
        pods.extend(c.consumer_pods.iter().cloned());
    }
    claims.sort();
    claims.dedup();
    pods.sort();
    pods.dedup();

    let allocated = dra_seen.len();
    let pct = if total > 0 {
        ((allocated as f64) * 100.0 / (total as f64)).min(100.0)
    } else {
        0.0
    };
    AcceleratorOccupancy {
        total,
        allocated,
        occupancy_pct: pct,
        consumer_pods: pods,
        claim_names: claims,
        source: if total > 0 { "dra" } else { "none" }.to_string(),
    }
}

fn extract_dra_attr_text(val: &serde_json::Value) -> Option<String> {
    if let Some(s) = val
        .get("string")
        .or_else(|| val.get("stringValue"))
        .or_else(|| val.get("version"))
        .and_then(|v| v.as_str())
    {
        return Some(s.to_string());
    }
    if let Some(i) = val
        .get("int")
        .or_else(|| val.get("intValue"))
        .and_then(|v| v.as_i64())
    {
        return Some(i.to_string());
    }
    val.get("bool")
        .and_then(|v| v.as_bool())
        .map(|b| b.to_string())
}

fn infer_dra_device_class(driver: &str) -> String {
    let low = driver.to_ascii_lowercase();
    if low == "gpu.nvidia.com" || low.contains("nvidia") {
        "gpu.nvidia.com".to_string()
    } else if low.contains("npu") {
        "pits-intel-npu".to_string()
    } else {
        driver.to_string()
    }
}

pub(crate) fn parse_dra_slice_item(item: &serde_json::Value, driver: &str) -> DraSliceSummary {
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
    if let Some(devs) = item.pointer("/spec/devices").and_then(|v| v.as_array()) {
        for d in devs {
            let dn = d
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unnamed")
                .to_string();
            let mut attrs = BTreeMap::new();
            if let Some(obj) = d.get("attributes").and_then(|v| v.as_object()) {
                for (k, v) in obj {
                    if let Some(t) = extract_dra_attr_text(v) {
                        attrs.insert(k.clone(), t);
                    }
                }
            }
            let memory = d
                .pointer("/capacity/memory/value")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let product_name = attrs
                .get("productName")
                .or_else(|| attrs.get("model"))
                .cloned();
            let vendor = attrs.get("vendor").cloned().or_else(|| {
                let low = format!(
                    "{} {} {}",
                    driver,
                    product_name.as_deref().unwrap_or(""),
                    attrs.get("brand").map(String::as_str).unwrap_or("")
                )
                .to_ascii_lowercase();
                if low.contains("nvidia") {
                    Some("nvidia".to_string())
                } else if low.contains("amd") {
                    Some("amd".to_string())
                } else if low.contains("intel") {
                    Some("intel".to_string())
                } else {
                    None
                }
            });
            let pci_address = attrs
                .get("pciAddress")
                .or_else(|| attrs.get("pcieBusID"))
                .or_else(|| attrs.get("resource.kubernetes.io/pciBusID"))
                .cloned();
            let driver_version = attrs
                .get("driverVersion")
                .or_else(|| attrs.get("cudaDriverVersion"))
                .cloned();
            devices.push(DraDeviceSummary {
                name: dn,
                vendor,
                model: attrs.get("model").cloned().or_else(|| product_name.clone()),
                product_name,
                architecture: attrs.get("architecture").cloned(),
                uuid: attrs.get("uuid").cloned(),
                pci_address,
                kernel_driver: attrs.get("kernelDriver").cloned(),
                driver_version,
                memory,
                attributes: attrs,
            });
        }
    }
    DraSliceSummary {
        slice_name,
        node_name,
        driver: driver.to_string(),
        device_class: infer_dra_device_class(driver),
        devices,
    }
}

fn parse_npu_slice_item(item: &serde_json::Value, driver: String) -> NpuSliceSummary {
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
    NpuSliceSummary {
        slice_name,
        node_name,
        driver,
        devices,
        device_details,
    }
}

/// Hardware-vs-DRA cross-verification entry for a single physical machine in the PITS fleet.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineNpuAuditEntry {
    pub machine_id: String,
    pub cluster: String,
    pub node_ip: String,
    pub data_sources: Vec<String>,
    pub manufacturer: Option<String>,
    pub dmi_product_name: Option<String>,
    pub board_or_sku: Option<String>,
    pub bios_version: Option<String>,
    pub resolved_retail_model: String,
    pub cpu_model: String,
    pub cpu_cores_threads: Option<String>,
    pub cpu_npu_expectation: CpuNpuExpectation,
    pub physical_npus: Vec<DetectedNpuHardware>,
    pub hardware_matches_cpu_spec: bool,
    pub dra_published: bool,
    pub dra_slice_name: Option<String>,
    pub dra_device_class: Option<String>,
    pub dra_devices: Vec<NpuDraDeviceSummary>,
    pub status: String,
    pub notes: Vec<String>,
}

/// Complete 5-machine (`pits-core` + `netboot-001..004`) hardware model ↔ CPU ↔ PCI NPU ↔ K8s DRA audit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetNpuAuditReport {
    pub checked_at: DateTime<Utc>,
    pub dra_driver: String,
    pub registered_device_classes: Vec<String>,
    pub total_machines: usize,
    pub physical_npu_machines: usize,
    pub dra_ready_machines: usize,
    pub driver_pending_machines: usize,
    pub machines: Vec<MachineNpuAuditEntry>,
}

/// Cross-verifies every physical machine (`pits-core` + `netboot-001..004`) by combining:
/// 1. `pits-agent` live `/v1/host/audit` + Hook `/v1/pxe/inventory` reports
/// 2. Live Talos COSI `SystemInformations`, `Processors`, and `PCIDevices` (`:50000` gRPC)
/// 3. Live Kubernetes `DeviceClass`es, `ResourceSlice`s (`resource.k8s.io/v1`), and `Node` status.
pub async fn audit_fleet_npus(
    bundle: &WorkspaceBundle,
    agent: Option<&AgentClient>,
    core_k8s: Option<&KubeClient>,
    lab_k8s: Option<&KubeClient>,
    talos: Option<&TalosClient>,
) -> FleetNpuAuditReport {
    let host_audit_fut = async {
        match agent {
            Some(ag) => ag.get_json::<serde_json::Value>("/v1/host/audit").await.ok(),
            None => None,
        }
    };
    let hook_inv_fut = async {
        match agent {
            Some(ag) => pxe_list_inventory(ag).await.ok(),
            None => None,
        }
    };
    let core_node_fut = async {
        match core_k8s {
            Some(kc) => kc.get_json::<serde_json::Value>("/api/v1/nodes/pits-core").await.ok(),
            None => None,
        }
    };
    let lab_dc_fut = async {
        match lab_k8s {
            Some(kc) => kc
                .get_json::<serde_json::Value>("/apis/resource.k8s.io/v1/deviceclasses")
                .await
                .ok(),
            None => None,
        }
    };
    let lab_summary_fut = async {
        match lab_k8s {
            Some(kc) => Some(inspect_cluster_summary(kc).await),
            None => None,
        }
    };
    let talos_nodes_fut = async {
        let mut map = BTreeMap::new();
        if let Some(tc) = talos {
            let mut futs = Vec::new();
            for n in &bundle.nodes {
                let dev_id = n.device_id.clone();
                let ip = n.node_ip.clone();
                let tc_clone = tc.clone();
                futs.push(tokio::spawn(async move {
                    (dev_id, tc_clone.inspect_node(&ip).await)
                }));
            }
            for handle in futs {
                if let Ok((dev_id, st)) = handle.await {
                    map.insert(dev_id, st);
                }
            }
        }
        map
    };

    let (host_audit, hook_reports, core_node, lab_dcs, lab_summary, talos_map) = tokio::join!(
        host_audit_fut,
        hook_inv_fut,
        core_node_fut,
        lab_dc_fut,
        lab_summary_fut,
        talos_nodes_fut,
    );

    let mut registered_device_classes = Vec::new();
    if let Some(dc_list) = lab_dcs {
        if let Some(items) = dc_list.get("items").and_then(|v| v.as_array()) {
            for item in items {
                if let Some(name) = item.pointer("/metadata/name").and_then(|v| v.as_str()) {
                    registered_device_classes.push(name.to_string());
                }
            }
        }
    }
    registered_device_classes.sort();

    let mut machines = Vec::new();

    // 1. Audit `pits-core` (RKE2 Router / SAN / PXE Host)
    {
        let mut sources = Vec::new();
        let mut core_npus = Vec::new();
        let mut core_notes = Vec::new();
        let mut logical_cpus = 16u64;
        let mut mgmt_ip = "192.168.71.35".to_string();

        if let Some(ref ha) = host_audit {
            sources.push("pits-agent:/v1/host/audit".to_string());
            if let Some(c) = ha.get("logicalCpus").and_then(|v| v.as_u64()) {
                logical_cpus = c;
            }
            if let Some(addr) = ha.get("managementAddress").and_then(|v| v.as_str()) {
                if !addr.is_empty() {
                    mgmt_ip = addr.split('/').next().unwrap_or(addr).to_string();
                }
            }
            if let Some(pcis) = ha.get("pciBindings").and_then(|v| v.as_array()) {
                for p in pcis {
                    let addr = p.get("address").and_then(|v| v.as_str()).unwrap_or("");
                    let vid = p.get("vendorId").and_then(|v| v.as_str()).unwrap_or("");
                    let did = p.get("deviceId").and_then(|v| v.as_str()).unwrap_or("");
                    let drv = p.get("driver").and_then(|v| v.as_str());
                    if let Some(npu) = classify_pci_npu(addr, vid, did, None, drv) {
                        core_npus.push(npu);
                    }
                }
            }
        }
        if let Some(ref cn) = core_node {
            sources.push("rke2:/api/v1/nodes/pits-core".to_string());
            if let Some(os_img) = cn.pointer("/status/nodeInfo/osImage").and_then(|v| v.as_str()) {
                core_notes.push(format!("Host OS: {os_img}"));
            }
        }
        if sources.is_empty() {
            sources.push("pits-baseline:pits-core".to_string());
        }
        core_notes.push(
            "12th Gen Intel Alder Lake-P (i5-12600H) pre-dates Intel AI Boost; PCI 0000:00:0d.2 (8086:463e [0c03]) is Thunderbolt 4 NHI, not an NPU."
                .to_string(),
        );

        let core_cpu = "12th Gen Intel(R) Core(TM) i5-12600H".to_string();
        let core_exp = expected_npu_for_cpu(&core_cpu);
        let matches_spec = !core_exp.expected_present && core_npus.is_empty();

        machines.push(MachineNpuAuditEntry {
            machine_id: "pits-core".to_string(),
            cluster: "pits-core".to_string(),
            node_ip: mgmt_ip,
            data_sources: sources,
            manufacturer: Some("Micro Computer (HK) Tech Limited".to_string()),
            dmi_product_name: Some("Venus Series".to_string()),
            board_or_sku: Some("AHWSA".to_string()),
            bios_version: None,
            resolved_retail_model: "Minisforum MS-01 (Venus Series / AHWSA)".to_string(),
            cpu_model: core_cpu,
            cpu_cores_threads: Some(format!("12C/{logical_cpus}T")),
            cpu_npu_expectation: core_exp,
            physical_npus: core_npus,
            hardware_matches_cpu_spec: matches_spec,
            dra_published: false,
            dra_slice_name: None,
            dra_device_class: None,
            dra_devices: Vec::new(),
            status: "NoNpuHardware".to_string(),
            notes: core_notes,
        });
    }

    // 2. Audit `netboot-001..004` (Talos Linux v1.14.5 + Lab K8s v1.37.1)
    for node in &bundle.nodes {
        let dev_id = &node.device_id;
        let mut sources = Vec::new();
        let mut notes = Vec::new();

        let hook_analysis = hook_reports
            .as_ref()
            .and_then(|m| m.get(dev_id))
            .and_then(|raw| analyze_hook_inventory(raw).ok());
        if hook_analysis.is_some() {
            sources.push("hook-inventory:/v1/pxe/inventory".to_string());
        }

        let live_talos = talos_map.get(dev_id).filter(|t| t.reachable);
        if live_talos.is_some() {
            sources.push("talos-cosi:hardware+runtime".to_string());
        }

        let manufacturer = live_talos
            .and_then(|t| t.hardware.as_ref())
            .and_then(|h| h.manufacturer.clone())
            .or_else(|| hook_analysis.as_ref().and_then(|h| h.system_vendor.clone()));
        let dmi_product_name = live_talos
            .and_then(|t| t.hardware.as_ref())
            .and_then(|h| h.product_name.clone())
            .or_else(|| hook_analysis.as_ref().and_then(|h| h.product_name.clone()));
        let board_or_sku = hook_analysis
            .as_ref()
            .and_then(|h| h.board_serial.clone())
            .or_else(|| {
                live_talos
                    .and_then(|t| t.hardware.as_ref())
                    .and_then(|h| h.sku_number.clone())
                    .filter(|s| !s.eq_ignore_ascii_case("Default string"))
            });
        let bios_version = live_talos
            .and_then(|t| t.hardware.as_ref())
            .and_then(|h| h.bios_version.clone());

        let cpu_model = live_talos
            .and_then(|t| t.hardware.as_ref())
            .and_then(|h| h.cpu_model.clone())
            .or_else(|| hook_analysis.as_ref().and_then(|h| h.cpu_model.clone()))
            .unwrap_or_else(|| node.cpu_model.clone());
        let cpu_cores_threads = live_talos
            .and_then(|t| t.hardware.as_ref())
            .and_then(|h| match (h.cpu_cores, h.cpu_threads) {
                (Some(c), Some(th)) => Some(format!("{c}C/{th}T")),
                _ => None,
            });

        let resolved_retail_model = if manufacturer.is_none()
            && dmi_product_name.is_none()
            && board_or_sku.is_none()
        {
            node.model.clone()
        } else {
            resolve_retail_machine_model(
                manufacturer.as_deref(),
                dmi_product_name.as_deref(),
                board_or_sku.as_deref(),
                Some(&cpu_model),
            )
        };

        let physical_npus = if let Some(t) = live_talos {
            if !t.physical_npus.is_empty() {
                t.physical_npus.clone()
            } else {
                hook_analysis
                    .as_ref()
                    .map(|h| h.npus_detected.clone())
                    .unwrap_or_default()
            }
        } else {
            hook_analysis
                .as_ref()
                .map(|h| h.npus_detected.clone())
                .unwrap_or_default()
        };

        let cpu_exp = expected_npu_for_cpu(&cpu_model);
        let hardware_matches_cpu_spec = if cpu_exp.expected_present {
            physical_npus.iter().any(|n| {
                cpu_exp
                    .expected_pci_id
                    .as_deref()
                    .map(|id| id.eq_ignore_ascii_case(&n.pci_id))
                    .unwrap_or(true)
            })
        } else {
            physical_npus.is_empty()
        };

        let matching_slice = lab_summary
            .as_ref()
            .and_then(|s| s.npu_slices.iter().find(|sl| &sl.node_name == dev_id));
        if lab_summary.as_ref().map(|s| s.reachable).unwrap_or(false) {
            sources.push("k8s-dra:resource.k8s.io/v1".to_string());
        }

        if let Some(knode) = lab_summary
            .as_ref()
            .and_then(|s| s.nodes.iter().find(|kn| &kn.name == dev_id))
        {
            for retired in &knode.retired_zero_resources {
                notes.push(format!("Node status note: {retired}"));
            }
        }

        let dra_published = matching_slice
            .map(|sl| !sl.device_details.is_empty())
            .unwrap_or(false);
        let dra_slice_name = matching_slice.map(|sl| sl.slice_name.clone());
        let dra_devices = matching_slice
            .map(|sl| sl.device_details.clone())
            .unwrap_or_default();
        let dra_device_class = if dra_published {
            let v = dra_devices
                .first()
                .and_then(|d| d.vendor.as_deref())
                .or_else(|| physical_npus.first().map(|p| p.vendor.as_str()));
            match v {
                Some("intel") => Some("pits-intel-npu".to_string()),
                Some("amd") => Some("pits-amd-npu".to_string()),
                _ => Some("pits-npu".to_string()),
            }
        } else {
            None
        };

        let status = if physical_npus.is_empty() {
            "NoNpuHardware".to_string()
        } else if dra_published {
            "ReadyDraV1".to_string()
        } else if physical_npus.iter().any(|p| !p.driver_bound) {
            for p in &physical_npus {
                if !p.driver_bound {
                    notes.push(format!(
                        "Physical {} ({} @ {}) is present on PCI bus, but kernel driver '{}' is unbound (Talos 6.18.58-talos lacks CONFIG_DRM_ACCEL_AMDXDNA with module.sig_enforce=1), so /dev/accel/accel* is absent and DeviceClass '{}' has 0 published ResourceSlices.",
                        p.architecture, p.pci_id, p.pci_address, p.expected_kernel_driver, p.expected_device_class
                    ));
                }
            }
            "HardwarePresentDriverPending".to_string()
        } else {
            notes.push("Kernel driver bound, but no ResourceSlice found in Lab K8s".to_string());
            "DriverBoundSliceMissing".to_string()
        };

        machines.push(MachineNpuAuditEntry {
            machine_id: dev_id.clone(),
            cluster: "pits-lab".to_string(),
            node_ip: node.node_ip.clone(),
            data_sources: sources,
            manufacturer,
            dmi_product_name,
            board_or_sku,
            bios_version,
            resolved_retail_model,
            cpu_model,
            cpu_cores_threads,
            cpu_npu_expectation: cpu_exp,
            physical_npus,
            hardware_matches_cpu_spec,
            dra_published,
            dra_slice_name,
            dra_device_class,
            dra_devices,
            status,
            notes,
        });
    }

    let physical_npu_machines = machines
        .iter()
        .filter(|m| !m.physical_npus.is_empty())
        .count();
    let dra_ready_machines = machines.iter().filter(|m| m.dra_published).count();
    let driver_pending_machines = machines
        .iter()
        .filter(|m| m.status == "HardwarePresentDriverPending")
        .count();

    FleetNpuAuditReport {
        checked_at: Utc::now(),
        dra_driver: PITS_NPU_DRA_DRIVER.to_string(),
        registered_device_classes,
        total_machines: machines.len(),
        physical_npu_machines,
        dra_ready_machines,
        driver_pending_machines,
        machines,
    }
}

/// Renders node-pinned static SAN `PersistentVolume` + `PersistentVolumeClaim` manifests
/// (`san-node-local` on `/var/mnt/userdata/...` or `pits-vm-local` on `/var/mnt/vmstore/...`).
pub fn render_san_local_pv_pvc(
    pv_name: &str,
    namespace: &str,
    pvc_name: &str,
    node_hostname: &str,
    storage_class: &str,
    host_path: &str,
    capacity_gib: u32,
) -> String {
    format!(
        r#"apiVersion: v1
kind: PersistentVolume
metadata:
  name: {pv_name}
  labels:
    pits.io/san-backed: "true"
    pits.io/node: {node_hostname}
spec:
  capacity:
    storage: {capacity_gib}Gi
  volumeMode: Filesystem
  accessModes:
    - ReadWriteOnce
  persistentVolumeReclaimPolicy: Retain
  storageClassName: {storage_class}
  local:
    path: {host_path}
  nodeAffinity:
    required:
      nodeSelectorTerms:
        - matchExpressions:
            - key: kubernetes.io/hostname
              operator: In
              values:
                - {node_hostname}
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: {pvc_name}
  namespace: {namespace}
spec:
  accessModes:
    - ReadWriteOnce
  storageClassName: {storage_class}
  volumeName: {pv_name}
  resources:
    requests:
      storage: {capacity_gib}Gi
"#
    )
}

/// Applies static SAN `PersistentVolume` + `PersistentVolumeClaim` via Server-Side Apply.
pub async fn apply_san_local_pv_pvc(
    kc: &KubeClient,
    pv_name: &str,
    namespace: &str,
    pvc_name: &str,
    node_hostname: &str,
    storage_class: &str,
    host_path: &str,
    capacity_gib: u32,
) -> Result<()> {
    let yaml = render_san_local_pv_pvc(
        pv_name,
        namespace,
        pvc_name,
        node_hostname,
        storage_class,
        host_path,
        capacity_gib,
    );
    let docs: Vec<&str> = yaml.split("\n---\n").collect();
    kc.server_side_apply(
        &format!("/api/v1/persistentvolumes/{pv_name}"),
        "pitsctl-k8s",
        docs[0].as_bytes(),
        true,
    )
    .await?;
    kc.server_side_apply(
        &format!("/api/v1/namespaces/{namespace}/persistentvolumeclaims/{pvc_name}"),
        "pitsctl-k8s",
        docs[1].as_bytes(),
        true,
    )
    .await?;
    Ok(())
}

/// Renders Intel NPU DRA v1 (`resource.k8s.io/v1`) `ResourceClaim` + rootless UID 1000
/// OpenVINO inference validation `Job` (matching `docs/npu-dra-rollout-20261109.md`).
pub fn render_npu_validation_manifests(
    namespace: &str,
    claim_name: &str,
    job_name: &str,
    device_class_name: &str,
    target_node: Option<&str>,
) -> String {
    let node_selector_block = match target_node.filter(|s| !s.is_empty()) {
        Some(n) => format!(
            "      nodeSelector:\n        kubernetes.io/hostname: {n}\n"
        ),
        None => String::new(),
    };

    format!(
        r#"apiVersion: resource.k8s.io/v1
kind: ResourceClaim
metadata:
  name: {claim_name}
  namespace: {namespace}
spec:
  devices:
    requests:
      - name: intel-npu
        exactly:
          deviceClassName: {device_class_name}
          allocationMode: ExactCount
          count: 1
---
apiVersion: batch/v1
kind: Job
metadata:
  name: {job_name}
  namespace: {namespace}
  labels:
    pits.io/npu-validation: "true"
spec:
  backoffLimit: 1
  template:
    spec:
      restartPolicy: Never
{node_selector_block}      securityContext:
        runAsNonRoot: true
        runAsUser: 1000
        runAsGroup: 1000
        supplementalGroups:
          - 44
          - 109
      containers:
        - name: openvino-npu-probe
          image: openvino/ubuntu24_runtime:2026.0.0
          command:
            - python3
            - -c
            - |
              import openvino as ov
              core = ov.Core()
              devs = core.available_devices
              print("Available OpenVINO devices:", devs)
              assert "NPU" in devs, f"Intel NPU not visible in {{devs}}"
              print("NPU_DEVICE_NAME:", core.get_property("NPU", "FULL_DEVICE_NAME"))
          resources:
            claims:
              - name: npu
      resourceClaims:
        - name: npu
          resourceClaimName: {claim_name}
"#
    )
}

/// Applies the Intel NPU `ResourceClaim` and rootless OpenVINO validation `Job` on Lab Talos K8s.
pub async fn launch_npu_validation_job(
    lab_k8s: &KubeClient,
    namespace: &str,
    claim_name: &str,
    job_name: &str,
    device_class_name: &str,
    target_node: Option<&str>,
) -> Result<()> {
    let yaml = render_npu_validation_manifests(
        namespace,
        claim_name,
        job_name,
        device_class_name,
        target_node,
    );
    let docs: Vec<&str> = yaml.split("\n---\n").collect();
    lab_k8s
        .server_side_apply(
            &format!("/apis/resource.k8s.io/v1/namespaces/{namespace}/resourceclaims/{claim_name}"),
            "pitsctl-npu",
            docs[0].as_bytes(),
            true,
        )
        .await?;
    let _ = lab_k8s
        .delete_resource(&format!("/apis/batch/v1/namespaces/{namespace}/jobs/{job_name}"))
        .await;
    lab_k8s
        .server_side_apply(
            &format!("/apis/batch/v1/namespaces/{namespace}/jobs/{job_name}"),
            "pitsctl-npu",
            docs[1].as_bytes(),
            true,
        )
        .await?;
    Ok(())
}

/// Renders a GPU DRA v1 (`resource.k8s.io/v1`) `ResourceClaim` + CUDA verification `Job`.
pub fn render_gpu_validation_manifests(
    namespace: &str,
    claim_name: &str,
    job_name: &str,
    device_class_name: &str,
    target_node: Option<&str>,
) -> String {
    let node_selector_block = match target_node.filter(|s| !s.is_empty()) {
        Some(n) => format!(
            "      nodeSelector:\n        kubernetes.io/hostname: {n}\n"
        ),
        None => String::new(),
    };

    format!(
        r#"apiVersion: resource.k8s.io/v1
kind: ResourceClaim
metadata:
  name: {claim_name}
  namespace: {namespace}
spec:
  devices:
    requests:
      - name: gpu
        exactly:
          deviceClassName: {device_class_name}
          allocationMode: ExactCount
          count: 1
---
apiVersion: batch/v1
kind: Job
metadata:
  name: {job_name}
  namespace: {namespace}
spec:
  backoffLimit: 1
  template:
    spec:
      restartPolicy: Never
{node_selector_block}      containers:
        - name: cuda-dra-probe
          image: nvcr.io/nvidia/cuda:13.0.0-base-ubuntu24.04
          command: ["nvidia-smi"]
          resources:
            claims:
              - name: gpu
      resourceClaims:
        - name: gpu
          resourceClaimName: {claim_name}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_san_pv_and_npu_dra_v1_manifests() {
        let pv = render_san_local_pv_pvc(
            "pv-win11-vmstore",
            "pits-vms",
            "win11-vmstore-pvc",
            "netboot-004",
            "pits-vm-local",
            "/var/mnt/vmstore/win11-001",
            320,
        );
        assert!(pv.contains("netboot-004"));
        assert!(pv.contains("pits-vm-local"));

        let npu = render_npu_validation_manifests(
            "default",
            "npu-claim-002",
            "npu-verify-002",
            "pits-intel-npu",
            Some("netboot-002"),
        );
        assert!(npu.contains("apiVersion: resource.k8s.io/v1"));
        assert!(npu.contains("deviceClassName: pits-intel-npu"));
        assert!(npu.contains("runAsUser: 1000"));
        assert!(npu.contains("netboot-002"));
    }
}
