import type { Entity } from '@backstage/catalog-model';

export type StatusLevel = 'ok' | 'warn' | 'error' | 'info' | string;

export interface ResourceTypeSummary {
  resourceType: string;
  category: string;
  title: string;
  description: string;
  entityCount: number;
  healthyCount: number;
  warningCount: number;
  actionIds: string[];
}

export interface AttributeItem {
  key: string;
  label: string;
  value: string;
  status?: StatusLevel;
  monospace?: boolean;
}

export interface AttributeSection {
  id: string;
  title: string;
  items: AttributeItem[];
}

export interface ActionParamSpec {
  name: string;
  label: string;
  paramType: 'string' | 'integer' | 'number' | 'boolean' | 'select' | 'yaml' | string;
  required?: boolean;
  defaultValue?: unknown;
  options?: string[];
  description?: string;
}

export interface EntityActionSpec {
  id: string;
  title: string;
  description: string;
  category: string;
  dangerous?: boolean;
  supportsDryRun?: boolean;
  enabled: boolean;
  disabledReason?: string;
  parameters?: ActionParamSpec[];
}

export interface PitsEntityDetail {
  entity: Entity;
  resourceType: string;
  status: string;
  statusLevel: StatusLevel;
  summaryLine: string;
  refreshedAt: string;
  liveConnected: boolean;
  attributeSections: AttributeSection[];
  actions: EntityActionSpec[];
  manifests: Record<string, string>;
  diagnostics: unknown;
}

export interface ActionInvokeRequest {
  parameters: Record<string, unknown>;
  dryRun: boolean;
}

export interface ActionExecutionResult {
  success: boolean;
  entityRef: string;
  actionId: string;
  dryRun: boolean;
  executedAt: string;
  summary: string;
  changesOrWarnings: string[];
  output: unknown;
  renderedArtifact?: string;
}

export interface OperatorProfileState {
  activeWorkspaceSource: string;
  sourcingDbConfigured?: boolean;
  sourcingDbEndpointRedacted?: string;
  agentEndpoint: string;
  hasClientPki: boolean;
  hasBearerToken: boolean;
  coreKubeconfigPath: string;
  coreContext: string;
  labKubeconfigPath: string;
  labContext: string;
  labViaAgentBridge: boolean;
  talosconfigPath: string;
  talosContext: string;
  talosViaAgentBridge: boolean;
  stateDir?: string;
  profilePath?: string;
}

export interface OperatorProfilePatch {
  activeWorkspaceSource?: string;
  sourcingDbUrl?: string;
  agentEndpoint?: string;
  coreKubeconfigPath?: string;
  coreContext?: string;
  labKubeconfigPath?: string;
  labContext?: string;
  labViaAgentBridge?: boolean;
  talosconfigPath?: string;
  talosContext?: string;
  talosViaAgentBridge?: boolean;
}

export interface GeneratedPkiResult {
  savedProfile?: string;
  caCertPem: string;
  serverCertPem: string;
  serverKeyPem: string;
}

export type ResourceDomainGroup =
  | 'Compute & Accelerators'
  | 'Network & Boot'
  | 'Storage Fabric'
  | 'Control Planes'
  | 'Sourcing Governance';

export interface ResourceTypeMeta {
  type: string;
  domain: ResourceDomainGroup;
  shortTitle: string;
  fullTitle: string;
  description: string;
}

export const RESOURCE_DOMAIN_ORDER: ResourceDomainGroup[] = [
  'Compute & Accelerators',
  'Network & Boot',
  'Storage Fabric',
  'Control Planes',
  'Sourcing Governance',
];

export const SOURCING_RESOURCE_TYPE_SET = new Set([
  'vendor-saas',
  'commercial-sdk',
  'managed-infra',
  'outsourced-dev',
]);

export interface ContractMeta {
  vendorName: string;
  renewalDate: string;
  lastAuditedAt: string;
  baselineProdDependents: number;
  blindScoreVarianceAlert: boolean;
}

export interface ManualScores {
  y2_coreCompetitiveness: number;
  y4_priceElasticity: number;
  x2_marketScarcity: number;
  x3_customizationDepth: number;
  x4_complianceFragility: number;
}

export interface SupplierPreference {
  estimatedWalletShareTier: string;
  accountAttractivenessScore: number;
  perceivedStatus: string;
}

export interface SourcingAuditSpec {
  contractMeta: ContractMeta;
  manualScores: ManualScores;
  supplierPreference: SupplierPreference;
}

export interface SourcingResourceRecord {
  name: string;
  namespace: string;
  description: string;
  resourceType: string;
  owner: string;
  system: string;
  adapterAbstracted: boolean;
  costAllocationId: string;
  dependsOnComponents: string[];
  audit: SourcingAuditSpec;
}

export interface DimensionBreakdown {
  y1BlastRadius: number;
  y2CoreCompetitiveness: number;
  y3TcoRatio: number;
  y4PriceElasticity: number;
  x1LockIn: number;
  x2MarketScarcity: number;
  x3CustomizationDepth: number;
  x4ComplianceFragility: number;
  rawYScore: number;
  rawXScore: number;
}

