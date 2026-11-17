//! Modern Kraljic × Dutch Windmill Dual-Axis Sourcing Audit Engine & 4-Layer Catalog Processor.
//!
//! Implements:
//! - Three-rule `CatalogProcessor` validation (anti-monolith granularity, domain lineage,
//!   adapter decoupling & FinOps secret hygiene).
//! - Automated `FactRetriever` + full 8-dimension 5-point knockout escalation
//!   (`Y1..Y4`, `X1..X4` >= 5.0 enforces floor >= 3.20 on that axis).
//! - `[2.7, 3.3]` critical stress band, Dutch Windmill bidirectional mismatch detection,
//!   and dynamic re-audit triggers (`renewalDate <= 180d`, dependency doubling, variance alert).
//! - Three Tech Insights `BooleanCheck` scorecards and actionable remediation playbooks.
//! - Dual-mode persistence (`PITS_STATE_DIR/sourcing-audit-store.yaml` + optional workspace
//!   `catalog/sourcing/*.yaml` GitOps sync with unified diff preview).

use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::catalog::sourcing_db::SourcingDbHandle;

pub const SOURCING_RESOURCE_TYPES: [&str; 4] = [
    "vendor-saas",
    "commercial-sdk",
    "managed-infra",
    "outsourced-dev",
];

pub const PERCEIVED_STATUSES: [&str; 4] = ["core", "development", "exploitable", "nuisance"];

pub const WALLET_SHARE_TIERS: [&str; 4] =
    ["under-1pct", "1-to-5pct", "5-to-15pct", "over-15pct"];

const FORBIDDEN_TYPE_PATTERNS: [&str; 7] = [
    "vendor",
    "supplier",
    "provider",
    "enterprise-agreement",
    "all-in-one",
    "cloud-vendor",
    "monolith",
];

const FORBIDDEN_NAME_SUFFIXES: [&str; 5] = [
    "-all",
    "-bundle",
    "-monolith",
    "-vendor-package",
    "-catchall",
];

