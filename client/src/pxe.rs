//! Domain 3: Tinkerbell PXE Server (`Hardware` / `Workflow`), iPXE script compiler,
//! Hook non-destructive hardware inventory analyzer, and boot asset SHA-256 verifier.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::config::EnrolledNodeContract;
use crate::transport::{AgentClient, KubeClient};

pub const MINISFORUM_OUI_PREFIX: &str = "58:47:ca:";
pub const ASSET_HTTP_BASE_DEFAULT: &str = "http://10.89.20.1:8080";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PxeBootMode {
    TalosNetboot,
    HookInventory,
}

/// Compiles a deterministic iPXE boot script for an enrolled diskless node.
/// Handles Minisforum (`58:47:ca:*`) UEFI HTTP/iPXE direct boot and strict kernel cmdline flags.
pub fn render_ipxe_script(
    node: &EnrolledNodeContract,
    mode: PxeBootMode,
    asset_http_base: Option<&str>,
) -> String {
    let base = asset_http_base
        .unwrap_or(ASSET_HTTP_BASE_DEFAULT)
        .trim_end_matches('/');
    let is_minisforum = node
        .pxe_mac
        .to_lowercase()
        .starts_with(MINISFORUM_OUI_PREFIX);
    let nic_comment = if is_minisforum {
        "# Minisforum UEFI direct-iPXE OUI (58:47:ca:*) detected: skip redundant DHCP re-chain"
    } else {
        "# Standard UEFI PXE client"
    };

    match mode {
        PxeBootMode::TalosNetboot => {
            let dev = &node.device_id;
            let cmdline = [
                "talos.platform=metal",
                &format!("talos.hostname={}", node.hostname),
                "console=tty0",
                "console=ttyS0,115200n8",
                "init_on_alloc=1",
                "slab_nomerge",
                "pti=on",
                "selinux=1",
                "module.sig_enforce=1",
                "intel_iommu=on",
                "iommu=pt",
                "hugepagesz=2M",
                "hugepages=1024",
                "nvme_core.multipath=N",
            ]
            .join(" ");

            format!(
                "#!ipxe\n\
                 {nic_comment}\n\
                 echo Booting PITS Diskless Talos Linux v1.14.5 for {dev} ({mac} -> {ip})\n\
                 set base-url {base}/talos/{dev}\n\
                 kernel ${{base-url}}/vmlinuz {cmdline} initrd=initramfs.xz\n\
                 initrd ${{base-url}}/initramfs.xz\n\
                 boot\n",
                nic_comment = nic_comment,
                dev = dev,
                mac = node.pxe_mac,
                ip = node.node_ip,
                base = base,
                cmdline = cmdline,
            )
        }
        PxeBootMode::HookInventory => {
            let dev = &node.device_id;
            format!(
                "#!ipxe\n\
                 {nic_comment}\n\
                 echo Booting PITS Non-Destructive Hook Hardware Inventory for {dev}\n\
                 set hook-url {base}/hook\n\
                 kernel ${{hook-url}}/vmlinuz-x86_64 console=tty0 console=ttyS0,115200n8 \\\n\
                   pits_device={dev} pits_report_url=https://10.89.20.1:18443/v1/pxe/inventory/{dev} \\\n\
                   pits_disk_mutation=forbidden initrd=initramfs-x86_64\n\
                 initrd ${{hook-url}}/initramfs-x86_64\n\
                 boot\n",
                nic_comment = nic_comment,
                dev = dev,
                base = base,
            )
        }
    }
}

