//! Dual-Track (`Static Policy × Dynamic Change`) Summary Builder & In-Memory RingBuffer
//! Delta Engine (`TelemetryHistory`) for all 16 PITS Resource Types.

use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};

use crate::catalog::model::{
    DynamicSignal, EntitySummary, ObservationMode, PitsEntityDetail, SummaryPill,
};
use crate::catalog::synthesizer::LiveTelemetry;
use crate::config::WorkspaceBundle;
use crate::k8s::{
    compute_cluster_gpu_occupancy, compute_cluster_npu_occupancy, ClusterControlPlaneSummary,
};
use crate::router::{MGMT0_PCI, MAX_VERIFICATION_AGE_SECS};
use crate::san::EXPECTED_BTRFS_UUID;

pub const MAX_RING_SAMPLES: usize = 20;

/// Returns the user-approved [`ObservationMode`] for any of the 16 PITS Resource Types.
pub fn observation_mode_for_type(resource_type: &str) -> ObservationMode {
    match resource_type {
        "pxe-boot-profile" | "auth-flow-bundle" => ObservationMode::PolicyFirst,
        "baremetal-host" | "npu-accelerator" | "san-volume" | "ai-storage-workspace" => {
            ObservationMode::Hybrid
        }
        "gpu-accelerator"
        | "virtual-machine"
        | "edge-router"
        | "storage-pool"
        | "kubernetes-cluster"
        | "cicd-pipeline" => ObservationMode::DynamicFirst,
        "vendor-saas" | "commercial-sdk" | "managed-infra" | "outsourced-dev" => {
            ObservationMode::GovernanceEvent
        }
        _ => ObservationMode::PolicyFirst,
    }
}

#[derive(Debug, Clone)]
pub struct VppCounterSample {
    pub sampled_at: DateTime<Utc>,
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub drops: u64,
    pub errors: u64,
}

#[derive(Debug, Clone)]
pub struct VppCounterDelta {
    pub elapsed_secs: f64,
    pub rx_mbps: f64,
    pub tx_mbps: f64,
    pub rx_pps: f64,
    pub tx_pps: f64,
    pub delta_drops: i64,
    pub delta_errors: i64,
    pub total_drops: u64,
    pub total_errors: u64,
    pub counter_reset: bool,
}

#[derive(Debug, Clone)]
pub struct BtrfsSample {
    pub sampled_at: DateTime<Utc>,
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub total_errors: u64,
}

#[derive(Debug, Clone)]
pub struct BtrfsDelta {
    pub elapsed_secs: f64,
    pub delta_used_bytes: i64,
    pub delta_errors: i64,
}

#[derive(Debug, Clone)]
pub struct ClusterSample {
    pub sampled_at: DateTime<Utc>,
    pub ready_nodes: usize,
    pub total_nodes: usize,
    pub running_pods: usize,
    pub total_pods: usize,
    pub allocated_gpus: usize,
    pub total_gpus: usize,
    pub allocated_npus: usize,
}

#[derive(Debug, Clone)]
pub struct ClusterDelta {
    pub delta_ready_nodes: i64,
    pub delta_running_pods: i64,
    pub delta_allocated_gpus: i64,
    pub delta_allocated_npus: i64,
}

#[derive(Debug, Clone)]
struct EntityChangeState {
    fingerprint: String,
    last_changed_at: DateTime<Utc>,
}

/// Process-local in-memory RingBuffer (`MAX_RING_SAMPLES = 20`) tracking consecutive telemetry
/// samples to compute rates (`Mbps`/`pps`), counter deltas (`Δ`), and `last_changed_at`.
#[derive(Debug, Clone, Default)]
pub struct TelemetryHistory {
    vpp_samples: VecDeque<VppCounterSample>,
    btrfs_samples: VecDeque<BtrfsSample>,
    cluster_samples: BTreeMap<String, VecDeque<ClusterSample>>,
    sourcing_deps: BTreeMap<String, VecDeque<(DateTime<Utc>, usize)>>,
    entity_changes: BTreeMap<String, EntityChangeState>,
}

fn push_ring<T>(buf: &mut VecDeque<T>, item: T) {
    if buf.len() >= MAX_RING_SAMPLES {
        buf.pop_front();
    }
    buf.push_back(item);
}

fn extract_vpp_sample(vpp_report: &Value, fallback_now: DateTime<Utc>) -> Option<VppCounterSample> {
    if let Some(c) = vpp_report.get("counters") {
        let sampled_at = c
            .get("sampledAt")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or(fallback_now);
        return Some(VppCounterSample {
            sampled_at,
            rx_packets: c.get("totalRxPackets").and_then(|v| v.as_u64()).unwrap_or(0),
            rx_bytes: c.get("totalRxBytes").and_then(|v| v.as_u64()).unwrap_or(0),
            tx_packets: c.get("totalTxPackets").and_then(|v| v.as_u64()).unwrap_or(0),
            tx_bytes: c.get("totalTxBytes").and_then(|v| v.as_u64()).unwrap_or(0),
            drops: c.get("totalDrops").and_then(|v| v.as_u64()).unwrap_or(0),
            errors: c.get("totalErrors").and_then(|v| v.as_u64()).unwrap_or(0),
        });
    }
    let ifaces = vpp_report.get("interfaces").and_then(|v| v.as_array())?;
    let mut has_any = false;
    let mut sample = VppCounterSample {
        sampled_at: fallback_now,
        rx_packets: 0,
        rx_bytes: 0,
        tx_packets: 0,
        tx_bytes: 0,
        drops: 0,
        errors: 0,
    };
    for iface in ifaces {
        let rxp = iface.get("rxPackets").and_then(|v| v.as_u64()).unwrap_or(0);
        let rxb = iface.get("rxBytes").and_then(|v| v.as_u64()).unwrap_or(0);
        let txp = iface.get("txPackets").and_then(|v| v.as_u64()).unwrap_or(0);
        let txb = iface.get("txBytes").and_then(|v| v.as_u64()).unwrap_or(0);
        let drp = iface.get("drops").and_then(|v| v.as_u64()).unwrap_or(0);
        let rxe = iface.get("rxErrors").and_then(|v| v.as_u64()).unwrap_or(0);
        let txe = iface.get("txErrors").and_then(|v| v.as_u64()).unwrap_or(0);
        if rxp > 0 || rxb > 0 || txp > 0 || txb > 0 || drp > 0 || rxe > 0 || txe > 0 {
            has_any = true;
        }
        sample.rx_packets += rxp;
        sample.rx_bytes += rxb;
        sample.tx_packets += txp;
        sample.tx_bytes += txb;
        sample.drops += drp;
        sample.errors += rxe + txe;
    }
    has_any.then_some(sample)
}

fn extract_btrfs_error_total(btrfs: &Value) -> u64 {
    let mut sum = 0u64;
    if let Some(devs) = btrfs.get("devices").and_then(|v| v.as_array()) {
        for d in devs {
            for key in [
                "readErrors",
                "writeErrors",
                "flushErrors",
                "corruptionErrors",
                "generationErrors",
            ] {
                sum += d.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
            }
        }
    }
    if let Some(stats) = btrfs.get("errorStats").and_then(|v| v.as_object()) {
        for v in stats.values() {
            sum += v.as_u64().unwrap_or(0);
        }
    }
    for key in [
        "readErrors",
        "writeErrors",
        "flushErrors",
        "corruptionErrors",
        "generationErrors",
    ] {
        sum += btrfs.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    }
    sum
}

impl TelemetryHistory {
    pub fn record_vpp_sample(&mut self, sample: VppCounterSample) {
        if let Some(last) = self.vpp_samples.back() {
            if sample.sampled_at <= last.sampled_at {
                return;
            }
        }
        push_ring(&mut self.vpp_samples, sample);
    }

    pub fn record_btrfs_sample(&mut self, sample: BtrfsSample) {
        if let Some(last) = self.btrfs_samples.back() {
            if sample.sampled_at <= last.sampled_at {
                return;
            }
        }
        push_ring(&mut self.btrfs_samples, sample);
    }

    pub fn record_cluster_sample(&mut self, cluster_slug: &str, sample: ClusterSample) {
        let entry = self
            .cluster_samples
            .entry(cluster_slug.to_string())
            .or_default();
        if let Some(last) = entry.back() {
            if sample.sampled_at <= last.sampled_at {
                return;
            }
        }
        push_ring(entry, sample);
    }

    pub fn record_sourcing_deps(&mut self, unit_key: &str, deps: usize, now: DateTime<Utc>) {
        let entry = self
            .sourcing_deps
            .entry(unit_key.to_string())
            .or_default();
        if let Some((last_ts, _)) = entry.back() {
            if now <= *last_ts {
                return;
            }
        }
        push_ring(entry, (now, deps));
    }

    /// Records all numeric counters from a live telemetry pass into the RingBuffers.
    pub(crate) fn record_live_telemetry(&mut self, live: &LiveTelemetry, now: DateTime<Utc>) {
        if let Some(vpp) = live
            .router_live
            .as_ref()
            .and_then(|r| r.vpp_report.as_ref())
        {
            if let Some(sample) = extract_vpp_sample(vpp, now) {
                self.record_vpp_sample(sample);
            }
        }

        if let Some(btrfs) = live.btrfs.as_ref() {
            if let (Some(used_bytes), Some(total_bytes)) = (
                btrfs.get("usedBytes").and_then(|v| v.as_u64()),
                btrfs.get("totalBytes").and_then(|v| v.as_u64()),
            ) {
                let total_errors = extract_btrfs_error_total(btrfs);
                self.record_btrfs_sample(BtrfsSample {
                    sampled_at: now,
                    used_bytes,
                    total_bytes,
                    total_errors,
                });
            }
        }

        let mut clusters: Vec<(String, &ClusterControlPlaneSummary)> = Vec::new();
        if let Some(ref c) = live.core_cluster {
            if c.reachable {
                clusters.push(("cluster-pits-core".to_string(), c));
            }
        }
        if let Some(ref c) = live.lab_cluster {
            if c.reachable {
                clusters.push(("cluster-pits-lab".to_string(), c));
            }
        }
        for ex in &live.extra_clusters {
            if ex.summary.reachable {
                clusters.push((
                    crate::catalog::discovery::sanitize_k8s_slug(&ex.config.name),
                    &ex.summary,
                ));
            }
        }

        for (slug, cp) in clusters {
            let gpu_occ = compute_cluster_gpu_occupancy(cp);
            let npu_occ = compute_cluster_npu_occupancy(cp);
            let ready_nodes = cp.nodes.iter().filter(|n| n.ready).count();
            self.record_cluster_sample(
                &slug,
                ClusterSample {
                    sampled_at: now,
                    ready_nodes,
                    total_nodes: cp.nodes.len(),
                    running_pods: cp.running_pods,
                    total_pods: cp.total_pods,
                    allocated_gpus: gpu_occ.allocated,
                    total_gpus: gpu_occ.total,
                    allocated_npus: npu_occ.allocated,
                },
            );
        }
    }

    pub fn vpp_delta(&self) -> Option<VppCounterDelta> {
        if self.vpp_samples.len() < 2 {
            return None;
        }
        let curr = self.vpp_samples.back()?;
        let prev = self.vpp_samples.get(self.vpp_samples.len() - 2)?;
        let elapsed_ms = (curr.sampled_at - prev.sampled_at).num_milliseconds();
        if elapsed_ms <= 0 {
            return None;
        }
        let elapsed_secs = (elapsed_ms as f64) / 1000.0;
        let counter_reset = curr.rx_bytes < prev.rx_bytes
            || curr.tx_bytes < prev.tx_bytes
            || curr.rx_packets < prev.rx_packets
            || curr.tx_packets < prev.tx_packets;
        if counter_reset {
            return Some(VppCounterDelta {
                elapsed_secs,
                rx_mbps: 0.0,
                tx_mbps: 0.0,
                rx_pps: 0.0,
                tx_pps: 0.0,
                delta_drops: 0,
                delta_errors: 0,
                total_drops: curr.drops,
                total_errors: curr.errors,
                counter_reset: true,
            });
        }
        let d_rx_bytes = curr.rx_bytes.saturating_sub(prev.rx_bytes) as f64;
        let d_tx_bytes = curr.tx_bytes.saturating_sub(prev.tx_bytes) as f64;
        let d_rx_pkts = curr.rx_packets.saturating_sub(prev.rx_packets) as f64;
        let d_tx_pkts = curr.tx_packets.saturating_sub(prev.tx_packets) as f64;
        let delta_drops = (curr.drops as i128 - prev.drops as i128) as i64;
        let delta_errors = (curr.errors as i128 - prev.errors as i128) as i64;
        Some(VppCounterDelta {
            elapsed_secs,
            rx_mbps: (d_rx_bytes * 8.0) / elapsed_secs / 1_000_000.0,
            tx_mbps: (d_tx_bytes * 8.0) / elapsed_secs / 1_000_000.0,
            rx_pps: d_rx_pkts / elapsed_secs,
            tx_pps: d_tx_pkts / elapsed_secs,
            delta_drops,
            delta_errors,
            total_drops: curr.drops,
            total_errors: curr.errors,
            counter_reset: false,
        })
    }

    pub fn latest_vpp_sample(&self) -> Option<&VppCounterSample> {
        self.vpp_samples.back()
    }

    pub fn btrfs_delta(&self) -> Option<BtrfsDelta> {
        if self.btrfs_samples.len() < 2 {
            return None;
        }
        let curr = self.btrfs_samples.back()?;
        let prev = self.btrfs_samples.get(self.btrfs_samples.len() - 2)?;
        let elapsed_ms = (curr.sampled_at - prev.sampled_at).num_milliseconds();
        if elapsed_ms <= 0 {
            return None;
        }
        Some(BtrfsDelta {
            elapsed_secs: (elapsed_ms as f64) / 1000.0,
            delta_used_bytes: (curr.used_bytes as i128 - prev.used_bytes as i128) as i64,
            delta_errors: (curr.total_errors as i128 - prev.total_errors as i128) as i64,
        })
    }

    pub fn cluster_delta(&self, cluster_slug: &str) -> Option<ClusterDelta> {
        let buf = self.cluster_samples.get(cluster_slug)?;
        if buf.len() < 2 {
            return None;
        }
        let curr = buf.back()?;
        let prev = buf.get(buf.len() - 2)?;
        Some(ClusterDelta {
            delta_ready_nodes: curr.ready_nodes as i64 - prev.ready_nodes as i64,
            delta_running_pods: curr.running_pods as i64 - prev.running_pods as i64,
            delta_allocated_gpus: curr.allocated_gpus as i64 - prev.allocated_gpus as i64,
            delta_allocated_npus: curr.allocated_npus as i64 - prev.allocated_npus as i64,
        })
    }

    pub fn sourcing_deps_delta(&self, unit_key: &str) -> Option<i64> {
        let buf = self.sourcing_deps.get(unit_key)?;
        if buf.len() < 2 {
            return None;
        }
        let (_, curr) = buf.back()?;
        let (_, prev) = buf.get(buf.len() - 2)?;
        Some(*curr as i64 - *prev as i64)
    }