const FORBIDDEN_PRICE_KEYS: [&str; 12] = [
    "contractprice",
    "unitprice",
    "annualcost",
    "monthlyfee",
    "contractamount",
    "licensefee",
    "quoteprice",
    "discountrate",
    "price",
    "pricing",
    "budget",
    "contractvalue",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContractMeta {
    pub vendor_name: String,
    pub renewal_date: String,
    pub last_audited_at: String,
    pub baseline_prod_dependents: usize,
    #[serde(default)]
    pub blind_score_variance_alert: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManualScores {
    #[serde(rename = "y2_coreCompetitiveness")]
    pub y2_core_competitiveness: f64,
    #[serde(rename = "y4_priceElasticity")]
    pub y4_price_elasticity: f64,
    #[serde(rename = "x2_marketScarcity")]
    pub x2_market_scarcity: f64,
    #[serde(rename = "x3_customizationDepth")]
    pub x3_customization_depth: f64,
    #[serde(rename = "x4_complianceFragility")]
    pub x4_compliance_fragility: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SupplierPreference {
    pub estimated_wallet_share_tier: String,
    pub account_attractiveness_score: f64,
    pub perceived_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourcingAuditSpec {
    pub contract_meta: ContractMeta,
    pub manual_scores: ManualScores,
    pub supplier_preference: SupplierPreference,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourcingResourceRecord {
    pub name: String,
    #[serde(default = "default_namespace")]
    pub namespace: String,
    pub description: String,
    pub resource_type: String,
    pub owner: String,
    pub system: String,
    pub adapter_abstracted: bool,
    pub cost_allocation_id: String,
    #[serde(default)]
    pub depends_on_components: Vec<String>,
    pub audit: SourcingAuditSpec,
}

fn default_namespace() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DomainLineageEntry {
    pub component_ref: String,
    pub component_title: String,
    pub system_ref: String,
    pub domain_ref: String,
    pub domain_tier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DimensionBreakdown {
    pub y1_blast_radius: f64,
    pub y2_core_competitiveness: f64,
    pub y3_tco_ratio: f64,
    pub y4_price_elasticity: f64,
    pub x1_lock_in: f64,
    pub x2_market_scarcity: f64,
    pub x3_customization_depth: f64,
    pub x4_compliance_fragility: f64,
    pub raw_y_score: f64,
    pub raw_x_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourcingFacts {
    #[serde(rename = "finalYScore")]
    pub final_y_score: f64,
    #[serde(rename = "finalXScore")]
    pub final_x_score: f64,
    pub quadrant: String,
    pub knockout_triggered: bool,
    pub y_knockout: bool,
    pub x_knockout: bool,
    pub knockout_dimensions: Vec<String>,
    pub in_critical_zone: bool,
    pub windmill_mismatch: bool,
    pub re_audit_required: bool,
    pub prod_dependents: usize,
    pub days_to_renewal: i64,
    pub re_audit_reasons: Vec<String>,
    pub breakdown: DimensionBreakdown,
    pub finops_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BooleanCheckResult {
    pub id: String,
    pub name: String,
    pub description: String,
    pub fact_key: String,
    pub passed: bool,
    pub triggered: bool,
    pub severity: String,
    pub message: String,
    pub remediation: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProcessorRuleResult {
    pub id: String,
    pub title: String,
    pub passed: bool,
    pub severity: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourcingValidationReport {
    pub valid: bool,
    pub rules: Vec<ProcessorRuleResult>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub inferred_domain: Option<String>,
    pub domain_tier: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BlindBallotRecord {
    pub ballot_id: String,
    pub unit_name: String,
    pub reviewer_role: String,
    pub reviewer_id: String,
    #[serde(rename = "y2_coreCompetitiveness")]
    pub y2_core_competitiveness: f64,
    #[serde(rename = "y4_priceElasticity")]
    pub y4_price_elasticity: f64,
    #[serde(rename = "x2_marketScarcity")]
    pub x2_market_scarcity: f64,
    #[serde(rename = "x3_customizationDepth")]
    pub x3_customization_depth: f64,
    #[serde(rename = "x4_complianceFragility")]
    pub x4_compliance_fragility: f64,
    pub evidence_notes: String,
    pub submitted_at: String,
    #[serde(default)]
    pub sync_pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryCandidateRecord {
    pub candidate_id: String,
    pub package_or_endpoint: String,
    pub ecosystem: String,
    pub suggested_type: String,
    pub detected_in_path: String,
    pub inferred_system: String,
    #[serde(default)]
    pub inferred_components: Vec<String>,
    pub adapter_detected: bool,
    pub monolith_risk_note: String,
    pub status: String,
    #[serde(default)]
    pub promoted_unit_name: Option<String>,
    pub discovered_at: String,
    #[serde(default)]
    pub sync_pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourcingAuditEvent {
    pub event_id: String,
    pub unit_name: String,
    pub event_type: String,
    pub actor: String,
    pub channel: String,
    pub summary: String,
    pub payload_json: String,
    pub created_at: String,
    #[serde(default)]
    pub sync_pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SourcingDbSyncStatus {
    pub backend_mode: String,
    pub sqlite_version: String,
    pub sqlite_path: String,
    pub sqlite_journal_mode: String,
    pub postgres_configured: bool,
    pub postgres_endpoint_redacted: Option<String>,
    pub postgres_connected: bool,
    pub pending_outbox_rows: usize,
    pub last_sync_at: Option<String>,
    pub last_sync_direction: Option<String>,
    pub last_sync_error: Option<String>,
    pub total_units: usize,
    pub total_ballots: usize,
    pub total_candidates: usize,
    pub total_events: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourcingEvaluation {
    pub entity_ref: String,
    pub record: SourcingResourceRecord,
    pub facts: SourcingFacts,
    pub checks: Vec<BooleanCheckResult>,
    pub validation: SourcingValidationReport,
    pub lineage: Vec<DomainLineageEntry>,
    pub resolved_domain: String,
    pub resolved_domain_tier: String,
    pub remediation_playbook: Vec<String>,
    #[serde(default = "default_source_origin")]
    pub source_origin: String,
    #[serde(default)]
    pub ballot_count: usize,
    #[serde(default)]
    pub max_ballot_spread: f64,
    #[serde(default)]
    pub ballots: Vec<BlindBallotRecord>,
}

fn default_source_origin() -> String {
    "seed-baseline".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourcingFactSchemaField {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TechInsightsFactSchemaResponse {
    pub id: String,
    pub version: String,
    pub entity_filter: Vec<Value>,
    pub schema: BTreeMap<String, Value>,
    pub fields: Vec<SourcingFactSchemaField>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TechInsightsLatestFactsResponse {
    pub entity: Value,
    pub retriever_id: String,
    pub version: String,
    pub timestamp: String,
    pub facts: SourcingFacts,
    pub breakdown: DimensionBreakdown,
    pub lineage: Vec<DomainLineageEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TechInsightsCheckDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub fact_retriever_id: String,
    pub fact_key: String,
    pub severity: String,
    #[serde(rename = "type")]
    pub check_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TechInsightsCheckEntityResponse {
    pub entity_ref: String,
    pub entity: Value,
    pub evaluated_at: String,
    pub passed_all: bool,
    pub redline_count: usize,
    pub facts: SourcingFacts,
    pub checks: Vec<BooleanCheckResult>,
    pub lineage: Vec<DomainLineageEntry>,
    pub remediation_playbook: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourcingOverviewResponse {
    pub generated_at: String,
    pub total_units: usize,
    pub quadrant_counts: BTreeMap<String, usize>,
    pub redline_counts: BTreeMap<String, usize>,
    pub items: Vec<SourcingEvaluation>,
    pub available_systems: Vec<Value>,
    pub available_components: Vec<Value>,
    pub workspace_yaml_dir: Option<String>,
    #[serde(default)]
    pub db_sync: SourcingDbSyncStatus,
    #[serde(default)]
    pub discovery_candidates: Vec<DiscoveryCandidateRecord>,
    #[serde(default)]
    pub blind_ballots: Vec<BlindBallotRecord>,
    #[serde(default)]
    pub recent_events: Vec<SourcingAuditEvent>,
    #[serde(default)]
    pub mcp_endpoints: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourcingWizardUpsertRequest {
    pub record: SourcingResourceRecord,
    #[serde(default)]
    pub write_workspace_yaml: bool,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub override_reason: Option<String>,
    #[serde(default)]
    pub actor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourcingWizardUpsertResponse {
    pub ok: bool,
    pub dry_run: bool,
    pub entity_ref: String,
    pub persistence_mode: String,
    pub state_store_path: String,
    pub workspace_yaml_path: Option<String>,
    pub workspace_yaml_written: bool,
    pub generated_yaml: String,
    pub unified_diff: String,
    pub evaluation: SourcingEvaluation,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PersistedSourcingState {
    #[serde(default = "default_state_version")]
    version: u32,
    #[serde(default)]
    records: BTreeMap<String, SourcingResourceRecord>,
    #[serde(default)]
    component_extra_deps: BTreeMap<String, Vec<String>>,
}

fn default_state_version() -> u32 {
    1
}

#[derive(Debug, Clone)]
pub struct SourcingStore {
    pub state_store_path: PathBuf,
    pub workspace_yaml_dir: Option<PathBuf>,
    pub db: Arc<SourcingDbHandle>,
    records: BTreeMap<String, SourcingResourceRecord>,
    pub source_origins: BTreeMap<String, String>,
    pub ballots_by_unit: BTreeMap<String, Vec<BlindBallotRecord>>,
    pub max_spread_by_unit: BTreeMap<String, f64>,
    component_extra_deps: BTreeMap<String, Vec<String>>,
}

pub const SEED_YAMLS: [(&str, &str); 5] = [
    (
        "payment-fraud-engine-sdk.yaml",
        include_str!("sourcing_seeds/payment-fraud-engine-sdk.yaml"),
    ),
    (
        "tailscale-coordination-saas.yaml",
        include_str!("sourcing_seeds/tailscale-coordination-saas.yaml"),
    ),
    (
        "commercial-mip-solver-sdk.yaml",
        include_str!("sourcing_seeds/commercial-mip-solver-sdk.yaml"),
    ),
    (
        "transit-bgp-managed-uplink.yaml",
        include_str!("sourcing_seeds/transit-bgp-managed-uplink.yaml"),
    ),
    (
        "hero-landmark-art-outsourcing.yaml",
        include_str!("sourcing_seeds/hero-landmark-art-outsourcing.yaml"),
    ),
];

pub fn is_sourcing_resource_type(spec_type: &str) -> bool {
    let lower = spec_type.trim().to_ascii_lowercase();
    SOURCING_RESOURCE_TYPES.iter().any(|t| *t == lower)
}

pub fn round2(v: f64) -> f64 {
    (v * 100.0 + 1e-9).round() / 100.0
}

pub fn resolve_finops_tco_ratio(cost_allocation_id: &str) -> (f64, String) {
    let normalized = cost_allocation_id.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "cc-sec-089" => (2.5, "finops-ledger:cc-sec-089".to_string()),
        "cc-infra-net-012" => (4.2, "finops-ledger:cc-infra-net-012".to_string()),
        "cc-sim-solver-021" => (2.2, "finops-ledger:cc-sim-solver-021".to_string()),
        "cc-infra-wan-034" => (2.0, "finops-ledger:cc-infra-wan-034".to_string()),
        "cc-art-ops-052" => (1.8, "finops-ledger:cc-art-ops-052".to_string()),
        "" => (2.5, "finops-ledger:unallocated-default".to_string()),
        other => {
            // Deterministic hash mapped to [1.8, 3.8] for ad-hoc cost centers created via Wizard.
            let mut acc: u64 = 0;
            for b in other.bytes() {
                acc = acc.wrapping_mul(31).wrapping_add(b as u64);
            }
            let ratio = round2(1.8 + ((acc % 21) as f64) * 0.1);
            (ratio, format!("finops-ledger:derived:{}", other))
        }
    }
}

pub fn default_system_domain_map() -> BTreeMap<String, (&'static str, &'static str)> {
    let mut map = BTreeMap::new();
    map.insert(
        "system:default/core-checkout".to_string(),
        ("domain:default/core-commerce", "Core Transaction Domain"),
    );
    map.insert(
        "system:default/pits-infrastructure".to_string(),
        ("domain:default/platform-infra", "Platform Infrastructure Domain"),
    );
    map.insert(
        "system:default/pits-city-pipeline".to_string(),
        ("domain:default/spatial-simulation", "Vertical Simulation Pipeline"),
    );
    map
}

pub fn canonical_system_ref(system: &str) -> String {
    let trimmed = system.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.contains(':') {
        trimmed.to_ascii_lowercase()
    } else if let Some((_, name)) = trimmed.split_once('/') {
        format!("system:default/{}", name.to_ascii_lowercase())
    } else {
        format!("system:default/{}", trimmed.to_ascii_lowercase())
    }
}

pub fn canonical_component_ref(comp: &str) -> String {
    let trimmed = comp.trim();
    if trimmed.contains(':') {
        trimmed.to_ascii_lowercase()
    } else if let Some((_, name)) = trimmed.split_once('/') {
        format!("component:default/{}", name.to_ascii_lowercase())
    } else {
        format!("component:default/{}", trimmed.to_ascii_lowercase())
    }
}

pub fn parse_sourcing_yaml_str(yaml_src: &str) -> Result<SourcingResourceRecord, String> {
    let val: serde_yaml::Value =
        serde_yaml::from_str(yaml_src).map_err(|e| format!("Invalid YAML syntax: {}", e))?;
    parse_sourcing_yaml_value(&val)
}

pub fn parse_sourcing_yaml_value(val: &serde_yaml::Value) -> Result<SourcingResourceRecord, String> {
    let kind = val
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if !kind.eq_ignore_ascii_case("Resource") {
        return Err(format!(
            "CatalogProcessor rejected entity: expected kind: Resource, got '{}'",
            kind
        ));
    }

    let meta = val
        .get("metadata")
        .ok_or_else(|| "Missing required 'metadata' block".to_string())?;
    let name = meta
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Missing metadata.name".to_string())?
        .to_string();
    let namespace = meta
        .get("namespace")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
        .to_string();
    let description = meta
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();

    let annotations = meta.get("annotations").and_then(|v| v.as_mapping());
    let cost_allocation_id = annotations
        .and_then(|m| {
            m.get(&serde_yaml::Value::String(
                "finops.company.io/cost-allocation-id".to_string(),
            ))
        })
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();

    let adapter_str = annotations
        .and_then(|m| {
            m.get(&serde_yaml::Value::String(
                "sourcing.company.io/adapter-abstracted".to_string(),
            ))
        })
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let adapter_abstracted = match adapter_str.as_str() {
        "true" => true,
        "false" => false,
        other => {
            return Err(format!(
                "Annotation 'sourcing.company.io/adapter-abstracted' must be 'true' or 'false' (got '{}')",
                other
            ))
        }
    };

    let spec = val
        .get("spec")
        .ok_or_else(|| "Missing required 'spec' block".to_string())?;
    let resource_type = spec
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let owner = spec
        .get("owner")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let system = spec
        .get("system")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();

    let audit_val = spec
        .get("sourcingAudit")
        .ok_or_else(|| "Missing required 'spec.sourcingAudit' object".to_string())?;
    let audit: SourcingAuditSpec = serde_yaml::from_value(audit_val.clone())
        .map_err(|e| format!("Invalid spec.sourcingAudit schema: {}", e))?;

    let mut depends_on_components = Vec::new();
    if let Some(seq) = spec.get("upstreamComponents").and_then(|v| v.as_sequence()) {
        for item in seq {
            if let Some(s) = item.as_str() {
                let c = canonical_component_ref(s);
                if !depends_on_components.contains(&c) {
                    depends_on_components.push(c);
                }
            }
        }
    }

    Ok(SourcingResourceRecord {
        name,
        namespace,
        description,
        resource_type,
        owner,
        system,
        adapter_abstracted,
        cost_allocation_id,
        depends_on_components,
        audit,
    })
}

fn scan_forbidden_price_keys(val: &Value, path: &str, hits: &mut Vec<String>) {
    match val {
        Value::Object(map) => {
            for (k, v) in map {
                let lower = k.to_ascii_lowercase().replace(['-', '_'], "");
                let child_path = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{}.{}", path, k)
                };
                if FORBIDDEN_PRICE_KEYS.iter().any(|fk| lower == *fk || lower.ends_with(fk)) {
                    hits.push(child_path.clone());
                }
                scan_forbidden_price_keys(v, &child_path, hits);
            }
        }
        Value::Array(arr) => {
            for (idx, item) in arr.iter().enumerate() {
                scan_forbidden_price_keys(item, &format!("{}[{}]", path, idx), hits);
            }
        }
        _ => {}
    }
}

/// Runs the three-rule CatalogProcessor validator on a sourcing resource record plus optional raw
/// YAML/JSON payload (to catch plaintext contract prices or invalid annotations before normalization).
pub fn validate_sourcing_record(
    record: &SourcingResourceRecord,
    raw_value: Option<&Value>,
    system_domains: &BTreeMap<String, (String, String)>,
) -> SourcingValidationReport {
    let mut rules = Vec::new();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    // --- Rule 1: Anti-monolith granularity & FinOps secret hygiene ---
    let mut r1_errs = Vec::new();
    let lower_name = record.name.trim().to_ascii_lowercase();
    if lower_name.is_empty()
        || !lower_name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        r1_errs.push(
            "metadata.name must be lowercase alphanumeric/hyphen DNS-safe identifier".to_string(),
        );
    }

    let lower_type = record.resource_type.trim().to_ascii_lowercase();
    if !is_sourcing_resource_type(&lower_type) {
        if FORBIDDEN_TYPE_PATTERNS.iter().any(|p| lower_type.contains(p)) {
            r1_errs.push(format!(
                "Forbidden vendor-level spec.type '{}': unpack into granular units ({})",
                record.resource_type,
                SOURCING_RESOURCE_TYPES.join(", ")
            ));
        } else {
            r1_errs.push(format!(
                "spec.type '{}' must be one of [{}]",
                record.resource_type,
                SOURCING_RESOURCE_TYPES.join(", ")
            ));
        }
    }

    if FORBIDDEN_NAME_SUFFIXES
        .iter()
        .any(|sfx| lower_name.ends_with(sfx))
    {
        r1_errs.push(format!(
            "metadata.name '{}' looks like a vendor-level catch-all package; register independently sourceable units instead",
            record.name
        ));
    }

    let vendor_slug: String = record
        .audit
        .contract_meta
        .vendor_name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if !vendor_slug.is_empty() && lower_name == vendor_slug {
        r1_errs.push(format!(
            "metadata.name '{}' matches bare vendor name '{}'; split by functional sourcing unit (e.g. '{}-sdk' or '{}-saas')",
            record.name, record.audit.contract_meta.vendor_name, vendor_slug, vendor_slug
        ));
    }

    if record.cost_allocation_id.trim().is_empty() {
        r1_errs.push(
            "Missing annotation 'finops.company.io/cost-allocation-id' (required instead of plaintext price)".to_string(),
        );
    }

    let combined_text = format!("{} {}", record.description, record.cost_allocation_id);
    let lower_text = combined_text.to_ascii_lowercase();
    let has_price_keyword = [
        "contractprice",
        "contract_price",
        "annualcost",
        "annual_cost",
        "unitprice",
        "unit_price",
        "monthlyfee",
        "licensefee",
    ]
    .iter()
    .any(|kw| lower_text.contains(kw));
    let has_currency_amount = combined_text
        .chars()
        .collect::<Vec<char>>()
        .windows(2)
        .any(|w| matches!(w[0], '$' | '€' | '£') && w[1].is_ascii_digit());
    if has_price_keyword || has_currency_amount {
        r1_errs.push(
            "FINOPS_SECRET_LEAK: Plaintext contract price values are forbidden in catalog descriptions or annotations; store only 'finops.company.io/cost-allocation-id'".to_string(),
        );
    }

    if let Some(raw) = raw_value {
        let mut price_hits = Vec::new();
        scan_forbidden_price_keys(raw, "", &mut price_hits);
        if !price_hits.is_empty() {
            r1_errs.push(format!(
                "FINOPS_SECRET_LEAK: Plaintext contract price fields are forbidden in catalog manifests ({}); store only 'finops.company.io/cost-allocation-id'",
                price_hits.join(", ")
            ));
        }
        if let Some(ann) = raw
            .pointer("/metadata/annotations")
            .and_then(|v| v.as_object())
        {
            if let Some(raw_adapter) = ann.get("sourcing.company.io/adapter-abstracted") {
                let s = raw_adapter.as_str().unwrap_or_default();
                if s != "true" && s != "false" {
                    r1_errs.push(format!(
                        "Annotation 'sourcing.company.io/adapter-abstracted' must be string \"true\" or \"false\" (got {:?})",
                        raw_adapter
                    ));
                }
            } else {
                r1_errs.push(
                    "Missing explicit annotation 'sourcing.company.io/adapter-abstracted'".to_string(),
                );
            }
        }
    }

    let r1_passed = r1_errs.is_empty();
    let r1_msg = if r1_passed {
        format!(
            "Unit '{}' unpacked cleanly as granular spec.type='{}' with FinOps allocation ID '{}'",
            record.name, record.resource_type, record.cost_allocation_id
        )
    } else {
        errors.extend(r1_errs.iter().cloned());
        r1_errs.join("; ")
    };
    rules.push(ProcessorRuleResult {
        id: "no-vendor-monolith".to_string(),
        title: "Rule 1: Anti-Vendor Monolith & FinOps Isolation".to_string(),
        passed: r1_passed,
        severity: if r1_passed { "pass" } else { "error" }.to_string(),
        message: r1_msg,
    });

    // --- Rule 2: 4-Layer Domain Lineage (Domain <- System <- Component <- Resource) ---
    let mut r2_errs = Vec::new();
    if record.owner.trim().is_empty() {
        r2_errs.push("Missing required spec.owner (e.g. group:default/operators)".to_string());
    }
    let sys_ref = canonical_system_ref(&record.system);
    let mut inferred_domain = None;
    let mut domain_tier = None;
    if sys_ref.is_empty() {
        r2_errs.push("Missing required spec.system to anchor 4-layer catalog topology".to_string());
    } else if let Some((dom, tier)) = system_domains.get(&sys_ref) {
        inferred_domain = Some(dom.clone());
        domain_tier = Some(tier.clone());
    } else {
        // Fallback check against built-in default systems
        let defaults = default_system_domain_map();
        if let Some((dom, tier)) = defaults.get(&sys_ref) {
            inferred_domain = Some((*dom).to_string());
            domain_tier = Some((*tier).to_string());
        } else {
            r2_errs.push(format!(
                "System '{}' does not resolve to a registered Backstage System/Domain",
                record.system
            ));
        }
    }

    if record.depends_on_components.is_empty() {
        warnings.push(
            "No upstream Business Component declared yet; blast radius (Y1) will evaluate using live Component dependsOn edges (or default 0 -> Y1=1)".to_string(),
        );
    }

    let r2_passed = r2_errs.is_empty();
    let r2_msg = if r2_passed {
        format!(
            "Bound to {} -> {} ({})",
            sys_ref,
            inferred_domain.as_deref().unwrap_or("domain:default/unknown"),
            domain_tier.as_deref().unwrap_or("Unknown Tier")
        )
    } else {
        errors.extend(r2_errs.iter().cloned());
        r2_errs.join("; ")
    };
    rules.push(ProcessorRuleResult {
        id: "domain-lineage-required".to_string(),
        title: "Rule 2: 4-Layer System -> Domain Attribution".to_string(),
        passed: r2_passed,
        severity: if r2_passed { "pass" } else { "error" }.to_string(),
        message: r2_msg,
    });

    // --- Rule 3: Adapter Decoupling Annotation & Blind-Review / Renewal Schema Bounds ---
    let mut r3_errs = Vec::new();
    let m = &record.audit.manual_scores;
    let score_checks = [
        ("y2_coreCompetitiveness", m.y2_core_competitiveness),
        ("y4_priceElasticity", m.y4_price_elasticity),
        ("x2_marketScarcity", m.x2_market_scarcity),
        ("x3_customizationDepth", m.x3_customization_depth),
        ("x4_complianceFragility", m.x4_compliance_fragility),
        (
            "accountAttractivenessScore",
            record.audit.supplier_preference.account_attractiveness_score,
        ),
    ];
    for (field, val) in score_checks {
        if !val.is_finite() || !(1.0..=5.0).contains(&val) {
            r3_errs.push(format!(
                "Score '{}' = {} must be finite within 1.0..=5.0",
                field, val
            ));
        }
    }
    if !PERCEIVED_STATUSES.contains(&record.audit.supplier_preference.perceived_status.as_str()) {
        r3_errs.push(format!(
            "supplierPreference.perceivedStatus '{}' must be one of [{}]",
            record.audit.supplier_preference.perceived_status,
            PERCEIVED_STATUSES.join(", ")
        ));
    }
    if !WALLET_SHARE_TIERS.contains(
        &record
            .audit
            .supplier_preference
            .estimated_wallet_share_tier
            .as_str(),
    ) {
        r3_errs.push(format!(
            "supplierPreference.estimatedWalletShareTier '{}' must be one of [{}]",
            record.audit.supplier_preference.estimated_wallet_share_tier,
            WALLET_SHARE_TIERS.join(", ")
        ));
    }
    if NaiveDate::parse_from_str(record.audit.contract_meta.renewal_date.trim(), "%Y-%m-%d")
        .is_err()
    {
        r3_errs.push(format!(
            "contractMeta.renewalDate '{}' must use ISO format YYYY-MM-DD",
            record.audit.contract_meta.renewal_date
        ));
    }
    if record.audit.contract_meta.vendor_name.trim().is_empty() {
        r3_errs.push("contractMeta.vendorName cannot be empty".to_string());
    }

    let r3_passed = r3_errs.is_empty();
    let r3_msg = if r3_passed {
        format!(
            "Adapter abstraction explicitly declared ('{}'); 1-5 manual & Windmill scores validated",
            record.adapter_abstracted
        )
    } else {
        errors.extend(r3_errs.iter().cloned());
        r3_errs.join("; ")
    };
    rules.push(ProcessorRuleResult {
        id: "decoupling-and-score-bounds".to_string(),
        title: "Rule 3: Explicit Adapter Declaration & Parameter Bounds".to_string(),
        passed: r3_passed,
        severity: if r3_passed { "pass" } else { "error" }.to_string(),
        message: r3_msg,
    });

    SourcingValidationReport {
        valid: errors.is_empty(),
        rules,
        errors,
        warnings,
        inferred_domain,
        domain_tier,
    }
}

pub fn validate_raw_sourcing_yaml_payload(
    yaml_text: &str,
    system_domains: &BTreeMap<String, (String, String)>,
) -> SourcingValidationReport {
    let raw_yaml: serde_yaml::Value = match serde_yaml::from_str(yaml_text) {
        Ok(v) => v,
        Err(e) => {
            return SourcingValidationReport {
                valid: false,
                rules: vec![ProcessorRuleResult {
                    id: "yaml-parse".to_string(),
                    title: "YAML Schema Parse".to_string(),
                    passed: false,
                    severity: "error".to_string(),
                    message: format!("YAML syntax error: {}", e),
                }],
                errors: vec![format!("YAML syntax error: {}", e)],
                warnings: Vec::new(),
                inferred_domain: None,
                domain_tier: None,
            }
        }
    };
    let raw_json: Value = serde_json::to_value(&raw_yaml).unwrap_or(Value::Null);
    match parse_sourcing_yaml_value(&raw_yaml) {
        Ok(rec) => validate_sourcing_record(&rec, Some(&raw_json), system_domains),
        Err(e) => {
            let mut price_hits = Vec::new();
            scan_forbidden_price_keys(&raw_json, "", &mut price_hits);
            let mut errors = vec![e.clone()];
            if !price_hits.is_empty() {
                errors.push(format!(
                    "FINOPS_SECRET_LEAK: Plaintext price fields forbidden ({})",
                    price_hits.join(", ")
                ));
            }
            SourcingValidationReport {
                valid: false,
                rules: vec![ProcessorRuleResult {
                    id: "schema-contract".to_string(),
                    title: "Backstage Resource Schema Contract".to_string(),
                    passed: false,
                    severity: "error".to_string(),
                    message: errors.join("; "),
                }],
                errors,
                warnings: Vec::new(),
                inferred_domain: None,
                domain_tier: None,
            }
        }
    }
}

pub fn evaluate_sourcing_audit_at(
    record: &SourcingResourceRecord,
    lineage: Vec<DomainLineageEntry>,
    system_domains: &BTreeMap<String, (String, String)>,
    today: NaiveDate,
) -> SourcingEvaluation {
    let prod_count = lineage.len();
    let y1 = if prod_count >= 5 {
        5.0
    } else if prod_count >= 2 {
        4.0
    } else if prod_count == 1 {
        3.0
    } else {
        1.0
    };
    let (y3, finops_source) = resolve_finops_tco_ratio(&record.cost_allocation_id);
    let x1 = if !record.adapter_abstracted && prod_count >= 3 {
        5.0
    } else if !record.adapter_abstracted {
        4.0
    } else {
        2.0
    };

    let m = &record.audit.manual_scores;
    let y2 = m.y2_core_competitiveness;
    let y4 = m.y4_price_elasticity;
    let x2 = m.x2_market_scarcity;
    let x3 = m.x3_customization_depth;
    let x4 = m.x4_compliance_fragility;

    let raw_y = y1 * 0.35 + y2 * 0.25 + y3 * 0.25 + y4 * 0.15;
    let raw_x = x1 * 0.35 + x2 * 0.25 + x3 * 0.25 + x4 * 0.15;

    let mut knockout_dimensions = Vec::new();
    let y_dims = [
        ("y1_blastRadius", y1),
        ("y2_coreCompetitiveness", y2),
        ("y3_tcoRatio", y3),
        ("y4_priceElasticity", y4),
    ];
    let x_dims = [
        ("x1_lockIn", x1),
        ("x2_marketScarcity", x2),
        ("x3_customizationDepth", x3),
        ("x4_complianceFragility", x4),
    ];
    let mut y_knockout = false;
    for (dim, score) in y_dims {
        if score >= 5.0 {
            y_knockout = true;
            knockout_dimensions.push(dim.to_string());
        }
    }
    let mut x_knockout = false;
    for (dim, score) in x_dims {
        if score >= 5.0 {
            x_knockout = true;
            knockout_dimensions.push(dim.to_string());
        }
    }
    let knockout_triggered = y_knockout || x_knockout;

    let final_y_score = round2(if y_knockout { raw_y.max(3.2) } else { raw_y });
    let final_x_score = round2(if x_knockout { raw_x.max(3.2) } else { raw_x });

    let quadrant = if final_y_score >= 3.0 {
        if final_x_score >= 3.0 {
            "strategic"
        } else {
            "leverage"
        }
    } else if final_x_score >= 3.0 {
        "bottleneck"
    } else {
        "non-critical"
    }
    .to_string();

    let in_critical_zone =
        (2.7..=3.3).contains(&final_y_score) || (2.7..=3.3).contains(&final_x_score);

    let perceived_status = record
        .audit
        .supplier_preference
        .perceived_status
        .to_ascii_lowercase();
    let windmill_mismatch = (quadrant == "strategic" || quadrant == "bottleneck")
        && (perceived_status == "nuisance" || perceived_status == "exploitable");

    let days_to_renewal = NaiveDate::parse_from_str(
        record.audit.contract_meta.renewal_date.trim(),
        "%Y-%m-%d",
    )
    .map(|d| (d - today).num_days())
    .unwrap_or(999);

    let baseline_deps = record.audit.contract_meta.baseline_prod_dependents.max(1);
    let mut re_audit_reasons = Vec::new();
    if days_to_renewal <= 180 {
        re_audit_reasons.push(format!(
            "Contract renewal in {} days (<= 180d SLA window, date {})",
            days_to_renewal, record.audit.contract_meta.renewal_date
        ));
    }
    if prod_count >= baseline_deps * 2 {
        re_audit_reasons.push(format!(
            "Production dependents doubled ({} active vs baseline {})",
            prod_count, record.audit.contract_meta.baseline_prod_dependents
        ));
    }
    if record.audit.contract_meta.blind_score_variance_alert {
        re_audit_reasons.push(
            "Cross-team blind evaluation variance alert flagged for calibration".to_string(),
        );
    }
    let re_audit_required = !re_audit_reasons.is_empty();

    let facts = SourcingFacts {
        final_y_score,
        final_x_score,
        quadrant: quadrant.clone(),
        knockout_triggered,
        y_knockout,
        x_knockout,
        knockout_dimensions: knockout_dimensions.clone(),
        in_critical_zone,
        windmill_mismatch,
        re_audit_required,
        prod_dependents: prod_count,
        days_to_renewal,
        re_audit_reasons: re_audit_reasons.clone(),
        breakdown: DimensionBreakdown {
            y1_blast_radius: round2(y1),
            y2_core_competitiveness: round2(y2),
            y3_tco_ratio: round2(y3),
            y4_price_elasticity: round2(y4),
            x1_lock_in: round2(x1),
            x2_market_scarcity: round2(x2),
            x3_customization_depth: round2(x3),
            x4_compliance_fragility: round2(x4),
            raw_y_score: round2(raw_y),
            raw_x_score: round2(raw_x),
        },
        finops_source,
    };

    // Build 3 BooleanCheck results + playbook
    let mut playbook = Vec::new();
    if !record.adapter_abstracted && (quadrant == "strategic" || quadrant == "bottleneck") {
        playbook.push(format!(
            "Architecture Decoupling: Introduce an anti-corruption Adapter layer around '{}' so `sourcing.company.io/adapter-abstracted` can move from \"false\" to \"true\" (lowers X1 lock-in from {:.1} to 2.0).",
            record.name, x1
        ));
    }
    if windmill_mismatch {
        playbook.push(format!(
            "Windmill Re-Balancing: We sit in '{}' quadrant (Y={:.2}, X={:.2}) while {} perceives us as '{}' ({} wallet share). Consolidate adjacent demand, sign multi-year co-innovation SLAs, or cultivate a dual-source challenger before renewal.",
            quadrant,
            final_y_score,
            final_x_score,
            record.audit.contract_meta.vendor_name,
            perceived_status,
            record.audit.supplier_preference.estimated_wallet_share_tier
        ));
    }
    if knockout_triggered {
        playbook.push(format!(
            "5-Point Knockout Mitigation: Forced floor escalation triggered by [{}]. Establish secondary fallback supplier or localized escrow/compliance perimeter.",
            knockout_dimensions.join(", ")
        ));
    }
    if in_critical_zone {
        playbook.push(format!(
            "Critical Boundary Stress Test ([2.70, 3.30]): Current coordinates (X={:.2}, Y={:.2}) lie near a quadrant boundary. Run +/-0.3 sensitivity stress test with Risk Engineering & Procurement before locking sourcing strategy.",
            final_x_score, final_y_score
        ));
    }
    if re_audit_required {
        playbook.push(format!(
            "Dynamic Re-Audit Action: Complete procurement + architecture calibration ({}) and run 'acknowledge-reaudit-baseline' to reset baseline dependents.",
            re_audit_reasons.join("; ")
        ));
    }
    if playbook.is_empty() {
        playbook.push(format!(
            "Healthy Governance Posture: Keep dual-source competitive tension and review ahead of renewal on {}.",
            record.audit.contract_meta.renewal_date
        ));
    }

    let check_windmill = BooleanCheckResult {
        id: "sourcing-windmill-mismatch-redline".to_string(),
        name: "Windmill Symmetric Relationship Check".to_string(),
        description: "Flags critical/bottleneck sourcing units where the supplier treats PITS as 'nuisance' or 'exploitable'".to_string(),
        fact_key: "windmillMismatch".to_string(),
        passed: !windmill_mismatch,
        triggered: windmill_mismatch,
        severity: if windmill_mismatch { "critical" } else { "pass" }.to_string(),
        message: if windmill_mismatch {
            format!(
                "REDLINE: High dependency '{}' quadrant paired with supplier perceivedStatus='{}' ({})",
                quadrant, perceived_status, record.audit.supplier_preference.estimated_wallet_share_tier
            )
        } else {
            format!(
                "Aligned supplier perception ('{}') for '{}' quadrant",
                perceived_status, quadrant
            )
        },
        remediation: if windmill_mismatch {
            vec![
                "Initiate executive QBR / co-development sponsorship or qualify a backup vendor"
                    .to_string(),
                "Abstract proprietary APIs behind an internal Adapter interface".to_string(),
            ]
        } else {
            Vec::new()
        },
    };

    let check_reaudit = BooleanCheckResult {
        id: "sourcing-dynamic-reaudit-redline".to_string(),
        name: "Dynamic Contract & Topology Re-Audit Check".to_string(),
        description: "Triggers when renewal <= 180 days, production dependents double (>= 2x baseline), or blind variance alert is set".to_string(),
        fact_key: "reAuditRequired".to_string(),
        passed: !re_audit_required,
        triggered: re_audit_required,
        severity: if re_audit_required { "warning" } else { "pass" }.to_string(),
        message: if re_audit_required {
            format!("RE-AUDIT DUE: {}", re_audit_reasons.join("; "))
        } else {
            format!(
                "Re-audit cadence nominal ({}d until renewal; {} active vs baseline {} dependents)",
                days_to_renewal, prod_count, record.audit.contract_meta.baseline_prod_dependents
            )
        },
        remediation: if re_audit_required {
            vec![
                "Execute joint Architecture + Procurement blind re-scoring in the Sourcing Wizard"
                    .to_string(),
                "Invoke 'acknowledge-reaudit-baseline' once renewal or baseline is refreshed"
                    .to_string(),
            ]
        } else {
            Vec::new()
        },
    };

    let check_critical = BooleanCheckResult {
        id: "sourcing-critical-zone-redline".to_string(),
        name: "Quadrant Boundary Stress-Test Check (2.7–3.3)".to_string(),
        description: "Flags units whose X or Y calibrated score sits within [2.70, 3.30] sensitivity zone".to_string(),
        fact_key: "inCriticalZone".to_string(),
        passed: !in_critical_zone,
        triggered: in_critical_zone,
        severity: if in_critical_zone { "warning" } else { "pass" }.to_string(),
        message: if in_critical_zone {
            format!(
                "STRESS ZONE: Final scores (X={:.2}, Y={:.2}) sit in [2.70, 3.30] quadrant boundary band",
                final_x_score, final_y_score
            )
        } else {
            format!(
                "Clear quadrant margin outside [2.70, 3.30] band (X={:.2}, Y={:.2})",
                final_x_score, final_y_score
            )
        },
        remediation: if in_critical_zone {
            vec![
                "Verify FinOps TCO tier and check whether adapter decoupling shifts X below 2.70"
                    .to_string(),
            ]
        } else {
            Vec::new()
        },
    };

    let validation = validate_sourcing_record(record, None, system_domains);
    let sys_ref = canonical_system_ref(&record.system);
    let (resolved_domain, resolved_domain_tier) = system_domains
        .get(&sys_ref)
        .cloned()
        .or_else(|| {
            default_system_domain_map()
                .get(&sys_ref)
                .map(|(d, t)| ((*d).to_string(), (*t).to_string()))
        })
        .unwrap_or_else(|| {
            (
                "domain:default/platform-infra".to_string(),
                "Platform Infrastructure Domain".to_string(),
            )
        });

    let unit_key = record.name.trim().to_ascii_lowercase();
    SourcingEvaluation {
        entity_ref: format!(
            "resource:{}/{}",
            record.namespace.to_ascii_lowercase(),
            unit_key
        ),
        record: record.clone(),
        facts,
        checks: vec![check_windmill, check_reaudit, check_critical],
        validation,
        lineage,
        resolved_domain,
        resolved_domain_tier,
        remediation_playbook: playbook,
        source_origin: "seed-baseline".to_string(),
        ballot_count: 0,
        max_ballot_spread: 0.0,
        ballots: Vec::new(),
    }
}

pub fn render_catalog_info_yaml(
    record: &SourcingResourceRecord,
    facts: Option<&SourcingFacts>,
) -> String {
    let quadrant_label = facts
        .map(|f| f.quadrant.as_str())
        .unwrap_or("unclassified");
    let mismatch_label = facts
        .map(|f| if f.windmill_mismatch { "true" } else { "false" })
        .unwrap_or("false");

    let mut manifest = serde_yaml::Mapping::new();
    manifest.insert(
        serde_yaml::Value::String("apiVersion".to_string()),
        serde_yaml::Value::String("backstage.io/v1alpha1".to_string()),
    );
    manifest.insert(
        serde_yaml::Value::String("kind".to_string()),
        serde_yaml::Value::String("Resource".to_string()),
    );

    let mut metadata = serde_yaml::Mapping::new();
    metadata.insert(
        serde_yaml::Value::String("name".to_string()),
        serde_yaml::Value::String(record.name.clone()),
    );
    if !record.namespace.eq_ignore_ascii_case("default") {
        metadata.insert(
            serde_yaml::Value::String("namespace".to_string()),
            serde_yaml::Value::String(record.namespace.clone()),
        );
    }
    metadata.insert(
        serde_yaml::Value::String("description".to_string()),
        serde_yaml::Value::String(record.description.clone()),
    );

    let mut labels = serde_yaml::Mapping::new();
    labels.insert(
        serde_yaml::Value::String("sourcing.company.io/quadrant".to_string()),
        serde_yaml::Value::String(quadrant_label.to_string()),
    );
    labels.insert(
        serde_yaml::Value::String("sourcing.company.io/windmill-mismatch".to_string()),
        serde_yaml::Value::String(mismatch_label.to_string()),
    );
    metadata.insert(
        serde_yaml::Value::String("labels".to_string()),
        serde_yaml::Value::Mapping(labels),
    );

    let mut annotations = serde_yaml::Mapping::new();
    annotations.insert(
        serde_yaml::Value::String("finops.company.io/cost-allocation-id".to_string()),
        serde_yaml::Value::String(record.cost_allocation_id.clone()),
    );
    annotations.insert(
        serde_yaml::Value::String("sourcing.company.io/adapter-abstracted".to_string()),
        serde_yaml::Value::String(if record.adapter_abstracted {
            "true".to_string()
        } else {
            "false".to_string()
        }),
    );
    metadata.insert(
        serde_yaml::Value::String("annotations".to_string()),
        serde_yaml::Value::Mapping(annotations),
    );
    manifest.insert(
        serde_yaml::Value::String("metadata".to_string()),
        serde_yaml::Value::Mapping(metadata),
    );

    let mut spec = serde_yaml::Mapping::new();
    spec.insert(
        serde_yaml::Value::String("type".to_string()),
        serde_yaml::Value::String(record.resource_type.clone()),
    );
    spec.insert(
        serde_yaml::Value::String("owner".to_string()),
        serde_yaml::Value::String(record.owner.clone()),
    );
    spec.insert(
        serde_yaml::Value::String("system".to_string()),
        serde_yaml::Value::String(record.system.clone()),
    );
    if let Ok(audit_yaml) = serde_yaml::to_value(&record.audit) {
        spec.insert(
            serde_yaml::Value::String("sourcingAudit".to_string()),
            audit_yaml,
        );
    }
    manifest.insert(
        serde_yaml::Value::String("spec".to_string()),
        serde_yaml::Value::Mapping(spec),
    );

    serde_yaml::to_string(&serde_yaml::Value::Mapping(manifest)).unwrap_or_default()
}

pub fn compute_unified_diff(old_text: &str, new_text: &str, path_label: &str) -> String {
    if old_text == new_text {
        return format!("--- a/{}\n+++ b/{}\n@@ no changes @@\n", path_label, path_label);
    }
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();
    let mut prefix = 0usize;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix + prefix < old_lines.len()
        && suffix + prefix < new_lines.len()
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let ctx_before = prefix.saturating_sub(2);
    let old_end = old_lines.len() - suffix;
    let new_end = new_lines.len() - suffix;
    let ctx_after_end = (old_end + 2).min(old_lines.len());

    let mut out = String::new();
    out.push_str(&format!("--- a/{}\n+++ b/{}\n", path_label, path_label));
    out.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        ctx_before + 1,
        (old_end - ctx_before) + (ctx_after_end - old_end),
        ctx_before + 1,
        (new_end - ctx_before) + (ctx_after_end - old_end)
    ));
    for line in &old_lines[ctx_before..prefix] {
        out.push_str(&format!(" {}\n", line));
    }
    for line in &old_lines[prefix..old_end] {
        out.push_str(&format!("-{}\n", line));
    }
    for line in &new_lines[prefix..new_end] {
        out.push_str(&format!("+{}\n", line));
    }
    for line in &old_lines[old_end..ctx_after_end] {
        out.push_str(&format!(" {}\n", line));
    }
    out
}

pub fn tech_insights_fact_schema() -> TechInsightsFactSchemaResponse {
    let fields = vec![
        SourcingFactSchemaField {
            name: "finalYScore".to_string(),
            field_type: "float".to_string(),
            description: "校准后业务价值影响分 (1-5，含8维5分熔断升档)".to_string(),
        },
        SourcingFactSchemaField {
            name: "finalXScore".to_string(),
            field_type: "float".to_string(),
            description: "校准后供应锁定风险分 (1-5，含8维5分熔断升档)".to_string(),
        },
        SourcingFactSchemaField {
            name: "quadrant".to_string(),
            field_type: "string".to_string(),
            description: "卡拉杰克象限归属 (strategic | leverage | bottleneck | non-critical)"
                .to_string(),
        },
        SourcingFactSchemaField {
            name: "knockoutTriggered".to_string(),
            field_type: "boolean".to_string(),
            description: "是否触发Y1..Y4/X1..X4任意5分红线强制升档 (>=3.20)".to_string(),
        },
        SourcingFactSchemaField {
            name: "inCriticalZone".to_string(),
            field_type: "boolean".to_string(),
            description: "是否落入 2.70-3.30 临界压力测试带".to_string(),
        },
        SourcingFactSchemaField {
            name: "windmillMismatch".to_string(),
            field_type: "boolean".to_string(),
            description: "是否存在我方重度依赖但乙方视作 nuisance/exploitable 的双向错配"
                .to_string(),
        },
        SourcingFactSchemaField {
            name: "reAuditRequired".to_string(),
            field_type: "boolean".to_string(),
            description: "是否触发距续约<=180天、依赖翻倍或盲评方差重审".to_string(),
        },
    ];
    let mut schema = BTreeMap::new();
    for f in &fields {
        schema.insert(
            f.name.clone(),
            json!({
                "type": f.field_type,
                "description": f.description,
            }),
        );
    }
    TechInsightsFactSchemaResponse {
        id: "sourcing-kraljic-windmill-retriever".to_string(),
        version: "1.0.0".to_string(),
        entity_filter: vec![json!({
            "kind": "Resource",
            "spec.type": SOURCING_RESOURCE_TYPES,
        })],
        schema,
        fields,
    }
}

pub fn tech_insights_check_definitions() -> Vec<TechInsightsCheckDefinition> {
    vec![
        TechInsightsCheckDefinition {
            id: "sourcing-windmill-mismatch-redline".to_string(),
            name: "Windmill Symmetric Relationship Check".to_string(),
            description: "Passes when Strategic/Bottleneck dependencies are not viewed as 'nuisance' or 'exploitable' by the vendor".to_string(),
            fact_retriever_id: "sourcing-kraljic-windmill-retriever".to_string(),
            fact_key: "windmillMismatch".to_string(),
            severity: "critical".to_string(),
            check_type: "boolean".to_string(),
        },
        TechInsightsCheckDefinition {
            id: "sourcing-dynamic-reaudit-redline".to_string(),
            name: "Dynamic Contract & Topology Re-Audit Check".to_string(),
            description: "Passes when renewal > 180d, active prod dependents < 2x baseline, and variance alert is false".to_string(),
            fact_retriever_id: "sourcing-kraljic-windmill-retriever".to_string(),
            fact_key: "reAuditRequired".to_string(),
            severity: "warning".to_string(),
            check_type: "boolean".to_string(),
        },
        TechInsightsCheckDefinition {
            id: "sourcing-critical-zone-redline".to_string(),
            name: "Quadrant Boundary Stress-Test Check (2.7–3.3)".to_string(),
            description: "Passes when neither finalXScore nor finalYScore falls into [2.70, 3.30] boundary band".to_string(),
            fact_retriever_id: "sourcing-kraljic-windmill-retriever".to_string(),
            fact_key: "inCriticalZone".to_string(),
            severity: "warning".to_string(),
            check_type: "boolean".to_string(),
        },
    ]
}

impl SourcingStore {
    pub fn load(state_store_path: PathBuf, workspace_yaml_dir: Option<PathBuf>) -> Self {
        Self::load_with_options(state_store_path, workspace_yaml_dir, None, false)
    }

    pub fn load_with_options(
        state_store_path: PathBuf,
        workspace_yaml_dir: Option<PathBuf>,
        pg_url: Option<String>,
        in_memory: bool,
    ) -> Self {
        let mut legacy_records: BTreeMap<String, SourcingResourceRecord> = BTreeMap::new();
        let mut component_extra_deps: BTreeMap<String, Vec<String>> = BTreeMap::new();

        // 1. Load embedded seeds so clean fresh binary always boots complete demo & risk cases.
        for (_fname, content) in SEED_YAMLS {
            if let Ok(rec) = parse_sourcing_yaml_str(content) {
                legacy_records.insert(rec.name.to_ascii_lowercase(), rec);
            }
        }

        // 2. Overlay workspace GitOps manifests (<workspace>/catalog/sourcing/*.yaml) if present.
        if let Some(dir) = workspace_yaml_dir.as_ref().filter(|p| p.is_dir()) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                let mut paths: Vec<PathBuf> = entries
                    .filter_map(|e| e.ok().map(|x| x.path()))
                    .filter(|p| {
                        p.extension()
                            .and_then(|s| s.to_str())
                            .map(|ext| ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml"))
                            .unwrap_or(false)
                    })
                    .collect();
                paths.sort();
                for path in paths {
                    if let Ok(raw) = std::fs::read_to_string(&path) {
                        if let Ok(rec) = parse_sourcing_yaml_str(&raw) {
                            if is_sourcing_resource_type(&rec.resource_type) {
                                legacy_records.insert(rec.name.to_ascii_lowercase(), rec);
                            }
                        }
                    }
                }
            }
        }

        // 3. Overlay legacy YAML state overrides if migrating from older version.
        let legacy_yaml_path = if state_store_path
            .extension()
            .and_then(|s| s.to_str())
            .map(|e| e.eq_ignore_ascii_case("yaml") || e.eq_ignore_ascii_case("yml"))
            .unwrap_or(false)
        {
            state_store_path.clone()
        } else {
            state_store_path.with_file_name("sourcing-audit-store.yaml")
        };
        if legacy_yaml_path.is_file() {
            if let Ok(raw) = std::fs::read_to_string(&legacy_yaml_path) {
                if let Ok(persisted) = serde_yaml::from_str::<PersistedSourcingState>(&raw) {
                    for (k, rec) in persisted.records {
                        legacy_records.insert(k.to_ascii_lowercase(), rec);
                    }
                    component_extra_deps = persisted.component_extra_deps;
                }
            }
        }

        let sqlite_path = if state_store_path
            .extension()
            .and_then(|s| s.to_str())
            .map(|e| e.eq_ignore_ascii_case("yaml") || e.eq_ignore_ascii_case("yml"))
            .unwrap_or(false)
        {
            state_store_path.with_extension("sqlite3")
        } else {
            state_store_path.clone()
        };

        let db = SourcingDbHandle::open_or_init(sqlite_path, in_memory, pg_url, &legacy_records)
            .expect("initialize SQLite WAL sourcing database");

        let loaded = db.load_snapshot_records().expect("load sourcing DB snapshot");
        let mut store = Self {
            state_store_path,
            workspace_yaml_dir,
            db,
            records: loaded.records,
            source_origins: loaded.source_origins,
            ballots_by_unit: loaded.ballots_by_unit,
            max_spread_by_unit: loaded.max_spread_by_unit,
            component_extra_deps,
        };
        store.rebuild_component_extra_deps();
        store
    }

    pub fn refresh_from_db(&mut self) -> Result<(), String> {
        let loaded = self.db.load_snapshot_records()?;
        self.records = loaded.records;
        self.source_origins = loaded.source_origins;
        self.ballots_by_unit = loaded.ballots_by_unit;
        self.max_spread_by_unit = loaded.max_spread_by_unit;
        self.rebuild_component_extra_deps();
        Ok(())
    }

    pub fn refresh_if_db_changed(&mut self) -> bool {
        if self.db.check_external_db_mutation() {
            let _ = self.refresh_from_db();
            true
        } else {
            false
        }
    }

    fn rebuild_component_extra_deps(&mut self) {
        let mut next: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for rec in self.records.values() {
            if rec.depends_on_components.is_empty() {
                continue;
            }
            let target_ref = format!(
                "resource:{}/{}",
                rec.namespace.to_ascii_lowercase(),
                rec.name.to_ascii_lowercase()
            );
            for raw_comp in &rec.depends_on_components {
                let comp_ref = canonical_component_ref(raw_comp);
                let entry = next.entry(comp_ref).or_default();
                if !entry.iter().any(|r| r.eq_ignore_ascii_case(&target_ref)) {
                    entry.push(target_ref.clone());
                }
            }
        }
        // Preserve any legacy persisted edges that aren't already implied
        for (comp_ref, list) in &self.component_extra_deps {
            let entry = next.entry(comp_ref.clone()).or_default();
            for r in list {
                if self
                    .records
                    .values()
                    .any(|rec| {
                        let rr = format!(
                            "resource:{}/{}",
                            rec.namespace.to_ascii_lowercase(),
                            rec.name.to_ascii_lowercase()
                        );
                        rr.eq_ignore_ascii_case(r) && !rec.depends_on_components.is_empty()
                    })
                {
                    if !entry.iter().any(|e| e.eq_ignore_ascii_case(r)) {
                        entry.push(r.clone());
                    }
                }
            }
        }
        next.retain(|_, v| !v.is_empty());
        self.component_extra_deps = next;
    }

    pub fn records(&self) -> impl Iterator<Item = &SourcingResourceRecord> {
        self.records.values()
    }

    pub fn get(&self, name: &str) -> Option<&SourcingResourceRecord> {
        self.records.get(&name.trim().to_ascii_lowercase())
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut SourcingResourceRecord> {
        self.records.get_mut(&name.trim().to_ascii_lowercase())
    }

    pub fn component_extra_deps(&self) -> &BTreeMap<String, Vec<String>> {
        &self.component_extra_deps
    }

    pub fn enrich_evaluation(&self, eval: &mut SourcingEvaluation) {
        let key = eval.record.name.to_ascii_lowercase();
        eval.source_origin = self
            .source_origins
            .get(&key)
            .cloned()
            .unwrap_or_else(|| "seed-baseline".to_string());
        eval.ballots = self.ballots_by_unit.get(&key).cloned().unwrap_or_default();
        eval.ballot_count = eval.ballots.len();
        eval.max_ballot_spread = self.max_spread_by_unit.get(&key).copied().unwrap_or(0.0);
    }

    pub fn upsert_record(
        &mut self,
        record: SourcingResourceRecord,
        persist_to_disk: bool,
    ) -> Result<(), String> {
        self.upsert_record_with_meta(
            record,
            persist_to_disk,
            "preserve",
            "pitsctl",
            "portal-action",
            "Updated sourcing unit metadata",
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_record_with_meta(
        &mut self,
        mut record: SourcingResourceRecord,
        persist_to_disk: bool,
        source_origin: &str,
        actor: &str,
        channel: &str,
        event_summary: &str,
        clear_manual_variance_flag: bool,
    ) -> Result<(), String> {
        record.name = record.name.trim().to_ascii_lowercase();
        if record.namespace.trim().is_empty() {
            record.namespace = "default".to_string();
        }
        record.resource_type = record.resource_type.trim().to_ascii_lowercase();
        let declared_comps: BTreeSet<String> = record
            .depends_on_components
            .iter()
            .map(|c| canonical_component_ref(c))
            .collect();
        record.depends_on_components = declared_comps.into_iter().collect();

        self.db.upsert_unit_record(
            &record,
            source_origin,
            actor,
            channel,
            event_summary,
            clear_manual_variance_flag,
        )?;
        self.refresh_from_db()?;

        if persist_to_disk
            && self
                .state_store_path
                .extension()
                .and_then(|s| s.to_str())
                .map(|e| e.eq_ignore_ascii_case("yaml") || e.eq_ignore_ascii_case("yml"))
                .unwrap_or(false)
        {
            self.save_state_file()?;
        }
        Ok(())
    }

    pub fn save_state_file(&self) -> Result<(), String> {
        if let Some(parent) = self.state_store_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "Failed to create state directory '{}': {}",
                    parent.display(),
                    e
                )
            })?;
        }
        let persisted = PersistedSourcingState {
            version: 1,
            records: self.records.clone(),
            component_extra_deps: self.component_extra_deps.clone(),
        };
        let yaml = serde_yaml::to_string(&persisted)
            .map_err(|e| format!("Failed to serialize sourcing state YAML: {}", e))?;
        std::fs::write(&self.state_store_path, yaml).map_err(|e| {
            format!(
                "Failed to write sourcing state store '{}': {}",
                self.state_store_path.display(),
                e
            )
        })
    }

    pub fn write_workspace_yaml(
        &self,
        record: &SourcingResourceRecord,
        facts: Option<&SourcingFacts>,
    ) -> Result<PathBuf, String> {
        let dir = self.workspace_yaml_dir.as_ref().ok_or_else(|| {
            "Workspace catalog directory is not configured in the active profile".to_string()
        })?;
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Failed to create '{}': {}", dir.display(), e))?;
        let path = dir.join(format!("{}.yaml", record.name.to_ascii_lowercase()));
        let yaml = render_catalog_info_yaml(record, facts);
        std::fs::write(&path, yaml)
            .map_err(|e| format!("Failed to write workspace YAML '{}': {}", path.display(), e))?;
        Ok(path)
    }

    pub fn read_existing_workspace_yaml(&self, name: &str) -> String {
        let Some(dir) = &self.workspace_yaml_dir else {
            return String::new();
        };
        let path = dir.join(format!("{}.yaml", name.trim().to_ascii_lowercase()));
        std::fs::read_to_string(path).unwrap_or_default()
    }
}

pub fn today_utc_date() -> NaiveDate {
    Utc::now().date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "pits_sourcing_test_{}_{}",
            tag,
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn sys_map() -> BTreeMap<String, (String, String)> {
        default_system_domain_map()
            .into_iter()
            .map(|(k, (d, t))| (k, (d.to_string(), t.to_string())))
            .collect()
    }

    fn make_lineage(count: usize, sys: &str, dom: &str, tier: &str) -> Vec<DomainLineageEntry> {
        (0..count)
            .map(|i| DomainLineageEntry {
                component_ref: format!("component:default/comp-{}", i),
                component_title: format!("Component {}", i),
                system_ref: sys.to_string(),
                domain_ref: dom.to_string(),
                domain_tier: tier.to_string(),
            })
            .collect()
    }

    #[test]
    fn seed_payment_fraud_engine_matches_exact_kraljic_and_windmill_math() {
        let dir = temp_dir("math");
        let store = SourcingStore::load(dir.join("state.yaml"), None);
        let rec = store.get("payment-fraud-engine-sdk").expect("seed exists");
        let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let eval = evaluate_sourcing_audit_at(
            rec,
            make_lineage(
                3,
                "system:default/core-checkout",
                "domain:default/core-commerce",
                "Core Transaction Domain",
            ),
            &sys_map(),
            today,
        );
        assert_eq!(eval.facts.final_y_score, 3.45);
        assert_eq!(eval.facts.final_x_score, 4.63);
        assert_eq!(eval.facts.quadrant, "strategic");
        assert!(eval.facts.knockout_triggered);
        assert!(eval.facts.x_knockout);
        assert!(!eval.facts.y_knockout);
        assert!(eval
            .facts
            .knockout_dimensions
            .contains(&"x1_lockIn".to_string()));
        assert!(eval
            .facts
            .knockout_dimensions
            .contains(&"x4_complianceFragility".to_string()));
        assert!(eval.facts.windmill_mismatch);
        assert!(eval.facts.re_audit_required);
        assert!(!eval.facts.in_critical_zone);
        assert_eq!(eval.resolved_domain, "domain:default/core-commerce");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn knockout_rule_applies_across_all_eight_dimensions() {
        let dir = temp_dir("knockout8");
        let store = SourcingStore::load(dir.join("state.yaml"), None);
        let mut rec = store.get("hero-landmark-art-outsourcing").unwrap().clone();
        // Low weighted score on Y overall (y1=1, y2=1, y3=1.8, y4=5.0 -> rawY = 0.35 + 0.25 + 0.45 + 0.75 = 1.80)
        // Because y4_priceElasticity == 5.0, 5-point knockout MUST clamp finalYScore >= 3.20.
        rec.audit.manual_scores.y2_core_competitiveness = 1.0;
        rec.audit.manual_scores.y4_price_elasticity = 5.0;
        rec.audit.manual_scores.x2_market_scarcity = 1.0;
        rec.audit.manual_scores.x3_customization_depth = 1.0;
        rec.audit.manual_scores.x4_compliance_fragility = 5.0;
        // rawX = 2*0.35 + 1*0.25 + 1*0.25 + 5*0.15 = 0.70 + 0.25 + 0.25 + 0.75 = 1.95 -> clamped to 3.20!
        let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let eval = evaluate_sourcing_audit_at(&rec, Vec::new(), &sys_map(), today);
        assert!(eval.facts.y_knockout);
        assert!(eval.facts.x_knockout);
        assert_eq!(eval.facts.final_y_score, 3.2);
        assert_eq!(eval.facts.final_x_score, 3.2);
        assert_eq!(eval.facts.quadrant, "strategic");
        assert!(eval.facts.in_critical_zone);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_processor_rejects_monolith_and_plaintext_prices() {
        let monolith_yaml = r#"
apiVersion: backstage.io/v1alpha1
kind: Resource
metadata:
  name: aws-all
  description: Whole cloud vendor monolith
  annotations:
    finops.company.io/cost-allocation-id: "cc-1"
    sourcing.company.io/adapter-abstracted: "false"
    finops.company.io/annualCost: "120000"
spec:
  type: cloud-vendor
  owner: group:default/operators
  system: pits-infrastructure
  sourcingAudit:
    contractMeta:
      vendorName: "AWS"
      renewalDate: "2027-06-01"
      lastAuditedAt: "2026-11-15"
      baselineProdDependents: 2
    manualScores:
      y2_coreCompetitiveness: 4.0
      y4_priceElasticity: 2.0
      x2_marketScarcity: 3.0
      x3_customizationDepth: 3.0
      x4_complianceFragility: 4.0
    supplierPreference:
      estimatedWalletShareTier: "under-1pct"
      accountAttractivenessScore: 2.0
      perceivedStatus: "nuisance"
"#;
        let report = validate_raw_sourcing_yaml_payload(monolith_yaml, &sys_map());
        assert!(!report.valid);
        assert!(report
            .errors
            .iter()
            .any(|e| e.contains("Forbidden vendor-level spec.type")));
        assert!(report
            .errors
            .iter()
            .any(|e| e.contains("vendor-level catch-all package")));
        assert!(report
            .errors
            .iter()
            .any(|e| e.contains("FINOPS_SECRET_LEAK")));
    }

    #[test]
    fn store_persists_state_and_workspace_yaml_cleanly_in_temp_dir() {
        let dir = temp_dir("persist");
        let state_file = dir.join("state").join("sourcing-audit-store.yaml");
        let ws_dir = dir.join("workspace_sourcing");
        let mut store = SourcingStore::load(state_file.clone(), Some(ws_dir.clone()));

        let mut rec = store.get("hero-landmark-art-outsourcing").unwrap().clone();
        rec.name = "custom-tts-sdk".to_string();
        rec.resource_type = "commercial-sdk".to_string();
        rec.system = "core-checkout".to_string();
        rec.depends_on_components = vec!["component:default/checkout-risk-gateway".to_string()];
        store.upsert_record(rec.clone(), true).unwrap();
        store.write_workspace_yaml(&rec, None).unwrap();

        assert!(state_file.is_file());
        assert!(ws_dir.join("custom-tts-sdk.yaml").is_file());

        let reloaded = SourcingStore::load(state_file, Some(ws_dir));
        let fetched = reloaded.get("custom-tts-sdk").expect("reloaded record");
        assert_eq!(fetched.system, "core-checkout");
        assert_eq!(
            reloaded
                .component_extra_deps()
                .get("component:default/checkout-risk-gateway")
                .cloned()
                .unwrap_or_default(),
            vec!["resource:default/custom-tts-sdk".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