/// Renders a Tinkerbell `tinkerbell.org/v1alpha1 Hardware` manifest (`tink-system`)
/// with `allowPXE: true`, `allowWorkflow: false` (diskless Talos default) or `true` (Hook inventory).
pub fn render_tinkerbell_hardware_yaml(
    node: &EnrolledNodeContract,
    mode: PxeBootMode,
    asset_http_base: Option<&str>,
) -> String {
    let ipxe_script = render_ipxe_script(node, mode, asset_http_base);
    let indented_script: String = ipxe_script
        .lines()
        .map(|l| format!("          {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    let allow_workflow = matches!(mode, PxeBootMode::HookInventory);

    format!(
        r#"apiVersion: tinkerbell.org/v1alpha1
kind: Hardware
metadata:
  name: {dev}
  namespace: tink-system
  labels:
    pits.io/device-id: {dev}
    pits.io/boot-mode: {mode_str}
spec:
  metadata:
    facility:
      facility_code: pits-lab-vlan20
    instance:
      id: "{uuid}"
      hostname: "{host}"
      operating_system:
        distro: talos
        version: "v1.14.5"
  disks: []
  interfaces:
    - dhcp:
        arch: x86_64
        hostname: {host}
        ip:
          address: {ip}
          gateway: 10.89.20.1
          netmask: 255.255.255.0
        lease_time: 86400
        mac: "{mac}"
        name_servers:
          - 10.89.20.1
        uefi: true
      netboot:
        allowPXE: true
        allowWorkflow: {allow_workflow}
        ipxe:
          contents: |
{indented_script}
"#,
        dev = node.device_id,
        mode_str = match mode {
            PxeBootMode::TalosNetboot => "talos-netboot",
            PxeBootMode::HookInventory => "hook-inventory",
        },
        uuid = node.product_uuid,
        host = node.hostname,
        ip = node.node_ip,
        mac = node.pxe_mac,
        allow_workflow = allow_workflow,
        indented_script = indented_script,
    )
}

/// Hardware PCI Neural Processing Unit (NPU) descriptor classified by exact
/// PCI vendor ID, device ID, and base PCI class (`0x12` Processing accelerators /
/// `0x1180` Signal processing controller).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedNpuHardware {
    pub vendor: String,
    pub vendor_id: String,
    pub device_id: String,
    pub pci_id: String,
    pub pci_address: String,
    pub pci_class: Option<String>,
    pub architecture: String,
    pub expected_kernel_driver: String,
    pub expected_device_class: String,
    pub bound_driver: Option<String>,
    pub driver_bound: bool,
}

/// Expected physical NPU derived from CPU microarchitecture / model string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CpuNpuExpectation {
    pub expected_present: bool,
    pub vendor: Option<String>,
    pub expected_pci_id: Option<String>,
    pub microarchitecture: String,
    pub summary: String,
}

/// Classifies a single PCI function into a physical NPU (`Intel AI Boost` or `AMD Ryzen AI XDNA`)
/// while strictly rejecting non-NPU devices with visually similar hex IDs (e.g. `8086:463e`
/// Thunderbolt 4 NHI `[0c03]` on Alder Lake `i5-12600H` or `8086:464f` GNA `[0880]`).
pub fn classify_pci_npu(
    pci_address: &str,
    vendor_raw: &str,
    device_raw: &str,
    class_raw: Option<&str>,
    bound_driver_raw: Option<&str>,
) -> Option<DetectedNpuHardware> {
    let vendor_u16 = parse_hex_u16(vendor_raw)?;
    let device_u16 = parse_hex_u16(device_raw)?;
    let base_class = class_raw.and_then(parse_pci_base_class);

    let (vendor, arch, expected_drv, expected_dc, valid_classes): (
        &str,
        &str,
        &str,
        &str,
        &[u8],
    ) = match (vendor_u16, device_u16) {
        // Intel AI Boost (VPU / NPU)
        (0x8086, 0x7d1d) => (
            "intel",
            "Intel AI Boost (Meteor Lake NPU)",
            "intel_vpu",
            "pits-intel-npu",
            &[0x12, 0x11],
        ),
        (0x8086, 0xad1d) => (
            "intel",
            "Intel AI Boost (Arrow Lake NPU)",
            "intel_vpu",
            "pits-intel-npu",
            &[0x12, 0x11],
        ),
        (0x8086, 0x643e) => (
            "intel",
            "Intel AI Boost (Lunar Lake NPU 4)",
            "intel_vpu",
            "pits-intel-npu",
            &[0x12, 0x11],
        ),
        (0x8086, 0xb03e) => (
            "intel",
            "Intel AI Boost (Panther Lake NPU 5)",
            "intel_vpu",
            "pits-intel-npu",
            &[0x12, 0x11],
        ),
        // AMD Ryzen AI (XDNA / IPU)
        (0x1022, 0x1502) => (
            "amd",
            "AMD Ryzen AI (Phoenix / Hawk Point XDNA 1 NPU)",
            "amdxdna",
            "pits-amd-npu",
            &[0x11, 0x12],
        ),
        (0x1022, 0x17f0) => (
            "amd",
            "AMD Ryzen AI (Strix Point / Krackan XDNA 2 NPU)",
            "amdxdna",
            "pits-amd-npu",
            &[0x11, 0x12],
        ),
        _ => return None,
    };

    if let Some(bc) = base_class {
        if !valid_classes.contains(&bc) {
            return None;
        }
    }

    let bound_driver = bound_driver_raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let driver_bound = bound_driver.is_some();

    Some(DetectedNpuHardware {
        vendor: vendor.to_string(),
        vendor_id: format!("{vendor_u16:04x}"),
        device_id: format!("{device_u16:04x}"),
        pci_id: format!("{vendor_u16:04x}:{device_u16:04x}"),
        pci_address: pci_address.to_string(),
        pci_class: class_raw
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        architecture: arch.to_string(),
        expected_kernel_driver: expected_drv.to_string(),
        expected_device_class: expected_dc.to_string(),
        bound_driver,
        driver_bound,
    })
}