    pub fn vpp_sample_count(&self) -> usize {
        self.vpp_samples.len()
    }

    /// Tracks state-change fingerprints per entity key and returns `(last_changed_at, changed_now)`.
    pub fn track_entity_change(
        &mut self,
        entity_key: &str,
        fingerprint: String,
        now: DateTime<Utc>,
    ) -> (DateTime<Utc>, bool) {
        if let Some(prev) = self.entity_changes.get_mut(entity_key) {
            if prev.fingerprint != fingerprint {
                prev.fingerprint = fingerprint;
                prev.last_changed_at = now;
                return (now, true);
            }
            return (prev.last_changed_at, false);
        }
        self.entity_changes.insert(
            entity_key.to_string(),
            EntityChangeState {
                fingerprint,
                last_changed_at: now,
            },
        );
        (now, false)
    }
}

fn pill(key: &str, label: &str, value: impl Into<String>, status: Option<&str>) -> SummaryPill {
    SummaryPill {
        key: key.to_string(),
        label: label.to_string(),
        value: value.into(),
        status: status.map(str::to_string),
    }
}

fn sig(
    key: &str,
    label: &str,
    value: impl Into<String>,
    delta: Option<String>,
    trend: Option<&str>,
    observed_age_secs: Option<i64>,
    status: Option<&str>,
) -> DynamicSignal {
    DynamicSignal {
        key: key.to_string(),
        label: label.to_string(),
        value: value.into(),
        delta,
        trend: trend.map(str::to_string),
        observed_age_secs,
        status: status.map(str::to_string),
    }
}

fn format_signed_int(delta: i64) -> String {
    if delta > 0 {
        format!("+{delta}")
    } else {
        delta.to_string()
    }
}

fn trend_from_i64(delta: i64) -> &'static str {
    if delta > 0 {
        "up"
    } else if delta < 0 {
        "down"
    } else {
        "flat"
    }
}

fn format_signed_bytes(delta_bytes: i64) -> String {
    let sign = if delta_bytes > 0 {
        "+"
    } else if delta_bytes < 0 {
        "-"
    } else {
        ""
    };
    let abs = delta_bytes.unsigned_abs() as f64;
    if abs >= 1_073_741_824.0 {
        format!("{sign}{:.2} GiB", abs / 1_073_741_824.0)
    } else if abs >= 1_048_576.0 {
        format!("{sign}{:.1} MiB", abs / 1_048_576.0)
    } else if abs >= 1024.0 {
        format!("{sign}{:.1} KiB", abs / 1024.0)
    } else {
        format!("{sign}{:.0} B", abs)
    }
}