export interface SourcingFacts {
  finalYScore: number;
  finalXScore: number;
  quadrant: string;
  knockoutTriggered: boolean;
  yKnockout: boolean;
  xKnockout: boolean;
  knockoutDimensions: string[];
  inCriticalZone: boolean;
  windmillMismatch: boolean;
  reAuditRequired: boolean;
  prodDependents: number;
  daysToRenewal: number;
  reAuditReasons: string[];
  breakdown: DimensionBreakdown;
  finopsSource: string;
}

export interface BooleanCheckResult {
  id: string;
  name: string;
  description: string;
  factKey: string;
  passed: boolean;
  triggered: boolean;
  severity: string;
  message: string;
  remediation: string[];
}

export interface ProcessorRuleResult {
  id: string;
  title: string;
  passed: boolean;
  severity: string;
  message: string;
}

export interface SourcingValidationReport {
  valid: boolean;
  rules: ProcessorRuleResult[];
  errors: string[];
  warnings: string[];
  inferredDomain?: string;
  domainTier?: string;
}

export interface DomainLineageEntry {
  componentRef: string;
  componentTitle: string;
  systemRef: string;
  domainRef: string;
  domainTier: string;
}

export interface BlindBallotRecord {
  ballotId: string;
  unitName: string;
  reviewerRole: string;
  reviewerId: string;
  y2_coreCompetitiveness: number;
  y4_priceElasticity: number;
  x2_marketScarcity: number;
  x3_customizationDepth: number;
  x4_complianceFragility: number;
  evidenceNotes: string;
  submittedAt: string;
}

export interface DiscoveryCandidateRecord {
  candidateId: string;
  packageOrEndpoint: string;
  ecosystem: string;
  suggestedType: string;
  detectedInPath: string;
  inferredSystem: string;
  inferredComponents: string[];
  adapterDetected: boolean;
  monolithRiskNote: string;
  status: string;
  promotedUnitName?: string;
  discoveredAt: string;
}

export interface SourcingAuditEvent {
  eventId: string;
  unitName: string;
  eventType: string;
  actor: string;
  channel: string;
  summary: string;
  payload: unknown;
  createdAt: string;
}

export interface SourcingDbSyncStatus {
  backendMode: string;
  sqliteVersion: string;
  sqlitePath: string;
  sqliteJournalMode: string;
  postgresConfigured: boolean;
  postgresEndpointRedacted?: string;
  postgresConnected: boolean;
  pendingOutboxRows: number;
  lastSyncAt?: string;
  lastSyncDirection?: string;
  lastSyncError?: string;
  totalUnits: number;
  totalBallots: number;
  totalCandidates: number;
  totalEvents: number;
}

export interface SourcingEvaluation {
  entityRef: string;
  record: SourcingResourceRecord;
  facts: SourcingFacts;
  checks: BooleanCheckResult[];
  validation: SourcingValidationReport;
  lineage: DomainLineageEntry[];
  resolvedDomain: string;
  resolvedDomainTier: string;
  remediationPlaybook: string[];
  sourceOrigin?: string;
  ballotCount?: number;
  maxBallotSpread?: number;
  ballots?: BlindBallotRecord[];
}

export interface SourcingOverviewResponse {
  generatedAt: string;
  totalUnits: number;
  quadrantCounts: Record<string, number>;
  redlineCounts: Record<string, number>;
  items: SourcingEvaluation[];
  availableSystems: Array<{ ref: string; name: string; title: string; domain: string }>;
  availableComponents: Array<{ ref: string; name: string; title: string; system: string }>;
  workspaceYamlDir?: string;
  dbSync?: SourcingDbSyncStatus;
  discoveryCandidates?: DiscoveryCandidateRecord[];
  blindBallots?: BlindBallotRecord[];
  recentEvents?: SourcingAuditEvent[];
  mcpEndpoints?: Record<string, unknown>;
}

export interface SourcingWizardUpsertRequest {
  record: SourcingResourceRecord;
  writeWorkspaceYaml: boolean;
  dryRun: boolean;
  overrideReason?: string;
  actor?: string;
}

export interface SourcingWizardUpsertResponse {
  ok: boolean;
  dryRun: boolean;
  entityRef: string;
  persistenceMode: string;
  stateStorePath: string;
  workspaceYamlPath?: string;
  workspaceYamlWritten: boolean;
  generatedYaml: string;
  unifiedDiff: string;
  evaluation: SourcingEvaluation;
}

export interface TechInsightsCheckEntityResponse {
  entityRef: string;
  evaluatedAt: string;
  passedAll: boolean;
  redlineCount: number;
  facts: SourcingFacts;
  breakdown?: DimensionBreakdown;
  checks: BooleanCheckResult[];
  lineage: DomainLineageEntry[];
  remediationPlaybook: string[];
}