/// Determines whether a given x86_64 CPU model integrates a physical NPU and which PCI ID is expected.
pub fn expected_npu_for_cpu(cpu_model: &str) -> CpuNpuExpectation {
    let lower = cpu_model.to_lowercase();
    if lower.contains("core(tm) ultra") || lower.contains("core ultra") {
        // Distinguish Series 1 (1xx -> Meteor Lake 7d1d) vs Series 2 (2xxV -> Lunar Lake 643e, 2xx -> Arrow Lake ad1d)
        if lower.contains(" 2")
            && (lower.ends_with('v') || lower.contains("256v") || lower.contains("258v") || lower.contains("268v") || lower.contains("288v"))
        {
            return CpuNpuExpectation {
                expected_present: true,
                vendor: Some("intel".to_string()),
                expected_pci_id: Some("8086:643e".to_string()),
                microarchitecture: "Intel Lunar Lake".to_string(),
                summary: "Intel AI Boost NPU 4 (8086:643e)".to_string(),
            };
        }
        if lower.contains(" 285")
            || lower.contains(" 265")
            || lower.contains(" 255")
            || lower.contains(" 245")
            || lower.contains(" 235")
            || lower.contains(" 225")
        {
            return CpuNpuExpectation {
                expected_present: true,
                vendor: Some("intel".to_string()),
                expected_pci_id: Some("8086:ad1d".to_string()),
                microarchitecture: "Intel Arrow Lake".to_string(),
                summary: "Intel AI Boost NPU (8086:ad1d)".to_string(),
            };
        }
        return CpuNpuExpectation {
            expected_present: true,
            vendor: Some("intel".to_string()),
            expected_pci_id: Some("8086:7d1d".to_string()),
            microarchitecture: "Intel Meteor Lake (Core Ultra Series 1)".to_string(),
            summary: "Intel AI Boost NPU (8086:7d1d)".to_string(),
        };
    }

    if lower.contains("ryzen ai") {
        return CpuNpuExpectation {
            expected_present: true,
            vendor: Some("amd".to_string()),
            expected_pci_id: Some("1022:17f0".to_string()),
            microarchitecture: "AMD Strix Point / Krackan (Zen 5)".to_string(),
            summary: "AMD Ryzen AI XDNA 2 NPU (1022:17f0)".to_string(),
        };
    }

    if lower.contains("ryzen")
        && (lower.contains("7840")
            || lower.contains("7940")
            || lower.contains("7640")
            || lower.contains("8845")
            || lower.contains("8840")
            || lower.contains("8945")
            || lower.contains("8645")
            || lower.contains("8640"))
    {
        return CpuNpuExpectation {
            expected_present: true,
            vendor: Some("amd".to_string()),
            expected_pci_id: Some("1022:1502".to_string()),
            microarchitecture: "AMD Phoenix / Hawk Point (Zen 4)".to_string(),
            summary: "AMD Ryzen AI XDNA 1 NPU (1022:1502)".to_string(),
        };
    }

    if lower.contains("12th gen intel")
        || lower.contains("13th gen intel")
        || lower.contains("14th gen intel")
        || lower.contains("i5-12")
        || lower.contains("i7-12")
        || lower.contains("i9-12")
        || lower.contains("i9-13")
    {
        return CpuNpuExpectation {
            expected_present: false,
            vendor: None,
            expected_pci_id: None,
            microarchitecture: "Intel Alder Lake / Raptor Lake (Pre-NPU)".to_string(),
            summary: "None (12th-14th Gen Core CPUs do not integrate an NPU)".to_string(),
        };
    }

    CpuNpuExpectation {
        expected_present: false,
        vendor: None,
        expected_pci_id: None,
        microarchitecture: "Standard x86_64".to_string(),
        summary: "None expected from CPU model string".to_string(),
    }
}

