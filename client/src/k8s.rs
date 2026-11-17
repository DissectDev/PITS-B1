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
    pub error: Option<String>,
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

    // Kubernetes v1.37+ DRA v1 ResourceSlice discovery (`resource.k8s.io/v1`)
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
                if driver != PITS_NPU_DRA_DRIVER && !driver.to_lowercase().contains("npu") {
                    continue;
                }
                out.npu_slices.push(parse_npu_slice_item(item, driver));
            }
        }
    }

    out
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
