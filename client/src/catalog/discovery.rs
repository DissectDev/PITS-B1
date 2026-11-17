//! Unified Crossplane (XRD / XR / Claim / Provider) & Kubernetes (`Node` / `Service`)
//! auto-discovery engine for the PITS Backstage Software Catalog.
//!
//! Preserves PITS's domain-specific `resourceType` taxonomy (`edge-router`, `virtual-machine`,
//! `san-volume`, `baremetal-host`, etc.) instead of collapsing everything into generic types,
//! while adopting standard `terasky.backstage.io/*` and `crossplane.io/*` annotations so
//! cluster-native objects, Crossplane claims/composites, and external `ExternalName` Services /
//! Crossplane `Provider`s are discovered automatically without modifying `Cargo.toml`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::catalog::model::{AttributeSection, PitsEntityDetail};
use crate::catalog::sourcing::{
    canonical_component_ref, canonical_system_ref, DiscoveryCandidateRecord, SourcingResourceRecord,
    SOURCING_RESOURCE_TYPES,
};
use crate::catalog::synthesizer::{
    attr, kube_get_retry, make_catalog_entity, rel, LiveTelemetry, RESOURCE_TYPE_METAS,
};
use crate::k8s::KubeNodeSummary;
use crate::pxe::analyze_hook_inventory;
use crate::transport::KubeClient;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct K8sOwnerRefSummary {
    pub api_version: String,
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub uid: String,
    #[serde(default)]
    pub controller: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredK8sResource {
    pub cluster_name: String,
    pub cluster_ref: String,
    pub api_version: String,
    pub kind: String,
    pub namespace: String,
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
    pub owner_refs: Vec<K8sOwnerRefSummary>,
    pub spec: Value,
    pub status: Value,
}

struct GvkMappingRule {
    group: &'static str,
    kind: &'static str,
    backstage_kind: &'static str,
    resource_type: &'static str,
    category: &'static str,
}

const GVK_MAPPINGS: &[GvkMappingRule] = &[
    GvkMappingRule {
        group: "platform.edge.router",
        kind: "EdgeRouter",
        backstage_kind: "Resource",
        resource_type: "edge-router",
        category: "Network & Boot",
    },
    GvkMappingRule {
        group: "edge.router",
        kind: "Router",
        backstage_kind: "Resource",
        resource_type: "edge-router",
        category: "Network & Boot",
    },
    GvkMappingRule {
        group: "kubevirt.io",
        kind: "VirtualMachine",
        backstage_kind: "Resource",
        resource_type: "virtual-machine",
        category: "Compute & Accelerators",
    },
    GvkMappingRule {
        group: "kubevirt.io",
        kind: "VirtualMachineInstance",
        backstage_kind: "Resource",
        resource_type: "virtual-machine",
        category: "Compute & Accelerators",
    },
    GvkMappingRule {
        group: "pkg.crossplane.io",
        kind: "Provider",
        backstage_kind: "Resource",
        resource_type: "managed-infra",
        category: "Sourcing Governance",
    },
    GvkMappingRule {
        group: "apiextensions.crossplane.io",
        kind: "CompositeResourceDefinition",
        backstage_kind: "Component",
        resource_type: "crossplane-xrd",
        category: "Component",
    },
    GvkMappingRule {
        group: "",
        kind: "Service",
        backstage_kind: "Component",
        resource_type: "service",
        category: "Component",
    },
];

pub fn sanitize_k8s_slug(raw: &str) -> String {
    let slug: String = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<&str>>()
        .join("-");
    if slug.is_empty() {
        "discovered-k8s-unit".to_string()
    } else {
        slug
    }
}

fn api_group_of(api_version: &str) -> &str {
    match api_version.split_once('/') {
        Some((group, _)) => group,
        None => "",
    }
}

fn json_string_map(val: Option<&Value>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(obj) = val.and_then(|v| v.as_object()) else {
        return out;
    };
    for (k, v) in obj {
        if let Some(s) = v.as_str() {
            out.insert(k.clone(), s.to_string());
        }
    }
    out
}

fn annotation_or_label<'a>(obj: &'a DiscoveredK8sResource, key: &str) -> Option<&'a str> {
    obj.annotations
        .get(key)
        .or_else(|| obj.labels.get(key))
        .map(String::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Parses a Kubernetes `*List` JSON payload into normalized [`DiscoveredK8sResource`] records.
pub fn parse_k8s_list_items(
    cluster_name: &str,
    cluster_ref: &str,
    api_version_fallback: &str,
    kind_fallback: &str,
    list_val: &Value,
) -> Vec<DiscoveredK8sResource> {
    let Some(items) = list_val.get("items").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = item
            .pointer("/metadata/name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let namespace = item
            .pointer("/metadata/namespace")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("default")
            .to_string();
        let api_version = item
            .get("apiVersion")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(api_version_fallback)
            .to_string();
        let kind = item
            .get("kind")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(kind_fallback)
            .to_string();

        let owner_refs = item
            .pointer("/metadata/ownerReferences")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|o| {
                        let o_name = o.get("name").and_then(|v| v.as_str())?;
                        Some(K8sOwnerRefSummary {
                            api_version: o
                                .get("apiVersion")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            kind: o
                                .get("kind")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            name: o_name.to_string(),
                            uid: o
                                .get("uid")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            controller: o
                                .get("controller")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        out.push(DiscoveredK8sResource {
            cluster_name: cluster_name.to_string(),
            cluster_ref: cluster_ref.to_string(),
            api_version,
            kind,
            namespace,
            name: name.to_string(),
            labels: json_string_map(item.pointer("/metadata/labels")),
            annotations: json_string_map(item.pointer("/metadata/annotations")),
            owner_refs,
            spec: item.get("spec").cloned().unwrap_or(Value::Null),
            status: item.get("status").cloned().unwrap_or(Value::Null),
        });
    }
    out
}

/// Extracts dynamically served `(api_path, kind_hint)` endpoints from Crossplane XRDs.
pub fn extract_xrd_collection_paths(xrd_list: &Value) -> Vec<(String, String, String)> {
    let Some(items) = xrd_list.get("items").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut endpoints = Vec::new();
    let mut seen = BTreeSet::new();

    for item in items {
        let Some(group) = item.pointer("/spec/group").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(plural) = item.pointer("/spec/names/plural").and_then(|v| v.as_str()) else {
            continue;
        };
        let kind_name = item
            .pointer("/spec/names/kind")
            .and_then(|v| v.as_str())
            .unwrap_or(plural);
        let version = item
            .pointer("/spec/versions")
            .and_then(|v| v.as_array())
            .and_then(|vers| {
                vers.iter().find_map(|ver| {
                    let served = ver.get("served").and_then(|b| b.as_bool()).unwrap_or(true);
                    if served {
                        ver.get("name").and_then(|n| n.as_str())
                    } else {
                        None
                    }
                })
            })
            .unwrap_or("v1alpha1");

        let xr_path = format!("/apis/{group}/{version}/{plural}");
        if seen.insert(xr_path.clone()) {
            endpoints.push((
                xr_path,
                format!("{group}/{version}"),
                kind_name.to_string(),
            ));
        }

        if let Some(claim_plural) = item
            .pointer("/spec/claimNames/plural")
            .and_then(|v| v.as_str())
        {
            let claim_kind = item
                .pointer("/spec/claimNames/kind")
                .and_then(|v| v.as_str())
                .unwrap_or(claim_plural);
            let claim_path = format!("/apis/{group}/{version}/{claim_plural}");
            if seen.insert(claim_path.clone()) {
                endpoints.push((
                    claim_path,
                    format!("{group}/{version}"),
                    claim_kind.to_string(),
                ));
            }
        }
    }

    endpoints
}

/// Checks `@terasky/backstage-plugin-kubernetes-ingestor`, Crossplane, and automatic non-system K8s `Service` gates.
pub fn should_ingest_k8s_object(obj: &DiscoveredK8sResource) -> bool {
    if obj
        .annotations
        .get("terasky.backstage.io/exclude-from-catalog")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return false;
    }

    if obj
        .annotations
        .get("terasky.backstage.io/add-to-catalog")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || obj
            .annotations
            .contains_key("terasky.backstage.io/component-type")
    {
        return true;
    }

    let is_crossplane = annotation_or_label(obj, "crossplane.io/composite").is_some()
        || annotation_or_label(obj, "crossplane.io/claim-name").is_some()
        || annotation_or_label(obj, "crossplane.io/external-name").is_some()
        || obj.api_version.contains("crossplane.io")
        || matches!(
            api_group_of(&obj.api_version),
            "platform.edge.router" | "edge.router"
        );
    if is_crossplane {
        return true;
    }

    if obj.kind == "Service" {
        let is_k8s_system = matches!(
            obj.namespace.as_str(),
            "kube-system" | "kube-public" | "kube-node-lease"
        ) || (obj.namespace == "default" && obj.name == "kubernetes");
        return !is_k8s_system;
    }

    false
}

/// Resolves `(BackstageKind, spec.type, category)` uniformly using annotations first, then GVK table.
pub fn resolve_catalog_type(obj: &DiscoveredK8sResource) -> (&'static str, String, &'static str) {
    if let Some(explicit) = obj.annotations.get("terasky.backstage.io/component-type") {
        let low = explicit.trim().to_ascii_lowercase();
        if let Some(meta) = RESOURCE_TYPE_METAS.iter().find(|m| m.resource_type == low) {
            return ("Resource", meta.resource_type.to_string(), meta.category);
        }
        if !low.is_empty() {
            return ("Component", low, "Component");
        }
    }

    let group = api_group_of(&obj.api_version);
    if let Some(rule) = GVK_MAPPINGS
        .iter()
        .find(|r| r.group == group && r.kind.eq_ignore_ascii_case(&obj.kind))
    {
        return (
            rule.backstage_kind,
            rule.resource_type.to_string(),
            rule.category,
        );
    }

    let kind_low = obj.kind.to_ascii_lowercase();
    if kind_low.contains("router") || kind_low.contains("gateway") {
        return ("Resource", "edge-router".to_string(), "Network & Boot");
    }
    if kind_low.contains("virtualmachine") || kind_low == "vm" {
        return (
            "Resource",
            "virtual-machine".to_string(),
            "Compute & Accelerators",
        );
    }
    if kind_low.contains("san") || kind_low.contains("volume") {
        return ("Resource", "san-volume".to_string(), "Storage Fabric");
    }
    if kind_low.contains("cluster") {
        return (
            "Resource",
            "kubernetes-cluster".to_string(),
            "Control Planes",
        );
    }

    if annotation_or_label(obj, "crossplane.io/claim-name").is_some() {
        return ("Component", "crossplane-claim".to_string(), "Component");
    }
    if annotation_or_label(obj, "crossplane.io/composite").is_some()
        || !obj.owner_refs.is_empty()
    {
        return ("Component", "crossplane-xr".to_string(), "Component");
    }

    ("Component", "service".to_string(), "Component")
}

/// Returns the canonical baseline `details` key if `obj` maps cleanly onto a hardcoded baseline node/entity.
pub fn resolve_baseline_alias(obj: &DiscoveredK8sResource) -> Option<&'static str> {
    if obj
        .annotations
        .contains_key("terasky.backstage.io/name")
    {
        return None;
    }
    let group = api_group_of(&obj.api_version);
    if matches!(group, "platform.edge.router" | "edge.router")
        && obj.namespace == "edge"
        && obj.name == "pits-core"
    {
        return Some("default/pits-core-router");
    }
    if group == "kubevirt.io" && matches!(obj.kind.as_str(), "VirtualMachine" | "VirtualMachineInstance")
    {
        return match obj.name.as_str() {
            "win11-001" => Some("default/win11-001"),
            "access-lab" => Some("default/access-lab"),
            _ => None,
        };
    }
    None
}

/// Enriches an existing baseline entity with live Crossplane & `terasky.backstage.io/*` annotations.
pub fn enrich_existing_detail_from_k8s(
    detail: &mut PitsEntityDetail,
    obj: &DiscoveredK8sResource,
) {
    for (k, v) in &obj.annotations {
        if k.starts_with("crossplane.io/") || k.starts_with("terasky.backstage.io/") {
            detail.entity.metadata.annotations.insert(k.clone(), v.clone());
        }
    }
    for (k, v) in &obj.labels {
        if k.starts_with("crossplane.io/") {
            detail.entity.metadata.labels.insert(k.clone(), v.clone());
        }
    }
    detail.entity.metadata.annotations.insert(
        "pits.io/k8s-gvk".to_string(),
        format!("{}/{}", obj.api_version, obj.kind),
    );
    detail
        .entity
        .metadata
        .annotations
        .insert("pits.io/cluster".to_string(), obj.cluster_name.clone());

    if let Some(raw_deps) = obj.annotations.get("terasky.backstage.io/dependsOn") {
        for dep in raw_deps.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let target = if dep.contains(':') {
                dep.to_ascii_lowercase()
            } else {
                format!("resource:default/{}", dep.to_ascii_lowercase())
            };
            if !detail
                .entity
                .relations
                .iter()
                .any(|r| r.relation_type == "dependsOn" && r.target_ref == target)
            {
                detail.entity.relations.push(rel("dependsOn", &target));
            }
        }
    }

    for tag in ["crossplane", "k8s-discovered"] {
        if !detail.entity.metadata.tags.iter().any(|t| t == tag) {
            detail.entity.metadata.tags.push(tag.to_string());
        }
    }
    if let Some(diag) = detail.diagnostics.as_object_mut() {
        diag.insert("k8sLiveObject".to_string(), json!({
            "apiVersion": obj.api_version,
            "kind": obj.kind,
            "namespace": obj.namespace,
            "name": obj.name,
            "cluster": obj.cluster_name,
            "ownerReferences": obj.owner_refs,
        }));
    }
}

/// Synthesizes a `baremetal-host` [`PitsEntityDetail`] for any live K8s node missing from `bundle.nodes`.
pub fn build_discovered_k8s_node_detail(
    cluster_ref: &str,
    cluster_name: &str,
    cluster_tag: &str,
    k8s_node: &KubeNodeSummary,
    now: DateTime<Utc>,
) -> (String, PitsEntityDetail) {
    let entity_name = sanitize_k8s_slug(&k8s_node.name);
    let key = format!("default/{entity_name}");
    let (status, status_level) = if k8s_node.ready {
        ("Ready", "ok")
    } else {
        ("NotReady", "warn")
    };
    let roles_str = if k8s_node.roles.is_empty() {
        "worker".to_string()
    } else {
        k8s_node.roles.join(", ")
    };
    let summary = format!(
        "Auto-discovered Kubernetes Node ({}) · {} · roles=[{}] · {}",
        cluster_name,
        if k8s_node.kubelet_version.is_empty() {
            "kubelet"
        } else {
            &k8s_node.kubelet_version
        },
        roles_str,
        if k8s_node.internal_ip.is_empty() {
            "no InternalIP"
        } else {
            &k8s_node.internal_ip
        }
    );

    let mut entity = make_catalog_entity(
        "Resource",
        "default",
        &entity_name,
        Some(&format!("{} (Discovered K8s Node)", k8s_node.name)),
        Some(&summary),
        "baremetal-host",
        status,
        status_level,
        "Compute & Accelerators",
        true,
        vec![
            "baremetal".to_string(),
            "k8s-discovered".to_string(),
            cluster_tag.to_string(),
        ],
        json!({
            "role": roles_str,
            "cluster": cluster_name,
            "internalIp": k8s_node.internal_ip,
            "kubeletVersion": k8s_node.kubelet_version,
            "autoDiscovered": true,
        }),
        vec![rel("partOf", cluster_ref)],
    );
    entity.metadata.annotations.insert(
        "terasky.backstage.io/component-type".to_string(),
        "baremetal-host".to_string(),
    );
    entity.metadata.annotations.insert(
        "terasky.backstage.io/system".to_string(),
        "pits-infrastructure".to_string(),
    );
    entity.metadata.annotations.insert(
        "pits.io/discovery-source".to_string(),
        "kubernetes-node".to_string(),
    );
    entity
        .metadata
        .annotations
        .insert("pits.io/cluster".to_string(), cluster_name.to_string());

    let sections = vec![AttributeSection {
        id: "discovered-node".to_string(),
        title: "Discovered Kubernetes Node Telemetry".to_string(),
        items: vec![
            attr("nodeName", "Kubernetes Node Name", &k8s_node.name, Some("ok"), true),
            attr("cluster", "Control Plane", cluster_name, Some("info"), true),
            attr("internalIp", "Internal IP", &k8s_node.internal_ip, Some("ok"), true),
            attr("kubeletVersion", "Kubelet Version", &k8s_node.kubelet_version, None, true),
            attr("roles", "Node Roles", &roles_str, None, true),
        ],
    }];

    let mut manifests = BTreeMap::new();
    manifests.insert(
        "discovered-k8s-node.json".to_string(),
        serde_json::to_string_pretty(&k8s_node).unwrap_or_default(),
    );

    let detail = PitsEntityDetail {
        entity,
        resource_type: "baremetal-host".to_string(),
        status: status.to_string(),
        status_level: status_level.to_string(),
        summary_line: summary,
        refreshed_at: now,
        live_connected: true,
        attribute_sections: sections,
        actions: Vec::new(),
        manifests,
        diagnostics: json!({
            "discoverySource": "kubernetes-node",
            "k8sNode": k8s_node,
        }),
    };
    (key, detail)
}

#[derive(Default)]
struct DiscoveredBaremetalSeed {
    raw_name: String,
    cluster_ref: String,
    cluster_name: String,
    cluster_tag: String,
    k8s_node: Option<KubeNodeSummary>,
    tink_mac: Option<String>,
    tink_ip: Option<String>,
    tink_enrollment: Option<String>,
    hook_raw: Option<Value>,
}

fn extract_ip_from_ipxe(contents: &str) -> Option<String> {
    for token in contents.split_whitespace() {
        if let Some(rest) = token.strip_prefix("ip=") {
            let candidate = rest.split(':').next().unwrap_or("").trim();
            if candidate.split('.').count() == 4
                && candidate.chars().all(|c| c.is_ascii_digit() || c == '.')
            {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

/// Discovers unregistered bare-metal hosts across all live control-plane sources:
/// Kubernetes `/api/v1/nodes` (Core + Lab), Tinkerbell `Hardware` CRs (`tink-system`),
/// and `pits-agent` Hook hardware inventory reports.
pub(crate) fn collect_live_discovered_nodes(
    live: &LiveTelemetry,
    existing_details: &BTreeMap<String, PitsEntityDetail>,
    now: DateTime<Utc>,
) -> Vec<(String, PitsEntityDetail)> {
    if live.core_cluster.is_none()
        && live.lab_cluster.is_none()
        && live.tink_hardware.is_none()
        && live.hook_inventory.is_empty()
    {
        return Vec::new();
    }
    let mut seeds: BTreeMap<String, DiscoveredBaremetalSeed> = BTreeMap::new();

    for (cp_opt, cluster_ref, cluster_name, tag) in [
        (
            &live.core_cluster,
            "resource:default/cluster-pits-core",
            "cluster-pits-core",
            "core-plane",
        ),
        (
            &live.lab_cluster,
            "resource:default/cluster-pits-lab",
            "cluster-pits-lab",
            "lab-vlan20",
        ),
    ] {
        if let Some(cp) = cp_opt {
            for node in &cp.nodes {
                let slug = sanitize_k8s_slug(&node.name);
                if existing_details.contains_key(&format!("default/{slug}")) {
                    continue;
                }
                let entry = seeds.entry(slug).or_insert_with(|| DiscoveredBaremetalSeed {
                    raw_name: node.name.clone(),
                    cluster_ref: cluster_ref.to_string(),
                    cluster_name: cluster_name.to_string(),
                    cluster_tag: tag.to_string(),
                    ..Default::default()
                });
                entry.k8s_node = Some(node.clone());
            }
        }
    }

    if let Some(items) = live
        .tink_hardware
        .as_ref()
        .and_then(|v| v.get("items"))
        .and_then(|v| v.as_array())
    {
        for hw in items {
            let Some(name) = hw
                .pointer("/metadata/name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let slug = sanitize_k8s_slug(name);
            if existing_details.contains_key(&format!("default/{slug}")) {
                continue;
            }
            let mac = hw
                .pointer("/spec/interfaces/0/dhcp/mac")
                .and_then(|v| v.as_str())
                .map(|s| s.to_ascii_lowercase());
            let ipxe_ip = hw
                .pointer("/spec/interfaces/0/netboot/ipxe/contents")
                .and_then(|v| v.as_str())
                .and_then(extract_ip_from_ipxe);
            let dhcp_ip = hw
                .pointer("/spec/interfaces/0/dhcp/ip/address")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let enrollment = hw
                .pointer("/metadata/labels/pits.io~1enrollment")
                .and_then(|v| v.as_str())
                .map(str::to_string);

            let entry = seeds.entry(slug).or_insert_with(|| DiscoveredBaremetalSeed {
                raw_name: name.to_string(),
                cluster_ref: "resource:default/cluster-pits-lab".to_string(),
                cluster_name: "cluster-pits-lab".to_string(),
                cluster_tag: "lab-vlan20".to_string(),
                ..Default::default()
            });
            entry.tink_mac = mac;
            entry.tink_ip = ipxe_ip.or(dhcp_ip);
            entry.tink_enrollment = enrollment;
        }
    }

    for (dev_name, raw_inv) in &live.hook_inventory {
        let slug = sanitize_k8s_slug(dev_name);
        if existing_details.contains_key(&format!("default/{slug}")) {
            continue;
        }
        let entry = seeds.entry(slug).or_insert_with(|| DiscoveredBaremetalSeed {
            raw_name: dev_name.clone(),
            cluster_ref: "resource:default/cluster-pits-lab".to_string(),
            cluster_name: "cluster-pits-lab".to_string(),
            cluster_tag: "lab-vlan20".to_string(),
            ..Default::default()
        });
        entry.hook_raw = Some(raw_inv.clone());
    }

    let mut out = Vec::with_capacity(seeds.len());
    for (slug, seed) in seeds {
        if let (Some(k8s_node), None, None) = (&seed.k8s_node, &seed.tink_mac, &seed.hook_raw) {
            out.push(build_discovered_k8s_node_detail(
                &seed.cluster_ref,
                &seed.cluster_name,
                &seed.cluster_tag,
                k8s_node,
                now,
            ));
            continue;
        }

        let hook_analysis = seed
            .hook_raw
            .as_ref()
            .and_then(|r| analyze_hook_inventory(r).ok());
        let retail_model = hook_analysis
            .as_ref()
            .map(|h| h.resolved_model.clone())
            .unwrap_or_else(|| "Discovered Baremetal Node".to_string());
        let logical_cpus = seed
            .hook_raw
            .as_ref()
            .and_then(|r| r.get("logicalCPUs"))
            .and_then(|v| v.as_u64());
        let dmi_uuid = hook_analysis
            .as_ref()
            .and_then(|h| h.dmi_product_uuid.clone())
            .unwrap_or_else(|| "-".to_string());
        let node_ip = seed
            .k8s_node
            .as_ref()
            .map(|n| n.internal_ip.clone())
            .filter(|s| !s.is_empty())
            .or(seed.tink_ip.clone())
            .unwrap_or_else(|| "-".to_string());
        let mac_str = seed.tink_mac.clone().unwrap_or_else(|| "-".to_string());

        let (status, status_level) = match &seed.k8s_node {
            Some(n) if n.ready => ("Ready", "ok"),
            Some(_) => ("NotReady", "warn"),
            None if seed.tink_mac.is_some() && seed.hook_raw.is_some() => ("EnrolledLive", "ok"),
            None => ("DiscoveredTinkerbell", "info"),
        };

        let mut sources = Vec::new();
        if seed.k8s_node.is_some() {
            sources.push("k8s-node");
        }
        if seed.tink_mac.is_some() {
            sources.push("tinkerbell-hardware");
        }
        if seed.hook_raw.is_some() {
            sources.push("hook-inventory");
        }
        let source_label = sources.join("+");

        let summary = format!(
            "{retail_model}{} · {} · MAC {mac_str} · Auto-discovered ({source_label})",
            logical_cpus.map(|c| format!(" ({c} vCPUs)")).unwrap_or_default(),
            node_ip,
        );
        let key = format!("default/{slug}");
        let mut entity = make_catalog_entity(
            "Resource",
            "default",
            &slug,
            Some(&format!("{} ({retail_model})", seed.raw_name)),
            Some(&summary),
            "baremetal-host",
            status,
            status_level,
            "Compute & Accelerators",
            true,
            vec![
                "baremetal".to_string(),
                "auto-discovered".to_string(),
                seed.cluster_tag.clone(),
            ],
            json!({
                "role": seed.tink_enrollment.as_deref().unwrap_or("discovered-baremetal-host"),
                "cluster": seed.cluster_name,
                "retailModel": retail_model,
                "nodeIp": node_ip,
                "pxeMac": mac_str,
                "productUuid": dmi_uuid,
                "discoverySources": sources,
                "autoDiscovered": true,
            }),
            vec![rel("partOf", &seed.cluster_ref)],
        );
        entity.metadata.annotations.insert(
            "terasky.backstage.io/component-type".to_string(),
            "baremetal-host".to_string(),
        );
        entity.metadata.annotations.insert(
            "pits.io/discovery-source".to_string(),
            source_label.clone(),
        );

        let mut items = vec![
            attr("deviceName", "Node / Hardware Name", &seed.raw_name, Some("ok"), true),
            attr("retailModel", "Resolved Hardware Model", &retail_model, Some("ok"), false),
            attr("discoverySource", "Live Discovery Sources", &source_label, Some("info"), true),
            attr("nodeIp", "Observed IP", &node_ip, Some("ok"), true),
            attr("pxeMac", "Tinkerbell PXE MAC", &mac_str, None, true),
            attr("productUuid", "DMI Product UUID", &dmi_uuid, None, true),
        ];
        if let Some(cpus) = logical_cpus {
            items.push(attr("logicalCpus", "Logical CPUs", cpus.to_string(), None, true));
        }

        let sections = vec![AttributeSection {
            id: "discovered-baremetal".to_string(),
            title: "Multi-Source Baremetal Discovery (Tinkerbell + Hook + K8s)".to_string(),
            items,
        }];

        let detail = PitsEntityDetail {
            entity,
            resource_type: "baremetal-host".to_string(),
            status: status.to_string(),
            status_level: status_level.to_string(),
            summary_line: summary,
            refreshed_at: now,
            live_connected: true,
            attribute_sections: sections,
            actions: Vec::new(),
            manifests: BTreeMap::new(),
            diagnostics: json!({
                "discoverySources": source_label,
                "tinkerbellEnrollment": seed.tink_enrollment,
                "hookAnalysis": hook_analysis,
                "k8sNode": seed.k8s_node,
            }),
        };
        out.push((key, detail));
    }

    out
}

fn evaluate_k8s_reconcile_status(obj: &DiscoveredK8sResource) -> (&'static str, &'static str) {
    if obj.annotations.get("crossplane.io/paused").map(String::as_str) == Some("true") {
        return ("Paused", "warn");
    }
    if let Some(conds) = obj.status.get("conditions").and_then(|v| v.as_array()) {
        let ready = conds
            .iter()
            .find(|c| c.get("type").and_then(|v| v.as_str()) == Some("Ready"))
            .and_then(|c| c.get("status").and_then(|v| v.as_str()));
        if let Some(r) = ready {
            return if r == "True" {
                ("SyncedReady", "ok")
            } else {
                ("ReconcilePending", "warn")
            };
        }
    }
    ("Active", "ok")
}

/// Synthesizes a Backstage `Resource` or `Component` [`PitsEntityDetail`] for an ingested K8s/Crossplane object.
pub fn build_discovered_k8s_entity_detail(
    obj: &DiscoveredK8sResource,
    now: DateTime<Utc>,
) -> (String, PitsEntityDetail) {
    let (backstage_kind, resolved_type, category) = resolve_catalog_type(obj);
    let raw_name = obj
        .annotations
        .get("terasky.backstage.io/name")
        .map(String::as_str)
        .unwrap_or(&obj.name);
    let entity_name = sanitize_k8s_slug(raw_name);
    let target_ns = if obj.namespace.trim().is_empty() {
        "default".to_string()
    } else {
        sanitize_k8s_slug(&obj.namespace)
    };
    let key = format!("{target_ns}/{entity_name}");
    let title = obj
        .annotations
        .get("terasky.backstage.io/title")
        .cloned()
        .unwrap_or_else(|| format!("{} ({}/{})", obj.name, obj.kind, obj.cluster_name));
    let owner = obj
        .annotations
        .get("terasky.backstage.io/owner")
        .cloned()
        .unwrap_or_else(|| "group:default/operators".to_string());
    let system_raw = obj
        .annotations
        .get("terasky.backstage.io/system")
        .map(String::as_str)
        .unwrap_or("pits-infrastructure");
    let system_ref = canonical_system_ref(system_raw);
    let system_name = system_ref
        .split_once('/')
        .map(|(_, s)| s.to_string())
        .unwrap_or_else(|| "pits-infrastructure".to_string());
    let lifecycle = obj
        .annotations
        .get("terasky.backstage.io/lifecycle")
        .cloned()
        .unwrap_or_else(|| "production".to_string());

    let (status, status_level) = evaluate_k8s_reconcile_status(obj);
    let claim_name = annotation_or_label(obj, "crossplane.io/claim-name").unwrap_or("-");
    let composite_name = annotation_or_label(obj, "crossplane.io/composite").unwrap_or("-");
    let summary = format!(
        "Discovered {} ({}) in {}/{} · type={} · status={}",
        obj.kind, obj.api_version, obj.cluster_name, obj.namespace, resolved_type, status
    );

    let mut relations = vec![
        rel("ownedBy", &owner),
        rel("partOf", &system_ref),
        rel("partOf", &obj.cluster_ref),
    ];
    if let Some(raw_deps) = obj.annotations.get("terasky.backstage.io/dependsOn") {
        for dep in raw_deps.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let target = if dep.contains(':') {
                dep.to_ascii_lowercase()
            } else {
                canonical_component_ref(dep)
            };
            relations.push(rel("dependsOn", &target));
        }
    }
    if composite_name != "-" {
        relations.push(rel(
            "dependsOn",
            &format!("component:{}/{}", target_ns, sanitize_k8s_slug(composite_name)),
        ));
    }

    let mut tags = vec![
        "k8s-discovered".to_string(),
        resolved_type.clone(),
        obj.cluster_name.clone(),
    ];
    if obj.api_version.contains("crossplane.io")
        || claim_name != "-"
        || composite_name != "-"
        || matches!(
            api_group_of(&obj.api_version),
            "platform.edge.router" | "edge.router"
        )
    {
        tags.push("crossplane".to_string());
    }

    let mut entity = make_catalog_entity(
        backstage_kind,
        &target_ns,
        &entity_name,
        Some(&title),
        Some(&summary),
        &resolved_type,
        status,
        status_level,
        category,
        true,
        tags,
        json!({
            "type": resolved_type,
            "lifecycle": lifecycle,
            "owner": owner,
            "system": system_name,
            "k8sApiVersion": obj.api_version,
            "k8sKind": obj.kind,
            "k8sNamespace": obj.namespace,
            "k8sName": obj.name,
            "cluster": obj.cluster_name,
            "autoDiscovered": true,
        }),
        relations,
    );

    for (k, v) in &obj.annotations {
        if k.starts_with("crossplane.io/") || k.starts_with("terasky.backstage.io/") {
            entity.metadata.annotations.insert(k.clone(), v.clone());
        }
    }
    for (k, v) in &obj.labels {
        if k.starts_with("crossplane.io/") {
            entity.metadata.labels.insert(k.clone(), v.clone());
        }
    }
    entity.metadata.annotations.insert(
        "pits.io/discovery-source".to_string(),
        "crossplane-k8s-connector".to_string(),
    );
    entity.metadata.annotations.insert(
        "pits.io/k8s-gvk".to_string(),
        format!("{}/{}", obj.api_version, obj.kind),
    );

    let sections = vec![AttributeSection {
        id: "crossplane-k8s-connector".to_string(),
        title: "Crossplane & Kubernetes Connector Metadata".to_string(),
        items: vec![
            attr("gvk", "API Version / Kind", format!("{}/{}", obj.api_version, obj.kind), Some("info"), true),
            attr("clusterRef", "Cluster / Namespace", format!("{}/{}", obj.cluster_name, obj.namespace), Some("ok"), true),
            attr("claimName", "Crossplane Claim", claim_name, None, true),
            attr("composite", "Crossplane Composite (XR)", composite_name, None, true),
            attr("reconcileStatus", "Sync / Ready State", status, Some(status_level), false),
        ],
    }];

    let mut manifests = BTreeMap::new();
    manifests.insert(
        "discovered-k8s-object.json".to_string(),
        serde_json::to_string_pretty(&json!({
            "apiVersion": obj.api_version,
            "kind": obj.kind,
            "metadata": {
                "namespace": obj.namespace,
                "name": obj.name,
                "labels": obj.labels,
                "annotations": obj.annotations,
                "ownerReferences": obj.owner_refs,
            },
            "spec": obj.spec,
            "status": obj.status,
        }))
        .unwrap_or_default(),
    );

    let detail = PitsEntityDetail {
        entity,
        resource_type: resolved_type,
        status: status.to_string(),
        status_level: status_level.to_string(),
        summary_line: summary,
        refreshed_at: now,
        live_connected: true,
        attribute_sections: sections,
        actions: Vec::new(),
        manifests,
        diagnostics: json!({
            "discoverySource": "crossplane-k8s-connector",
            "apiVersion": obj.api_version,
            "kind": obj.kind,
            "cluster": obj.cluster_name,
            "ownerReferences": obj.owner_refs,
        }),
    };
    (key, detail)
}

/// Converts discovered Crossplane `Provider`s and K8s `ExternalName` Services into sourcing queue candidates.
pub fn extract_sourcing_candidates_from_k8s(
    objects: &[DiscoveredK8sResource],
    registered_units: &BTreeMap<String, SourcingResourceRecord>,
) -> Vec<DiscoveryCandidateRecord> {
    let now_iso = Utc::now().to_rfc3339();
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();

    for obj in objects {
        let is_provider = api_group_of(&obj.api_version) == "pkg.crossplane.io"
            && obj.kind.eq_ignore_ascii_case("Provider");
        let external_name = if obj.kind == "Service"
            && obj
                .spec
                .get("type")
                .and_then(|v| v.as_str())
                .map(|t| t.eq_ignore_ascii_case("ExternalName"))
                .unwrap_or(false)
        {
            obj.spec
                .get("externalName")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        } else {
            None
        };

        if !is_provider && external_name.is_none() {
            continue;
        }

        let (cid, pkg_or_endpoint, ecosystem, suggested_type, note) = if is_provider {
            let pkg = obj
                .spec
                .get("package")
                .and_then(|v| v.as_str())
                .unwrap_or(&obj.name)
                .to_string();
            (
                format!("crossplane-provider:{}", sanitize_k8s_slug(&obj.name)),
                pkg.clone(),
                "crossplane-provider",
                "managed-infra",
                format!(
                    "Active Crossplane Provider '{}' ({}) discovered on cluster {}; audit vendor/infrastructure lock-in.",
                    obj.name, pkg, obj.cluster_name
                ),
            )
        } else {
            let host = external_name.unwrap_or(&obj.name);
            (
                format!("k8s-external-svc:{}-{}", sanitize_k8s_slug(&obj.namespace), sanitize_k8s_slug(&obj.name)),
                host.to_string(),
                "k8s-external-service",
                "vendor-saas",
                format!(
                    "Kubernetes ExternalName Service '{}/{}' -> '{}' on {}; verify vendor SaaS dependency & adapter boundary.",
                    obj.namespace, obj.name, host, obj.cluster_name
                ),
            )
        };

        if !seen.insert(cid.clone()) {
            continue;
        }

        let slug = sanitize_k8s_slug(&obj.name);
        let promoted_unit_name = registered_units
            .keys()
            .find(|n| n.eq_ignore_ascii_case(&slug) || n.contains(&slug))
            .cloned();
        let status = if promoted_unit_name.is_some() {
            "promoted".to_string()
        } else {
            "pending".to_string()
        };
        let suggested = if SOURCING_RESOURCE_TYPES.contains(&suggested_type) {
            suggested_type.to_string()
        } else {
            "managed-infra".to_string()
        };
        let system = obj
            .annotations
            .get("terasky.backstage.io/system")
            .cloned()
            .unwrap_or_else(|| "pits-infrastructure".to_string());

        out.push(DiscoveryCandidateRecord {
            candidate_id: cid,
            package_or_endpoint: pkg_or_endpoint,
            ecosystem: ecosystem.to_string(),
            suggested_type: suggested,
            detected_in_path: format!("k8s://{}/{}/{}", obj.cluster_name, obj.namespace, obj.name),
            inferred_system: system,
            inferred_components: vec!["component:default/edge-ingress-proxy".to_string()],
            adapter_detected: obj
                .annotations
                .get("sourcing.company.io/adapter-abstracted")
                .map(|v| v == "true")
                .unwrap_or(false),
            monolith_risk_note: note,
            status,
            promoted_unit_name,
            discovered_at: now_iso.clone(),
            sync_pending: true,
        });
    }

    out
}

/// Concurrently probes Kubernetes Services, Crossplane XRDs/Providers, and EdgeRouter CRs.
pub(crate) async fn discover_k8s_crossplane_fast(
    kc: &KubeClient,
    cluster_name: &str,
    cluster_ref: &str,
    sem: Option<&Arc<Semaphore>>,
) -> Vec<DiscoveredK8sResource> {
    let sem_ref = sem.map(Arc::as_ref);
    let (svc_res, xrd_res, prov_res, er_res, rtr_res) = tokio::join!(
        kube_get_retry(kc, "/api/v1/services", None, sem_ref),
        kube_get_retry(
            kc,
            "/apis/apiextensions.crossplane.io/v1/compositeresourcedefinitions",
            None,
            sem_ref
        ),
        kube_get_retry(kc, "/apis/pkg.crossplane.io/v1/providers", None, sem_ref),
        kube_get_retry(kc, "/apis/platform.edge.router/v1alpha1/edgerouters", None, sem_ref),
        kube_get_retry(kc, "/apis/edge.router/v1alpha1/routers", None, sem_ref),
    );

    let mut discovered = Vec::new();
    if let Ok(ref v) = svc_res {
        discovered.extend(parse_k8s_list_items(
            cluster_name,
            cluster_ref,
            "v1",
            "Service",
            v,
        ));
    }
    if let Ok(ref v) = prov_res {
        discovered.extend(parse_k8s_list_items(
            cluster_name,
            cluster_ref,
            "pkg.crossplane.io/v1",
            "Provider",
            v,
        ));
    }
    if let Ok(ref v) = er_res {
        discovered.extend(parse_k8s_list_items(
            cluster_name,
            cluster_ref,
            "platform.edge.router/v1alpha1",
            "EdgeRouter",
            v,
        ));
    }
    if let Ok(ref v) = rtr_res {
        discovered.extend(parse_k8s_list_items(
            cluster_name,
            cluster_ref,
            "edge.router/v1alpha1",
            "Router",
            v,
        ));
    }

    if let Ok(ref xrd_val) = xrd_res {
        discovered.extend(parse_k8s_list_items(
            cluster_name,
            cluster_ref,
            "apiextensions.crossplane.io/v1",
            "CompositeResourceDefinition",
            xrd_val,
        ));
        let dynamic_endpoints: Vec<(String, String, String)> = extract_xrd_collection_paths(xrd_val)
            .into_iter()
            .filter(|(p, _, _)| {
                p != "/apis/platform.edge.router/v1alpha1/edgerouters"
                    && p != "/apis/edge.router/v1alpha1/routers"
            })
            .take(16)
            .collect();

        let mut join_set = JoinSet::new();
        for (path, api_ver, kind_hint) in dynamic_endpoints {
            let kc_c = kc.clone();
            let sem_c = sem.cloned();
            join_set.spawn(async move {
                let res = kube_get_retry(&kc_c, &path, None, sem_c.as_deref()).await;
                (api_ver, kind_hint, res)
            });
        }
        while let Some(joined) = join_set.join_next().await {
            if let Ok((api_ver, kind_hint, Ok(val))) = joined {
                discovered.extend(parse_k8s_list_items(
                    cluster_name,
                    cluster_ref,
                    &api_ver,
                    &kind_hint,
                    &val,
                ));
            }
        }
    }

    discovered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::sourcing::SourcingStore;
    use crate::catalog::synthesizer::{synthesize_snapshot_from_inputs, LiveTelemetry};
    use crate::config::{ResolvedKubeEndpoint, WorkspaceBundle};
    use crate::k8s::ClusterControlPlaneSummary;
    use axum::{routing::get, Json, Router};

    #[test]
    fn dynamic_live_nodes_and_terasky_crossplane_annotations_synthesize_cleanly() {
        let now = Utc::now();
        let bundle = WorkspaceBundle::builtin_pits_baseline();
        let p = std::env::temp_dir().join(format!("pits-disc-test-{}.yaml", uuid::Uuid::new_v4()));
        let sourcing = SourcingStore::load_with_options(p, None, None, true);

        let mut live = LiveTelemetry::default();
        live.any_connected = true;
        live.lab_cluster = Some(ClusterControlPlaneSummary {
            cluster: "pits-lab".to_string(),
            server_url: "https://10.89.20.21:6443".to_string(),
            via_agent_bridge: true,
            reachable: true,
            checked_at: now,
            git_version: Some("v1.37.1".to_string()),
            nodes: vec![KubeNodeSummary {
                name: "worker-epyc-05".to_string(),
                ready: true,
                kubelet_version: "v1.37.1".to_string(),
                internal_ip: "10.89.20.25".to_string(),
                roles: vec!["worker".to_string(), "gpu".to_string()],
                custom_allocatable: BTreeMap::new(),
                retired_zero_resources: Vec::new(),
            }],
            running_pods: 12,
            total_pods: 12,
            bound_pvs: 0,
            npu_slices: Vec::new(),
            error: None,
        });

        live.discovered_k8s_resources = vec![
            // 1. Baseline EdgeRouter enrichment (no duplicate entity)
            DiscoveredK8sResource {
                cluster_name: "cluster-pits-core".to_string(),
                cluster_ref: "resource:default/cluster-pits-core".to_string(),
                api_version: "platform.edge.router/v1alpha1".to_string(),
                kind: "EdgeRouter".to_string(),
                namespace: "edge".to_string(),
                name: "pits-core".to_string(),
                labels: BTreeMap::new(),
                annotations: BTreeMap::from([
                    ("crossplane.io/composite".to_string(), "edge-pits-core-xr".to_string()),
                    (
                        "terasky.backstage.io/source-file-url".to_string(),
                        "https://git.local/edgerouter.yaml".to_string(),
                    ),
                ]),
                owner_refs: Vec::new(),
                spec: json!({}),
                status: json!({}),
            },
            // 2. Crossplane Claim mapped directly to `virtual-machine` via `terasky.backstage.io/component-type`
            DiscoveredK8sResource {
                cluster_name: "cluster-pits-lab".to_string(),
                cluster_ref: "resource:default/cluster-pits-lab".to_string(),
                api_version: "infra.pits.io/v1alpha1".to_string(),
                kind: "WorkstationClaim".to_string(),
                namespace: "dev-ai".to_string(),
                name: "gpu-dev-ws".to_string(),
                labels: BTreeMap::from([(
                    "crossplane.io/claim-name".to_string(),
                    "gpu-dev-ws".to_string(),
                )]),
                annotations: BTreeMap::from([
                    (
                        "terasky.backstage.io/component-type".to_string(),
                        "virtual-machine".to_string(),
                    ),
                    (
                        "terasky.backstage.io/system".to_string(),
                        "pits-infrastructure".to_string(),
                    ),
                    (
                        "terasky.backstage.io/dependsOn".to_string(),
                        "resource:default/worker-epyc-05".to_string(),
                    ),
                ]),
                owner_refs: Vec::new(),
                spec: json!({}),
                status: json!({ "conditions": [{ "type": "Ready", "status": "True" }] }),
            },
            // 3. Standard non-system K8s Service (even without `terasky.backstage.io/add-to-catalog`) -> Component (service)
            DiscoveredK8sResource {
                cluster_name: "cluster-pits-core".to_string(),
                cluster_ref: "resource:default/cluster-pits-core".to_string(),
                api_version: "v1".to_string(),
                kind: "Service".to_string(),
                namespace: "payments".to_string(),
                name: "fraud-scorer-svc".to_string(),
                labels: BTreeMap::new(),
                annotations: BTreeMap::new(),
                owner_refs: Vec::new(),
                spec: json!({ "type": "ClusterIP" }),
                status: Value::Null,
            },
            // 3b. Built-in kube-system Service must be excluded automatically
            DiscoveredK8sResource {
                cluster_name: "cluster-pits-core".to_string(),
                cluster_ref: "resource:default/cluster-pits-core".to_string(),
                api_version: "v1".to_string(),
                kind: "Service".to_string(),
                namespace: "kube-system".to_string(),
                name: "rke2-coredns".to_string(),
                labels: BTreeMap::new(),
                annotations: BTreeMap::new(),
                owner_refs: Vec::new(),
                spec: json!({ "type": "ClusterIP" }),
                status: Value::Null,
            },
            // 4. Opted-out Service must be excluded
            DiscoveredK8sResource {
                cluster_name: "cluster-pits-core".to_string(),
                cluster_ref: "resource:default/cluster-pits-core".to_string(),
                api_version: "v1".to_string(),
                kind: "Service".to_string(),
                namespace: "kube-system".to_string(),
                name: "internal-metrics".to_string(),
                labels: BTreeMap::new(),
                annotations: BTreeMap::from([
                    ("terasky.backstage.io/add-to-catalog".to_string(), "true".to_string()),
                    (
                        "terasky.backstage.io/exclude-from-catalog".to_string(),
                        "true".to_string(),
                    ),
                ]),
                owner_refs: Vec::new(),
                spec: json!({ "type": "ClusterIP" }),
                status: Value::Null,
            },
            // 5. ExternalName Service & Crossplane Provider -> Sourcing candidates
            DiscoveredK8sResource {
                cluster_name: "cluster-pits-core".to_string(),
                cluster_ref: "resource:default/cluster-pits-core".to_string(),
                api_version: "v1".to_string(),
                kind: "Service".to_string(),
                namespace: "payments".to_string(),
                name: "stripe-api-egress".to_string(),
                labels: BTreeMap::new(),
                annotations: BTreeMap::new(),
                owner_refs: Vec::new(),
                spec: json!({ "type": "ExternalName", "externalName": "api.stripe.com" }),
                status: Value::Null,
            },
            DiscoveredK8sResource {
                cluster_name: "cluster-pits-core".to_string(),
                cluster_ref: "resource:default/cluster-pits-core".to_string(),
                api_version: "pkg.crossplane.io/v1".to_string(),
                kind: "Provider".to_string(),
                namespace: "crossplane-system".to_string(),
                name: "provider-aws-ec2".to_string(),
                labels: BTreeMap::new(),
                annotations: BTreeMap::new(),
                owner_refs: Vec::new(),
                spec: json!({ "package": "xpkg.upbound.io/upbound/provider-aws-ec2:v1.12.0" }),
                status: json!({ "conditions": [{ "type": "Healthy", "status": "True" }] }),
            },
        ];

        let sourcing_candidates =
            extract_sourcing_candidates_from_k8s(&live.discovered_k8s_resources, &BTreeMap::new());
        assert_eq!(sourcing_candidates.len(), 2);
        assert!(sourcing_candidates
            .iter()
            .any(|c| c.ecosystem == "crossplane-provider" && c.suggested_type == "managed-infra"));
        assert!(sourcing_candidates
            .iter()
            .any(|c| c.ecosystem == "k8s-external-service" && c.package_or_endpoint == "api.stripe.com"));

        let snap = synthesize_snapshot_from_inputs(&bundle, None, &live, &sourcing, now);

        // Node auto-discovery added `default/worker-epyc-05` as `baremetal-host`
        let new_node = snap
            .details
            .get("default/worker-epyc-05")
            .expect("discovered K8s node entity");
        assert_eq!(new_node.resource_type, "baremetal-host");
        assert_eq!(new_node.status, "Ready");

        // Baseline EdgeRouter enriched without duplicate creation
        let router_count = snap
            .entities
            .iter()
            .filter(|e| e.metadata.name == "pits-core-router")
            .count();
        assert_eq!(router_count, 1);
        let router_ent = snap.details.get("default/pits-core-router").unwrap();
        assert_eq!(
            router_ent
                .entity
                .metadata
                .annotations
                .get("crossplane.io/composite")
                .map(String::as_str),
            Some("edge-pits-core-xr")
        );

        // Crossplane Claim preserves `virtual-machine` type and links symmetrically to `worker-epyc-05`
        let claim_detail = snap
            .details
            .get("dev-ai/gpu-dev-ws")
            .expect("Crossplane WorkstationClaim entity");
        assert_eq!(claim_detail.entity.kind, "Resource");
        assert_eq!(claim_detail.resource_type, "virtual-machine");
        assert_eq!(claim_detail.status, "SyncedReady");
        assert!(new_node
            .entity
            .relations
            .iter()
            .any(|r| r.relation_type == "dependencyOf" && r.target_ref == "resource:dev-ai/gpu-dev-ws"));

        // Non-system Service ingested without manual annotation; kube-system & opted-out Services skipped
        assert!(snap.details.contains_key("payments/fraud-scorer-svc"));
        assert!(!snap.details.contains_key("kube-system/rke2-coredns"));
        assert!(!snap.details.contains_key("kube-system/internal-metrics"));
    }

    #[test]
    fn discovers_netboot_005_from_tinkerbell_hook_and_workspace_contract_even_when_lab_k8s_is_502() {
        let now = Utc::now();
        let p = std::env::temp_dir().join(format!("pits-nb5-test-{}.yaml", uuid::Uuid::new_v4()));
        let sourcing = SourcingStore::load_with_options(p, None, None, true);

        // A. Live discovery using baseline bundle (netboot-001..004 only) + unreachable lab K8s (`502`)
        //    must still discover `netboot-005` from `live.tink_hardware` + `live.hook_inventory`!
        let baseline_bundle = WorkspaceBundle::builtin_pits_baseline();
        let mut live = LiveTelemetry::default();
        live.any_connected = true;
        live.tink_hardware = Some(json!({
            "items": [{
                "metadata": {
                    "name": "netboot-005",
                    "namespace": "tink-system",
                    "labels": { "pits.io/enrollment": "san-validation" }
                },
                "spec": {
                    "interfaces": [{
                        "dhcp": {
                            "ip": { "address": "10.89.20.123" },
                            "mac": "00:1b:21:bd:05:2d"
                        },
                        "netboot": {
                            "ipxe": {
                                "contents": "#!ipxe\nkernel http://10.89.20.10:7173/talos/netboot-005/vmlinuz ip=10.89.20.25::10.89.20.1:255.255.255.0:netboot-005:enx001b21bd052d:off\n"
                            }
                        }
                    }]
                }
            }]
        }));
        live.hook_inventory.insert(
            "netboot-005".to_string(),
            json!({
                "schemaVersion": 1,
                "approvedForDiskMutation": false,
                "logicalCPUs": 112,
                "identity": {
                    "systemVendor": "Giga Computing",
                    "productName": "MS33-CP0-000",
                    "productUUID": "e9184000-7983-11ec-8000-74563cb7eb7a"
                },
                "pci": [],
                "disks": []
            }),
        );

        let snap_live = synthesize_snapshot_from_inputs(&baseline_bundle, None, &live, &sourcing, now);
        let nb5 = snap_live
            .details
            .get("default/netboot-005")
            .expect("netboot-005 discovered from Tinkerbell Hardware + Hook inventory");
        assert_eq!(nb5.resource_type, "baremetal-host");
        assert_eq!(nb5.status, "EnrolledLive");
        assert!(nb5.summary_line.contains("Giga Computing MS33-CP0-000"));
        assert!(nb5.summary_line.contains("10.89.20.25"));

        // B. Dynamic `*.contract.json` scan in WorkspaceBundle::load_from_edge_router discovers `netboot-005` & RTX 5090
        let tmp_ws = std::env::temp_dir().join(format!("pits-ws-nb5-{}", uuid::Uuid::new_v4()));
        let dev_dir = tmp_ws.join("deploy").join("tinkerbell").join("devices");
        std::fs::create_dir_all(&dev_dir).unwrap();
        std::fs::write(
            dev_dir.join("netboot-005.contract.json"),
            json!({
                "deviceID": "netboot-005",
                "productUUID": "e9184000-7983-11ec-8000-74563cb7eb7a",
                "pxeMACs": ["00:1b:21:bd:05:2d"],
                "network": { "address": "10.89.20.25" },
                "san": { "targetAddress": "10.89.20.18", "systemVolumeBytes": 70866960384i64, "userBackingAlignedBytes": 20000002048i64 },
                "accelerator": {
                    "model": "NVIDIA GeForce RTX 5090",
                    "architecture": "Blackwell",
                    "pciAddress": "0000:94:00.0",
                    "pciID": "10de:2b85",
                    "kernelDriver": "nvidia",
                    "draDeviceClass": "gpu.nvidia.com",
                    "draDriver": "registry.k8s.io/dra-driver-nvidia/dra-driver-nvidia-gpu:v0.5.1",
                    "draValidated": true,
                    "vramMiB": 32607
                }
            })
            .to_string(),
        )
        .unwrap();

        let loaded_bundle = WorkspaceBundle::load_from_edge_router(&tmp_ws).unwrap();
        let _ = std::fs::remove_dir_all(&tmp_ws);
        assert!(loaded_bundle.nodes.iter().any(|n| n.device_id == "netboot-005"));

        let snap_ws = synthesize_snapshot_from_inputs(
            &loaded_bundle,
            None,
            &LiveTelemetry::default(),
            &sourcing,
            now,
        );
        assert!(snap_ws.details.contains_key("default/netboot-005"));
        let nb5_gpu = snap_ws
            .details
            .get("default/netboot-005-npu")
            .expect("netboot-005 RTX 5090 accelerator entity");
        assert!(nb5_gpu.summary_line.contains("NVIDIA GeForce RTX 5090"));
        assert!(nb5_gpu.summary_line.contains("gpu.nvidia.com"));
    }

    #[tokio::test]
    async fn loopback_mock_k8s_and_crossplane_xrd_discovery_end_to_end() {
        let app = Router::new()
            .route(
                "/api/v1/services",
                get(|| async {
                    Json(json!({
                        "apiVersion": "v1",
                        "kind": "ServiceList",
                        "items": [
                            {
                                "metadata": {
                                    "namespace": "apps",
                                    "name": "checkout-api",
                                    "annotations": {
                                        "terasky.backstage.io/add-to-catalog": "true"
                                    }
                                },
                                "spec": { "type": "ClusterIP" }
                            }
                        ]
                    }))
                }),
            )
            .route(
                "/apis/apiextensions.crossplane.io/v1/compositeresourcedefinitions",
                get(|| async {
                    Json(json!({
                        "apiVersion": "apiextensions.crossplane.io/v1",
                        "kind": "CompositeResourceDefinitionList",
                        "items": [
                            {
                                "metadata": { "name": "xworkstations.infra.pits.io" },
                                "spec": {
                                    "group": "infra.pits.io",
                                    "names": { "kind": "XWorkstation", "plural": "xworkstations" },
                                    "claimNames": { "kind": "Workstation", "plural": "workstations" },
                                    "versions": [{ "name": "v1alpha1", "served": true, "referenceable": true }]
                                }
                            }
                        ]
                    }))
                }),
            )
            .route(
                "/apis/infra.pits.io/v1alpha1/workstations",
                get(|| async {
                    Json(json!({
                        "apiVersion": "infra.pits.io/v1alpha1",
                        "kind": "WorkstationList",
                        "items": [
                            {
                                "metadata": {
                                    "namespace": "lab-users",
                                    "name": "cad-station-02",
                                    "annotations": {
                                        "crossplane.io/claim-name": "cad-station-02",
                                        "terasky.backstage.io/component-type": "virtual-machine"
                                    }
                                },
                                "status": {
                                    "conditions": [{ "type": "Ready", "status": "True" }]
                                }
                            }
                        ]
                    }))
                }),
            )
            .route(
                "/apis/infra.pits.io/v1alpha1/xworkstations",
                get(|| async { Json(json!({ "items": [] })) }),
            )
            .route(
                "/apis/pkg.crossplane.io/v1/providers",
                get(|| async {
                    Json(json!({
                        "apiVersion": "pkg.crossplane.io/v1",
                        "kind": "ProviderList",
                        "items": [
                            {
                                "metadata": { "name": "provider-tailscale" },
                                "spec": { "package": "xpkg.upbound.io/crossplane-contrib/provider-tailscale:v0.5.0" }
                            }
                        ]
                    }))
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback mock k8s");
        let addr = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let kc = KubeClient::from_endpoint(
            "pits-lab",
            ResolvedKubeEndpoint {
                context_name: "mock".to_string(),
                server_url: format!("http://{addr}"),
                tls_server_name: None,
                ca_pem: None,
                client_cert_pem: None,
                client_key_pem: None,
                bearer_token: None,
                insecure_skip_tls_verify: true,
            },
            None,
        )
        .expect("build loopback KubeClient");

        let found = discover_k8s_crossplane_fast(
            &kc,
            "cluster-pits-lab",
            "resource:default/cluster-pits-lab",
            None,
        )
        .await;
        server_task.abort();

        assert!(found.iter().any(|o| o.kind == "Service" && o.name == "checkout-api"));
        assert!(found
            .iter()
            .any(|o| o.kind == "Provider" && o.name == "provider-tailscale"));
        let ws = found
            .iter()
            .find(|o| o.name == "cad-station-02")
            .expect("dynamic XRD claim workstation discovered");
        let (bs_kind, rtype, _) = resolve_catalog_type(ws);
        assert_eq!(bs_kind, "Resource");
        assert_eq!(rtype, "virtual-machine");
    }
}
