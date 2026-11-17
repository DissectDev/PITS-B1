//! Backstage `v1.56.x` Entity Registry (`backstage.io/v1alpha1`) & Contextual Action Engine
//! for PITS Hybrid Infrastructure + Sourcing Governance (3 Domains, 3 Systems, 7 Components, 12 Resource Types / 31 Resources).

pub mod actions;
pub mod discovery;
pub mod model;
pub mod sourcing;
pub mod sourcing_db;
pub mod synthesizer;

pub use model::{
    ActionExecutionResult, ActionInvokeRequest, ActionParamSpec, AttributeItem, AttributeSection,
    CatalogEntitiesQuery, CatalogEntity, CatalogFacetsResponse, CatalogQueryResponse,
    EntityActionSpec, EntityMetadata, EntityRelation, FacetEntry, PitsEntityDetail,
    ResourceTypeSummary,
};
pub use sourcing::{
    BlindBallotRecord, BooleanCheckResult, DiscoveryCandidateRecord, SourcingAuditEvent,
    SourcingDbSyncStatus, SourcingEvaluation, SourcingFacts, SourcingOverviewResponse,
    SourcingResourceRecord, SourcingStore, SourcingValidationReport, SourcingWizardUpsertRequest,
    SourcingWizardUpsertResponse,
};
pub use sourcing_db::{
    redact_db_url, AggregatedBallotStats, AssessWindmillRequest, CandidateDispositionRequest,
    SourcingDbHandle, SubmitBlindBallotRequest,
};
pub use synthesizer::CatalogRegistry;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ClientProfile, WorkspaceBundle};
    use serde::Deserialize;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn offline_registry() -> CatalogRegistry {
        let mut profile = ClientProfile::default();
        // Ensure zero live credentials so offline synthesis runs deterministically in <1ms
        profile.agent_bearer_token = None;
        profile.agent_ca_pem = None;
        profile.agent_client_cert_pem = None;
        profile.agent_client_key_pem = None;
        profile.core_kubeconfig_path = None;
        profile.lab_kubeconfig_path = None;
        profile.talosconfig_path = None;
        let bundle = WorkspaceBundle::builtin_pits_baseline();
        CatalogRegistry::new(Arc::new(RwLock::new(profile)), Arc::new(RwLock::new(bundle)))
    }

    #[tokio::test]
    async fn synthesizes_all_12_resource_types_and_44_4layer_entities_offline() {
        let reg = offline_registry();

        let types = reg.list_resource_types(false).await;
        assert_eq!(types.len(), 12, "expected 12 first-class Resource Types (8 infra + 4 sourcing)");

        let counts: BTreeMap<String, usize> = types
            .iter()
            .map(|t| (t.resource_type.clone(), t.entity_count))
            .collect();
        assert_eq!(counts.get("baremetal-host"), Some(&5));
        assert_eq!(counts.get("npu-accelerator"), Some(&4));
        assert_eq!(counts.get("virtual-machine"), Some(&2));
        assert_eq!(counts.get("edge-router"), Some(&1));
        assert_eq!(counts.get("pxe-boot-profile"), Some(&2));
        assert_eq!(counts.get("storage-pool"), Some(&1));
        assert_eq!(counts.get("san-volume"), Some(&9));
        assert_eq!(counts.get("kubernetes-cluster"), Some(&2));
        assert_eq!(counts.get("vendor-saas"), Some(&1));
        assert_eq!(counts.get("commercial-sdk"), Some(&2));
        assert_eq!(counts.get("managed-infra"), Some(&1));
        assert_eq!(counts.get("outsourced-dev"), Some(&1));

        let total_from_types: usize = types.iter().map(|t| t.entity_count).sum();
        assert_eq!(total_from_types, 31);

        // npu-accelerator must report 3 healthy (Intel) + 1 warning (netboot-003-npu AMD XDNA driver pending)
        let npu_summary = types
            .iter()
            .find(|t| t.resource_type == "npu-accelerator")
            .expect("npu-accelerator summary");
        assert_eq!(npu_summary.healthy_count, 3);
        assert_eq!(npu_summary.warning_count, 1);

        let entities = reg.list_entities(false).await;
        assert_eq!(
            entities.len(),
            44,
            "expected 3 Domains + 3 Systems + 7 Components + 31 Resources = 44 total entities"
        );

        let domain_count = entities.iter().filter(|e| e.kind == "Domain").count();
        let system_count = entities.iter().filter(|e| e.kind == "System").count();
        let component_count = entities.iter().filter(|e| e.kind == "Component").count();
        let resource_count = entities.iter().filter(|e| e.kind == "Resource").count();
        assert_eq!(domain_count, 3);
        assert_eq!(system_count, 3);
        assert_eq!(component_count, 7);
        assert_eq!(resource_count, 31);

        for ent in &entities {
            assert_eq!(ent.api_version, "backstage.io/v1alpha1");
            assert_eq!(ent.metadata.namespace, "default");
            assert!(!ent.metadata.uid.is_empty());
        }
    }

    #[tokio::test]
    async fn populates_symmetric_bidirectional_backstage_relations() {
        let reg = offline_registry();
        let entities = reg.list_entities(false).await;

        let by_ref: BTreeMap<String, &CatalogEntity> = entities
            .iter()
            .map(|e| {
                (
                    format!(
                        "{}:{}/{}",
                        e.kind.to_ascii_lowercase(),
                        e.metadata.namespace.to_ascii_lowercase(),
                        e.metadata.name.to_ascii_lowercase()
                    ),
                    e,
                )
            })
            .collect();

        let inverse_type = |t: &str| -> Option<&'static str> {
            match t {
                "dependsOn" => Some("dependencyOf"),
                "dependencyOf" => Some("dependsOn"),
                "partOf" => Some("hasPart"),
                "hasPart" => Some("partOf"),
                "ownedBy" => Some("ownerOf"),
                "ownerOf" => Some("ownedBy"),
                _ => None,
            }
        };

        let mut verified_symmetric_edges = 0usize;
        for (src_ref, ent) in &by_ref {
            assert!(
                !ent.relations.is_empty(),
                "entity {src_ref} should have Backstage relations"
            );
            for rel in &ent.relations {
                let target_key = rel.target_ref.to_ascii_lowercase();
                if let Some(target_ent) = by_ref.get(&target_key) {
                    if let Some(expected_inv) = inverse_type(&rel.relation_type) {
                        let has_inverse = target_ent.relations.iter().any(|tr| {
                            tr.relation_type == expected_inv
                                && tr.target_ref.eq_ignore_ascii_case(src_ref)
                        });
                        assert!(
                            has_inverse,
                            "missing inverse relation: {src_ref} --({})--> {target_key}, expected {target_key} --({expected_inv})--> {src_ref}",
                            rel.relation_type
                        );
                        verified_symmetric_edges += 1;
                    }
                }
            }
        }
        assert!(
            verified_symmetric_edges >= 60,
            "expected rich symmetric relation graph, got {verified_symmetric_edges} edges"
        );
    }

    #[tokio::test]
    async fn supports_backstage_query_filters_projection_search_and_pagination() {
        let reg = offline_registry();

        // 1. Comma-separated AND clauses + case-insensitivity
        let san_resp = reg
            .query_catalog_entities(
                &CatalogEntitiesQuery {
                    filters: vec!["kind=resource,spec.type=SAN-VOLUME".to_string()],
                    ..Default::default()
                },
                false,
            )
            .await;
        assert_eq!(san_resp.total_items, 9);
        assert_eq!(san_resp.items.len(), 9);

        // 2. Multiple filters OR'd together
        let multi_resp = reg
            .query_catalog_entities(
                &CatalogEntitiesQuery {
                    filters: vec![
                        "kind=system".to_string(),
                        "kind=resource,spec.type=edge-router".to_string(),
                        "kind=resource,spec.type=storage-pool".to_string(),
                    ],
                    ..Default::default()
                },
                false,
            )
            .await;
        assert_eq!(multi_resp.total_items, 5);

        // 3. Array traversal on `relations.targetRef` and `metadata.tags`
        let rel_resp = reg
            .query_catalog_entities(
                &CatalogEntitiesQuery {
                    filters: vec![
                        "kind=resource,relations.targetRef=resource:default/pits-data".to_string(),
                    ],
                    ..Default::default()
                },
                false,
            )
            .await;
        // 9 SAN volumes + pits-core (hasPart/dependencyOf pits-data)
        assert!(rel_resp.total_items >= 9);

        let tag_resp = reg
            .query_catalog_entities(
                &CatalogEntitiesQuery {
                    filters: vec!["metadata.tags=amd-xdna".to_string()],
                    ..Default::default()
                },
                false,
            )
            .await;
        assert_eq!(tag_resp.total_items, 1);
        assert_eq!(
            tag_resp.items[0]
                .pointer("/metadata/name")
                .and_then(|v| v.as_str()),
            Some("netboot-003-npu")
        );

        // 4. Annotation key containing dots (`pits.io/status`)
        let warn_resp = reg
            .query_catalog_entities(
                &CatalogEntitiesQuery {
                    filters: vec![
                        "metadata.annotations.pits.io/status=HardwarePresentDriverPending"
                            .to_string(),
                    ],
                    ..Default::default()
                },
                false,
            )
            .await;
        assert_eq!(warn_resp.total_items, 1);

        // 5. Full-text search + fields projection + pagination
        let page1 = reg
            .query_catalog_entities(
                &CatalogEntitiesQuery {
                    filters: vec!["kind=resource,spec.type=baremetal-host".to_string()],
                    fields: Some("kind,metadata.name,spec.type".to_string()),
                    limit: Some(2),
                    offset: Some(0),
                    full_text_term: None,
                },
                false,
            )
            .await;
        assert_eq!(page1.total_items, 5);
        assert_eq!(page1.items.len(), 2);
        assert_eq!(
            page1.page_info.get("nextCursor").and_then(|v| v.as_str()),
            Some("2")
        );
        // Projected object should contain `metadata.name` and omit `relations`
        assert!(page1.items[0].pointer("/metadata/name").is_some());
        assert!(page1.items[0].get("relations").is_none());

        let search_resp = reg
            .query_catalog_entities(
                &CatalogEntitiesQuery {
                    full_text_term: Some("AtomMan 185H".to_string()),
                    ..Default::default()
                },
                false,
            )
            .await;
        assert!(
            search_resp.total_items >= 1,
            "expected AtomMan 185H full-text match"
        );
    }

    #[tokio::test]
    async fn computes_facets_and_resolves_entities_by_name_uid_and_refs() {
        let reg = offline_registry();

        let facets_resp = reg
            .compute_facets(
                &[
                    "kind".to_string(),
                    "spec.type".to_string(),
                    "metadata.tags".to_string(),
                ],
                &["kind=resource".to_string()],
                false,
            )
            .await;

        let kind_facets = facets_resp.facets.get("kind").expect("kind facets");
        assert_eq!(kind_facets.len(), 1);
        assert_eq!(kind_facets[0].value, "Resource");
        assert_eq!(kind_facets[0].count, 31);

        let type_facets = facets_resp
            .facets
            .get("spec.type")
            .expect("spec.type facets");
        assert_eq!(type_facets.len(), 12);
        assert_eq!(type_facets[0].value, "san-volume");
        assert_eq!(type_facets[0].count, 9);

        // get_entity_by_name + get_entity_by_uid
        let nb4 = reg
            .get_entity_by_name("resource", "default", "netboot-004", false)
            .await
            .expect("netboot-004 entity");
        let by_uid = reg
            .get_entity_by_uid(&nb4.metadata.uid, false)
            .await
            .expect("netboot-004 by uid");
        assert_eq!(by_uid.metadata.name, "netboot-004");

        // get_entities_by_refs preserves exact order, length, and None for missing
        let refs = vec![
            "resource:default/netboot-004".to_string(),
            "system:default/pits-infrastructure".to_string(),
            "resource:does-not-exist".to_string(),
            "resource:pits-data".to_string(),
        ];
        let resolved = reg.get_entities_by_refs(&refs, false).await;
        assert_eq!(resolved.len(), 4);
        assert_eq!(
            resolved[0].as_ref().map(|e| e.metadata.name.as_str()),
            Some("netboot-004")
        );
        assert_eq!(
            resolved[1].as_ref().map(|e| e.metadata.name.as_str()),
            Some("pits-infrastructure")
        );
        assert!(resolved[2].is_none());
        assert_eq!(
            resolved[3].as_ref().map(|e| e.metadata.name.as_str()),
            Some("pits-data")
        );
    }

    #[tokio::test]
    async fn entity_detail_and_safety_guards_enforce_hardware_and_session_invariants() {
        let reg = offline_registry();

        // 1. Inspect netboot-003-npu: HardwarePresentDriverPending & disabled launch-openvino-verify
        let npu3 = reg
            .get_pits_entity_detail("default", "npu-accelerator/netboot-003-npu", false)
            .await
            .expect("netboot-003-npu detail");
        assert_eq!(npu3.status, "HardwarePresentDriverPending");
        assert_eq!(npu3.status_level, "warn");
        let verify_action = npu3
            .actions
            .iter()
            .find(|a| a.id == "launch-openvino-verify")
            .expect("launch-openvino-verify spec");
        assert!(!verify_action.enabled);
        assert!(verify_action
            .disabled_reason
            .as_deref()
            .unwrap_or("")
            .contains("amdxdna"));

        // Non-dry-run execution on netboot-003-npu must be rejected by precondition guard
        let err = reg
            .execute_entity_action(
                "default",
                "npu-accelerator/netboot-003-npu",
                "launch-openvino-verify",
                ActionInvokeRequest {
                    parameters: BTreeMap::new(),
                    dry_run: false,
                },
            )
            .await
            .expect_err("non-dry-run on netboot-003-npu must fail guard");
        assert!(err.to_string().contains("HardwarePresentDriverPending"));

        // Dry-run preview on netboot-003-npu is permitted for manifest inspection
        let dry_npu3 = reg
            .execute_entity_action(
                "default",
                "netboot-003-npu",
                "launch-openvino-verify",
                ActionInvokeRequest {
                    parameters: BTreeMap::new(),
                    dry_run: true,
                },
            )
            .await
            .expect("dry-run on netboot-003-npu should succeed");
        assert!(dry_npu3.success && dry_npu3.dry_run);
        assert!(dry_npu3.rendered_artifact.is_some());

        // 2. EdgeRouter mgmt0 (0000:58:00.0) PCI guard
        let mut bad_router = crate::router::pits_core_default_edge_router();
        bad_router.spec.wan.pci_address = "0000:58:00.0".to_string();
        let bad_router_yaml =
            serde_yaml::to_string(&bad_router).expect("serialize mutated EdgeRouter");
        let mut router_params = BTreeMap::new();
        router_params.insert("yaml".to_string(), json!(bad_router_yaml));
        let router_err = reg
            .execute_entity_action(
                "default",
                "edge-router/pits-core-router",
                "apply-router-spec",
                ActionInvokeRequest {
                    parameters: router_params,
                    dry_run: true,
                },
            )
            .await
            .expect_err("mgmt0 0000:58:00.0 theft must be blocked");
        assert!(router_err.to_string().contains("0000:58:00.0"));

        // Default router spec passes dry-run validation
        let router_ok = reg
            .execute_entity_action(
                "default",
                "edge-router/pits-core-router",
                "apply-router-spec",
                ActionInvokeRequest {
                    parameters: BTreeMap::new(),
                    dry_run: true,
                },
            )
            .await
            .expect("valid router spec dry-run");
        assert!(router_ok.success);

        // 3. Talos netboot-guard blocks DiskWipeConfig / machine.install
        let bad_talos_yaml = r#"
apiVersion: v1alpha1
kind: DiskWipeConfig
name: wipe-local-nvme
"#;
        let mut talos_params = BTreeMap::new();
        talos_params.insert("yaml".to_string(), json!(bad_talos_yaml));
        let talos_err = reg
            .execute_entity_action(
                "default",
                "baremetal-host/netboot-004",
                "apply-talos-config",
                ActionInvokeRequest {
                    parameters: talos_params,
                    dry_run: true,
                },
            )
            .await
            .expect_err("DiskWipeConfig must be rejected by netboot-guard");
        assert!(talos_err.to_string().contains("netboot-guard"));

        let talos_ok = reg
            .execute_entity_action(
                "default",
                "baremetal-host/netboot-004",
                "apply-talos-config",
                ActionInvokeRequest {
                    parameters: BTreeMap::new(),
                    dry_run: true,
                },
            )
            .await
            .expect("rendered netboot Talos config should pass netboot-guard");
        assert!(talos_ok.success);

        // 4. win11-001 self-session stop lock (`confirmSelfSessionStop == true`)
        let stop_err = reg
            .execute_entity_action(
                "default",
                "virtual-machine/win11-001",
                "stop",
                ActionInvokeRequest {
                    parameters: BTreeMap::new(),
                    dry_run: true,
                },
            )
            .await
            .expect_err("win11-001 stop without confirmSelfSessionStop must fail");
        assert!(stop_err.to_string().contains("confirmSelfSessionStop"));

        let mut confirm_params = BTreeMap::new();
        confirm_params.insert("confirmSelfSessionStop".to_string(), json!(true));
        let stop_ok = reg
            .execute_entity_action(
                "default",
                "virtual-machine/win11-001",
                "stop",
                ActionInvokeRequest {
                    parameters: confirm_params,
                    dry_run: true,
                },
            )
            .await
            .expect("win11-001 dry-run stop with confirmSelfSessionStop=true should succeed");
        assert!(stop_ok.success);

        let switch_dry = reg
            .execute_entity_action(
                "default",
                "virtual-machine/win11-001",
                "switch-win11-pool-host",
                ActionInvokeRequest {
                    parameters: BTreeMap::from([
                        ("targetNode".to_string(), json!("netboot-003")),
                        ("upgradeNetboot004".to_string(), json!(true)),
                    ]),
                    dry_run: true,
                },
            )
            .await
            .expect("win11-001 switch-win11-pool-host dry-run should succeed");
        assert!(switch_dry.success);
        assert!(switch_dry
            .rendered_artifact
            .as_deref()
            .unwrap_or("")
            .contains("win11-pool-switch.py switch --target netboot-003 --upgrade-004"));

        let switch_unconfirmed = reg
            .execute_entity_action(
                "default",
                "virtual-machine/win11-001",
                "switch-win11-pool-host",
                ActionInvokeRequest {
                    parameters: BTreeMap::from([("targetNode".to_string(), json!("netboot-003"))]),
                    dry_run: false,
                },
            )
            .await
            .expect_err("non-dry-run switch-win11-pool-host without confirmSelfSessionStop must fail");
        assert!(switch_unconfirmed
            .to_string()
            .contains("confirmSelfSessionStop"));

        let live_mig_dry = reg
            .execute_entity_action(
                "default",
                "virtual-machine/win11-001",
                "live-migrate-vm",
                ActionInvokeRequest {
                    parameters: BTreeMap::from([("targetNode".to_string(), json!("netboot-001"))]),
                    dry_run: true,
                },
            )
            .await
            .expect("win11-001 live-migrate-vm dry-run for netboot-001 should succeed");
        assert!(live_mig_dry.success);
        assert!(live_mig_dry
            .rendered_artifact
            .as_deref()
            .unwrap_or("")
            .contains("kind: VirtualMachineInstanceMigration"));
        let live_mig_bad = reg
            .execute_entity_action(
                "default",
                "virtual-machine/win11-001",
                "live-migrate-vm",
                ActionInvokeRequest {
                    parameters: BTreeMap::from([("targetNode".to_string(), json!("netboot-003"))]),
                    dry_run: true,
                },
            )
            .await
            .expect_err("live-migrate-vm to AMD netboot-003 must fail");
        assert!(live_mig_bad.to_string().contains("netboot-003"));

        // 5. SAN volume guards: system NSID 1 blocked from expand; userdata online expand allowed in dry-run
        let sys_expand_err = reg
            .execute_entity_action(
                "default",
                "san-volume/netboot-004-system",
                "expand-volume",
                ActionInvokeRequest {
                    parameters: BTreeMap::from([("sizeGib".to_string(), json!(128))]),
                    dry_run: true,
                },
            )
            .await
            .expect_err("system boot NSID 1 must not expand online");
        assert!(sys_expand_err.to_string().contains("NSID 1"));

        let ud_expand_ok = reg
            .execute_entity_action(
                "default",
                "san-volume/netboot-004-userdata",
                "expand-volume",
                ActionInvokeRequest {
                    parameters: BTreeMap::from([("sizeGib".to_string(), json!(160))]),
                    dry_run: true,
                },
            )
            .await
            .expect("userdata dry-run expand to 160 GiB should succeed");
        assert!(ud_expand_ok.success);
    }

    #[tokio::test]
    async fn exports_valid_multi_document_backstage_catalog_yaml() {
        let reg = offline_registry();
        let yaml = reg.export_catalog_yaml(false).await.expect("export yaml");

        let mut parsed = Vec::new();
        for doc in serde_yaml::Deserializer::from_str(&yaml) {
            let ent = CatalogEntity::deserialize(doc).expect("valid CatalogEntity YAML document");
            parsed.push(ent);
        }
        assert_eq!(parsed.len(), 44);
    }

    #[tokio::test]
    async fn sourcing_entity_action_and_wizard_upsert_recompute_kraljic_facts_end_to_end() {
        let tmp = std::env::temp_dir().join(format!(
            "pits_mod_sourcing_e2e_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let store = SourcingStore::load(
            tmp.join("sourcing-audit-store.yaml"),
            Some(tmp.join("workspace_sourcing")),
        );
        let reg = CatalogRegistry::new_with_sourcing_store(
            Arc::new(RwLock::new(ClientProfile::default())),
            Arc::new(RwLock::new(WorkspaceBundle::builtin_pits_baseline())),
            store,
        );

        // 1. Inspect seeded payment-fraud-engine-sdk detail (3 upstream components -> Y=3.45, X=4.63)
        let eval = reg
            .evaluate_sourcing_for_entity("default", "resource", "payment-fraud-engine-sdk")
            .await
            .expect("seeded evaluation");
        assert_eq!(eval.facts.quadrant, "strategic");
        assert_eq!(eval.facts.prod_dependents, 3);
        assert!(eval.facts.windmill_mismatch);
        assert_eq!(eval.resolved_domain, "domain:default/core-commerce");

        // 2. Toggle adapter-abstracted to true (persisted to temp store) -> X1 drops from 5.0 to 2.0
        let res = reg
            .execute_entity_action(
                "default",
                "commercial-sdk/payment-fraud-engine-sdk",
                "toggle-adapter-abstraction",
                ActionInvokeRequest {
                    parameters: BTreeMap::from([("adapterAbstracted".to_string(), json!(true))]),
                    dry_run: false,
                },
            )
            .await
            .expect("toggle adapter action");
        assert!(res.success);

        let updated_eval = reg
            .evaluate_sourcing_for_entity("default", "resource", "payment-fraud-engine-sdk")
            .await
            .expect("updated evaluation");
        assert_eq!(updated_eval.facts.breakdown.x1_lock_in, 2.0);
        // X4 complianceFragility is still 5.0 -> 5-point knockout keeps x_knockout=true and finalXScore >= 3.20 (rawX=3.58)
        assert_eq!(updated_eval.facts.final_x_score, 3.58);
        assert!(updated_eval.facts.knockout_triggered);

        // 3. Upsert a new sourcing unit via Wizard wired to 2 components & write workspace YAML
        let mut new_rec = updated_eval.record.clone();
        new_rec.name = "wizard-ocr-sdk".to_string();
        new_rec.description = "OCR document extraction SDK".to_string();
        new_rec.depends_on_components = vec![
            "component:default/checkout-risk-gateway".to_string(),
            "component:default/payment-auth-router".to_string(),
        ];
        new_rec.adapter_abstracted = true;
        new_rec.audit.manual_scores.x4_compliance_fragility = 2.0;
        new_rec.audit.supplier_preference.perceived_status = "core".to_string();

        let wizard_resp = reg
            .upsert_sourcing_from_wizard(SourcingWizardUpsertRequest {
                record: new_rec,
                write_workspace_yaml: true,
                dry_run: false,
                override_reason: Some("integration test emergency fallback".to_string()),
                actor: Some("test-operator".to_string()),
            })
            .await
            .expect("wizard upsert");
        assert!(wizard_resp.ok);
        assert!(wizard_resp.workspace_yaml_written);
        assert_eq!(wizard_resp.evaluation.facts.prod_dependents, 2);
        assert!(!wizard_resp.evaluation.facts.windmill_mismatch);
        assert_eq!(wizard_resp.evaluation.source_origin, "wizard-fallback");

        let overview = reg.sourcing_overview(false).await;
        assert_eq!(overview.total_units, 6);
        assert_eq!(overview.db_sync.backend_mode, "sqlite-wal-local");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn multi_role_blind_ballots_compute_mean_and_auto_variance_alert() {
        let profile = Arc::new(RwLock::new(ClientProfile::default()));
        let bundle = Arc::new(RwLock::new(WorkspaceBundle::builtin_pits_baseline()));
        let reg = CatalogRegistry::new(profile, bundle);

        // Submit two blind ballots with wide divergence (>= 1.5 spread) on payment-fraud-engine-sdk
        reg.sourcing_submit_ballot(SubmitBlindBallotRequest {
            unit_name: "payment-fraud-engine-sdk".to_string(),
            reviewer_role: "architecture".to_string(),
            reviewer_id: "arch-lead".to_string(),
            y2_core_competitiveness: 4.8,
            y4_price_elasticity: 2.0,
            x2_market_scarcity: 4.9,
            x3_customization_depth: 4.2,
            x4_compliance_fragility: 5.0,
            evidence_notes: "Deep SDK coupling".to_string(),
        })
        .await
        .expect("arch ballot");

        let stats = reg
            .sourcing_submit_ballot(SubmitBlindBallotRequest {
                unit_name: "payment-fraud-engine-sdk".to_string(),
                reviewer_role: "procurement".to_string(),
                reviewer_id: "sourcing-buyer".to_string(),
                y2_core_competitiveness: 2.8,
                y4_price_elasticity: 2.0,
                x2_market_scarcity: 4.1,
                x3_customization_depth: 3.8,
                x4_compliance_fragility: 5.0,
                evidence_notes: "Alternate vendors claim parity".to_string(),
            })
            .await
            .expect("procurement ballot");

        assert!(stats.variance_alert);
        assert!(stats.max_spread >= 1.5);

        let eval = reg
            .evaluate_sourcing_for_entity("default", "resource", "payment-fraud-engine-sdk")
            .await
            .expect("re-evaluated fraud sdk");
        assert!(eval.record.audit.contract_meta.blind_score_variance_alert);
        assert_eq!(eval.ballot_count, 2);
        assert!((eval.record.audit.manual_scores.y2_core_competitiveness - 3.8).abs() < 0.01);
        assert!((eval.record.audit.manual_scores.x2_market_scarcity - 4.5).abs() < 0.01);
    }

    #[tokio::test]
    async fn thunderbolt_busypoll_action_validates_and_renders_patch_offline() {
        let profile = Arc::new(RwLock::new(ClientProfile::default()));
        let bundle = Arc::new(RwLock::new(WorkspaceBundle::builtin_pits_baseline()));
        let reg = CatalogRegistry::new(profile, bundle);

        let mut ok_params = BTreeMap::new();
        ok_params.insert("socketBusyPollUs".to_string(), json!(64));
        ok_params.insert("streamRingSize".to_string(), json!(512));
        ok_params.insert("streamBusyPoll".to_string(), json!(true));
        let ok_res = reg
            .execute_entity_action(
                "default",
                "baremetal-host/netboot-003",
                "configure-thunderbolt-busypoll",
                ActionInvokeRequest {
                    dry_run: true,
                    parameters: ok_params,
                },
            )
            .await
            .expect("dry-run configure-thunderbolt-busypoll");
        assert!(ok_res.success);
        let rendered = ok_res.rendered_artifact.expect("rendered YAML patch");
        assert!(rendered.contains("net.core.busy_poll: \"64\""));
        assert!(rendered.contains("PITS_TBSTREAM_RING_SIZE=512"));
        assert!(rendered.contains("PITS_TBSTREAM_IN_HOPID=-1"));

        let mut bad_params = BTreeMap::new();
        bad_params.insert("streamRingSize".to_string(), json!(8));
        let err = reg
            .execute_entity_action(
                "default",
                "baremetal-host/netboot-003",
                "configure-thunderbolt-busypoll",
                ActionInvokeRequest {
                    dry_run: true,
                    parameters: bad_params,
                },
            )
            .await
            .expect_err("invalid ring size must be rejected");
        assert!(err.to_string().contains("streamRingSize"));
    }
}