/// Resolves generic OEM DMI strings (`Venus series`, `AtomMan X Series`, `NUC14RVH-B`)
/// combined with board serial / SKU / CPU hints into exact retail hardware models.
pub fn resolve_retail_machine_model(
    manufacturer: Option<&str>,
    product_name: Option<&str>,
    sku_or_board: Option<&str>,
    cpu_model: Option<&str>,
) -> String {
    let mfg = manufacturer.unwrap_or("").trim();
    let prod = product_name.unwrap_or("").trim();
    let sku = sku_or_board.unwrap_or("").trim();
    let cpu = cpu_model.unwrap_or("").trim();

    if prod.eq_ignore_ascii_case("NUC14RVH-B") || sku.eq_ignore_ascii_case("NUC14RVH-B") {
        return "ASUS NUC 14 Pro (NUC14RVH-B)".to_string();
    }
    if prod.eq_ignore_ascii_case("AtomMan X Series")
        || sku.to_uppercase().starts_with("KTBAC")
        || sku.eq_ignore_ascii_case("MGMTBAC")
    {
        return "Minisforum AtomMan X7 Ti (AtomMan X Series / KTBAC)".to_string();
    }
    if prod.eq_ignore_ascii_case("Venus series") {
        let sku_up = sku.to_uppercase();
        if sku_up.starts_with("F7BSD") || cpu.to_lowercase().contains("7840hs") {
            return "Minisforum UM780 XTX (Venus series / F7BSD)".to_string();
        }
        if sku_up.contains("AHWSA") || cpu.to_lowercase().contains("12600h") || cpu.to_lowercase().contains("12900h") || cpu.to_lowercase().contains("13900h") {
            return "Minisforum MS-01 (Venus Series / AHWSA)".to_string();
        }
    }
    if !mfg.is_empty() && !prod.is_empty() {
        format!("{mfg} {prod}")
    } else if !prod.is_empty() {
        prod.to_string()
    } else {
        "Unknown x86_64 Machine".to_string()
    }
}

fn parse_hex_u16(raw: &str) -> Option<u16> {
    let trimmed = raw
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    if trimmed.is_empty() || trimmed.len() > 4 {
        return None;
    }
    u16::from_str_radix(trimmed, 16).ok()
}

fn parse_pci_base_class(raw: &str) -> Option<u8> {
    let trimmed = raw
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    if trimmed.len() >= 6 {
        // Full 24-bit sysfs class e.g. "120000" or "118000" -> top byte
        u8::from_str_radix(&trimmed[..2], 16).ok()
    } else if trimmed.len() == 4 || trimmed.len() == 2 {
        // 16-bit class+subclass ("1200") or 8-bit COSI class_id ("12")
        u8::from_str_radix(&trimmed[..2], 16).ok()
    } else {
        None
    }
}

/// Structured analysis of a Hook non-destructive hardware discovery JSON report.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryAnalysis {
    pub valid: bool,
    pub disk_mutation_forbid_verified: bool,
    pub dmi_product_uuid: Option<String>,
    pub system_vendor: Option<String>,
    pub product_name: Option<String>,
    pub board_serial: Option<String>,
    pub resolved_model: String,
    pub cpu_vendor: Option<String>,
    pub cpu_model: Option<String>,
    pub requires_intel_ucode: bool,
    pub requires_amd_ucode: bool,
    pub requires_realtek_firmware: bool,
    pub intel_npu_detected: bool,
    pub intel_npu_pci: Option<String>,
    pub amd_npu_detected: bool,
    pub amd_npu_pci: Option<String>,
    pub npus_detected: Vec<DetectedNpuHardware>,
    pub local_disks_observed: usize,
    pub isolated_iommu_pci_devices: Vec<String>,
    pub shared_iommu_pci_devices: Vec<String>,
    pub warnings: Vec<String>,
}