export const RESOURCE_TYPE_DIRECTORY_META: Record<string, ResourceTypeMeta> = {
  'baremetal-host': {
    type: 'baremetal-host',
    domain: 'Compute & Accelerators',
    shortTitle: 'Baremetal Hosts',
    fullTitle: 'Baremetal Hosts (baremetal-host)',
    description:
      'NixOS control-plane host (pits-core) and diskless Sidero Talos NVMe/TCP netboot nodes (netboot-001..004) with DMI, IOMMU, and COSI telemetry.',
  },
  'npu-accelerator': {
    type: 'npu-accelerator',
    domain: 'Compute & Accelerators',
    shortTitle: 'AI NPU Accelerators',
    fullTitle: 'AI NPU Accelerators (npu-accelerator)',
    description:
      'Physical Intel AI Boost (8086:7d1d) and AMD XDNA Ryzen AI (1022:1502) accelerators with Kubernetes DRA v1 ResourceSlice readiness and driver binding gates.',
  },
  'virtual-machine': {
    type: 'virtual-machine',
    domain: 'Compute & Accelerators',
    shortTitle: 'Virtual Machines',
    fullTitle: 'KubeVirt Virtual Machines (virtual-machine)',
    description:
      'KubeVirt guest workloads (win11-001 workstation and access-lab) with VMI phase, QEMU guest agent telemetry, and X.509 thumbprint-pinned RDP.',
  },
  'edge-router': {
    type: 'edge-router',
    domain: 'Network & Boot',
    shortTitle: 'Edge Routers',
    fullTitle: 'DPDK/VPP Edge Routers (edge-router)',
    description:
      'Declarative Crossplane EdgeRouter dataplane on pits-core enforcing mgmt0 (0000:58:00.0) protection, VLAN 10/20 isolation, and GoVPP packet counters.',
  },
  'pxe-boot-profile': {
    type: 'pxe-boot-profile',
    domain: 'Network & Boot',
    shortTitle: 'PXE Boot Profiles',
    fullTitle: 'PXE & Tinkerbell Boot Profiles (pxe-boot-profile)',
    description:
      'Deterministic iPXE netboot profiles (Talos v1.14.5 diskless netboot and Hook hardware inventory) with live SHA-256 kernel/initramfs verification.',
  },
  'storage-pool': {
    type: 'storage-pool',
    domain: 'Storage Fabric',
    shortTitle: 'Storage Pools',
    fullTitle: 'Btrfs Storage Pools (storage-pool)',
    description:
      'Backing Btrfs storage pool (pits-data on /var/mnt/tank) with subvolume layout, sysfs I/O checksum counters, FITRIM, and scrub verification.',
  },
  'san-volume': {
    type: 'san-volume',
    domain: 'Storage Fabric',
    shortTitle: 'SAN Volumes',
    fullTitle: 'NVMe/TCP SAN Volumes (san-volume)',
    description:
      'Deterministic UUIDv5 NVMe/TCP namespaces (system, userdata, vmstore) with 4KiB alignment, FS_NOCOW_FL (+C), configfs export, and GPT partition health.',
  },
  'kubernetes-cluster': {
    type: 'kubernetes-cluster',
    domain: 'Control Planes',
    shortTitle: 'Kubernetes Clusters',
    fullTitle: 'Kubernetes Control Planes (kubernetes-cluster)',
    description:
      'RKE2 management cluster (cluster-pits-core) and Sidero Talos workload cluster (cluster-pits-lab) via direct mTLS or pits-agent HTTP CONNECT tunnel.',
  },
  'vendor-saas': {
    type: 'vendor-saas',
    domain: 'Sourcing Governance',
    shortTitle: 'Vendor SaaS',
    fullTitle: 'Vendor SaaS Control Planes (vendor-saas)',
    description:
      'External SaaS coordination planes and telemetry subscriptions audited for supplier lock-in, FinOps TCO tier, and renewal SLA.',
  },
  'commercial-sdk': {
    type: 'commercial-sdk',
    domain: 'Sourcing Governance',
    shortTitle: 'Commercial SDKs',
    fullTitle: 'Commercial Closed-Source SDKs (commercial-sdk)',
    description:
      'Proprietary closed-source fraud models and mathematical solvers scored via 8-dimension Kraljic × Dutch Windmill redline rules.',
  },
  'managed-infra': {
    type: 'managed-infra',
    domain: 'Sourcing Governance',
    shortTitle: 'Managed Infra',
    fullTitle: 'Managed External Infrastructure (managed-infra)',
    description:
      'External carrier BGP transit links and managed hardware circuits traced upward through System & Business Domain topology.',
  },
  'outsourced-dev': {
    type: 'outsourced-dev',
    domain: 'Sourcing Governance',
    shortTitle: 'Outsourced Dev & Art',
    fullTitle: 'Outsourced Dev & Asset Units (outsourced-dev)',
    description:
      'Granular outsourced engineering and digital asset pipelines evaluated for substitution elasticity and supplier wallet attractiveness.',
  },
};
