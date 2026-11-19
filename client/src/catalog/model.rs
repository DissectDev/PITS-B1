//! Backstage `v1.56.x` Software Catalog (`backstage.io/v1alpha1`) entity models,
//! structured attribute sections, contextual action specifications, and query/facet contracts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Standard Backstage Software Catalog Entity (`backstage.io/v1alpha1`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntity {
    pub api_version: String,
    pub kind: String,
    pub metadata: EntityMetadata,
    pub spec: serde_json::Value,
    #[serde(default)]
    pub relations: Vec<EntityRelation>,
}

/// Standard Backstage `EntityMeta` descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityMetadata {
    pub uid: String,
    pub namespace: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Symmetric Backstage entity relation (`dependsOn`/`dependencyOf`, `partOf`/`hasPart`, `ownedBy`/`ownerOf`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityRelation {
    #[serde(rename = "type")]
    pub relation_type: String,
    pub target_ref: String,
}

/// Observation mode governing the static-policy vs. dynamic-telemetry balance of a Resource Type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ObservationMode {
    PolicyFirst,
    #[default]
    Hybrid,
    DynamicFirst,
    GovernanceEvent,
}

impl ObservationMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ObservationMode::PolicyFirst => "policy-first",
            ObservationMode::Hybrid => "hybrid",
            ObservationMode::DynamicFirst => "dynamic-first",
            ObservationMode::GovernanceEvent => "governance-event",
        }
    }

    pub fn cadence_tier(&self) -> &'static str {
        match self {
            ObservationMode::PolicyFirst => "slow-300s",
            ObservationMode::Hybrid => "standard-60s",
            ObservationMode::DynamicFirst => "fast-15s",
            ObservationMode::GovernanceEvent => "event-driven",
        }
    }
}

/// Compact static-policy or type-level highlight pill (`key`, `label`, `value`, `status`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryPill {
    pub key: String,
    pub label: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Live dynamic telemetry signal with delta (`Δ`), trend indicator, and observation freshness age.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicSignal {
    pub key: String,
    pub label: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_age_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Dual-track structured summary combining static contract invariants and live dynamic signals.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntitySummary {
    pub observation_mode: ObservationMode,
    pub cadence_tier: String,
    pub headline: String,
    #[serde(default)]
    pub static_policy: Vec<SummaryPill>,
    #[serde(default)]
    pub dynamic_signals: Vec<DynamicSignal>,
    #[serde(default)]
    pub drift_detected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_changed_at: Option<DateTime<Utc>>,
}

/// First-class Resource Type (`spec.type`) summary for portal directory & `pitsctl types`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTypeSummary {
    pub resource_type: String,
    pub category: String,
    pub title: String,
    pub description: String,
    pub entity_count: usize,
    pub healthy_count: usize,
    pub warning_count: usize,
    pub action_ids: Vec<String>,
    #[serde(default)]
    pub observation_mode: String,
    #[serde(default)]
    pub cadence_tier: String,
    #[serde(default)]
    pub policy_compliant_count: usize,
    #[serde(default)]
    pub drifted_count: usize,
    #[serde(default)]
    pub highlights: Vec<SummaryPill>,
}

/// Grouped key-value attribute table on an Entity Landing Page.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributeSection {
    pub id: String,
    pub title: String,
    pub items: Vec<AttributeItem>,
}

/// Single typed attribute row with optional semantic health badge and monospace formatting.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributeItem {
    pub key: String,
    pub label: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default)]
    pub monospace: bool,
}

/// Typed parameter schema for a contextual entity action.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionParamSpec {
    pub name: String,
    pub label: String,
    pub param_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<serde_json::Value>,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Contextual action specification gated by entity-level precondition evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityActionSpec {
    pub id: String,
    pub title: String,
    pub description: String,
    pub category: String,
    #[serde(default)]
    pub dangerous: bool,
    #[serde(default)]
    pub supports_dry_run: bool,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    #[serde(default)]
    pub parameters: Vec<ActionParamSpec>,
}

/// Rich Entity Landing Page payload combining Backstage `CatalogEntity`, structured attributes,
/// precondition-gated contextual actions, declarative YAML manifests, and deep diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PitsEntityDetail {
    pub entity: CatalogEntity,
    pub resource_type: String,
    pub status: String,
    pub status_level: String,
    pub summary_line: String,
    #[serde(default)]
    pub summary: EntitySummary,
    pub refreshed_at: DateTime<Utc>,
    pub live_connected: bool,
    pub attribute_sections: Vec<AttributeSection>,
    pub actions: Vec<EntityActionSpec>,
    pub manifests: BTreeMap<String, String>,
    pub diagnostics: serde_json::Value,
}

/// Request body for executing a contextual action on a specific catalog entity.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionInvokeRequest {
    #[serde(default)]
    pub parameters: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub dry_run: bool,
}

/// Structured execution report returned after running (or dry-running) an entity action.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionExecutionResult {
    pub success: bool,
    pub entity_ref: String,
    pub action_id: String,
    pub dry_run: bool,
    pub executed_at: DateTime<Utc>,
    pub summary: String,
    pub changes_or_warnings: Vec<String>,
    pub output: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered_artifact: Option<String>,
}

/// Query descriptor for Backstage `/api/catalog/entities` & `/api/catalog/entities/by-query`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntitiesQuery {
    #[serde(default)]
    pub filters: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_text_term: Option<String>,
}

/// Response payload for Backstage `/api/catalog/entities/by-query`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogQueryResponse {
    pub items: Vec<serde_json::Value>,
    pub total_items: usize,
    pub page_info: serde_json::Value,
}

/// Single value/count bucket in a Backstage catalog facet response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetEntry {
    pub value: String,
    pub count: usize,
}

/// Response payload for Backstage `/api/catalog/entity-facets`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogFacetsResponse {
    pub facets: BTreeMap<String, Vec<FacetEntry>>,
}

/// Internal point-in-time synthesized catalog snapshot cached with a 15-second TTL.
#[derive(Debug, Clone)]
pub(crate) struct CatalogSnapshot {
    pub entities: Vec<CatalogEntity>,
    pub details: BTreeMap<String, PitsEntityDetail>,
    pub resource_types: Vec<ResourceTypeSummary>,
}