/// Analyzes a Hook inventory JSON payload (replacing `inventory.py` & `prepare-additional-nodes.py`).
pub fn analyze_hook_inventory(raw_json: &serde_json::Value) -> Result<InventoryAnalysis> {
    let schema = raw_json
        .get("schemaVersion")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if schema != 1 {
        bail!("unsupported Hook inventory schemaVersion {schema} (expected 1)");
    }

    let approved_mutation = raw_json
        .get("approvedForDiskMutation")
        .and_then(|v| v.as_bool());
    if approved_mutation != Some(false) {
        bail!("Hook inventory report must explicitly set approvedForDiskMutation=false");
    }

    let dmi_product_uuid = raw_json
        .pointer("/identity/productUUID")
        .or_else(|| raw_json.pointer("/dmi/productUUID"))
        .or_else(|| raw_json.get("productUUID"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase());
    let system_vendor = raw_json
        .pointer("/identity/systemVendor")
        .or_else(|| raw_json.pointer("/dmi/systemVendor"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let product_name = raw_json
        .pointer("/identity/productName")
        .or_else(|| raw_json.pointer("/dmi/productName"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let board_serial = raw_json
        .pointer("/identity/boardSerial")
        .or_else(|| raw_json.pointer("/dmi/boardSerial"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let cpu_vendor = raw_json
        .get("cpuVendor")
        .or_else(|| raw_json.pointer("/cpu/vendor"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let cpu_model = raw_json
        .pointer("/cpu/modelName")
        .or_else(|| raw_json.get("cpuModel"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let vendor_lower = cpu_vendor.as_deref().unwrap_or("").to_lowercase();
    let model_lower = cpu_model.as_deref().unwrap_or("").to_lowercase();
    let requires_amd_ucode = vendor_lower.contains("amd") || model_lower.contains("amd") || model_lower.contains("ryzen");
    let requires_intel_ucode = if requires_amd_ucode {
        false
    } else {
        vendor_lower.contains("intel") || model_lower.contains("intel")
    };

    let mut requires_realtek_firmware = false;
    let mut npus_detected = Vec::new();
    let mut iommu_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();

    if let Some(pci_list) = raw_json.get("pci").and_then(|v| v.as_array()) {
        for dev in pci_list {
            let addr = dev
                .get("address")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let vendor = dev
                .get("vendor")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let device = dev
                .get("device")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let class_str = dev.get("class").and_then(|v| v.as_str());
            let drv_str = dev.get("driver").and_then(|v| v.as_str());

            if parse_hex_u16(vendor) == Some(0x10ec) {
                requires_realtek_firmware = true;
            }
            if let Some(npu) = classify_pci_npu(&addr, vendor, device, class_str, drv_str) {
                npus_detected.push(npu);
            }
            if let Some(grp) = dev.get("iommuGroup").and_then(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_i64().map(|n| n.to_string()))
            }) {
                let grp_trimmed = grp.trim();
                if !addr.is_empty() && !grp_trimmed.is_empty() {
                    iommu_groups
                        .entry(grp_trimmed.to_string())
                        .or_default()
                        .push(addr);
                }
            }
        }
    }

    let intel_npu_pci = npus_detected
        .iter()
        .find(|n| n.vendor == "intel")
        .map(|n| n.pci_address.clone());
    let intel_npu_detected = intel_npu_pci.is_some();
    let amd_npu_pci = npus_detected
        .iter()
        .find(|n| n.vendor == "amd")
        .map(|n| n.pci_address.clone());
    let amd_npu_detected = amd_npu_pci.is_some();

    let mut isolated_iommu_pci_devices = BTreeSet::new();
    let mut shared_iommu_pci_devices = BTreeSet::new();
    for members in iommu_groups.values() {
        if members.len() == 1 {
            isolated_iommu_pci_devices.insert(members[0].clone());
        } else {
            for m in members {
                shared_iommu_pci_devices.insert(m.clone());
            }
        }
    }

    let local_disks_observed = if let Some(disks) = raw_json.get("disks").and_then(|v| v.as_array())
    {
        disks
            .iter()
            .filter(|d| {
                let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let bytes = d.get("bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                !name.starts_with("nbd") && !name.starts_with("loop") && bytes > 0
            })
            .count()
    } else {
        raw_json
            .get("blockDevices")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0)
    };

    let mut warnings = Vec::new();
    if local_disks_observed > 0 {
        warnings.push(format!(
            "{local_disks_observed} non-ramdisk block device(s) present — netboot-guard strictly forbids touching them"
        ));
    }
    for npu in &npus_detected {
        if npu.vendor == "amd" && !npu.driver_bound {
            warnings.push(format!(
                "Physical {} ({} @ {}) detected with unbound kernel driver (Talos kernel lacks CONFIG_DRM_ACCEL_AMDXDNA)",
                npu.architecture, npu.pci_id, npu.pci_address
            ));
        }
    }

    let resolved_model = resolve_retail_machine_model(
        system_vendor.as_deref(),
        product_name.as_deref(),
        board_serial.as_deref(),
        cpu_model.as_deref(),
    );

    Ok(InventoryAnalysis {
        valid: true,
        disk_mutation_forbid_verified: true,
        dmi_product_uuid,
        system_vendor,
        product_name,
        board_serial,
        resolved_model,
        cpu_vendor,
        cpu_model,
        requires_intel_ucode,
        requires_amd_ucode,
        requires_realtek_firmware,
        intel_npu_detected,
        intel_npu_pci,
        amd_npu_detected,
        amd_npu_pci,
        npus_detected,
        local_disks_observed,
        isolated_iommu_pci_devices: isolated_iommu_pci_devices.into_iter().collect(),
        shared_iommu_pci_devices: shared_iommu_pci_devices.into_iter().collect(),
        warnings,
    })
}

/// Applies a Tinkerbell `Hardware` object to `pits-core` (`tink-system`).
pub async fn apply_tinkerbell_hardware(
    core_k8s: &KubeClient,
    node: &EnrolledNodeContract,
    mode: PxeBootMode,
    asset_http_base: Option<&str>,
) -> Result<serde_json::Value> {
    let yaml = render_tinkerbell_hardware_yaml(node, mode, asset_http_base);
    let path = format!(
        "/apis/tinkerbell.org/v1alpha1/namespaces/tink-system/hardware/{}",
        node.device_id
    );
    core_k8s
        .server_side_apply(&path, "pitsctl-pxe", yaml.as_bytes(), true)
        .await
}

/// Lists Tinkerbell `Hardware` CRs in `tink-system` on `pits-core`.
pub async fn list_tinkerbell_hardware(core_k8s: &KubeClient) -> Result<serde_json::Value> {
    core_k8s
        .get_json("/apis/tinkerbell.org/v1alpha1/namespaces/tink-system/hardware")
        .await
}

/// Lists Tinkerbell `Workflow` CRs in `tink-system` on `pits-core`.
pub async fn list_tinkerbell_workflows(core_k8s: &KubeClient) -> Result<serde_json::Value> {
    core_k8s
        .get_json("/apis/tinkerbell.org/v1alpha1/namespaces/tink-system/workflows")
        .await
}

/// Lists Hook hardware inventory reports stored on `pits-core` via `pits-agent`.
pub async fn pxe_list_inventory(
    agent: &AgentClient,
) -> Result<BTreeMap<String, serde_json::Value>> {
    agent.get_json("/v1/pxe/inventory").await
}

/// Verifies staged `vmlinuz` and `initramfs.xz` SHA-256 digests on `pits-core` via `pits-agent`.
pub async fn pxe_verify_assets(
    agent: &AgentClient,
    device_id: &str,
    expected_checksums: BTreeMap<String, String>,
) -> Result<serde_json::Value> {
    let req = serde_json::json!({
        "deviceID": device_id,
        "checksums": expected_checksums,
    });
    agent.post_json("/v1/pxe/assets/verify", &req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WorkspaceBundle;

    #[test]
    fn test_ipxe_minisforum_and_hook_inventory_analysis() {
        let bundle = WorkspaceBundle::builtin_pits_baseline();
        let node4 = bundle.find_node("netboot-004").unwrap();
        let script = render_ipxe_script(node4, PxeBootMode::TalosNetboot, None);
        assert!(script.contains("Minisforum UEFI direct-iPXE OUI"));
        assert!(script.contains("hugepages=1024"));
        assert!(script.contains("selinux=1 module.sig_enforce=1"));

        let inv = serde_json::json!({
            "schemaVersion": 1,
            "approvedForDiskMutation": false,
            "dmi": { "productUUID": "c39be600-2d37-11ef-9418-3174c88d7400" },
            "cpu": { "modelName": "Intel(R) Core(TM) Ultra 9 185H" },
            "pci": [
                { "address": "0000:01:00.0", "vendor": "0x10de", "device": "0x2800", "iommuGroup": "14" },
                { "address": "0000:00:0b.0", "vendor": "0x8086", "device": "0x7d1d", "iommuGroup": "4" },
                { "address": "0000:58:00.0", "vendor": "0x10ec", "device": "0x8125", "iommuGroup": "19" }
            ],
            "blockDevices": [{ "name": "nvme0n1" }]
        });
        let analysis = analyze_hook_inventory(&inv).unwrap();
        assert!(analysis.valid);
        assert!(analysis.requires_realtek_firmware);
        assert!(analysis.intel_npu_detected);
        assert!(!analysis.amd_npu_detected);
        assert!(analysis
            .isolated_iommu_pci_devices
            .contains(&"0000:01:00.0".to_string()));
    }

    #[test]
    fn test_amd_xdna_inventory_and_alder_lake_thunderbolt_rejection() {
        // 1. pits-core: Alder Lake i5-12600H with 8086:463e Thunderbolt 4 NHI [0c03] -> NO NPU!
        assert!(classify_pci_npu("0000:00:0d.2", "0x8086", "0x463e", Some("0x0c0300"), None).is_none());
        // Even if Lunar Lake device ID 643e appeared under USB class 0x0c, class guard rejects it:
        assert!(classify_pci_npu("0000:00:0d.2", "0x8086", "0x643e", Some("0x0c0300"), None).is_none());
        let core_exp = expected_npu_for_cpu("12th Gen Intel(R) Core(TM) i5-12600H");
        assert!(!core_exp.expected_present);

        // 2. netboot-003: Real Hook inventory schema (Minisforum UM780 XTX / F7BSD, AuthenticAMD, 1022:1502 @ 0000:c6:00.1)
        let inv_003 = serde_json::json!({
            "schemaVersion": 1,
            "approvedForDiskMutation": false,
            "cpuVendor": "AuthenticAMD",
            "identity": {
                "biosVersion": "1.06",
                "boardSerial": "F7BSD784043A1022505",
                "productName": "Venus series",
                "productUUID": "ed059400-7d9c-11ee-a72c-de277c485e00",
                "systemVendor": "Micro Computer (HK) Tech Limited"
            },
            "disks": [
                { "name": "nbd0", "bytes": 0 },
                { "name": "nvme0n1", "bytes": 1000204886016u64 }
            ],
            "pci": [
                { "address": "0000:00:00.0", "class": "0x060000", "vendor": "0x1022", "device": "0x14e8", "driver": "", "iommuGroup": "" },
                { "address": "0000:01:00.0", "class": "0x010802", "vendor": "0x144d", "device": "0xa809", "driver": "nvme", "iommuGroup": "14" },
                { "address": "0000:03:00.0", "class": "0x020000", "vendor": "0x10ec", "device": "0x8125", "driver": "r8169", "iommuGroup": "16" },
                { "address": "0000:c6:00.1", "class": "0x118000", "vendor": "0x1022", "device": "0x1502", "driver": "", "iommuGroup": "22" }
            ]
        });
        let rep_003 = analyze_hook_inventory(&inv_003).unwrap();
        assert_eq!(rep_003.resolved_model, "Minisforum UM780 XTX (Venus series / F7BSD)");
        assert!(!rep_003.requires_intel_ucode);
        assert!(rep_003.requires_amd_ucode);
        assert!(rep_003.requires_realtek_firmware);
        assert!(!rep_003.intel_npu_detected);
        assert!(rep_003.amd_npu_detected);
        assert_eq!(rep_003.amd_npu_pci.as_deref(), Some("0000:c6:00.1"));
        assert_eq!(rep_003.npus_detected.len(), 1);
        assert_eq!(rep_003.npus_detected[0].pci_id, "1022:1502");
        assert_eq!(rep_003.npus_detected[0].expected_kernel_driver, "amdxdna");
        assert!(!rep_003.npus_detected[0].driver_bound);
        assert_eq!(rep_003.local_disks_observed, 1);
        assert!(!rep_003.shared_iommu_pci_devices.contains(&"0000:00:00.0".to_string()));

        let amd_exp = expected_npu_for_cpu("AMD Ryzen 7 7840HS w/ Radeon 780M Graphics");
        assert!(amd_exp.expected_present);
        assert_eq!(amd_exp.expected_pci_id.as_deref(), Some("1022:1502"));
    }
}