fn format_bytes_human(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1_073_741_824.0 {
        format!("{:.2} GiB", b / 1_073_741_824.0)
    } else if b >= 1_048_576.0 {
        format!("{:.1} MiB", b / 1_048_576.0)
    } else if b >= 1024.0 {
        format!("{:.1} KiB", b / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn short_hash(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn spec_str<'a>(detail: &'a PitsEntityDetail, key: &str) -> &'a str {
    detail
        .entity
        .spec
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("-")
}

/// Populates dual-track [`EntitySummary`] on every entity in `details` using live telemetry
/// and the in-memory [`TelemetryHistory`] RingBuffer.
pub(crate) fn populate_dual_track_summaries(
    details: &mut BTreeMap<String, PitsEntityDetail>,
    bundle: &WorkspaceBundle,
    live: &LiveTelemetry,
    history: &mut TelemetryHistory,
    now: DateTime<Utc>,
) {
    let keys: Vec<String> = details.keys().cloned().collect();
    for key in keys {
        let Some(detail) = details.get_mut(&key) else {
            continue;
        };

        let summary = if detail.entity.kind != "Resource" {
            build_non_resource_summary(detail, now)
        } else {
            match detail.resource_type.as_str() {
                "baremetal-host" => build_baremetal_summary(&key, detail, bundle, live, history, now),
                "npu-accelerator" => build_npu_summary(&key, detail, live, history, now),
                "gpu-accelerator" => build_gpu_summary(&key, detail, history, now),
                "virtual-machine" => build_vm_summary(&key, detail, history, now),
                "edge-router" => build_edge_router_summary(&key, detail, live, history, now),
                "pxe-boot-profile" => build_pxe_summary(&key, detail, bundle, live, history, now),
                "storage-pool" => build_storage_pool_summary(&key, detail, live, history, now),
                "san-volume" => build_san_volume_summary(&key, detail, bundle, live, history, now),
                "ai-storage-workspace" => {
                    build_ai_storage_summary(&key, detail, history, now)
                }
                "kubernetes-cluster" => {
                    build_cluster_summary(&key, detail, live, history, now)
                }
                "cicd-pipeline" => build_cicd_summary(&key, detail, live, history, now),
                "auth-flow-bundle" => build_auth_flow_summary(&key, detail, history, now),
                "vendor-saas" | "commercial-sdk" | "managed-infra" | "outsourced-dev" => {
                    build_sourcing_summary(&key, detail, history, now)
                }
                _ => build_non_resource_summary(detail, now),
            }
        };

        detail.entity.metadata.annotations.insert(
            "pits.io/observation-mode".to_string(),
            summary.observation_mode.as_str().to_string(),
        );
        detail.entity.metadata.annotations.insert(
            "pits.io/drift-detected".to_string(),
            summary.drift_detected.to_string(),
        );
        detail.summary = summary;
    }
}

fn build_non_resource_summary(detail: &PitsEntityDetail, now: DateTime<Utc>) -> EntitySummary {
    let mode = ObservationMode::PolicyFirst;
    let owner = detail
        .entity
        .spec
        .get("owner")
        .and_then(|v| v.as_str())
        .unwrap_or("group:default/operators");
    EntitySummary {
        observation_mode: mode,
        cadence_tier: mode.cadence_tier().to_string(),
        headline: detail.summary_line.clone(),
        static_policy: vec![
            pill("kind", "Catalog Kind", &detail.entity.kind, Some("info")),
            pill("owner", "Owner", owner, Some("ok")),
        ],
        dynamic_signals: vec![sig(
            "relations",
            "Graph Relations",
            detail.entity.relations.len().to_string(),
            None,
            Some("flat"),
            Some(0),
            Some("info"),
        )],
        drift_detected: false,
        drift_summary: None,
        last_changed_at: Some(now),
    }
}

fn build_baremetal_summary(
    key: &str,
    detail: &PitsEntityDetail,
    bundle: &WorkspaceBundle,
    live: &LiveTelemetry,
    history: &mut TelemetryHistory,
    now: DateTime<Utc>,
) -> EntitySummary {
    let mode = ObservationMode::Hybrid;
    let name = &detail.entity.metadata.name;
    let mut static_policy = Vec::new();
    let mut dynamic_signals = Vec::new();
    let mut drift_reasons = Vec::new();

    if name == "pits-core" {
        static_policy.push(pill("mgmt0Guard", "mgmt0 Guard", format!("{MGMT0_PCI} Protected"), Some("ok")));
        static_policy.push(pill("osContract", "OS Contract", "NixOS Flake + RKE2", Some("ok")));
        static_policy.push(pill("vfioScope", "DPDK PCIs", "5b:00.0 / 03:02.0", Some("info")));

        let k8s_ready = live
            .core_cluster
            .as_ref()
            .and_then(|c| c.nodes.iter().find(|n| n.name == "pits-core"))
            .map(|n| n.ready);
        let k8s_label = match k8s_ready {
            Some(true) => "Ready",
            Some(false) => {
                drift_reasons.push("pits-core RKE2 node is NotReady".to_string());
                "NotReady"
            }
            None => "Baseline",
        };
        dynamic_signals.push(sig(
            "k8sReady",
            "K8s Node",
            k8s_label,
            None,
            Some("flat"),
            Some(0),
            Some(if k8s_ready == Some(false) { "warn" } else { "ok" }),
        ));
        let agent_ok = live.agent_health.is_some();
        dynamic_signals.push(sig(
            "pitsAgent",
            "pits-agent",
            if agent_ok { "Online (mTLS)" } else { "Offline" },
            None,
            Some("flat"),
            Some(0),
            Some(if agent_ok { "ok" } else { "info" }),
        ));

        let fp = format!("pits-core|{k8s_label}|{agent_ok}");
        let (last_changed_at, _) = history.track_entity_change(key, fp, now);
        let drift_detected = !drift_reasons.is_empty();
        return EntitySummary {
            observation_mode: mode,
            cadence_tier: mode.cadence_tier().to_string(),
            headline: detail.summary_line.clone(),
            static_policy,
            dynamic_signals,
            drift_detected,
            drift_summary: drift_detected.then(|| drift_reasons.join("; ")),
            last_changed_at: Some(last_changed_at),
        };
    }

    if let Some(node) = bundle.nodes.iter().find(|n| n.device_id == *name) {
        let expected_wwids = crate::talos::allowed_wwids_for_node(node);
        static_policy.push(pill("netbootGuard", "netboot-guard", "Diskless Enforced", Some("ok")));
        static_policy.push(pill(
            "vfioPci",
            "VFIO Isolated",
            if node.approved_vfio_pci.is_empty() {
                "None".to_string()
            } else {
                format!("{} PCI(s)", node.approved_vfio_pci.len())
            },
            Some("ok"),
        ));
        static_policy.push(pill(
            "contractWwids",
            "Contract WWIDs",
            format!("{} SAN WWIDs", expected_wwids.len()),
            Some("info"),
        ));

        let k8s_node = live
            .lab_cluster
            .as_ref()
            .and_then(|c| c.nodes.iter().find(|n| n.name == node.device_id));
        let k8s_str = match k8s_node {
            Some(n) if n.ready => "Ready",
            Some(_) => {
                drift_reasons.push(format!("K8s node {} NotReady", node.device_id));
                "NotReady"
            }
            None if live.any_connected => {
                drift_reasons.push(format!("K8s node {} missing from Lab API", node.device_id));
                "Unreachable"
            }
            None => "Baseline",
        };
        dynamic_signals.push(sig(
            "k8sReady",
            "K8s Ready",
            k8s_str,
            None,
            Some("flat"),
            Some(0),
            Some(if matches!(k8s_str, "NotReady" | "Unreachable") { "warn" } else { "ok" }),
        ));

        let talos_st = live.talos_by_device.get(&node.device_id);
        let svc_str = match talos_st.filter(|t| t.reachable) {
            Some(t) => {
                let ok = t.services.iter().filter(|s| s.state == "Running").count();
                let tot = t.services.len();
                if tot > 0 && ok < tot {
                    drift_reasons.push(format!("Talos services degraded ({ok}/{tot})"));
                }
                format!("{ok}/{tot} Running")
            }
            None => "Baseline".to_string(),
        };
        dynamic_signals.push(sig(
            "talosServices",
            "Talos Services",
            &svc_str,
            None,
            Some("flat"),
            Some(0),
            Some(if svc_str.contains("Running") && !drift_reasons.iter().any(|r| r.contains("Talos services")) { "ok" } else { "info" }),
        ));

        let cosi_wwids = talos_st
            .map(|t| t.cosi_disk_wwids.as_slice())
            .unwrap_or(&[]);
        let (cosi_str, cosi_status) = if !cosi_wwids.is_empty() {
            let matched = expected_wwids
                .iter()
                .filter(|w| cosi_wwids.iter().any(|cw| cw == *w))
                .count();
            if matched == expected_wwids.len() {
                (format!("{matched}/{} Aligned", expected_wwids.len()), "ok")
            } else {
                drift_reasons.push(format!(
                    "COSI SAN WWID drift ({matched}/{})",
                    expected_wwids.len()
                ));
                (format!("{matched}/{} Mismatch", expected_wwids.len()), "warn")
            }
        } else {
            ("Pending COSI".to_string(), "info")
        };
        dynamic_signals.push(sig(
            "cosiWwidAlign",
            "COSI WWIDs",
            &cosi_str,
            None,
            Some("flat"),
            Some(0),
            Some(cosi_status),
        ));

        let boot_mode = detail
            .attribute_sections
            .iter()
            .flat_map(|s| s.items.iter())
            .find(|i| i.key == "tinkerbellBootMode")
            .map(|i| i.value.as_str())
            .unwrap_or("talos-netboot");
        let boot_drifted = boot_mode != "talos-netboot";
        if boot_drifted {
            drift_reasons.push(format!("BootMode drifted to {boot_mode}"));
        }
        dynamic_signals.push(sig(
            "bootMode",
            "BootMode",
            boot_mode,
            None,
            Some(if boot_drifted { "alert" } else { "flat" }),
            Some(0),
            Some(if boot_drifted { "warn" } else { "ok" }),
        ));

        let fp = format!("{}|{}|{}|{}|{}", node.device_id, k8s_str, svc_str, cosi_str, boot_mode);
        let (last_changed_at, _) = history.track_entity_change(key, fp, now);
        let drift_detected = !drift_reasons.is_empty();
        return EntitySummary {
            observation_mode: mode,
            cadence_tier: mode.cadence_tier().to_string(),
            headline: detail.summary_line.clone(),
            static_policy,
            dynamic_signals,
            drift_detected,
            drift_summary: drift_detected.then(|| drift_reasons.join("; ")),
            last_changed_at: Some(last_changed_at),
        };
    }

    // Auto-discovered baremetal host
    let cluster = spec_str(detail, "cluster");
    static_policy.push(pill("cluster", "Cluster", cluster, Some("info")));
    static_policy.push(pill("discovery", "Enrollment", "Auto-Discovered", Some("info")));
    let is_warn = detail.status_level == "warn" || detail.status_level == "error";
    if is_warn {
        drift_reasons.push(format!("Node status {}", detail.status));
    }
    dynamic_signals.push(sig(
        "k8sReady",
        "Observed State",
        &detail.status,
        None,
        Some("flat"),
        Some(0),
        Some(&detail.status_level),
    ));
    let fp = format!("{name}|{}", detail.status);
    let (last_changed_at, _) = history.track_entity_change(key, fp, now);
    let drift_detected = !drift_reasons.is_empty();
    EntitySummary {
        observation_mode: mode,
        cadence_tier: mode.cadence_tier().to_string(),
        headline: detail.summary_line.clone(),
        static_policy,
        dynamic_signals,
        drift_detected,
        drift_summary: drift_detected.then(|| drift_reasons.join("; ")),
        last_changed_at: Some(last_changed_at),
    }
}

fn build_npu_summary(
    key: &str,
    detail: &PitsEntityDetail,
    _live: &LiveTelemetry,
    history: &mut TelemetryHistory,
    now: DateTime<Utc>,
) -> EntitySummary {
    let mode = ObservationMode::Hybrid;
    let pci_id = spec_str(detail, "pciId");
    let pci_addr = spec_str(detail, "pciAddress");
    let expected_drv = spec_str(detail, "expectedKernelDriver");
    let device_class = spec_str(detail, "deviceClass");
    let bound_drv = detail
        .entity
        .spec
        .get("boundDriver")
        .and_then(|v| v.as_str())
        .unwrap_or("unbound");
    let dra_published = detail
        .entity
        .spec
        .get("draPublished")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let alloc = detail
        .entity
        .spec
        .get("allocatedCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total = detail
        .entity
        .spec
        .get("totalCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(1);
    let occ_pct = detail
        .entity
        .spec
        .get("occupancyPct")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let static_policy = vec![
        pill("pciMatch", "PCI / Silicon", format!("{pci_id} @ {pci_addr}"), Some("ok")),
        pill("expectedDriver", "Expected Driver", expected_drv, Some("info")),
        pill("deviceClass", "DRA DeviceClass", device_class, Some("info")),
    ];

    let mut drift_reasons = Vec::new();
    let driver_ok = bound_drv != "unbound" && !bound_drv.is_empty();
    if !driver_ok {
        drift_reasons.push(format!("Kernel driver '{expected_drv}' unbound"));
    }
    if detail.status != "ReadyDraV1" {
        drift_reasons.push(format!("DRA status={}", detail.status));
    }

    let dynamic_signals = vec![
        sig(
            "boundDriver",
            "Bound Driver",
            bound_drv,
            None,
            Some(if driver_ok { "flat" } else { "alert" }),
            Some(0),
            Some(if driver_ok { "ok" } else { "warn" }),
        ),
        sig(
            "draSlice",
            "DRA Slice",
            if dra_published { "Published" } else { "Missing" },
            None,
            Some(if dra_published { "flat" } else { "alert" }),
            Some(0),
            Some(if dra_published { "ok" } else { "warn" }),
        ),
        sig(
            "draOccupancy",
            "DRA Claim Occupancy",
            format!("{alloc}/{total} ({occ_pct:.0}%)"),
            None,
            Some(if alloc > 0 { "up" } else { "flat" }),
            Some(0),
            Some(if alloc > 0 { "info" } else { "ok" }),
        ),
    ];

    let fp = format!("{bound_drv}|{dra_published}|{alloc}/{total}|{}", detail.status);
    let (last_changed_at, _) = history.track_entity_change(key, fp, now);
    let drift_detected = !drift_reasons.is_empty();

    EntitySummary {
        observation_mode: mode,
        cadence_tier: mode.cadence_tier().to_string(),
        headline: detail.summary_line.clone(),
        static_policy,
        dynamic_signals,
        drift_detected,
        drift_summary: drift_detected.then(|| drift_reasons.join("; ")),
        last_changed_at: Some(last_changed_at),
    }
}

fn build_gpu_summary(
    key: &str,
    detail: &PitsEntityDetail,
    history: &mut TelemetryHistory,
    now: DateTime<Utc>,
) -> EntitySummary {
    let mode = ObservationMode::DynamicFirst;
    let product = detail
        .entity
        .spec
        .get("productName")
        .and_then(|v| v.as_str())
        .or_else(|| detail.entity.spec.get("allocatableResource").and_then(|v| v.as_str()))
        .unwrap_or("GPU");
    let mem = detail
        .entity
        .spec
        .get("memory")
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let pci = detail
        .entity
        .spec
        .get("pciAddress")
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let dra_driver = detail
        .entity
        .spec
        .get("draDriver")
        .and_then(|v| v.as_str())
        .unwrap_or("device-plugin");
    let dra_published = detail
        .entity
        .spec
        .get("draPublished")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let alloc = detail
        .entity
        .spec
        .get("allocatedCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total = detail
        .entity
        .spec
        .get("totalCount")
        .and_then(|v| v.as_u64())
        .or_else(|| detail.entity.spec.get("gpuCount").and_then(|v| v.as_u64()))
        .unwrap_or(1);
    let occ_pct = detail
        .entity
        .spec
        .get("occupancyPct")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let consumers: Vec<String> = detail
        .entity
        .spec
        .get("consumerPods")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let occ_source = detail
        .entity
        .spec
        .get("occupancySource")
        .and_then(|v| v.as_str())
        .unwrap_or(if dra_published { "dra" } else { "device-plugin" });

    let static_policy = vec![
        pill(
            "modelVram",
            "GPU / VRAM",
            if mem != "-" {
                format!("{product} ({mem})")
            } else {
                product.to_string()
            },
            Some("ok"),
        ),
        pill("pciAddress", "PCI BDF", pci, Some("info")),
        pill("draDriver", "Allocation Driver", dra_driver, Some(if dra_published { "ok" } else { "warn" })),
    ];

    let mut drift_reasons = Vec::new();
    if !dra_published {
        drift_reasons.push("DevicePluginOnly fallback (missing DRA v1 ResourceSlice)".to_string());
    }
    let consumer_str = if consumers.is_empty() {
        if alloc > 0 {
            format!("{alloc} active ({occ_source})")
        } else {
            "Idle".to_string()
        }
    } else {
        consumers.join(", ")
    };

    let dynamic_signals = vec![
        sig(
            "gpuOccupancy",
            "GPU Occupancy",
            format!("{alloc}/{total} ({occ_pct:.0}%)"),
            None,
            Some(if alloc > 0 { "up" } else { "flat" }),
            Some(0),
            Some(if occ_pct >= 90.0 { "info" } else { "ok" }),
        ),
        sig(
            "consumerPods",
            "Consumer Pods",
            &consumer_str,
            None,
            Some("flat"),
            Some(0),
            Some(if alloc > 0 { "info" } else { "ok" }),
        ),
        sig(
            "draMode",
            "DRA vs DevicePlugin",
            if dra_published { "DRA v1 Ready" } else { "DevicePlugin Drift" },
            None,
            Some(if dra_published { "flat" } else { "alert" }),
            Some(0),
            Some(if dra_published { "ok" } else { "warn" }),
        ),
    ];

    let fp = format!("{alloc}/{total}|{consumer_str}|{dra_published}");
    let (last_changed_at, _) = history.track_entity_change(key, fp, now);
    let drift_detected = !drift_reasons.is_empty();

    EntitySummary {
        observation_mode: mode,
        cadence_tier: mode.cadence_tier().to_string(),
        headline: detail.summary_line.clone(),
        static_policy,
        dynamic_signals,
        drift_detected,
        drift_summary: drift_detected.then(|| drift_reasons.join("; ")),
        last_changed_at: Some(last_changed_at),
    }
}

fn build_vm_summary(
    key: &str,
    detail: &PitsEntityDetail,
    history: &mut TelemetryHistory,
    now: DateTime<Utc>,
) -> EntitySummary {
    let mode = ObservationMode::DynamicFirst;
    let name = &detail.entity.metadata.name;
    let vcpus = detail.entity.spec.get("vcpus").and_then(|v| v.as_u64()).unwrap_or(0);
    let mem_gib = detail
        .entity
        .spec
        .get("memoryGib")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let actual_host = detail
        .entity
        .spec
        .get("hostNode")
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let contract_host = detail
        .entity
        .spec
        .get("contractHostNode")
        .and_then(|v| v.as_str())
        .unwrap_or(actual_host);
    let host_drifted = detail
        .entity
        .spec
        .get("hostDrifted")
        .and_then(|v| v.as_bool())
        .unwrap_or(actual_host != contract_host);
    let iso_detached = detail
        .entity
        .spec
        .get("installIsoDetached")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let rdp_verified = detail
        .entity
        .spec
        .get("rdpTlsVerified")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut static_policy = vec![
        pill(
            "compute",
            "Compute Spec",
            if name == "win11-001" {
                format!("{vcpus} vCPU / {mem_gib} GiB · UEFI+vTPM")
            } else {
                format!("{vcpus} vCPU / {mem_gib} GiB")
            },
            Some("ok"),
        ),
        pill("contractHost", "Contract Host", contract_host, Some("info")),
    ];
    if name == "win11-001" {
        static_policy.push(pill("selfSessionLock", "Safety Guard", "Self-Session Lock", Some("ok")));
    } else if let Some(mac) = detail.entity.spec.get("macAddress").and_then(|v| v.as_str()) {
        static_policy.push(pill("fixedMac", "Fixed MAC", mac, Some("ok")));
    }

    let mut drift_reasons = Vec::new();
    if host_drifted {
        drift_reasons.push(format!("Host migrated: {contract_host} -> {actual_host}"));
    }
    if !iso_detached {
        drift_reasons.push("Install ISO still attached (run finalize-install-media)".to_string());
    }
    if detail.status_level == "warn" || detail.status_level == "error" {
        drift_reasons.push(format!("VMI phase {}", detail.status));
    }

    let fp = format!("{actual_host}|{}|{iso_detached}|{rdp_verified}", detail.status);
    let (last_changed_at, changed_now) = history.track_entity_change(key, fp, now);

    let mut dynamic_signals = vec![
        sig(
            "actualHost",
            "Active Host",
            actual_host,
            host_drifted.then(|| format!("from {contract_host}")),
            Some(if host_drifted || changed_now { "changed" } else { "flat" }),
            Some(0),
            Some(if host_drifted { "warn" } else { "ok" }),
        ),
        sig(
            "vmiPhase",
            "VMI Phase",
            &detail.status,
            None,
            Some("flat"),
            Some(0),
            Some(&detail.status_level),
        ),
    ];
    if name == "win11-001" {
        dynamic_signals.push(sig(
            "isoMedia",
            "Install ISO",
            if iso_detached { "Detached" } else { "Attached" },
            None,
            Some(if iso_detached { "flat" } else { "alert" }),
            Some(0),
            Some(if iso_detached { "ok" } else { "warn" }),
        ));
        dynamic_signals.push(sig(
            "rdpTls",
            "RDP TLS",
            if rdp_verified { "Thumbprint Verified" } else { "Unverified" },
            None,
            Some("flat"),
            Some(0),
            Some(if rdp_verified { "ok" } else { "info" }),
        ));
    }

    let drift_detected = !drift_reasons.is_empty();
    EntitySummary {
        observation_mode: mode,
        cadence_tier: mode.cadence_tier().to_string(),
        headline: detail.summary_line.clone(),
        static_policy,
        dynamic_signals,
        drift_detected,
        drift_summary: drift_detected.then(|| drift_reasons.join("; ")),
        last_changed_at: Some(last_changed_at),
    }
}

fn build_edge_router_summary(
    key: &str,
    detail: &PitsEntityDetail,
    live: &LiveTelemetry,
    history: &mut TelemetryHistory,
    now: DateTime<Utc>,
) -> EntitySummary {
    let mode = ObservationMode::DynamicFirst;
    let sha = short_hash(spec_str(detail, "desiredSpecSha256"), 12);
    let val = detail.diagnostics.get("policyValidation");
    let core_lab = val
        .and_then(|v| v.get("coreLabIsolated"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let lab_mgmt = val
        .and_then(|v| v.get("labMgmtBlocked"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let mgmt0_safe = val
        .and_then(|v| v.get("mgmt0PciSafe"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let static_policy = vec![
        pill(
            "vlanIsolation",
            "VLAN 10↔20",
            if core_lab { "Isolated" } else { "VIOLATION" },
            Some(if core_lab { "ok" } else { "error" }),
        ),
        pill(
            "labMgmtBlock",
            "Lab→Mgmt",
            if lab_mgmt { "Blocked" } else { "VIOLATION" },
            Some(if lab_mgmt { "ok" } else { "error" }),
        ),
        pill(
            "mgmt0Guard",
            "mgmt0 Guard",
            format!("{MGMT0_PCI} {}", if mgmt0_safe { "Safe" } else { "BREACH" }),
            Some(if mgmt0_safe { "ok" } else { "error" }),
        ),
        pill("specSha256", "Spec SHA256", &sha, Some("info")),
    ];

    let mut drift_reasons = Vec::new();
    if !core_lab || !lab_mgmt || !mgmt0_safe {
        drift_reasons.push("Router security isolation or mgmt0 PCI policy violation".to_string());
    }

    let rtr = live.router_live.as_ref();
    let age_secs = rtr.and_then(|r| r.verification_age_secs);
    let fresh = rtr.map(|r| r.verification_fresh).unwrap_or(false);
    let paused = rtr.map(|r| r.crossplane_paused).unwrap_or(false);
    if paused {
        drift_reasons.push("Crossplane reconciliation paused (crossplane.io/paused=true)".to_string());
    }
    if let Some(age) = age_secs {
        if age > MAX_VERIFICATION_AGE_SECS || !fresh {
            drift_reasons.push(format!("VPP verification stale ({age}s > {MAX_VERIFICATION_AGE_SECS}s)"));
        }
    }

    let (links_up, links_total) = rtr
        .and_then(|r| r.vpp_report.as_ref())
        .and_then(|v| v.get("interfaces"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            let total = arr.len();
            let up = arr
                .iter()
                .filter(|i| {
                    i.get("linkUp").and_then(|v| v.as_bool()).unwrap_or(false)
                        || i.get("adminUp").and_then(|v| v.as_bool()).unwrap_or(false)
                })
                .count();
            (up, total)
        })
        .unwrap_or((0, 0));

    let vpp_delta = history.vpp_delta();
    let vpp_latest = history.latest_vpp_sample().cloned().or_else(|| {
        rtr.and_then(|r| r.vpp_report.as_ref())
            .and_then(|v| extract_vpp_sample(v, now))
    });

    let (rate_val, rate_delta, rate_trend) = if let Some(ref d) = vpp_delta {
        if d.counter_reset {
            (
                "Counter Reset".to_string(),
                Some("reset".to_string()),
                "changed",
            )
        } else {
            (
                format!("Rx {:.2} / Tx {:.2} Mbps", d.rx_mbps, d.tx_mbps),
                Some(format!("{:.0}/{:.0} pps", d.rx_pps, d.tx_pps)),
                if d.rx_mbps + d.tx_mbps > 0.01 { "up" } else { "flat" },
            )
        }
    } else if let Some(ref s) = vpp_latest {
        (
            format!(
                "Rx {} / Tx {}",
                format_bytes_human(s.rx_bytes),
                format_bytes_human(s.tx_bytes)
            ),
            Some(format!("{} / {} pkts", s.rx_packets, s.tx_packets)),
            "flat",
        )
    } else {
        ("Awaiting VPP sample".to_string(), None, "flat")
    };

    let (drops_val, drops_delta_str, drops_status) = if let Some(ref d) = vpp_delta {
        if d.delta_drops > 0 || d.delta_errors > 0 {
            drift_reasons.push(format!(
                "VPP dataplane packet drops/errors increased (Δdrops={}, Δerr={})",
                format_signed_int(d.delta_drops),
                format_signed_int(d.delta_errors)
            ));
        }
        (
            format!("{} drops / {} err", d.total_drops, d.total_errors),
            Some(format!(
                "Δ{} / Δ{}",
                format_signed_int(d.delta_drops),
                format_signed_int(d.delta_errors)
            )),
            if d.delta_drops > 0 || d.delta_errors > 0 {
                "warn"
            } else {
                "ok"
            },
        )
    } else if let Some(ref s) = vpp_latest {
        (
            format!("{} drops / {} err", s.drops, s.errors),
            None,
            if s.errors > 0 { "warn" } else { "ok" },
        )
    } else {
        ("0 drops / 0 err".to_string(), None, "info")
    };

    let mut dynamic_signals = vec![
        sig(
            "vppFreshness",
            "VPP Freshness",
            match age_secs {
                Some(a) => format!("{a}s ({})", if fresh { "<=30s Fresh" } else { "Stale" }),
                None => "30s SLA".to_string(),
            },
            None,
            Some(if age_secs.is_some() && !fresh { "alert" } else { "flat" }),
            age_secs,
            Some(if age_secs.is_some() && !fresh { "warn" } else { "ok" }),
        ),
        sig(
            "crossplaneSync",
            "Crossplane Sync",
            if paused { "Paused" } else { "Active" },
            None,
            Some(if paused { "alert" } else { "flat" }),
            Some(0),
            Some(if paused { "warn" } else { "ok" }),
        ),
        sig(
            "vppLinks",
            "VPP Links",
            if links_total > 0 {
                format!("{links_up}/{links_total} Up")
            } else {
                "Baseline (4 ifaces)".to_string()
            },
            None,
            Some("flat"),
            Some(0),
            Some(if links_total > 0 && links_up == 0 { "warn" } else { "ok" }),
        ),
        sig(
            "vppThroughput",
            "GoVPP Throughput",
            rate_val,
            rate_delta,
            Some(rate_trend),
            Some(0),
            Some("info"),
        ),
        sig(
            "vppDropsErrors",
            "GoVPP Drops / Errors",
            drops_val,
            drops_delta_str,
            Some(if drops_status == "warn" { "alert" } else { "flat" }),
            Some(0),
            Some(drops_status),
        ),
    ];
    let _ = &mut dynamic_signals;

    let fp = format!(
        "{sha}|{}|{paused}|{links_up}/{links_total}|{}",
        detail.status,
        vpp_latest.as_ref().map(|s| s.drops + s.errors).unwrap_or(0)
    );
    let (last_changed_at, _) = history.track_entity_change(key, fp, now);
    let drift_detected = !drift_reasons.is_empty();

    EntitySummary {
        observation_mode: mode,
        cadence_tier: mode.cadence_tier().to_string(),
        headline: detail.summary_line.clone(),
        static_policy,
        dynamic_signals,
        drift_detected,
        drift_summary: drift_detected.then(|| drift_reasons.join("; ")),
        last_changed_at: Some(last_changed_at),
    }
}

fn build_pxe_summary(
    key: &str,
    detail: &PitsEntityDetail,
    bundle: &WorkspaceBundle,
    live: &LiveTelemetry,
    history: &mut TelemetryHistory,
    now: DateTime<Utc>,
) -> EntitySummary {
    let mode = ObservationMode::PolicyFirst;
    let boot_mode = spec_str(detail, "bootMode");
    let is_talos = boot_mode == "talos-netboot";
    let total_nodes = bundle.nodes.len();
    let verified = live
        .pxe_asset_verify
        .values()
        .filter(|v| v.get("verified").and_then(|b| b.as_bool()) == Some(true))
        .count();
    let failed = live
        .pxe_asset_verify
        .values()
        .filter(|v| v.get("verified").and_then(|b| b.as_bool()) == Some(false))
        .count();

    let hook_nodes = live
        .tink_hardware
        .as_ref()
        .and_then(|h| h.get("items"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|it| {
                    it.pointer("/spec/interfaces/0/netboot/allowWorkflow")
                        .and_then(|v| v.as_bool())
                        == Some(true)
                })
                .count()
        })
        .unwrap_or(0);

    let static_policy = vec![
        pill("kernelSec", "Kernel Policy", "selinux=1 sig_enforce=1", Some("ok")),
        pill("diskGuard", "Disk Safety", "No Local Disk Write", Some("ok")),
        pill("ouiRule", "OUI Direct-iPXE", "58:47:ca:*", Some("info")),
    ];

    let mut drift_reasons = Vec::new();
    if failed > 0 {
        drift_reasons.push(format!("{failed} node PXE SHA-256 asset check failed"));
    }
    if is_talos && hook_nodes > 0 {
        drift_reasons.push(format!("{hook_nodes} node(s) currently switched to Hook mode"));
    }

    let sha_str = if live.pxe_asset_verify.is_empty() {
        format!("0/{total_nodes} (On-Demand)")
    } else {
        format!("{verified}/{total_nodes} Verified")
    };
    let dynamic_signals = vec![
        sig(
            "sha256Verify",
            "SHA-256 Asset Ratio",
            &sha_str,
            None,
            Some("flat"),
            Some(0),
            Some(if failed > 0 { "error" } else if verified > 0 { "ok" } else { "info" }),
        ),
        sig(
            "hookModeNodes",
            "Hook Mode Nodes",
            format!("{hook_nodes}/{total_nodes}"),
            None,
            Some(if hook_nodes > 0 { "changed" } else { "flat" }),
            Some(0),
            Some(if is_talos && hook_nodes > 0 { "warn" } else { "ok" }),
        ),
    ];

    let fp = format!("{boot_mode}|{verified}/{failed}|{hook_nodes}");
    let (last_changed_at, _) = history.track_entity_change(key, fp, now);
    let drift_detected = !drift_reasons.is_empty();

    EntitySummary {
        observation_mode: mode,
        cadence_tier: mode.cadence_tier().to_string(),
        headline: detail.summary_line.clone(),
        static_policy,
        dynamic_signals,
        drift_detected,
        drift_summary: drift_detected.then(|| drift_reasons.join("; ")),
        last_changed_at: Some(last_changed_at),
    }
}

fn build_storage_pool_summary(
    key: &str,
    detail: &PitsEntityDetail,
    live: &LiveTelemetry,
    history: &mut TelemetryHistory,
    now: DateTime<Utc>,
) -> EntitySummary {
    let mode = ObservationMode::DynamicFirst;
    let backend = detail
        .entity
        .spec
        .get("backend")
        .and_then(|v| v.as_str())
        .unwrap_or("btrfs");

    if backend == "mayastor-spdk" {
        let node = spec_str(detail, "node");
        let used = spec_str(detail, "used");
        let cap = spec_str(detail, "capacity");
        let alert = spec_str(detail, "alertStatus");
        let pool_status = spec_str(detail, "poolStatus");
        let mut drift_reasons = Vec::new();
        if detail.status_level != "ok" {
            drift_reasons.push(format!("Mayastor DiskPool {pool_status} (alert={alert})"));
        }
        let static_policy = vec![
            pill("backend", "Backend", "Mayastor SPDK", Some("info")),
            pill("node", "Node", node, Some("ok")),
        ];
        let dynamic_signals = vec![
            sig("utilization", "Capacity", format!("{used} / {cap}"), None, Some("flat"), Some(0), Some("info")),
            sig(
                "mayastorAlert",
                "Pool / Alert",
                format!("{pool_status} · {alert}"),
                None,
                Some(if detail.status_level != "ok" { "alert" } else { "flat" }),
                Some(0),
                Some(&detail.status_level),
            ),
        ];
        let fp = format!("{node}|{used}/{cap}|{pool_status}|{alert}");
        let (last_changed_at, _) = history.track_entity_change(key, fp, now);
        let drift_detected = !drift_reasons.is_empty();
        return EntitySummary {
            observation_mode: mode,
            cadence_tier: mode.cadence_tier().to_string(),
            headline: detail.summary_line.clone(),
            static_policy,
            dynamic_signals,
            drift_detected,
            drift_summary: drift_detected.then(|| drift_reasons.join("; ")),
            last_changed_at: Some(last_changed_at),
        };
    }

    // Core Btrfs pool (`pits-data`)
    let fs_uuid = spec_str(detail, "filesystemUuid");
    let uuid_ok = fs_uuid == EXPECTED_BTRFS_UUID;
    let btrfs = live.btrfs.as_ref();
    let total_bytes = btrfs.and_then(|b| b.get("totalBytes")).and_then(|v| v.as_u64());
    let used_bytes = btrfs.and_then(|b| b.get("usedBytes")).and_then(|v| v.as_u64());
    let err_total = btrfs.map(extract_btrfs_error_total).unwrap_or(0);
    let healthy = btrfs.and_then(|b| b.get("healthy")).and_then(|v| v.as_bool()).unwrap_or(true);
    let last_trim = btrfs
        .and_then(|b| b.get("lastTrimWeek"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Scheduled");
    let last_scrub = btrfs
        .and_then(|b| b.get("lastScrubMonth"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Scheduled");

    let mut drift_reasons = Vec::new();
    if !uuid_ok {
        drift_reasons.push(format!("Btrfs UUID mismatch ({fs_uuid})"));
    }
    if !healthy || err_total > 0 {
        drift_reasons.push(format!("Btrfs sysfs I/O errors detected (total={err_total})"));
    }

    let btrfs_delta = history.btrfs_delta();
    if let Some(ref d) = btrfs_delta {
        if d.delta_errors > 0 {
            drift_reasons.push(format!("Btrfs errors increased by +{}", d.delta_errors));
        }
    }

    let (util_str, util_pct_opt) = match (used_bytes, total_bytes) {
        (Some(u), Some(t)) if t > 0 => {
            let pct = (u as f64) * 100.0 / (t as f64);
            if pct >= 85.0 {
                drift_reasons.push(format!("Storage pool high watermark ({pct:.1}% >= 85%)"));
            }
            (
                format!(
                    "{pct:.1}% ({:.1}/{:.1} GiB)",
                    (u as f64) / 1_073_741_824.0,
                    (t as f64) / 1_073_741_824.0
                ),
                Some(pct),
            )
        }
        _ => ("Baseline (9 SAN extents)".to_string(), None),
    };

    let static_policy = vec![
        pill("fsUuid", "Btrfs UUID", short_hash(fs_uuid, 8), Some(if uuid_ok { "ok" } else { "error" })),
        pill("subvols", "Subvolumes", "@data/@archive/@snapshots", Some("ok")),
    ];

    let dynamic_signals = vec![
        sig(
            "utilization",
            "Pool Usage",
            &util_str,
            btrfs_delta.as_ref().map(|d| format_signed_bytes(d.delta_used_bytes)),
            Some(
                btrfs_delta
                    .as_ref()
                    .map(|d| trend_from_i64(d.delta_used_bytes))
                    .unwrap_or("flat"),
            ),
            Some(0),
            Some(if util_pct_opt.unwrap_or(0.0) >= 85.0 { "warn" } else { "ok" }),
        ),
        sig(
            "ioErrors",
            "Sysfs I/O Errors",
            err_total.to_string(),
            btrfs_delta.as_ref().map(|d| format_signed_int(d.delta_errors)),
            Some(if err_total > 0 || btrfs_delta.as_ref().map(|d| d.delta_errors > 0).unwrap_or(false) { "alert" } else { "flat" }),
            Some(0),
            Some(if !healthy || err_total > 0 { "warn" } else { "ok" }),
        ),
        sig(
            "maintenance",
            "TRIM / Scrub",
            format!("{last_trim} / {last_scrub}"),
            None,
            Some("flat"),
            Some(0),
            Some("info"),
        ),
    ];

    let fp = format!("{fs_uuid}|{util_str}|{err_total}|{healthy}");
    let (last_changed_at, _) = history.track_entity_change(key, fp, now);
    let drift_detected = !drift_reasons.is_empty();

    EntitySummary {
        observation_mode: mode,
        cadence_tier: mode.cadence_tier().to_string(),
        headline: detail.summary_line.clone(),
        static_policy,
        dynamic_signals,
        drift_detected,
        drift_summary: drift_detected.then(|| drift_reasons.join("; ")),
        last_changed_at: Some(last_changed_at),
    }
}

fn build_san_volume_summary(
    key: &str,
    detail: &PitsEntityDetail,
    bundle: &WorkspaceBundle,
    live: &LiveTelemetry,
    history: &mut TelemetryHistory,
    now: DateTime<Utc>,
) -> EntitySummary {
    let mode = ObservationMode::Hybrid;
    let dev_id = spec_str(detail, "deviceId");
    let nsid = detail.entity.spec.get("namespaceId").and_then(|v| v.as_i64()).unwrap_or(0);
    let role = spec_str(detail, "volumeRole");
    let wwid = spec_str(detail, "wwid");
    let actual_bytes = detail.entity.spec.get("bytes").and_then(|v| v.as_i64()).unwrap_or(0);

    let contract_bytes = bundle
        .nodes
        .iter()
        .find(|n| n.device_id == dev_id)
        .and_then(|n| n.san.namespaces.iter().find(|ns| ns.id as i64 == nsid))
        .map(|ns| ns.bytes)
        .unwrap_or(actual_bytes);
    let cap_drifted = actual_bytes > 0 && contract_bytes > 0 && actual_bytes != contract_bytes;

    let san_target = live.san_by_device.get(dev_id);
    let live_ns = detail.diagnostics.get("sanNamespaceLive");
    let configfs_enabled = live_ns
        .and_then(|r| r.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let nocow_ok = san_target
        .and_then(|t| t.get("nocowVerified"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let gpt_verified = live_ns
        .and_then(|r| r.get("gptVerified"))
        .and_then(|v| v.as_bool())
        .unwrap_or(nsid == 1);
    let cosi_mounted = live
        .talos_by_device
        .get(dev_id)
        .map(|t| t.cosi_disk_wwids.iter().any(|w| w == wwid))
        .unwrap_or(false);

    let static_policy = vec![
        pill("nsid", "NSID / Role", format!("NSID {nsid} ({role})"), Some("info")),
        pill("wwid", "UUIDv5 WWID", short_hash(wwid, 18), Some("ok")),
        pill("hostAcl", "Host ACL", dev_id, Some("ok")),
        pill(
            "expandGuard",
            "Expand Policy",
            if nsid == 1 { "NSID 1 Locked" } else { "4KiB Online Expand" },
            Some(if nsid == 1 { "info" } else { "ok" }),
        ),
    ];

    let mut drift_reasons = Vec::new();
    if san_target.is_some() && !configfs_enabled {
        drift_reasons.push("nvmet configfs namespace disabled (0)".to_string());
    }
    if san_target.is_some() && !nocow_ok {
        drift_reasons.push("Missing Btrfs FS_NOCOW_FL (+C)".to_string());
    }
    if cap_drifted {
        drift_reasons.push(format!(
            "Capacity drift: actual {} vs contract {}",
            format_bytes_human(actual_bytes as u64),
            format_bytes_human(contract_bytes as u64)
        ));
    }
    if nsid == 1 && !gpt_verified {
        drift_reasons.push("4KiB GPT (META+STATE) verification pending".to_string());
    }

    let dynamic_signals = vec![
        sig(
            "configfs",
            "configfs",
            if configfs_enabled { "Enabled (1)" } else { "Disabled (0)" },
            None,
            Some(if configfs_enabled { "flat" } else { "alert" }),
            Some(0),
            Some(if configfs_enabled { "ok" } else { "warn" }),
        ),
        sig(
            "nocow",
            "NOCOW +C",
            if nocow_ok { "Verified" } else { "Missing" },
            None,
            Some(if nocow_ok { "flat" } else { "alert" }),
            Some(0),
            Some(if nocow_ok { "ok" } else { "warn" }),
        ),
        sig(
            "cosiMount",
            "COSI Mount",
            if cosi_mounted { "Attached" } else { "Pinned" },
            None,
            Some("flat"),
            Some(0),
            Some(if cosi_mounted { "ok" } else { "info" }),
        ),
        sig(
            "capacity",
            "Capacity",
            format_bytes_human(actual_bytes.max(0) as u64),
            cap_drifted.then(|| format_signed_bytes(actual_bytes - contract_bytes)),
            Some(if cap_drifted { "changed" } else { "flat" }),
            Some(0),
            Some(if cap_drifted { "warn" } else { "ok" }),
        ),
    ];

    let fp = format!("{configfs_enabled}|{nocow_ok}|{cosi_mounted}|{actual_bytes}|{gpt_verified}");
    let (last_changed_at, _) = history.track_entity_change(key, fp, now);
    let drift_detected = !drift_reasons.is_empty();

    EntitySummary {
        observation_mode: mode,
        cadence_tier: mode.cadence_tier().to_string(),
        headline: detail.summary_line.clone(),
        static_policy,
        dynamic_signals,
        drift_detected,
        drift_summary: drift_detected.then(|| drift_reasons.join("; ")),
        last_changed_at: Some(last_changed_at),
    }
}

fn build_ai_storage_summary(
    key: &str,
    detail: &PitsEntityDetail,
    history: &mut TelemetryHistory,
    now: DateTime<Utc>,
) -> EntitySummary {
    let mode = ObservationMode::Hybrid;
    let env = spec_str(detail, "environment");
    let domain = spec_str(detail, "domain");
    let auth_mode = spec_str(detail, "authMode");
    let mcp_ep = spec_str(detail, "mcpEndpoint");
    let ready_count = detail
        .entity
        .spec
        .get("readyArchetypes")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let static_policy = vec![
        pill("envDomain", "Env / Domain", format!("{env} · {domain}"), Some("info")),
        pill("authMode", "Zero-Trust Auth", auth_mode, Some("ok")),
        pill("contract", "Archetype Contract", "8-Archetype OpenDAL", Some("ok")),
    ];

    let mut drift_reasons = Vec::new();
    if detail.status_level != "ok" {
        drift_reasons.push(format!("Crossplane reconciliation {}", detail.status));
    }
    if ready_count < 8 {
        drift_reasons.push(format!("Only {ready_count}/8 semantic archetypes ready"));
    }

    let dynamic_signals = vec![
        sig(
            "crossplaneReady",
            "Crossplane Sync",
            &detail.status,
            None,
            Some(if detail.status_level != "ok" { "alert" } else { "flat" }),
            Some(0),
            Some(&detail.status_level),
        ),
        sig(
            "readyArchetypes",
            "Ready Archetypes",
            format!("{ready_count}/8"),
            None,
            Some(if ready_count < 8 { "alert" } else { "flat" }),
            Some(0),
            Some(if ready_count >= 8 { "ok" } else { "warn" }),
        ),
        sig(
            "mcpEndpoint",
            "MCP Endpoint",
            mcp_ep,
            None,
            Some("flat"),
            Some(0),
            Some(if mcp_ep != "-" { "ok" } else { "warn" }),
        ),
    ];

    let fp = format!("{}|{ready_count}|{mcp_ep}", detail.status);
    let (last_changed_at, _) = history.track_entity_change(key, fp, now);
    let drift_detected = !drift_reasons.is_empty();

    EntitySummary {
        observation_mode: mode,
        cadence_tier: mode.cadence_tier().to_string(),
        headline: detail.summary_line.clone(),
        static_policy,
        dynamic_signals,
        drift_detected,
        drift_summary: drift_detected.then(|| drift_reasons.join("; ")),
        last_changed_at: Some(last_changed_at),
    }
}

fn build_cluster_summary(
    key: &str,
    detail: &PitsEntityDetail,
    live: &LiveTelemetry,
    history: &mut TelemetryHistory,
    now: DateTime<Utc>,
) -> EntitySummary {
    let mode = ObservationMode::DynamicFirst;
    let slug = &detail.entity.metadata.name;
    let role = spec_str(detail, "clusterRole");
    let via_bridge = detail
        .entity
        .spec
        .get("viaAgentBridge")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let ready_nodes = detail
        .entity
        .spec
        .get("readyNodes")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total_nodes = detail
        .entity
        .spec
        .get("totalNodes")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let gpu_total = detail
        .entity
        .spec
        .get("gpuCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let gpu_alloc = detail
        .entity
        .spec
        .get("allocatedGpus")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let gpu_pct = detail
        .entity
        .spec
        .get("gpuOccupancyPct")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let npu_total = detail
        .entity
        .spec
        .get("npuCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let npu_alloc = detail
        .entity
        .spec
        .get("allocatedNpus")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let npu_pct = detail
        .entity
        .spec
        .get("npuOccupancyPct")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let cp_opt = if slug == "cluster-pits-core" {
        live.core_cluster.as_ref()
    } else if slug == "cluster-pits-lab" {
        live.lab_cluster.as_ref()
    } else {
        live.extra_clusters
            .iter()
            .find(|e| crate::catalog::discovery::sanitize_k8s_slug(&e.config.name) == *slug)
            .map(|e| &e.summary)
    };

    let running_pods = cp_opt.map(|c| c.running_pods).unwrap_or(0);
    let total_pods = cp_opt.map(|c| c.total_pods).unwrap_or(0);
    let bound_pvs = cp_opt.map(|c| c.bound_pvs).unwrap_or(0);
    let dcs = detail
        .attribute_sections
        .iter()
        .flat_map(|s| s.items.iter())
        .find(|i| i.key == "deviceClasses")
        .map(|i| i.value.as_str())
        .unwrap_or("None");

    let static_policy = vec![
        pill("role", "Cluster Role", role, Some("info")),
        pill(
            "transport",
            "API Transport",
            if via_bridge { "mTLS CONNECT Tunnel" } else { "Direct TLS" },
            Some("ok"),
        ),
        pill("deviceClasses", "DRA Classes", dcs, Some("info")),
    ];

    let mut drift_reasons = Vec::new();
    if detail.status_level == "warn" || detail.status_level == "error" {
        drift_reasons.push(format!("Cluster status: {}", detail.status));
    }
    if total_nodes > 0 && ready_nodes < total_nodes {
        drift_reasons.push(format!("Unready nodes ({ready_nodes}/{total_nodes} Ready)"));
    }

    let cdelta = history.cluster_delta(slug);
    let mut dynamic_signals = vec![
        sig(
            "apiReachable",
            "API Server",
            if detail.live_connected { "Reachable" } else { "Baseline/Offline" },
            None,
            Some("flat"),
            Some(0),
            Some(if detail.live_connected { "ok" } else { "info" }),
        ),
        sig(
            "readyNodes",
            "Ready Nodes",
            format!("{ready_nodes}/{total_nodes}"),
            cdelta.as_ref().map(|d| format_signed_int(d.delta_ready_nodes)),
            Some(cdelta.as_ref().map(|d| trend_from_i64(d.delta_ready_nodes)).unwrap_or("flat")),
            Some(0),
            Some(if total_nodes > 0 && ready_nodes < total_nodes { "warn" } else { "ok" }),
        ),
        sig(
            "runningPods",
            "Running Pods",
            if detail.live_connected {
                format!("{running_pods}/{total_pods}")
            } else {
                "Baseline".to_string()
            },
            cdelta.as_ref().map(|d| format_signed_int(d.delta_running_pods)),
            Some(cdelta.as_ref().map(|d| trend_from_i64(d.delta_running_pods)).unwrap_or("flat")),
            Some(0),
            Some("ok"),
        ),
        sig(
            "boundPvs",
            "Bound PVs",
            bound_pvs.to_string(),
            None,
            Some("flat"),
            Some(0),
            Some("info"),
        ),
    ];

    if gpu_total > 0 {
        dynamic_signals.push(sig(
            "gpuOccupancy",
            "DRA GPU Occupancy",
            format!("{gpu_alloc}/{gpu_total} ({gpu_pct:.0}%)"),
            cdelta.as_ref().map(|d| format_signed_int(d.delta_allocated_gpus)),
            Some(cdelta.as_ref().map(|d| trend_from_i64(d.delta_allocated_gpus)).unwrap_or("flat")),
            Some(0),
            Some(if gpu_pct >= 90.0 { "warn" } else { "ok" }),
        ));
    }
    if npu_total > 0 {
        dynamic_signals.push(sig(
            "npuOccupancy",
            "DRA NPU Occupancy",
            format!("{npu_alloc}/{npu_total} ({npu_pct:.0}%)"),
            cdelta.as_ref().map(|d| format_signed_int(d.delta_allocated_npus)),
            Some(cdelta.as_ref().map(|d| trend_from_i64(d.delta_allocated_npus)).unwrap_or("flat")),
            Some(0),
            Some("ok"),
        ));
    }

    let fp = format!(
        "{}|{ready_nodes}/{total_nodes}|{running_pods}/{total_pods}|{gpu_alloc}/{gpu_total}|{npu_alloc}/{npu_total}",
        detail.status
    );
    let (last_changed_at, _) = history.track_entity_change(key, fp, now);
    let drift_detected = !drift_reasons.is_empty();

    EntitySummary {
        observation_mode: mode,
        cadence_tier: mode.cadence_tier().to_string(),
        headline: detail.summary_line.clone(),
        static_policy,
        dynamic_signals,
        drift_detected,
        drift_summary: drift_detected.then(|| drift_reasons.join("; ")),
        last_changed_at: Some(last_changed_at),
    }
}

fn build_cicd_summary(
    key: &str,
    detail: &PitsEntityDetail,
    live: &LiveTelemetry,
    history: &mut TelemetryHistory,
    now: DateTime<Utc>,
) -> EntitySummary {
    let mode = ObservationMode::DynamicFirst;
    let cicd = live.lab_cicd.as_ref();

    let static_policy = vec![
        pill("workflows", "Workflows", "6-Workflow Matrix", Some("ok")),
        pill("baseIsolation", "Core Isolation", "Base/ Excluded", Some("ok")),
        pill("nodeExclusion", "Host Redline", "NotIn [netboot-004]", Some("ok")),
    ];

    let mut drift_reasons = Vec::new();
    let violations = cicd.map(|c| c.pods_on_netboot_004.len()).unwrap_or(0);
    if violations > 0 {
        drift_reasons.push(format!("{violations} CI/CD pod(s) scheduled on netboot-004"));
    }

    let services_up = cicd
        .map(|c| {
            usize::from(c.gitea_ready)
                + usize::from(c.cnpg_ready_instances >= 2)
                + usize::from(c.zot_ready)
                + usize::from(c.attic_ready)
        })
        .unwrap_or(4);
    if cicd.is_some() && services_up < 4 {
        drift_reasons.push(format!("Core CI services degraded ({services_up}/4 Ready)"));
    }
    if let Some(c) = cicd {
        if !c.flux_repo_ready || !c.flux_kust_ready {
            drift_reasons.push("Lab Flux CD reconciliation not ready".to_string());
        }
    }

    let flux_rev = cicd
        .and_then(|c| c.flux_revision.as_deref())
        .unwrap_or("main (baseline)");
    let runners_str = cicd
        .map(|c| {
            format!(
                "CPU {}/{} · NPU {}/{} · GPU {}/{}",
                c.runner_cpu_ready,
                c.runner_cpu_desired,
                c.runner_npu_ready,
                c.runner_npu_desired,
                c.runner_gpu_ready,
                c.runner_gpu_desired
            )
        })
        .unwrap_or_else(|| "CPU 3/3 · NPU 1/1 · GPU 1/1".to_string());

    let fp = format!("{violations}|{services_up}|{flux_rev}|{runners_str}");
    let (last_changed_at, changed_now) = history.track_entity_change(key, fp, now);

    let dynamic_signals = vec![
        sig(
            "exclusionGuard",
            "netboot-004 Guard",
            if violations == 0 {
                "0 Violations".to_string()
            } else {
                format!("{violations} VIOLATIONS")
            },
            None,
            Some(if violations > 0 { "alert" } else { "flat" }),
            Some(0),
            Some(if violations > 0 { "error" } else { "ok" }),
        ),
        sig(
            "coreServices",
            "Gitea/CNPG/Zot/Attic",
            format!("{services_up}/4 Ready"),
            None,
            Some(if services_up < 4 { "alert" } else { "flat" }),
            Some(0),
            Some(if services_up == 4 { "ok" } else { "warn" }),
        ),
        sig(
            "fluxRevision",
            "Flux Revision",
            flux_rev,
            changed_now.then(|| "updated".to_string()),
            Some(if changed_now { "changed" } else { "flat" }),
            Some(0),
            Some(if cicd.map(|c| c.flux_repo_ready && c.flux_kust_ready).unwrap_or(true) { "ok" } else { "warn" }),
        ),
        sig(
            "runners",
            "act_runner Pools",
            &runners_str,
            None,
            Some("flat"),
            Some(0),
            Some(if cicd.map(|c| c.runner_cpu_ready >= 1).unwrap_or(true) { "ok" } else { "warn" }),
        ),
    ];

    let drift_detected = !drift_reasons.is_empty();
    EntitySummary {
        observation_mode: mode,
        cadence_tier: mode.cadence_tier().to_string(),
        headline: detail.summary_line.clone(),
        static_policy,
        dynamic_signals,
        drift_detected,
        drift_summary: drift_detected.then(|| drift_reasons.join("; ")),
        last_changed_at: Some(last_changed_at),
    }
}

fn build_auth_flow_summary(
    key: &str,
    detail: &PitsEntityDetail,
    history: &mut TelemetryHistory,
    now: DateTime<Utc>,
) -> EntitySummary {
    let mode = ObservationMode::PolicyFirst;
    let realm = spec_str(detail, "realm");
    let flow_alias = spec_str(detail, "flowAlias");
    let active_flow = spec_str(detail, "activeFlowAlias");
    let bind_browser = detail
        .entity
        .spec
        .get("bindAsRealmBrowserFlow")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let static_policy = vec![
        pill("realm", "Keycloak Realm", realm, Some("info")),
        pill("flowAlias", "Desired Flow", flow_alias, Some("ok")),
        pill(
            "browserBinding",
            "Browser Binding",
            if bind_browser { "Realm Browser Flow" } else { "Subflow Only" },
            Some("ok"),
        ),
        pill("loa", "Assurance Policy", "LoA-2 FIDO2/WebAuthn", Some("ok")),
    ];

    let mut drift_reasons = Vec::new();
    let exact_match = active_flow != "-" && active_flow == flow_alias;
    let suffix_match =
        active_flow != "-" && flow_alias != "-" && active_flow.starts_with(&format!("{flow_alias}-"));
    let alias_drifted = active_flow == "-" || (!exact_match && !suffix_match);
    if alias_drifted {
        drift_reasons.push(format!(
            "activeFlowAlias '{active_flow}' drifted from spec.flowAlias '{flow_alias}'"
        ));
    }
    if detail.status_level != "ok" {
        drift_reasons.push(format!("Crossplane status={}", detail.status));
    }

    let active_label = if exact_match {
        format!("{active_flow} (Exact)")
    } else if suffix_match {
        format!("{active_flow} (Rev Aligned)")
    } else {
        format!("{active_flow} != {flow_alias}")
    };

    let dynamic_signals = vec![
        sig(
            "activeFlowMatch",
            "Active Flow vs Spec",
            &active_label,
            alias_drifted.then(|| "drift".to_string()),
            Some(if alias_drifted { "alert" } else { "flat" }),
            Some(0),
            Some(if alias_drifted { "warn" } else { "ok" }),
        ),
        sig(
            "crossplaneReady",
            "Crossplane Ready",
            &detail.status,
            None,
            Some(if detail.status_level != "ok" { "alert" } else { "flat" }),
            Some(0),
            Some(&detail.status_level),
        ),
    ];

    let fp = format!("{flow_alias}|{active_flow}|{}", detail.status);
    let (last_changed_at, _) = history.track_entity_change(key, fp, now);
    let drift_detected = !drift_reasons.is_empty();

    EntitySummary {
        observation_mode: mode,
        cadence_tier: mode.cadence_tier().to_string(),
        headline: detail.summary_line.clone(),
        static_policy,
        dynamic_signals,
        drift_detected,
        drift_summary: drift_detected.then(|| drift_reasons.join("; ")),
        last_changed_at: Some(last_changed_at),
    }
}

fn build_sourcing_summary(
    key: &str,
    detail: &PitsEntityDetail,
    history: &mut TelemetryHistory,
    now: DateTime<Utc>,
) -> EntitySummary {
    let mode = ObservationMode::GovernanceEvent;
    let diag = &detail.diagnostics;
    let Some(facts_val) = diag.get("facts") else {
        return build_non_resource_summary(detail, now);
    };
    let rec_val = diag.get("record");
    let quadrant = facts_val
        .get("quadrant")
        .and_then(|v| v.as_str())
        .unwrap_or("non-critical");
    let final_y = facts_val.get("finalYScore").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let final_x = facts_val.get("finalXScore").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let knockout_triggered = facts_val
        .get("knockoutTriggered")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let windmill_mismatch = facts_val
        .get("windmillMismatch")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let re_audit_required = facts_val
        .get("reAuditRequired")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let in_critical_zone = facts_val
        .get("inCriticalZone")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let prod_dependents = facts_val
        .get("prodDependents")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let days_to_renewal = facts_val
        .get("daysToRenewal")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let bd = facts_val.get("breakdown");
    let y4_price_elasticity = bd.and_then(|b| b.get("y4PriceElasticity")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let x1_lock_in = bd.and_then(|b| b.get("x1LockIn")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let x2_market_scarcity = bd.and_then(|b| b.get("x2MarketScarcity")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let x3_customization_depth = bd.and_then(|b| b.get("x3CustomizationDepth")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let x4_compliance_fragility = bd.and_then(|b| b.get("x4ComplianceFragility")).and_then(|v| v.as_f64()).unwrap_or(0.0);

    let cost_allocation_id = rec_val
        .and_then(|r| r.get("costAllocationId"))
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let adapter_abstracted = rec_val
        .and_then(|r| r.get("adapterAbstracted"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let baseline_deps = rec_val
        .and_then(|r| r.pointer("/audit/contractMeta/baselineProdDependents"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let blind_variance_alert = rec_val
        .and_then(|r| r.pointer("/audit/contractMeta/blindScoreVarianceAlert"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let perceived_status = rec_val
        .and_then(|r| r.pointer("/audit/supplierPreference/perceivedStatus"))
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let wallet_share = rec_val
        .and_then(|r| r.pointer("/audit/supplierPreference/estimatedWalletShareTier"))
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let resolved_domain_tier = diag
        .get("resolvedDomainTier")
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let ballot_count = diag.get("ballotCount").and_then(|v| v.as_u64()).unwrap_or(0);
    let max_ballot_spread = diag
        .get("maxBallotSpread")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    history.record_sourcing_deps(key, prod_dependents, now);
    let dep_delta = history.sourcing_deps_delta(key);

    let dep_delta_vs_baseline = prod_dependents as i64 - baseline_deps as i64;
    let delta_str = dep_delta
        .filter(|d| *d != 0)
        .map(format_signed_int)
        .or_else(|| {
            (dep_delta_vs_baseline != 0)
                .then(|| format!("{} vs base", format_signed_int(dep_delta_vs_baseline)))
        });

    let mut drift_reasons = Vec::new();
    if windmill_mismatch {
        drift_reasons.push(format!(
            "Dutch Windmill mismatch (quadrant={quadrant} vs perceived={perceived_status})"
        ));
    }
    if re_audit_required {
        drift_reasons.push("Re-audit required".to_string());
    }
    if in_critical_zone {
        drift_reasons.push("Score in critical boundary [2.70-3.30]".to_string());
    }
    if blind_variance_alert {
        drift_reasons.push(format!(
            "Blind ballot variance alert (spread={max_ballot_spread:.2})"
        ));
    }

    let mut static_policy = Vec::new();
    let mut dynamic_signals = Vec::new();

    match detail.resource_type.as_str() {
        "vendor-saas" => {
            static_policy.push(pill(
                "quadrant",
                "Kraljic Quadrant",
                format!("{quadrant} (Y={final_y:.2} X={final_x:.2})"),
                Some("info"),
            ));
            static_policy.push(pill(
                "knockout",
                "5-Pt Knockout",
                if knockout_triggered { "Triggered" } else { "Clear" },
                Some(if knockout_triggered { "warn" } else { "ok" }),
            ));
            static_policy.push(pill("finops", "FinOps ID", cost_allocation_id, Some("info")));
            static_policy.push(pill("domainTier", "Domain Tier", resolved_domain_tier, Some("ok")));

            dynamic_signals.push(sig(
                "windmill",
                "Windmill Match",
                if windmill_mismatch { "Mismatch" } else { "Aligned" },
                None,
                Some(if windmill_mismatch { "alert" } else { "flat" }),
                Some(0),
                Some(if windmill_mismatch { "error" } else { "ok" }),
            ));
            dynamic_signals.push(sig(
                "renewal",
                "Renewal Countdown",
                format!("{days_to_renewal}d"),
                None,
                Some(if days_to_renewal <= 90 { "alert" } else { "flat" }),
                Some(0),
                Some(if days_to_renewal <= 90 { "warn" } else { "ok" }),
            ));
            dynamic_signals.push(sig(
                "prodDeps",
                "Prod Dependents",
                format!("{prod_dependents}/{baseline_deps}"),
                delta_str,
                Some(trend_from_i64(dep_delta.unwrap_or(dep_delta_vs_baseline))),
                Some(0),
                Some(if dep_delta_vs_baseline > 0 { "warn" } else { "ok" }),
            ));
            dynamic_signals.push(sig(
                "reAudit",
                "Re-Audit Gate",
                if re_audit_required { "Required" } else { "Current" },
                None,
                Some(if re_audit_required { "alert" } else { "flat" }),
                Some(0),
                Some(if re_audit_required { "warn" } else { "ok" }),
            ));
        }
        "commercial-sdk" => {
            if !adapter_abstracted {
                drift_reasons.push("X1 Adapter decoupling redline violated (direct SDK coupling)".to_string());
            }
            static_policy.push(pill(
                "adapterX1",
                "Adapter Redline (X1)",
                if adapter_abstracted {
                    format!("Abstracted (X1={x1_lock_in:.1})")
                } else {
                    format!("Direct Coupling (X1={x1_lock_in:.1})")
                },
                Some(if adapter_abstracted { "ok" } else { "error" }),
            ));
            static_policy.push(pill(
                "complianceX4",
                "Compliance (X4)",
                format!("X4={x4_compliance_fragility:.1}"),
                Some(if x4_compliance_fragility >= 4.5 { "warn" } else { "ok" }),
            ));
            static_policy.push(pill("quadrant", "Quadrant", quadrant, Some("info")));

            dynamic_signals.push(sig(
                "depExpansion",
                "Dependent Expansion",
                format!("{prod_dependents}/{baseline_deps} comps"),
                delta_str,
                Some(trend_from_i64(dep_delta.unwrap_or(dep_delta_vs_baseline))),
                Some(0),
                Some(if dep_delta_vs_baseline > 0 { "warn" } else { "ok" }),
            ));
            dynamic_signals.push(sig(
                "ballotSpread",
                "Blind Ballot Spread",
                format!("{ballot_count} ballots · Δ={max_ballot_spread:.2}"),
                None,
                Some(if blind_variance_alert { "alert" } else { "flat" }),
                Some(0),
                Some(if blind_variance_alert { "warn" } else { "ok" }),
            ));
            dynamic_signals.push(sig(
                "renewal",
                "Renewal Countdown",
                format!("{days_to_renewal}d"),
                None,
                Some("flat"),
                Some(0),
                Some(if days_to_renewal <= 90 { "warn" } else { "ok" }),
            ));
            dynamic_signals.push(sig(
                "reAudit",
                "Re-Audit",
                if re_audit_required { "Required" } else { "Current" },
                None,
                Some(if re_audit_required { "alert" } else { "flat" }),
                Some(0),
                Some(if re_audit_required { "warn" } else { "ok" }),
            ));
        }
        "managed-infra" => {
            static_policy.push(pill("domainLineage", "4-Layer Domain", resolved_domain_tier, Some("ok")));
            static_policy.push(pill("walletShare", "Wallet Share", wallet_share, Some("info")));
            static_policy.push(pill("quadrant", "Quadrant", quadrant, Some("info")));

            dynamic_signals.push(sig(
                "upstreamDeps",
                "Upstream Components",
                format!("{prod_dependents}/{baseline_deps}"),
                delta_str,
                Some(trend_from_i64(dep_delta.unwrap_or(dep_delta_vs_baseline))),
                Some(0),
                Some(if dep_delta_vs_baseline > 0 { "warn" } else { "ok" }),
            ));
            dynamic_signals.push(sig(
                "windmill",
                "Windmill Match",
                if windmill_mismatch { "Mismatch" } else { "Aligned" },
                None,
                Some(if windmill_mismatch { "alert" } else { "flat" }),
                Some(0),
                Some(if windmill_mismatch { "error" } else { "ok" }),
            ));
            dynamic_signals.push(sig(
                "criticalZone",
                "Critical [2.70-3.30]",
                if in_critical_zone { "In Zone" } else { "Clear" },
                None,
                Some(if in_critical_zone { "alert" } else { "flat" }),
                Some(0),
                Some(if in_critical_zone { "warn" } else { "ok" }),
            ));
            dynamic_signals.push(sig(
                "renewal",
                "Renewal Window",
                format!("{days_to_renewal}d"),
                None,
                Some("flat"),
                Some(0),
                Some(if days_to_renewal <= 90 { "warn" } else { "ok" }),
            ));
        }
        _ => {
            static_policy.push(pill(
                "elasticity",
                "Elasticity (Y4/X2)",
                format!("Y4={y4_price_elasticity:.1} / X2={x2_market_scarcity:.1}"),
                Some("info"),
            ));
            static_policy.push(pill(
                "customDepth",
                "Custom Depth (X3)",
                format!("X3={x3_customization_depth:.1}"),
                Some(if x3_customization_depth >= 4.0 { "warn" } else { "ok" }),
            ));
            static_policy.push(pill("quadrant", "Quadrant", quadrant, Some("info")));

            dynamic_signals.push(sig(
                "blindBallots",
                "Blind Ballots",
                format!("{ballot_count} ballots · spread={max_ballot_spread:.2}"),
                None,
                Some(if blind_variance_alert { "alert" } else { "flat" }),
                Some(0),
                Some(if blind_variance_alert { "warn" } else { "ok" }),
            ));
            dynamic_signals.push(sig(
                "depChanges",
                "Component Deps",
                format!("{prod_dependents}/{baseline_deps}"),
                delta_str,
                Some(trend_from_i64(dep_delta.unwrap_or(dep_delta_vs_baseline))),
                Some(0),
                Some(if dep_delta_vs_baseline > 0 { "warn" } else { "ok" }),
            ));
            dynamic_signals.push(sig(
                "reAudit",
                "Re-Audit",
                if re_audit_required { "Required" } else { "Current" },
                None,
                Some(if re_audit_required { "alert" } else { "flat" }),
                Some(0),
                Some(if re_audit_required { "warn" } else { "ok" }),
            ));
        }
    }

    let fp = format!(
        "{quadrant}|{prod_dependents}|{windmill_mismatch}|{re_audit_required}|{ballot_count}|{adapter_abstracted}"
    );
    let (last_changed_at, _) = history.track_entity_change(key, fp, now);
    let drift_detected = !drift_reasons.is_empty();

    EntitySummary {
        observation_mode: mode,
        cadence_tier: mode.cadence_tier().to_string(),
        headline: detail.summary_line.clone(),
        static_policy,
        dynamic_signals,
        drift_detected,
        drift_summary: drift_detected.then(|| drift_reasons.join("; ")),
        last_changed_at: Some(last_changed_at),
    }
}

/// Computes type-level aggregated `highlights` pills for all 16 Resource Types in
/// `ResourceTypeSummary` (`pitsctl types` & Backstage Directory Cards).
pub(crate) fn compute_type_highlights(
    resource_type: &str,
    matching: &[&PitsEntityDetail],
    live: &LiveTelemetry,
    history: &TelemetryHistory,
) -> Vec<SummaryPill> {
    let total = matching.len();
    let drifted = matching.iter().filter(|d| d.summary.drift_detected).count();
    match resource_type {
        "baremetal-host" => {
            let ready = matching
                .iter()
                .filter(|d| d.status_level == "ok")
                .count();
            let hook_nodes = matching
                .iter()
                .filter(|d| {
                    d.summary
                        .dynamic_signals
                        .iter()
                        .any(|s| s.key == "bootMode" && s.value != "talos-netboot")
                })
                .count();
            vec![
                pill("ready", "Ready", format!("{ready}/{total}"), Some(if ready == total { "ok" } else { "warn" })),
                pill("netbootGuard", "netboot-guard", "Enforced", Some("ok")),
                pill(
                    "bootDrift",
                    "Hook Drift",
                    hook_nodes.to_string(),
                    Some(if hook_nodes > 0 { "warn" } else { "ok" }),
                ),
            ]
        }
        "npu-accelerator" => {
            let dra_ok = matching.iter().filter(|d| d.status == "ReadyDraV1").count();
            let pending = matching
                .iter()
                .filter(|d| d.status == "HardwarePresentDriverPending")
                .count();
            let alloc: u64 = matching
                .iter()
                .filter_map(|d| d.entity.spec.get("allocatedCount").and_then(|v| v.as_u64()))
                .sum();
            let cap: u64 = matching
                .iter()
                .filter_map(|d| d.entity.spec.get("totalCount").and_then(|v| v.as_u64()))
                .sum::<u64>()
                .max(dra_ok as u64);
            let pct = if cap > 0 { (alloc as f64) * 100.0 / (cap as f64) } else { 0.0 };
            vec![
                pill("draReady", "DRA Ready", format!("{dra_ok}/{total}"), Some(if pending == 0 { "ok" } else { "warn" })),
                pill("driverPending", "Driver Pending", pending.to_string(), Some(if pending > 0 { "warn" } else { "ok" })),
                pill("occupancy", "NPU Occupancy", format!("{alloc}/{cap} ({pct:.0}%)"), Some("info")),
            ]
        }
        "gpu-accelerator" => {
            let alloc: u64 = matching
                .iter()
                .filter_map(|d| d.entity.spec.get("allocatedCount").and_then(|v| v.as_u64()))
                .sum();
            let cap: u64 = matching
                .iter()
                .filter_map(|d| {
                    d.entity
                        .spec
                        .get("totalCount")
                        .or_else(|| d.entity.spec.get("gpuCount"))
                        .and_then(|v| v.as_u64())
                })
                .sum();
            let pct = if cap > 0 { ((alloc as f64) * 100.0 / (cap as f64)).min(100.0) } else { 0.0 };
            let dp_only = matching
                .iter()
                .filter(|d| d.status == "DevicePluginOnly")
                .count();
            vec![
                pill("gpuOcc", "GPU Occupancy", format!("{alloc}/{cap} ({pct:.0}%)"), Some(if cap == 0 { "info" } else { "ok" })),
                pill("draCount", "DRA Slices", (total.saturating_sub(dp_only)).to_string(), Some("ok")),
                pill("dpFallback", "DP Fallback", dp_only.to_string(), Some(if dp_only > 0 { "warn" } else { "ok" })),
            ]
        }
        "virtual-machine" => {
            let running = matching.iter().filter(|d| d.status == "Running" || d.status_level == "ok").count();
            let host_drift = matching
                .iter()
                .filter(|d| d.entity.spec.get("hostDrifted").and_then(|v| v.as_bool()) == Some(true))
                .count();
            vec![
                pill("running", "Running", format!("{running}/{total}"), Some(if running == total { "ok" } else { "warn" })),
                pill("hostDrift", "Host Drift", host_drift.to_string(), Some(if host_drift > 0 { "warn" } else { "ok" })),
                pill("lock", "Workstation Lock", "Enabled", Some("ok")),
            ]
        }
        "edge-router" => {
            let rate = if let Some(d) = history.vpp_delta() {
                format!("{:.1}/{:.1} Mbps", d.rx_mbps, d.tx_mbps)
            } else if let Some(s) = history.latest_vpp_sample() {
                format!("Rx {}", format_bytes_human(s.rx_bytes))
            } else {
                "VPP Ready".to_string()
            };
            let drops = if let Some(d) = history.vpp_delta() {
                format!("{} (Δ{})", d.total_drops, format_signed_int(d.delta_drops))
            } else {
                "0".to_string()
            };
            let freshness = live
                .router_live
                .as_ref()
                .and_then(|r| r.verification_age_secs)
                .map(|a| format!("{a}s"))
                .unwrap_or_else(|| "<=30s".to_string());
            vec![
                pill("vppRate", "GoVPP Rate", rate, Some("info")),
                pill("vppDrops", "Drops", drops, Some(if drifted > 0 { "warn" } else { "ok" })),
                pill("freshness", "Verify Age", freshness, Some(if drifted > 0 { "warn" } else { "ok" })),
            ]
        }
        "pxe-boot-profile" => {
            let verified = live
                .pxe_asset_verify
                .values()
                .filter(|v| v.get("verified").and_then(|b| b.as_bool()) == Some(true))
                .count();
            vec![
                pill("policy", "Kernel Guard", "selinux=1 sig_enforce=1", Some("ok")),
                pill("sha256", "SHA-256 Verified", format!("{verified} sets"), Some("info")),
                pill("drift", "Drift", drifted.to_string(), Some(if drifted > 0 { "warn" } else { "ok" })),
            ]
        }
        "storage-pool" => {
            let usage = matching
                .iter()
                .find_map(|d| {
                    d.summary
                        .dynamic_signals
                        .iter()
                        .find(|s| s.key == "utilization")
                        .map(|s| s.value.clone())
                })
                .unwrap_or_else(|| "Baseline".to_string());
            let errs = live.btrfs.as_ref().map(extract_btrfs_error_total).unwrap_or(0);
            vec![
                pill("usage", "Pool Usage", usage, Some(if drifted > 0 { "warn" } else { "ok" })),
                pill("ioErr", "I/O Errors", errs.to_string(), Some(if errs > 0 { "warn" } else { "ok" })),
            ]
        }
        "san-volume" => {
            let exported = matching.iter().filter(|d| d.status_level == "ok").count();
            let cosi = matching
                .iter()
                .filter(|d| {
                    d.summary
                        .dynamic_signals
                        .iter()
                        .any(|s| s.key == "cosiMount" && s.value == "Attached")
                })
                .count();
            vec![
                pill("exported", "Exported", format!("{exported}/{total}"), Some(if exported == total { "ok" } else { "warn" })),
                pill("cosi", "COSI Attached", format!("{cosi}/{total}"), Some("info")),
                pill("nocow", "NOCOW +C", if drifted == 0 { "Verified" } else { "Check Drift" }, Some(if drifted == 0 { "ok" } else { "warn" })),
            ]
        }
        "ai-storage-workspace" => {
            let ready = matching.iter().filter(|d| d.status_level == "ok").count();
            let arch = matching
                .first()
                .and_then(|d| {
                    d.summary
                        .dynamic_signals
                        .iter()
                        .find(|s| s.key == "readyArchetypes")
                        .map(|s| s.value.clone())
                })
                .unwrap_or_else(|| "0/8".to_string());
            vec![
                pill("synced", "SyncedReady", format!("{ready}/{total}"), Some(if ready == total { "ok" } else { "warn" })),
                pill("archetypes", "Archetypes", arch, Some("info")),
            ]
        }
        "kubernetes-cluster" => {
            let healthy = matching.iter().filter(|d| d.status_level == "ok").count();
            let tot_gpus: u64 = matching
                .iter()
                .filter_map(|d| d.entity.spec.get("gpuCount").and_then(|v| v.as_u64()))
                .sum();
            let alloc_gpus: u64 = matching
                .iter()
                .filter_map(|d| d.entity.spec.get("allocatedGpus").and_then(|v| v.as_u64()))
                .sum();
            let gpu_pct = if tot_gpus > 0 {
                ((alloc_gpus as f64) * 100.0 / (tot_gpus as f64)).min(100.0)
            } else {
                0.0
            };
            vec![
                pill("clusters", "Healthy", format!("{healthy}/{total}"), Some(if healthy == total { "ok" } else { "warn" })),
                pill(
                    "gpuOcc",
                    "Fleet GPU Occ",
                    if tot_gpus > 0 {
                        format!("{alloc_gpus}/{tot_gpus} ({gpu_pct:.0}%)")
                    } else {
                        "0 GPUs".to_string()
                    },
                    Some("info"),
                ),
            ]
        }
        "cicd-pipeline" => {
            let viol = live
                .lab_cicd
                .as_ref()
                .map(|c| c.pods_on_netboot_004.len())
                .unwrap_or(0);
            let runners = live
                .lab_cicd
                .as_ref()
                .map(|c| {
                    format!(
                        "{}/{}",
                        c.runner_cpu_ready + c.runner_npu_ready + c.runner_gpu_ready,
                        c.runner_cpu_desired + c.runner_npu_desired + c.runner_gpu_desired
                    )
                })
                .unwrap_or_else(|| "5/5".to_string());
            vec![
                pill(
                    "nb004",
                    "netboot-004 Guard",
                    if viol == 0 { "0 Violations".to_string() } else { format!("{viol} VIOLATIONS") },
                    Some(if viol > 0 { "error" } else { "ok" }),
                ),
                pill("runners", "Runners Ready", runners, Some("ok")),
            ]
        }
        "auth-flow-bundle" => {
            let aligned = total.saturating_sub(drifted);
            vec![
                pill("aligned", "Flow Aligned", format!("{aligned}/{total}"), Some(if drifted == 0 { "ok" } else { "warn" })),
                pill("loa", "Policy", "LoA-2 FIDO2", Some("info")),
            ]
        }
        "vendor-saas" | "commercial-sdk" | "managed-infra" | "outsourced-dev" => {
            let mismatches = matching
                .iter()
                .filter(|d| {
                    d.entity
                        .metadata
                        .labels
                        .get("sourcing.company.io/windmill-mismatch")
                        .map(|v| v == "true")
                        .unwrap_or(false)
                })
                .count();
            let reaudits = matching
                .iter()
                .filter(|d| {
                    d.entity
                        .metadata
                        .labels
                        .get("sourcing.company.io/reaudit-required")
                        .map(|v| v == "true")
                        .unwrap_or(false)
                })
                .count();
            let mut pills = vec![
                pill(
                    "windmill",
                    "Windmill Mismatch",
                    mismatches.to_string(),
                    Some(if mismatches > 0 { "error" } else { "ok" }),
                ),
                pill(
                    "reAudit",
                    "Re-Audit Due",
                    reaudits.to_string(),
                    Some(if reaudits > 0 { "warn" } else { "ok" }),
                ),
            ];
            if resource_type == "commercial-sdk" {
                let adapted = matching
                    .iter()
                    .filter(|d| {
                        d.entity
                            .metadata
                            .annotations
                            .get("sourcing.company.io/adapter-abstracted")
                            .map(|v| v == "true")
                            .unwrap_or(false)
                    })
                    .count();
                pills.insert(
                    0,
                    pill(
                        "adapter",
                        "Adapter X1",
                        format!("{adapted}/{total}"),
                        Some(if adapted == total { "ok" } else { "error" }),
                    ),
                );
            }
            pills
        }
        _ => vec![pill("entities", "Entities", total.to_string(), Some("info"))],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::discovery::DiscoveredK8sResource;
    use crate::catalog::sourcing::SourcingStore;
    use crate::catalog::synthesizer::{
        synthesize_snapshot_from_inputs_with_history, CicdLiveSummary, ExtraClusterTelemetry,
    };
    use crate::config::ExtraClusterConfig;
    use crate::k8s::{
        parse_dra_claims_list, parse_pod_device_plugin_gpu_usage, DraClaimAllocation,
        DraDeviceSummary, DraSliceSummary, KubeNodeSummary,
    };
    use crate::router::RouterLiveSummary;
    use crate::vm::VirtualMachineSummary;
    use chrono::Duration as ChronoDuration;
    use serde_json::json;

    #[test]
    fn all_16_resource_types_have_dual_track_summaries_and_approved_observation_modes() {
        let now = Utc::now();
        let bundle = WorkspaceBundle::builtin_pits_baseline();
        let tmp = std::env::temp_dir().join(format!("pits-dual-track-16-{}.yaml", uuid::Uuid::new_v4()));
        let sourcing = SourcingStore::load_with_options(tmp, None, None, true);

        let mut live = LiveTelemetry::default();
        live.any_connected = true;
        live.extra_clusters.push(ExtraClusterTelemetry {
            config: ExtraClusterConfig {
                name: "work82-vcluster".to_string(),
                title: Some("work82 GPU DRA".to_string()),
                role: Some("vcluster-gpu-dra".to_string()),
                kubeconfig_path: "/tmp/vc.yaml".to_string(),
                context: None,
                via_agent_bridge: false,
            },
            summary: ClusterControlPlaneSummary {
                cluster: "work82-vcluster".to_string(),
                server_url: "https://100.83.142.15:18090".to_string(),
                via_agent_bridge: false,
                reachable: true,
                checked_at: now,
                git_version: Some("v1.36.6".to_string()),
                nodes: vec![KubeNodeSummary {
                    name: "cp0".to_string(),
                    ready: true,
                    kubelet_version: "v1.36.6".to_string(),
                    internal_ip: "10.98.35.85".to_string(),
                    roles: vec!["control-plane".to_string()],
                    custom_allocatable: BTreeMap::from([("nvidia.com/gpu".to_string(), "2".to_string())]),
                    retired_zero_resources: Vec::new(),
                }],
                running_pods: 10,
                total_pods: 12,
                bound_pvs: 2,
                npu_slices: Vec::new(),
                dra_slices: vec![DraSliceSummary {
                    slice_name: "cp0-nvidia".to_string(),
                    node_name: "cp0".to_string(),
                    driver: "gpu.nvidia.com".to_string(),
                    device_class: "gpu.nvidia.com".to_string(),
                    devices: vec![
                        DraDeviceSummary {
                            name: "gpu-0".to_string(),
                            vendor: Some("nvidia".to_string()),
                            model: Some("NVIDIA H20".to_string()),
                            product_name: Some("NVIDIA H20".to_string()),
                            architecture: Some("Hopper".to_string()),
                            uuid: Some("GPU-0000".to_string()),
                            pci_address: Some("0000:18:00.0".to_string()),
                            kernel_driver: Some("nvidia".to_string()),
                            driver_version: Some("580.126.20".to_string()),
                            memory: Some("97871Mi".to_string()),
                            attributes: BTreeMap::new(),
                        },
                        DraDeviceSummary {
                            name: "gpu-1".to_string(),
                            vendor: Some("nvidia".to_string()),
                            model: Some("NVIDIA H20".to_string()),
                            product_name: Some("NVIDIA H20".to_string()),
                            architecture: Some("Hopper".to_string()),
                            uuid: Some("GPU-0001".to_string()),
                            pci_address: Some("0000:19:00.0".to_string()),
                            kernel_driver: Some("nvidia".to_string()),
                            driver_version: Some("580.126.20".to_string()),
                            memory: Some("97871Mi".to_string()),
                            attributes: BTreeMap::new(),
                        },
                    ],
                }],
                dra_claims: vec![DraClaimAllocation {
                    namespace: "ai-train".to_string(),
                    name: "vllm-claim-0".to_string(),
                    driver: "gpu.nvidia.com".to_string(),
                    pool: "cp0".to_string(),
                    device: "gpu-0".to_string(),
                    node_name: Some("cp0".to_string()),
                    allocated: true,
                    consumer_pods: vec!["ai-train/vllm-0".to_string()],
                    consumer_count: 1,
                }],
                device_plugin_gpu_used: BTreeMap::new(),
                device_classes: vec!["gpu.nvidia.com".to_string()],
                error: None,
            },
        });
        live.discovered_k8s_resources = vec![
            DiscoveredK8sResource {
                cluster_name: "cluster-pits-lab".to_string(),
                cluster_ref: "resource:default/cluster-pits-lab".to_string(),
                api_version: "storage.ai.pits.io/v1alpha1".to_string(),
                kind: "AIStorageWorkspace".to_string(),
                namespace: "pits-cicd".to_string(),
                name: "pits-agent-space".to_string(),
                labels: BTreeMap::new(),
                annotations: BTreeMap::new(),
                owner_refs: Vec::new(),
                spec: json!({
                    "environment": "lab-rustfs",
                    "domain": "mcp.lab.martensite.dev",
                    "mcp": { "authMode": "keycloak-jwt" }
                }),
                status: json!({
                    "mcpEndpoint": "https://mcp.lab.martensite.dev/mcp",
                    "readyArchetypes": ["ws://", "mem://", "kb://"],
                    "conditions": [
                        { "type": "Synced", "status": "True" },
                        { "type": "Ready", "status": "True" }
                    ]
                }),
            },
            DiscoveredK8sResource {
                cluster_name: "cluster-pits-lab".to_string(),
                cluster_ref: "resource:default/cluster-pits-lab".to_string(),
                api_version: "iam.corp.io/v1alpha1".to_string(),
                kind: "AuthFlowBundle".to_string(),
                namespace: "iam-system".to_string(),
                name: "corp-fido2-browser".to_string(),
                labels: BTreeMap::new(),
                annotations: BTreeMap::new(),
                owner_refs: Vec::new(),
                spec: json!({
                    "realm": "corp",
                    "flowAlias": "corp-fido2-browser",
                    "bindAsRealmBrowserFlow": true
                }),
                status: json!({
                    "activeFlowAlias": "unexpected-browser-flow",
                    "conditions": [
                        { "type": "Synced", "status": "True" },
                        { "type": "Ready", "status": "True" }
                    ]
                }),
            },
        ];

        let mut hist = TelemetryHistory::default();
        hist.record_live_telemetry(&live, now);
        let snap = synthesize_snapshot_from_inputs_with_history(&bundle, None, &live, &sourcing, &mut hist, now);

        assert_eq!(snap.resource_types.len(), 16);
        let expected_modes = BTreeMap::from([
            ("pxe-boot-profile", ("policy-first", "slow-300s")),
            ("auth-flow-bundle", ("policy-first", "slow-300s")),
            ("baremetal-host", ("hybrid", "standard-60s")),
            ("npu-accelerator", ("hybrid", "standard-60s")),
            ("san-volume", ("hybrid", "standard-60s")),
            ("ai-storage-workspace", ("hybrid", "standard-60s")),
            ("gpu-accelerator", ("dynamic-first", "fast-15s")),
            ("virtual-machine", ("dynamic-first", "fast-15s")),
            ("edge-router", ("dynamic-first", "fast-15s")),
            ("storage-pool", ("dynamic-first", "fast-15s")),
            ("kubernetes-cluster", ("dynamic-first", "fast-15s")),
            ("cicd-pipeline", ("dynamic-first", "fast-15s")),
            ("vendor-saas", ("governance-event", "event-driven")),
            ("commercial-sdk", ("governance-event", "event-driven")),
            ("managed-infra", ("governance-event", "event-driven")),
            ("outsourced-dev", ("governance-event", "event-driven")),
        ]);

        for rt in &snap.resource_types {
            let (want_mode, want_cadence) = expected_modes
                .get(rt.resource_type.as_str())
                .expect("every resource type must be mapped");
            assert_eq!(rt.observation_mode, *want_mode, "mode for {}", rt.resource_type);
            assert_eq!(rt.cadence_tier, *want_cadence, "cadence for {}", rt.resource_type);
            assert!(!rt.highlights.is_empty(), "highlights missing for {}", rt.resource_type);
            assert!(rt.entity_count > 0, "expected >=1 entity for {}", rt.resource_type);
            assert_eq!(rt.policy_compliant_count + rt.drifted_count, rt.entity_count);
        }

        // Verify GPU DRA Claim occupancy: gpu-0 = 1/1 (100%), gpu-1 = 0/1 (0%), cluster = 1/2 (50%)
        let gpu0 = snap.details.get("default/work82-vcluster-cp0-gpu-0").expect("gpu-0");
        assert_eq!(gpu0.entity.spec.get("occupancyPct").and_then(|v| v.as_f64()), Some(100.0));
        assert!(gpu0.summary.dynamic_signals.iter().any(|s| s.key == "gpuOccupancy" && s.value == "1/1 (100%)"));
        assert!(gpu0.summary.dynamic_signals.iter().any(|s| s.key == "consumerPods" && s.value.contains("ai-train/vllm-0")));

        let gpu1 = snap.details.get("default/work82-vcluster-cp0-gpu-1").expect("gpu-1");
        assert_eq!(gpu1.entity.spec.get("occupancyPct").and_then(|v| v.as_f64()), Some(0.0));
        assert!(gpu1.summary.dynamic_signals.iter().any(|s| s.key == "gpuOccupancy" && s.value == "0/1 (0%)"));

        let vc = snap.details.get("default/work82-vcluster").expect("work82-vcluster");
        assert_eq!(vc.entity.spec.get("gpuOccupancyPct").and_then(|v| v.as_f64()), Some(50.0));
        assert!(vc.summary.dynamic_signals.iter().any(|s| s.key == "gpuOccupancy" && s.value == "1/2 (50%)"));

        // Verify AuthFlowBundle drift detection (`activeFlowAlias` vs `spec.flowAlias`)
        let auth = snap.details.get("iam-system/corp-fido2-browser").expect("auth flow bundle");
        assert!(auth.summary.drift_detected);
        assert!(auth.summary.drift_summary.as_deref().unwrap_or("").contains("unexpected-browser-flow"));

        // Verify AIStorageWorkspace drift detection (< 8 archetypes ready)
        let ai_ws = snap.details.get("pits-cicd/pits-agent-space").expect("ai storage workspace");
        assert!(ai_ws.summary.drift_detected);
        assert!(ai_ws.summary.dynamic_signals.iter().any(|s| s.key == "readyArchetypes" && s.value == "3/8"));
    }

    #[test]
    fn telemetry_history_ringbuffer_computes_vpp_rate_drops_btrfs_cluster_deltas_and_last_changed() {
        let t0 = Utc::now();
        let t1 = t0 + ChronoDuration::seconds(10);
        let t2 = t1 + ChronoDuration::seconds(10);
        let bundle = WorkspaceBundle::builtin_pits_baseline();
        let tmp = std::env::temp_dir().join(format!("pits-ringbuffer-{}.yaml", uuid::Uuid::new_v4()));
        let sourcing = SourcingStore::load_with_options(tmp, None, None, true);
        let mut hist = TelemetryHistory::default();

        // Sample 1 at t0
        let mut live0 = LiveTelemetry::default();
        live0.any_connected = true;
        live0.router_live = Some(RouterLiveSummary {
            checked_at: t0,
            edge_router_present: true,
            crossplane_paused: false,
            phase: "Applied".to_string(),
            ready: true,
            generation: 1,
            observed_generation: 1,
            last_verified_time: Some(t0.to_rfc3339()),
            verification_age_secs: Some(5),
            verification_fresh: true,
            host_audit: None,
            vpp_report: Some(json!({
                "version": "v25.02",
                "interfaces": [
                    { "name": "wan0", "linkUp": true, "adminUp": true },
                    { "name": "lan0", "linkUp": true, "adminUp": true }
                ],
                "counters": {
                    "sampledAt": t0.to_rfc3339(),
                    "totalRxPackets": 1000,
                    "totalRxBytes": 1_000_000,
                    "totalTxPackets": 500,
                    "totalTxBytes": 500_000,
                    "totalDrops": 2,
                    "totalErrors": 0
                }
            })),
        });
        live0.btrfs = Some(json!({
            "mountPath": "/srv/pits-storage/tank",
            "filesystemUuid": EXPECTED_BTRFS_UUID,
            "healthy": true,
            "totalBytes": 1_073_741_824_000u64,
            "usedBytes": 214_748_364_800u64,
            "freeBytes": 858_993_459_200u64,
            "readErrors": 0,
            "writeErrors": 0
        }));
        live0.lab_vms = Some(vec![VirtualMachineSummary {
            cluster: "pits-lab".to_string(),
            namespace: "pits-vms".to_string(),
            name: "win11-001".to_string(),
            printable_status: "Running".to_string(),
            ready: true,
            run_strategy: "Always".to_string(),
            node_name: Some("netboot-004".to_string()),
            guest_os: Some("Windows 11".to_string()),
            interfaces: Vec::new(),
            attached_disks: vec!["rootdisk".to_string()],
        }]);
        live0.lab_cicd = Some(CicdLiveSummary {
            labdevstack_present: true,
            labdevstack_synced: true,
            labdevstack_ready: true,
            cnpg_ready_instances: 2,
            cnpg_instances: 2,
            flux_repo_ready: true,
            flux_kust_ready: true,
            flux_revision: Some("main@sha1:1111111".to_string()),
            runner_cpu_ready: 3,
            runner_cpu_desired: 3,
            runner_npu_ready: 1,
            runner_npu_desired: 1,
            runner_gpu_ready: 1,
            runner_gpu_desired: 1,
            gitea_ready: true,
            zot_ready: true,
            attic_ready: true,
            pods_on_netboot_004: Vec::new(),
        });

        hist.record_live_telemetry(&live0, t0);
        let snap0 = synthesize_snapshot_from_inputs_with_history(&bundle, None, &live0, &sourcing, &mut hist, t0);
        let vm0 = snap0.details.get("default/win11-001").unwrap();
        assert!(!vm0.summary.drift_detected);
        assert_eq!(vm0.summary.last_changed_at, Some(t0));

        // Sample 2 at t1 (+10s): +12.5MB Rx (= 10.0 Mbps), +6.25MB Tx (= 5.0 Mbps), +3 drops, +10 MiB Btrfs, win11-001 migrated to netboot-001!
        let mut live1 = live0;
        live1.router_live.as_mut().unwrap().vpp_report = Some(json!({
            "version": "v25.02",
            "interfaces": [
                { "name": "wan0", "linkUp": true, "adminUp": true },
                { "name": "lan0", "linkUp": true, "adminUp": true }
            ],
            "counters": {
                "sampledAt": t1.to_rfc3339(),
                "totalRxPackets": 11_000,
                "totalRxBytes": 13_500_000,
                "totalTxPackets": 5_500,
                "totalTxBytes": 6_750_000,
                "totalDrops": 5,
                "totalErrors": 1
            }
        }));
        live1.btrfs = Some(json!({
            "mountPath": "/srv/pits-storage/tank",
            "filesystemUuid": EXPECTED_BTRFS_UUID,
            "healthy": false,
            "totalBytes": 1_073_741_824_000u64,
            "usedBytes": 214_748_364_800u64 + 10_485_760u64,
            "freeBytes": 858_993_459_200u64 - 10_485_760u64,
            "readErrors": 2,
            "writeErrors": 0
        }));
        live1.lab_vms.as_mut().unwrap()[0].node_name = Some("netboot-001".to_string());
        live1.lab_cicd.as_mut().unwrap().flux_revision = Some("main@sha1:2222222".to_string());

        hist.record_live_telemetry(&live1, t1);
        let snap1 = synthesize_snapshot_from_inputs_with_history(&bundle, None, &live1, &sourcing, &mut hist, t1);

        let rtr = snap1.details.get("default/pits-core-router").unwrap();
        let throughput = rtr.summary.dynamic_signals.iter().find(|s| s.key == "vppThroughput").unwrap();
        assert_eq!(throughput.value, "Rx 10.00 / Tx 5.00 Mbps");
        assert_eq!(throughput.delta.as_deref(), Some("1000/500 pps"));
        let drops = rtr.summary.dynamic_signals.iter().find(|s| s.key == "vppDropsErrors").unwrap();
        assert_eq!(drops.value, "5 drops / 1 err");
        assert_eq!(drops.delta.as_deref(), Some("Δ+3 / Δ+1"));
        assert!(rtr.summary.drift_detected);

        let pool = snap1.details.get("default/pits-data").unwrap();
        let usage = pool.summary.dynamic_signals.iter().find(|s| s.key == "utilization").unwrap();
        assert_eq!(usage.delta.as_deref(), Some("+10.0 MiB"));
        let io_err = pool.summary.dynamic_signals.iter().find(|s| s.key == "ioErrors").unwrap();
        assert_eq!(io_err.value, "2");
        assert_eq!(io_err.delta.as_deref(), Some("+2"));
        assert!(pool.summary.drift_detected);

        // win11-001 must NOT hardcode netboot-004 when migrated to netboot-001
        let vm1 = snap1.details.get("default/win11-001").unwrap();
        assert!(vm1.summary_line.contains("netboot-001"));
        assert!(vm1.summary_line.contains("10.89.20.21:30389"));
        assert!(vm1.summary.drift_detected);
        assert!(vm1.summary.drift_summary.as_deref().unwrap_or("").contains("netboot-004 -> netboot-001"));
        assert_eq!(vm1.summary.last_changed_at, Some(t1));

        // Sample 3 at t2 with identical VM host -> last_changed_at must remain t1 (not overwrite with t2)
        hist.record_live_telemetry(&live1, t2);
        let snap2 = synthesize_snapshot_from_inputs_with_history(&bundle, None, &live1, &sourcing, &mut hist, t2);
        let vm2 = snap2.details.get("default/win11-001").unwrap();
        assert_eq!(vm2.summary.last_changed_at, Some(t1));

        // RingBuffer caps at MAX_RING_SAMPLES (20)
        for i in 1..=25 {
            hist.record_vpp_sample(VppCounterSample {
                sampled_at: t2 + ChronoDuration::seconds(i * 15),
                rx_packets: 11_000 + (i as u64) * 100,
                rx_bytes: 13_500_000 + (i as u64) * 100_000,
                tx_packets: 5_500 + (i as u64) * 50,
                tx_bytes: 6_750_000 + (i as u64) * 50_000,
                drops: 5,
                errors: 1,
            });
        }
        assert_eq!(hist.vpp_sample_count(), MAX_RING_SAMPLES);
    }

    #[test]
    fn parses_k8s_dra_resourceclaims_and_device_plugin_gpu_pods() {
        let claims_json = json!({
            "items": [
                {
                    "metadata": { "namespace": "ml", "name": "h20-claim" },
                    "status": {
                        "allocation": {
                            "devices": {
                                "results": [
                                    { "driver": "gpu.nvidia.com", "pool": "cp0", "device": "gpu-0" },
                                    { "driver": "gpu.nvidia.com", "pool": "cp0", "device": "gpu-1" }
                                ]
                            },
                            "nodeSelector": {
                                "nodeSelectorTerms": [{
                                    "matchExpressions": [{
                                        "key": "kubernetes.io/hostname",
                                        "operator": "In",
                                        "values": ["cp0"]
                                    }]
                                }]
                            }
                        },
                        "reservedFor": [{ "resource": "pods", "name": "train-0" }]
                    }
                },
                {
                    "metadata": { "namespace": "ml", "name": "pending-claim" },
                    "status": {}
                }
            ]
        });
        let parsed = parse_dra_claims_list(&claims_json);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].node_name.as_deref(), Some("cp0"));
        assert_eq!(parsed[0].consumer_pods, vec!["ml/train-0".to_string()]);

        let pods_json = json!({
            "items": [
                {
                    "spec": {
                        "nodeName": "gpu-01",
                        "containers": [{
                            "resources": {
                                "limits": { "memory": "16Gi" },
                                "requests": { "nvidia.com/gpu": "2" }
                            }
                        }]
                    },
                    "status": { "phase": "Running" }
                },
                {
                    "spec": {
                        "nodeName": "gpu-01",
                        "containers": [{
                            "resources": {
                                "limits": { "nvidia.com/gpu": "1" }
                            }
                        }]
                    },
                    "status": { "phase": "Succeeded" }
                }
            ]
        });
        let dp = parse_pod_device_plugin_gpu_usage(&pods_json);
        assert_eq!(dp.get("gpu-01"), Some(&2));
    }
}

