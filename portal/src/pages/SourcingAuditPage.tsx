import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { Link as RouterLink, useLocation, useSearchParams } from 'react-router-dom';
import {
  AlertCircle,
  AlertTriangle,
  BarChart3,
  CheckCircle2,
  ClipboardCheck,
  Database,
  Eye,
  Info,
  Loader2,
  Lock,
  PlusCircle,
  Radar,
  RefreshCw,
  Save,
  ShieldCheck,
  Vote,
  X,
} from 'lucide-react';
import {
  discoverWorkspaceDependencies,
  disposeDiscoveryCandidate,
  fetchSourcingOverview,
  submitSourcingBlindBallot,
  syncSourcingDb,
  triggerCatalogRefresh,
  upsertSourcingAssessment,
  validateSourcingManifest,
} from '../api/pitsClient';
import { KraljicScatterMatrix, toMatrixPoint } from '../components/EntitySourcingScorecardCard';
import { StatusPill } from '../components/StatusPill';
import {
  SourcingEvaluation,
  SourcingOverviewResponse,
  SourcingResourceRecord,
  SourcingValidationReport,
  SourcingWizardUpsertResponse,
} from '../types';
import { PageBody, PageHeader, PageShell } from '../components/ui/page-shell';
import { Badge } from '../components/ui/badge';
import { Button } from '../components/ui/button';
import { Card, CardContent } from '../components/ui/card';
import { Input } from '../components/ui/input';
import { Textarea } from '../components/ui/textarea';
import { Alert, AlertDescription, AlertTitle } from '../components/ui/alert';
import { Separator } from '../components/ui/separator';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '../components/ui/table';
import {
  PitsCheckbox,
  PitsSelect,
  PitsSlider,
  PitsSwitch,
  PitsTabPanel,
  PitsTabs,
} from '../components/ui/pits-primitives';
import { cn } from '../lib/utils';

function blankSourcingRecord(defaultSystem = 'core-checkout'): SourcingResourceRecord {
  return {
    name: '',
    namespace: 'default',
    description: '',
    resourceType: 'commercial-sdk',
    owner: 'group:default/operators',
    system: defaultSystem,
    adapterAbstracted: true,
    costAllocationId: 'cc-new-001',
    dependsOnComponents: [],
    audit: {
      contractMeta: {
        vendorName: '',
        renewalDate: '2027-09-30',
        lastAuditedAt: new Date().toISOString().slice(0, 10),
        baselineProdDependents: 1,
        blindScoreVarianceAlert: false,
      },
      manualScores: {
        y2_coreCompetitiveness: 3.0,
        y4_priceElasticity: 3.0,
        x2_marketScarcity: 3.0,
        x3_customizationDepth: 3.0,
        x4_complianceFragility: 3.0,
      },
      supplierPreference: {
        estimatedWalletShareTier: '1-to-5pct',
        accountAttractivenessScore: 3.5,
        perceivedStatus: 'development',
      },
    },
  };
}

const SOURCING_TYPE_OPTIONS = [
  { value: 'commercial-sdk', label: 'commercial-sdk' },
  { value: 'vendor-saas', label: 'vendor-saas' },
  { value: 'managed-infra', label: 'managed-infra' },
  { value: 'outsourced-dev', label: 'outsourced-dev' },
];

const QUADRANT_FILTER_OPTIONS = [
  { value: 'all', label: 'All Quadrants' },
  { value: 'strategic', label: 'Strategic' },
  { value: 'leverage', label: 'Leverage' },
  { value: 'bottleneck', label: 'Bottleneck' },
  { value: 'non-critical', label: 'Non-Critical' },
];

const TYPE_FILTER_OPTIONS = [
  { value: 'all', label: 'All 4 Sourcing Types' },
  ...SOURCING_TYPE_OPTIONS,
];

const REVIEWER_ROLE_OPTIONS = [
  { value: 'architecture', label: 'architecture' },
  { value: 'procurement', label: 'procurement' },
  { value: 'security-compliance', label: 'security-compliance' },
  { value: 'domain-owner', label: 'domain-owner' },
];

const SUPPLIER_STATUS_OPTIONS = [
  { value: 'core', label: 'core (Strategic Partner)' },
  { value: 'development', label: 'development (Growth)' },
  { value: 'exploitable', label: 'exploitable (Captive)' },
  { value: 'nuisance', label: 'nuisance (Low Priority)' },
];

const WALLET_SHARE_OPTIONS = [
  { value: 'under-1pct', label: 'under-1pct' },
  { value: '1-to-5pct', label: '1-to-5pct' },
  { value: '5-to-15pct', label: '5-to-15pct' },
  { value: 'over-15pct', label: 'over-15pct' },
];

const Field: React.FC<{
  label: string;
  helper?: string;
  children: React.ReactNode;
}> = ({ label, helper, children }) => (
  <div className="space-y-1.5">
    <label className="block text-xs font-medium text-foreground">{label}</label>
    {children}
    {helper && <p className="m-0 text-[11px] text-muted-foreground">{helper}</p>}
  </div>
);

export const SourcingAuditPage: React.FC = () => {
  const [searchParams, setSearchParams] = useSearchParams();
  const location = useLocation();
  const entityParam = searchParams.get('entity');
  const initialWizard =
    location.pathname.includes('/wizard') ||
    Boolean(entityParam) ||
    searchParams.get('tab') === 'wizard';

  const [activeTab, setActiveTab] = useState<string>(initialWizard ? 'wizard' : 'matrix');
  const [overview, setOverview] = useState<SourcingOverviewResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [actionBanner, setActionBanner] = useState<{
    severity: 'success' | 'error' | 'info';
    text: string;
  } | null>(null);
  const [syncingDb, setSyncingDb] = useState(false);
  const [scanningWs, setScanningWs] = useState(false);

  // Portfolio matrix filters
  const [quadrantFilter, setQuadrantFilter] = useState<string>('all');
  const [typeFilter, setTypeFilter] = useState<string>('all');
  const [redlinesOnly, setRedlinesOnly] = useState<boolean>(false);

  // MCP Discovery Queue inline edits
  const [candidateTypeMap, setCandidateTypeMap] = useState<Record<string, string>>({});
  const [candidateCostMap, setCandidateCostMap] = useState<Record<string, string>>({});
  const [candidateNameMap, setCandidateNameMap] = useState<Record<string, string>>({});
  const [disposingId, setDisposingId] = useState<string | null>(null);

  // Blind Ballot form state
  const [ballotUnit, setBallotUnit] = useState<string>('payment-fraud-engine-sdk');
  const [ballotRole, setBallotRole] = useState<string>('architecture');
  const [ballotReviewerId, setBallotReviewerId] = useState<string>('operator@pits.local');
  const [ballotY2, setBallotY2] = useState<number>(4.0);
  const [ballotY4, setBallotY4] = useState<number>(2.5);
  const [ballotX2, setBallotX2] = useState<number>(4.0);
  const [ballotX3, setBallotX3] = useState<number>(3.5);
  const [ballotX4, setBallotX4] = useState<number>(4.0);
  const [ballotNotes, setBallotNotes] = useState<string>('');
  const [submittingBallot, setSubmittingBallot] = useState<boolean>(false);

  // Emergency Fallback Override Wizard state (locked by default)
  const [fallbackUnlocked, setFallbackUnlocked] = useState<boolean>(false);
  const [overrideReason, setOverrideReason] = useState<string>('');
  const [draft, setDraft] = useState<SourcingResourceRecord>(() => blankSourcingRecord());
  const [writeWorkspaceYaml, setWriteWorkspaceYaml] = useState<boolean>(false);
  const [validation, setValidation] = useState<SourcingValidationReport | null>(null);
  const [validating, setValidating] = useState<boolean>(false);
  const [submitting, setSubmitting] = useState<boolean>(false);
  const [wizardError, setWizardError] = useState<string | null>(null);
  const [wizardResult, setWizardResult] = useState<SourcingWizardUpsertResponse | null>(null);

  const loadOverview = useCallback(async (force = false) => {
    setLoading(true);
    setError(null);
    try {
      const data = await fetchSourcingOverview(force);
      setOverview(data);
      if (data.items.length > 0) {
        setBallotUnit(prev =>
          data.items.some(i => i.record.name === prev) ? prev : data.items[0].record.name,
        );
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadOverview(false);
  }, [loadOverview]);

  const loadEntityIntoDraft = useCallback((item: SourcingEvaluation) => {
    const recCopy: SourcingResourceRecord = JSON.parse(JSON.stringify(item.record));
    if (!recCopy.dependsOnComponents || recCopy.dependsOnComponents.length === 0) {
      recCopy.dependsOnComponents = item.lineage.map(l => l.componentRef);
    }
    setDraft(recCopy);
    setValidation(item.validation);
    setWizardResult(null);
    setWizardError(null);
    setActiveTab('wizard');
  }, []);

  useEffect(() => {
    if (entityParam && overview?.items) {
      const found = overview.items.find(
        i => i.record.name.toLowerCase() === entityParam.toLowerCase(),
      );
      if (found) {
        loadEntityIntoDraft(found);
      }
    }
  }, [entityParam, overview, loadEntityIntoDraft]);

  const filteredItems = useMemo(() => {
    if (!overview) return [];
    return overview.items.filter(ev => {
      if (quadrantFilter !== 'all' && ev.facts.quadrant !== quadrantFilter) return false;
      if (typeFilter !== 'all' && ev.record.resourceType !== typeFilter) return false;
      if (
        redlinesOnly &&
        !ev.facts.windmillMismatch &&
        !ev.facts.reAuditRequired &&
        !ev.facts.inCriticalZone
      ) {
        return false;
      }
      return true;
    });
  }, [overview, quadrantFilter, typeFilter, redlinesOnly]);

  const matrixPoints = useMemo(() => filteredItems.map(toMatrixPoint), [filteredItems]);

  const runProcessorValidation = async () => {
    setValidating(true);
    setWizardError(null);
    try {
      const rep = await validateSourcingManifest({ record: draft });
      setValidation(rep);
    } catch (e) {
      setWizardError(e instanceof Error ? e.message : String(e));
    } finally {
      setValidating(false);
    }
  };

  const handleSyncDb = async () => {
    setSyncingDb(true);
    setActionBanner(null);
    try {
      const status = await syncSourcingDb();
      setActionBanner({
        severity: status.lastSyncError ? 'error' : 'success',
        text: status.lastSyncError
          ? `PostgreSQL Sync Failed (SQLite WAL outbox active, ${status.pendingOutboxRows} pending): ${status.lastSyncError}`
          : `DB Sync complete (${status.backendMode}) · direction=${status.lastSyncDirection ?? 'local'} · pending=${status.pendingOutboxRows}`,
      });
      await loadOverview(false);
    } catch (e) {
      setActionBanner({
        severity: 'error',
        text: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setSyncingDb(false);
    }
  };

  const handleScanWorkspace = async () => {
    setScanningWs(true);
    setActionBanner(null);
    try {
      const res = await discoverWorkspaceDependencies({ actor: 'portal-discovery' });
      setActionBanner({
        severity: 'success',
        text: `MCP Workspace Discovery completed: ${res.totalCandidates} total candidates (${res.pendingCount} pending review).`,
      });
      await loadOverview(false);
      setActiveTab('discovery');
    } catch (e) {
      setActionBanner({
        severity: 'error',
        text: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setScanningWs(false);
    }
  };

  const handleDisposeCandidate = async (
    candidateId: string,
    pkg: string,
    suggestedType: string,
    action: 'promote' | 'ignore',
  ) => {
    setDisposingId(candidateId);
    setActionBanner(null);
    try {
      const chosenType = candidateTypeMap[candidateId] || suggestedType;
      const chosenCost = candidateCostMap[candidateId] || undefined;
      const chosenName = candidateNameMap[candidateId] || undefined;
      const res = await disposeDiscoveryCandidate({
        candidateId,
        action,
        name: chosenName,
        resourceType: action === 'promote' ? chosenType : undefined,
        costAllocationId: action === 'promote' ? chosenCost : undefined,
        actor: 'portal-discovery',
      });
      setActionBanner({
        severity: 'success',
        text:
          action === 'promote'
            ? `Promoted '${pkg}' -> resource:${res.evaluation?.record.name ?? res.candidate.promotedUnitName ?? 'unit'} (source=mcp-discovery).`
            : `Marked '${pkg}' as ignored.`,
      });
      await triggerCatalogRefresh();
      await loadOverview(false);
    } catch (e) {
      setActionBanner({
        severity: 'error',
        text: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setDisposingId(null);
    }
  };

  const handleSubmitBlindBallot = async (e: React.FormEvent) => {
    e.preventDefault();
    setSubmittingBallot(true);
    setActionBanner(null);
    try {
      const res = await submitSourcingBlindBallot({
        unitName: ballotUnit,
        reviewerRole: ballotRole,
        reviewerId: ballotReviewerId.trim() || 'operator@pits.local',
        y2_coreCompetitiveness: ballotY2,
        y4_priceElasticity: ballotY4,
        x2_marketScarcity: ballotX2,
        x3_customizationDepth: ballotX3,
        x4_complianceFragility: ballotX4,
        evidenceNotes: ballotNotes,
      });
      setActionBanner({
        severity: res.varianceAlert ? 'error' : 'success',
        text: res.varianceAlert
          ? `Blind ballot recorded for '${res.unitName}', triggering Variance Alert (maxSpread=${res.maxSpread.toFixed(2)} across ${res.varianceDimensions.join(', ')}).`
          : `Blind ballot recorded for '${res.unitName}' (${ballotRole}); maxSpread=${res.maxSpread.toFixed(2)}.`,
      });
      setBallotNotes('');
      await triggerCatalogRefresh();
      await loadOverview(false);
    } catch (err) {
      setActionBanner({
        severity: 'error',
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setSubmittingBallot(false);
    }
  };

  const handleSubmitWizard = async (dryRun: boolean) => {
    if (!dryRun && !fallbackUnlocked) {
      setWizardError('Unlock Emergency Fallback Override first before committing manual writes.');
      return;
    }
    if (!dryRun && !overrideReason.trim()) {
      setWizardError(
        'Emergency fallback overrideReason is mandatory when saving outside MCP Discovery / Blind Ballots.',
      );
      return;
    }
    setSubmitting(true);
    setWizardError(null);
    try {
      const rep = await validateSourcingManifest({ record: draft });
      setValidation(rep);
      if (!rep.valid) {
        setWizardError(`CatalogProcessor blocked submission: ${rep.errors.join(' | ')}`);
        setSubmitting(false);
        return;
      }
      const res = await upsertSourcingAssessment({
        record: draft,
        writeWorkspaceYaml,
        dryRun,
        overrideReason: overrideReason.trim() || 'dry-run-preview',
        actor: 'portal-operator',
      });
      setWizardResult(res);
      if (!dryRun) {
        await triggerCatalogRefresh();
        await loadOverview(false);
      }
    } catch (e) {
      setWizardError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  };

  const toggleComponentDep = (compRef: string) => {
    setDraft(prev => {
      const exists = prev.dependsOnComponents.includes(compRef);
      return {
        ...prev,
        dependsOnComponents: exists
          ? prev.dependsOnComponents.filter(c => c !== compRef)
          : [...prev.dependsOnComponents, compRef],
      };
    });
  };

  const updateManualScore = (
    key: keyof SourcingResourceRecord['audit']['manualScores'],
    val: number,
  ) => {
    setDraft(prev => ({
      ...prev,
      audit: {
        ...prev.audit,
        manualScores: {
          ...prev.audit.manualScores,
          [key]: val,
        },
      },
    }));
  };

  const pendingDiscoveryCount = (overview?.discoveryCandidates ?? []).filter(
    c => c.status === 'pending',
  ).length;

  const availableSystems = overview?.availableSystems ?? [
    { name: 'core-checkout', title: 'Core Checkout', domain: 'core-commerce' },
    { name: 'pits-infrastructure', title: 'PITS Infrastructure', domain: 'platform-infra' },
    { name: 'pits-city-pipeline', title: 'PITS City Pipeline', domain: 'spatial-simulation' },
  ];

  return (
    <PageShell>
      <PageHeader
        title="DB-First & MCP Sourcing Governance"
        badge={<Badge variant="outline">SQLite WAL + Postgres v3 + MCP Server</Badge>}
        description={
          <span>
            Automated workspace discovery &amp; blind multi-role ballots via{' '}
            <code className="rounded bg-muted px-1 py-0.5 font-mono text-[11px]">pitsctl mcp</code>{' '}
            /{' '}
            <code className="rounded bg-muted px-1 py-0.5 font-mono text-[11px]">POST /mcp</code>,
            persisted in ACID SQLite WAL with bi-directional PostgreSQL outbox sync.
          </span>
        }
        actions={
          <>
            <Button
              variant="outline"
              size="sm"
              onClick={() => void loadOverview(true)}
              disabled={loading}
            >
              {loading ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <RefreshCw className="h-3.5 w-3.5" />
              )}
              <span>Refresh Facts</span>
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => void handleSyncDb()}
              disabled={syncingDb}
            >
              {syncingDb ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Database className="h-3.5 w-3.5" />
              )}
              <span>{syncingDb ? 'Syncing DB...' : 'Sync PostgreSQL Now'}</span>
            </Button>
            <Button
              size="sm"
              onClick={() => void handleScanWorkspace()}
              disabled={scanningWs}
            >
              {scanningWs ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Radar className="h-3.5 w-3.5" />
              )}
              <span>{scanningWs ? 'Scanning...' : 'Scan Workspace (MCP)'}</span>
            </Button>
            <Button
              variant="warning"
              size="sm"
              onClick={() => {
                setDraft(blankSourcingRecord());
                setValidation(null);
                setWizardResult(null);
                setWizardError(null);
                setSearchParams({ tab: 'wizard' });
                setActiveTab('wizard');
              }}
            >
              <PlusCircle className="h-3.5 w-3.5" />
              <span>Fallback Override</span>
            </Button>
          </>
        }
      />

      <PageBody className="space-y-5">
        {/* DB Sync & Portfolio Summary Strip */}
        {(overview?.dbSync || overview) && (
          <Card>
            <CardContent className="space-y-3.5 p-4">
              {overview?.dbSync && (
                <div className="space-y-2 rounded-lg border border-solid border-border bg-muted/40 p-3">
                  <div className="flex flex-col justify-between gap-2 md:flex-row md:items-center">
                    <div className="flex flex-wrap items-center gap-1.5">
                      <Database className="h-3.5 w-3.5 text-muted-foreground" />
                      <Badge
                        variant={
                          overview.dbSync.backendMode === 'postgresql+sqlite-wal'
                            ? 'ok'
                            : overview.dbSync.backendMode === 'sqlite-wal-outbox-degraded'
                              ? 'warn'
                              : 'info'
                        }
                      >
                        DB Mode: {overview.dbSync.backendMode}
                      </Badge>
                      <Badge variant="outline" className="font-mono">
                        SQLite {overview.dbSync.sqliteVersion} (
                        {overview.dbSync.sqliteJournalMode.toUpperCase()})
                      </Badge>
                      <Badge
                        variant={overview.dbSync.pendingOutboxRows > 0 ? 'warn' : 'muted'}
                      >
                        Outbox Pending: {overview.dbSync.pendingOutboxRows}
                      </Badge>
                      {overview.dbSync.postgresConfigured && (
                        <Badge variant="outline" className="font-mono">
                          Postgres: {overview.dbSync.postgresEndpointRedacted ?? 'configured'}
                        </Badge>
                      )}
                    </div>
                    <div className="flex flex-wrap items-center gap-1.5">
                      <Badge variant="purple" className="font-mono">
                        MCP Stdio: pitsctl mcp
                      </Badge>
                      <Badge variant="purple" className="font-mono">
                        Streamable HTTP: /mcp
                      </Badge>
                    </div>
                  </div>

                  <div className="font-mono text-[11px] text-muted-foreground">
                    SQLite SSOT: {overview.dbSync.sqlitePath} | Units: {overview.dbSync.totalUnits}{' '}
                    | Ballots: {overview.dbSync.totalBallots} | Discovery Candidates:{' '}
                    {overview.dbSync.totalCandidates} | Events: {overview.dbSync.totalEvents}
                    {overview.dbSync.lastSyncAt
                      ? ` | Last Sync: ${overview.dbSync.lastSyncAt} (${overview.dbSync.lastSyncDirection ?? ''})`
                      : ''}
                    {overview.dbSync.lastSyncError
                      ? ` | Error: ${overview.dbSync.lastSyncError}`
                      : ''}
                  </div>
                </div>
              )}

              {overview && (
                <div className="flex flex-wrap items-center gap-2">
                  <Badge variant="default">Units: {overview.totalUnits}</Badge>
                  <Badge variant="error">
                    Strategic: {overview.quadrantCounts.strategic ?? 0}
                  </Badge>
                  <Badge variant="info">
                    Leverage: {overview.quadrantCounts.leverage ?? 0}
                  </Badge>
                  <Badge variant="warn">
                    Bottleneck: {overview.quadrantCounts.bottleneck ?? 0}
                  </Badge>
                  <Badge variant="ok">
                    Non-Critical: {overview.quadrantCounts['non-critical'] ?? 0}
                  </Badge>
                  <Badge variant="error" className="font-semibold">
                    Windmill Mismatch: {overview.redlineCounts.windmillMismatch ?? 0}
                  </Badge>
                  <Badge variant="warn" className="font-semibold">
                    Re-Audit Due: {overview.redlineCounts.reAuditRequired ?? 0}
                  </Badge>
                  <Badge variant="muted">
                    Critical Zone [2.7–3.3]: {overview.redlineCounts.inCriticalZone ?? 0}
                  </Badge>
                </div>
              )}
            </CardContent>
          </Card>
        )}

        {error && (
          <Alert variant="destructive">
            <AlertCircle />
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        {actionBanner && (
          <Alert
            variant={
              actionBanner.severity === 'error'
                ? 'destructive'
                : actionBanner.severity === 'success'
                  ? 'success'
                  : 'info'
            }
          >
            {actionBanner.severity === 'error' ? (
              <AlertCircle />
            ) : actionBanner.severity === 'success' ? (
              <CheckCircle2 />
            ) : (
              <Info />
            )}
            <AlertDescription className="flex-1">{actionBanner.text}</AlertDescription>
            <button
              type="button"
              onClick={() => setActionBanner(null)}
              aria-label="Dismiss"
              className="inline-flex h-5 w-5 cursor-pointer items-center justify-center rounded border-0 bg-transparent text-current opacity-70 hover:opacity-100"
            >
              <X className="h-3.5 w-3.5" />
            </button>
          </Alert>
        )}

        {/* Main 4-Tab Governance Workspace */}
        <Card className="overflow-hidden">
          <PitsTabs
            value={activeTab}
            onValueChange={setActiveTab}
            items={[
              {
                value: 'matrix',
                label: 'Portfolio Audit Matrix & Redlines',
                icon: <BarChart3 className="h-3.5 w-3.5" />,
              },
              {
                value: 'discovery',
                label: `MCP Discovery Queue (${pendingDiscoveryCount})`,
                icon: <Radar className="h-3.5 w-3.5" />,
              },
              {
                value: 'ballots',
                label: `Blind Ballots & Audit Ledger (${overview?.blindBallots?.length ?? 0})`,
                icon: <Vote className="h-3.5 w-3.5" />,
              },
              {
                value: 'wizard',
                label: 'Emergency Fallback Override (Wizard)',
                icon: <Lock className="h-3.5 w-3.5" />,
              },
            ]}
          >
            {/* TAB 0: MATRIX & REDLINES */}
            <PitsTabPanel value="matrix">
              <div className="grid grid-cols-1 gap-5 lg:grid-cols-12">
                <div className="lg:col-span-5">
                  <Card>
                    <CardContent className="p-4">
                      <div className="text-sm font-semibold text-foreground">
                        2D Kraljic × Windmill Scatter Plot
                      </div>
                      <p className="mb-3 mt-1 text-xs text-muted-foreground">
                        Click any sourcing node to inspect its Backstage Entity scorecard. Purple
                        dashed arrows show 5-point redline knockout escalation.
                      </p>
                      <KraljicScatterMatrix points={matrixPoints} height={330} />
                    </CardContent>
                  </Card>
                </div>

                <div className="space-y-4 lg:col-span-7">
                  <div className="flex flex-wrap items-center gap-3">
                    <div className="w-44">
                      <PitsSelect
                        size="sm"
                        value={quadrantFilter}
                        onValueChange={setQuadrantFilter}
                        options={QUADRANT_FILTER_OPTIONS}
                      />
                    </div>
                    <div className="w-48">
                      <PitsSelect
                        size="sm"
                        value={typeFilter}
                        onValueChange={setTypeFilter}
                        options={TYPE_FILTER_OPTIONS}
                      />
                    </div>
                    <label className="flex cursor-pointer items-center gap-2 text-xs text-foreground">
                      <PitsSwitch checked={redlinesOnly} onCheckedChange={setRedlinesOnly} />
                      <span>Redlines / Critical Only</span>
                    </label>
                  </div>

                  <div className="overflow-hidden rounded-lg border border-solid border-border">
                    <Table>
                      <TableHeader>
                        <TableRow>
                          <TableHead>Sourcing Unit</TableHead>
                          <TableHead>System → Domain</TableHead>
                          <TableHead>Y / X Scores</TableHead>
                          <TableHead>Source / Ballots</TableHead>
                          <TableHead>Quadrant</TableHead>
                          <TableHead>Windmill Status</TableHead>
                          <TableHead>Governance</TableHead>
                          <TableHead className="text-right">Override</TableHead>
                        </TableRow>
                      </TableHeader>
                      <TableBody>
                        {filteredItems.map(ev => {
                          const redlines = ev.checks.filter(c => c.triggered).length;
                          return (
                            <TableRow key={ev.record.name}>
                              <TableCell>
                                <RouterLink
                                  to={`/catalog/default/resource/${encodeURIComponent(ev.record.name)}`}
                                  className="text-xs font-semibold text-foreground hover:underline"
                                >
                                  {ev.record.name}
                                </RouterLink>
                                <div className="text-[11px] text-muted-foreground">
                                  {ev.record.resourceType} · {ev.record.audit.contractMeta.vendorName}
                                </div>
                              </TableCell>
                              <TableCell>
                                <div className="font-mono text-[11px] text-foreground">
                                  {ev.record.system}
                                </div>
                                <div className="text-[11px] text-muted-foreground">
                                  {ev.resolvedDomain.replace('domain:default/', '')} (
                                  {ev.facts.prodDependents} deps)
                                </div>
                              </TableCell>
                              <TableCell className="whitespace-nowrap">
                                <div className="font-mono text-xs font-semibold">
                                  Y={ev.facts.finalYScore.toFixed(2)} / X=
                                  {ev.facts.finalXScore.toFixed(2)}
                                </div>
                                {ev.facts.knockoutTriggered && (
                                  <Badge variant="purple" className="mt-1">
                                    5-pt Knockout
                                  </Badge>
                                )}
                              </TableCell>
                              <TableCell>
                                <Badge
                                  variant={
                                    ev.sourceOrigin === 'wizard-fallback' ? 'warn' : 'muted'
                                  }
                                >
                                  {ev.sourceOrigin ?? 'seed-baseline'}
                                </Badge>
                                <div className="mt-0.5 text-[11px] text-muted-foreground">
                                  {ev.ballotCount ?? 0} ballots · spread{' '}
                                  {(ev.maxBallotSpread ?? 0).toFixed(2)}
                                </div>
                                {ev.record.audit.contractMeta.blindScoreVarianceAlert && (
                                  <Badge variant="error" className="mt-1">
                                    Variance &gt;= 1.5
                                  </Badge>
                                )}
                              </TableCell>
                              <TableCell>
                                <Badge variant="outline">{ev.facts.quadrant}</Badge>
                              </TableCell>
                              <TableCell>
                                <StatusPill
                                  label={
                                    ev.facts.windmillMismatch
                                      ? `${ev.record.audit.supplierPreference.perceivedStatus} (MISMATCH)`
                                      : ev.record.audit.supplierPreference.perceivedStatus
                                  }
                                  level={ev.facts.windmillMismatch ? 'error' : 'ok'}
                                />
                              </TableCell>
                              <TableCell>
                                <StatusPill
                                  label={
                                    redlines === 0
                                      ? 'Healthy'
                                      : `${redlines} Redline${redlines > 1 ? 's' : ''}`
                                  }
                                  level={
                                    ev.facts.windmillMismatch
                                      ? 'error'
                                      : redlines > 0
                                        ? 'warn'
                                        : 'ok'
                                  }
                                />
                              </TableCell>
                              <TableCell className="text-right">
                                <Button
                                  size="xs"
                                  variant="warning"
                                  onClick={() => loadEntityIntoDraft(ev)}
                                >
                                  Override
                                </Button>
                              </TableCell>
                            </TableRow>
                          );
                        })}
                      </TableBody>
                    </Table>
                  </div>
                </div>
              </div>
            </PitsTabPanel>

            {/* TAB 1: MCP DISCOVERY QUEUE */}
            <PitsTabPanel value="discovery" className="space-y-4">
              <div className="flex flex-col justify-between gap-3 sm:flex-row sm:items-center">
                <div className="space-y-1">
                  <div className="text-sm font-semibold text-foreground">
                    MCP Automated Workspace Discovery Queue
                  </div>
                  <p className="m-0 text-xs text-muted-foreground">
                    Scanned from{' '}
                    <code className="rounded bg-muted px-1 py-0.5 font-mono text-[11px]">
                      Cargo.toml
                    </code>
                    ,{' '}
                    <code className="rounded bg-muted px-1 py-0.5 font-mono text-[11px]">
                      package.json
                    </code>
                    ,{' '}
                    <code className="rounded bg-muted px-1 py-0.5 font-mono text-[11px]">
                      go.mod
                    </code>
                    , Nix flakes &amp; K8s manifests via{' '}
                    <code className="rounded bg-muted px-1 py-0.5 font-mono text-[11px]">
                      sourcing_scan_workspace
                    </code>
                    . Promote verified granular units into{' '}
                    <code className="rounded bg-muted px-1 py-0.5 font-mono text-[11px]">
                      sourcing_units
                    </code>{' '}
                    (FinOps secret &amp; anti-monolith rules enforced automatically).
                  </p>
                </div>
                <Button
                  size="sm"
                  onClick={() => void handleScanWorkspace()}
                  disabled={scanningWs}
                  className="shrink-0"
                >
                  {scanningWs ? (
                    <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <Radar className="h-3.5 w-3.5" />
                  )}
                  <span>{scanningWs ? 'Scanning...' : 'Run Discovery Scan'}</span>
                </Button>
              </div>

              <div className="overflow-hidden rounded-lg border border-solid border-border">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Dependency / Package</TableHead>
                      <TableHead>Ecosystem &amp; Source</TableHead>
                      <TableHead>Adapter &amp; Granularity</TableHead>
                      <TableHead>Promotion Parameters</TableHead>
                      <TableHead>Status</TableHead>
                      <TableHead className="text-right">Disposition</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {(overview?.discoveryCandidates ?? []).length === 0 ? (
                      <TableRow>
                        <TableCell colSpan={6} className="py-8 text-center text-xs text-muted-foreground">
                          No candidates scanned yet. Click <strong>Run Discovery Scan</strong> or
                          invoke MCP tool{' '}
                          <code className="rounded bg-muted px-1 py-0.5 font-mono">
                            sourcing_scan_workspace
                          </code>
                          .
                        </TableCell>
                      </TableRow>
                    ) : (
                      (overview?.discoveryCandidates ?? []).map(cand => {
                        const chosenType =
                          candidateTypeMap[cand.candidateId] ?? cand.suggestedType;
                        const chosenCost = candidateCostMap[cand.candidateId] ?? '';
                        const chosenName = candidateNameMap[cand.candidateId] ?? '';
                        const busy = disposingId === cand.candidateId;
                        return (
                          <TableRow key={cand.candidateId}>
                            <TableCell>
                              <div className="text-xs font-semibold text-foreground">
                                {cand.packageOrEndpoint}
                              </div>
                              <div className="font-mono text-[11px] text-muted-foreground">
                                {cand.candidateId}
                              </div>
                            </TableCell>
                            <TableCell>
                              <Badge variant="outline">{cand.ecosystem}</Badge>
                              <div className="mt-1 text-[11px] text-muted-foreground">
                                {cand.detectedInPath} · sys: {cand.inferredSystem}
                              </div>
                            </TableCell>
                            <TableCell>
                              <Badge variant={cand.adapterDetected ? 'ok' : 'warn'}>
                                {cand.adapterDetected ? 'Adapter Detected' : 'No Adapter'}
                              </Badge>
                              <div className="mt-1 text-[11px] text-muted-foreground">
                                {cand.monolithRiskNote}
                              </div>
                            </TableCell>
                            <TableCell>
                              {cand.status === 'pending' ? (
                                <div className="flex flex-wrap items-center gap-1.5">
                                  <div className="w-36">
                                    <PitsSelect
                                      size="sm"
                                      value={chosenType}
                                      onValueChange={val =>
                                        setCandidateTypeMap(m => ({
                                          ...m,
                                          [cand.candidateId]: val,
                                        }))
                                      }
                                      options={SOURCING_TYPE_OPTIONS}
                                    />
                                  </div>
                                  <Input
                                    placeholder="Unit name (optional)"
                                    value={chosenName}
                                    onChange={e =>
                                      setCandidateNameMap(m => ({
                                        ...m,
                                        [cand.candidateId]: e.target.value,
                                      }))
                                    }
                                    className="h-8 w-36 text-xs"
                                  />
                                  <Input
                                    placeholder="FinOps CC ID (cc-mcp-...)"
                                    value={chosenCost}
                                    onChange={e =>
                                      setCandidateCostMap(m => ({
                                        ...m,
                                        [cand.candidateId]: e.target.value,
                                      }))
                                    }
                                    className="h-8 w-40 text-xs"
                                  />
                                </div>
                              ) : (
                                <span className="text-xs text-muted-foreground">
                                  {cand.promotedUnitName ? `Unit: ${cand.promotedUnitName}` : '—'}
                                </span>
                              )}
                            </TableCell>
                            <TableCell>
                              <Badge
                                variant={
                                  cand.status === 'promoted'
                                    ? 'ok'
                                    : cand.status === 'ignored'
                                      ? 'muted'
                                      : 'info'
                                }
                              >
                                {cand.status}
                              </Badge>
                            </TableCell>
                            <TableCell className="text-right">
                              {cand.status === 'pending' && (
                                <div className="flex justify-end gap-1.5">
                                  <Button
                                    size="xs"
                                    disabled={busy}
                                    onClick={() =>
                                      void handleDisposeCandidate(
                                        cand.candidateId,
                                        cand.packageOrEndpoint,
                                        cand.suggestedType,
                                        'promote',
                                      )
                                    }
                                  >
                                    Promote
                                  </Button>
                                  <Button
                                    size="xs"
                                    variant="outline"
                                    disabled={busy}
                                    onClick={() =>
                                      void handleDisposeCandidate(
                                        cand.candidateId,
                                        cand.packageOrEndpoint,
                                        cand.suggestedType,
                                        'ignore',
                                      )
                                    }
                                  >
                                    Ignore
                                  </Button>
                                </div>
                              )}
                            </TableCell>
                          </TableRow>
                        );
                      })
                    )}
                  </TableBody>
                </Table>
              </div>
            </PitsTabPanel>

            {/* TAB 2: BLIND BALLOTS & AUDIT LEDGER */}
            <PitsTabPanel value="ballots">
              <div className="grid grid-cols-1 gap-5 lg:grid-cols-12">
                <div className="lg:col-span-5">
                  <Card>
                    <form onSubmit={handleSubmitBlindBallot}>
                      <CardContent className="space-y-4 p-5">
                        <div>
                          <div className="text-sm font-semibold text-foreground">
                            Submit Independent Blind Role Ballot
                          </div>
                          <p className="mb-0 mt-1 text-xs leading-relaxed text-muted-foreground">
                            Each role (<code className="font-mono">architecture</code>,{' '}
                            <code className="font-mono">procurement</code>,{' '}
                            <code className="font-mono">security-compliance</code>,{' '}
                            <code className="font-mono">domain-owner</code>) submits scores
                            independently into SQLite WAL. Spread &gt;= 1.5 across reviewers
                            automatically sets{' '}
                            <code className="font-mono">blindScoreVarianceAlert = true</code>.
                          </p>
                        </div>

                        <Field label="Target Sourcing Unit">
                          <PitsSelect
                            value={ballotUnit}
                            onValueChange={setBallotUnit}
                            options={(overview?.items ?? []).map(ev => ({
                              value: ev.record.name,
                              label: `${ev.record.name} (${ev.record.resourceType})`,
                            }))}
                          />
                        </Field>

                        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                          <Field label="Reviewer Role">
                            <PitsSelect
                              value={ballotRole}
                              onValueChange={setBallotRole}
                              options={REVIEWER_ROLE_OPTIONS}
                            />
                          </Field>
                          <Field label="Reviewer ID / Email">
                            <Input
                              value={ballotReviewerId}
                              onChange={e => setBallotReviewerId(e.target.value)}
                            />
                          </Field>
                        </div>

                        <div className="space-y-3 pt-1">
                          {(
                            [
                              ['Y2 Core Competitiveness', ballotY2, setBallotY2],
                              ['Y4 Price Elasticity', ballotY4, setBallotY4],
                              ['X2 Market Scarcity', ballotX2, setBallotX2],
                              ['X3 Customization Depth', ballotX3, setBallotX3],
                              ['X4 Compliance Fragility', ballotX4, setBallotX4],
                            ] as const
                          ).map(([lbl, v, setter]) => (
                            <div key={lbl} className="space-y-1">
                              <div className="flex items-center justify-between text-xs">
                                <span className="font-medium text-foreground">{lbl}</span>
                                <span className="font-mono font-semibold text-foreground">
                                  {v.toFixed(1)}
                                </span>
                              </div>
                              <PitsSlider
                                min={1}
                                max={5}
                                step={0.5}
                                value={v}
                                onValueChange={setter}
                              />
                            </div>
                          ))}
                        </div>

                        <Field label="Evidence / Justification Notes">
                          <Textarea
                            rows={2}
                            value={ballotNotes}
                            onChange={e => setBallotNotes(e.target.value)}
                          />
                        </Field>

                        <Button type="submit" className="w-full" disabled={submittingBallot}>
                          {submittingBallot ? (
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                          ) : (
                            <Vote className="h-3.5 w-3.5" />
                          )}
                          <span>
                            {submittingBallot ? 'Recording Ballot...' : 'Submit Blind Role Ballot'}
                          </span>
                        </Button>
                      </CardContent>
                    </form>
                  </Card>
                </div>

                <div className="space-y-4 lg:col-span-7">
                  <Card className="overflow-hidden">
                    <div className="border-b border-solid border-border bg-muted/40 px-4 py-2.5 text-xs font-semibold text-foreground">
                      Blind Role Ballots ({overview?.blindBallots?.length ?? 0})
                    </div>
                    <div className="pits-scroll max-h-72 overflow-auto">
                      <Table>
                        <TableHeader>
                          <TableRow>
                            <TableHead>Unit</TableHead>
                            <TableHead>Role / Reviewer</TableHead>
                            <TableHead>Y2/Y4/X2/X3/X4</TableHead>
                            <TableHead>Notes</TableHead>
                          </TableRow>
                        </TableHeader>
                        <TableBody>
                          {(overview?.blindBallots ?? []).map(b => (
                            <TableRow key={b.ballotId}>
                              <TableCell className="font-semibold">{b.unitName}</TableCell>
                              <TableCell>
                                <Badge variant="outline">{b.reviewerRole}</Badge>
                                <div className="mt-0.5 text-[11px] text-muted-foreground">
                                  {b.reviewerId}
                                </div>
                              </TableCell>
                              <TableCell className="whitespace-nowrap font-mono text-xs">
                                {b.y2_coreCompetitiveness.toFixed(1)} /{' '}
                                {b.y4_priceElasticity.toFixed(1)} / {b.x2_marketScarcity.toFixed(1)}{' '}
                                / {b.x3_customizationDepth.toFixed(1)} /{' '}
                                {b.x4_complianceFragility.toFixed(1)}
                              </TableCell>
                              <TableCell className="text-xs text-muted-foreground">
                                {b.evidenceNotes || '—'}
                              </TableCell>
                            </TableRow>
                          ))}
                        </TableBody>
                      </Table>
                    </div>
                  </Card>

                  <Card className="overflow-hidden">
                    <div className="border-b border-solid border-border bg-muted/40 px-4 py-2.5 text-xs font-semibold text-foreground">
                      Immutable DB Audit Event Ledger ({overview?.recentEvents?.length ?? 0})
                    </div>
                    <div className="pits-scroll max-h-64 overflow-auto">
                      <Table>
                        <TableHeader>
                          <TableRow>
                            <TableHead>Type / Channel</TableHead>
                            <TableHead>Unit &amp; Actor</TableHead>
                            <TableHead>Summary</TableHead>
                          </TableRow>
                        </TableHeader>
                        <TableBody>
                          {(overview?.recentEvents ?? []).map(ev => (
                            <TableRow key={ev.eventId}>
                              <TableCell>
                                <Badge variant="outline">{ev.eventType}</Badge>
                                <div className="mt-0.5 text-[11px] text-muted-foreground">
                                  via {ev.channel}
                                </div>
                              </TableCell>
                              <TableCell>
                                <div className="text-xs font-semibold text-foreground">
                                  {ev.unitName}
                                </div>
                                <div className="text-[11px] text-muted-foreground">
                                  {ev.actor}
                                </div>
                              </TableCell>
                              <TableCell>
                                <div className="text-xs text-foreground">{ev.summary}</div>
                                <div className="mt-0.5 font-mono text-[11px] text-muted-foreground">
                                  {ev.createdAt}
                                </div>
                              </TableCell>
                            </TableRow>
                          ))}
                        </TableBody>
                      </Table>
                    </div>
                  </Card>
                </div>
              </div>
            </PitsTabPanel>

            {/* TAB 3: EMERGENCY FALLBACK OVERRIDE WIZARD */}
            <PitsTabPanel value="wizard" className="space-y-5">
              <Alert variant="warning" className="items-center justify-between gap-4">
                <div className="flex items-start gap-2.5">
                  <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
                  <div>
                    <AlertTitle>
                      Emergency Fallback Override Only — Primary Intake Uses MCP &amp; DB Sync
                    </AlertTitle>
                    <AlertDescription>
                      Standard onboarding uses{' '}
                      <code className="font-mono">pitsctl mcp</code> /{' '}
                      <code className="font-mono">POST /mcp</code> (
                      <code className="font-mono">sourcing_scan_workspace</code> →{' '}
                      <code className="font-mono">sourcing_promote_candidate</code> →{' '}
                      <code className="font-mono">sourcing_submit_blind_ballot</code>). Unlocking
                      and committing here stamps{' '}
                      <code className="font-mono">source_origin = &quot;wizard-fallback&quot;</code>{' '}
                      and logs your mandatory override reason in the SQLite WAL + PostgreSQL audit
                      ledger.
                    </AlertDescription>
                  </div>
                </div>
                <label className="flex shrink-0 cursor-pointer items-center gap-2 whitespace-nowrap text-xs font-semibold">
                  <PitsSwitch
                    tone="warning"
                    checked={fallbackUnlocked}
                    onCheckedChange={setFallbackUnlocked}
                  />
                  <span>{fallbackUnlocked ? 'Override UNLOCKED' : 'Locked (Enable Override)'}</span>
                </label>
              </Alert>

              <div className="grid grid-cols-1 gap-5 lg:grid-cols-12">
                <div className="space-y-4 lg:col-span-7">
                  {overview && overview.items.length > 0 && (
                    <div className="flex flex-wrap items-center gap-2 rounded-lg border border-solid border-border bg-muted/30 p-3.5">
                      <span className="text-xs font-semibold text-foreground">
                        Load Existing Sourcing Unit:
                      </span>
                      {overview.items.map(ev => {
                        const active = draft.name === ev.record.name;
                        return (
                          <button
                            key={ev.record.name}
                            type="button"
                            onClick={() => loadEntityIntoDraft(ev)}
                            className={cn(
                              'cursor-pointer rounded-md border border-solid px-2.5 py-1 font-mono text-[11px] font-medium transition-colors',
                              active
                                ? 'border-primary bg-primary text-primary-foreground'
                                : 'border-border bg-background text-foreground hover:bg-accent',
                            )}
                          >
                            {ev.record.name}
                          </button>
                        );
                      })}
                    </div>
                  )}

                  {/* Section 1 */}
                  <Card>
                    <CardContent className="space-y-4 p-5">
                      <div className="text-sm font-semibold text-foreground">
                        1 · 4-Layer Catalog Unpacking &amp; FinOps Cost Attribution
                        (CatalogProcessor)
                      </div>

                      <div className="grid grid-cols-1 gap-3.5 sm:grid-cols-2">
                        <Field
                          label="Entity Name (DNS-safe sourcing unit)"
                          helper="Must avoid vendor-level catch-alls like '-all' or bare vendor name"
                        >
                          <Input
                            placeholder="e.g. fraud-rule-engine-sdk"
                            value={draft.name}
                            onChange={e => setDraft({ ...draft, name: e.target.value })}
                          />
                        </Field>

                        <Field label="Granular Resource Type (spec.type)">
                          <PitsSelect
                            value={draft.resourceType}
                            onValueChange={val => setDraft({ ...draft, resourceType: val })}
                            options={SOURCING_TYPE_OPTIONS}
                          />
                        </Field>
                      </div>

                      <Field label="Description">
                        <Input
                          value={draft.description}
                          onChange={e => setDraft({ ...draft, description: e.target.value })}
                        />
                      </Field>

                      <div className="grid grid-cols-1 gap-3.5 sm:grid-cols-3">
                        <Field label="Parent System (spec.system)">
                          <PitsSelect
                            value={draft.system}
                            onValueChange={val => setDraft({ ...draft, system: val })}
                            options={availableSystems.map(sys => ({
                              value: sys.name,
                              label: `${sys.name} (${sys.domain})`,
                            }))}
                          />
                        </Field>
                        <Field label="Owner (spec.owner)">
                          <Input
                            value={draft.owner}
                            onChange={e => setDraft({ ...draft, owner: e.target.value })}
                          />
                        </Field>
                        <Field
                          label="FinOps Cost Allocation ID"
                          helper="Never enter plaintext contract price"
                        >
                          <Input
                            value={draft.costAllocationId}
                            onChange={e =>
                              setDraft({ ...draft, costAllocationId: e.target.value })
                            }
                          />
                        </Field>
                      </div>

                      <label className="flex cursor-pointer items-center gap-2.5 text-xs text-foreground">
                        <PitsSwitch
                          checked={draft.adapterAbstracted}
                          onCheckedChange={checked =>
                            setDraft({ ...draft, adapterAbstracted: checked })
                          }
                        />
                        <span>
                          <code className="font-mono">
                            sourcing.company.io/adapter-abstracted = &quot;
                            {String(draft.adapterAbstracted)}&quot;
                          </code>{' '}
                          (reduces architectural lock-in X1)
                        </span>
                      </label>

                      <div className="space-y-2">
                        <div className="text-xs font-semibold text-foreground">
                          Upstream Dependent Business Components (drives Y1 Blast Radius):
                        </div>
                        <div className="flex flex-wrap gap-3">
                          {(overview?.availableComponents ?? []).map(comp => (
                            <label
                              key={comp.ref}
                              className="flex cursor-pointer items-center gap-2 text-xs text-foreground"
                            >
                              <PitsCheckbox
                                checked={draft.dependsOnComponents.includes(comp.ref)}
                                onCheckedChange={() => toggleComponentDep(comp.ref)}
                              />
                              <span>
                                {comp.name}{' '}
                                <span className="text-muted-foreground">({comp.system})</span>
                              </span>
                            </label>
                          ))}
                        </div>
                      </div>
                    </CardContent>
                  </Card>

                  {/* Section 2 */}
                  <Card>
                    <CardContent className="space-y-4 p-5">
                      <div className="text-sm font-semibold text-foreground">
                        2 · Contract Meta &amp; Eight-Dimension Blind Review (5-Pt Knockout Rule)
                      </div>

                      <div className="grid grid-cols-1 gap-3.5 sm:grid-cols-3">
                        <Field label="Vendor Legal Name">
                          <Input
                            value={draft.audit.contractMeta.vendorName}
                            onChange={e =>
                              setDraft({
                                ...draft,
                                audit: {
                                  ...draft.audit,
                                  contractMeta: {
                                    ...draft.audit.contractMeta,
                                    vendorName: e.target.value,
                                  },
                                },
                              })
                            }
                          />
                        </Field>
                        <Field label="Renewal Date (YYYY-MM-DD)">
                          <Input
                            type="date"
                            value={draft.audit.contractMeta.renewalDate}
                            onChange={e =>
                              setDraft({
                                ...draft,
                                audit: {
                                  ...draft.audit,
                                  contractMeta: {
                                    ...draft.audit.contractMeta,
                                    renewalDate: e.target.value,
                                  },
                                },
                              })
                            }
                          />
                        </Field>
                        <Field label="Baseline Prod Dependents">
                          <Input
                            type="number"
                            value={draft.audit.contractMeta.baselineProdDependents}
                            onChange={e =>
                              setDraft({
                                ...draft,
                                audit: {
                                  ...draft.audit,
                                  contractMeta: {
                                    ...draft.audit.contractMeta,
                                    baselineProdDependents: Math.max(
                                      1,
                                      Number(e.target.value) || 1,
                                    ),
                                  },
                                },
                              })
                            }
                          />
                        </Field>
                      </div>

                      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
                        {(
                          [
                            ['y2_coreCompetitiveness', 'Y2 Core Competitiveness (25%)'],
                            ['y4_priceElasticity', 'Y4 Price Elasticity (15%)'],
                            ['x2_marketScarcity', 'X2 Market Scarcity (25%)'],
                            ['x3_customizationDepth', 'X3 Customization Depth (25%)'],
                            ['x4_complianceFragility', 'X4 Compliance Fragility (15%)'],
                          ] as const
                        ).map(([fieldKey, label]) => {
                          const val = draft.audit.manualScores[fieldKey];
                          return (
                            <div key={fieldKey} className="space-y-1">
                              <div className="flex items-center justify-between text-xs">
                                <span className="font-medium text-foreground">{label}</span>
                                <span
                                  className={cn(
                                    'font-mono font-semibold',
                                    val >= 5
                                      ? 'text-red-600 dark:text-red-400'
                                      : 'text-foreground',
                                  )}
                                >
                                  {val.toFixed(1)} {val >= 5 ? '(KNOCKOUT)' : ''}
                                </span>
                              </div>
                              <PitsSlider
                                min={1}
                                max={5}
                                step={0.5}
                                value={val}
                                onValueChange={v => updateManualScore(fieldKey, v)}
                              />
                            </div>
                          );
                        })}
                      </div>
                    </CardContent>
                  </Card>

                  {/* Section 3 */}
                  <Card>
                    <CardContent className="space-y-4 p-5">
                      <div className="text-sm font-semibold text-foreground">
                        3 · Dutch Windmill Reverse Audit (Supplier View of PITS)
                      </div>

                      <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
                        <Field label="Supplier Perceived Status">
                          <PitsSelect
                            value={draft.audit.supplierPreference.perceivedStatus}
                            onValueChange={val =>
                              setDraft({
                                ...draft,
                                audit: {
                                  ...draft.audit,
                                  supplierPreference: {
                                    ...draft.audit.supplierPreference,
                                    perceivedStatus: val,
                                  },
                                },
                              })
                            }
                            options={SUPPLIER_STATUS_OPTIONS}
                          />
                        </Field>
                        <Field label="Estimated Wallet Share">
                          <PitsSelect
                            value={draft.audit.supplierPreference.estimatedWalletShareTier}
                            onValueChange={val =>
                              setDraft({
                                ...draft,
                                audit: {
                                  ...draft.audit,
                                  supplierPreference: {
                                    ...draft.audit.supplierPreference,
                                    estimatedWalletShareTier: val,
                                  },
                                },
                              })
                            }
                            options={WALLET_SHARE_OPTIONS}
                          />
                        </Field>
                        <div className="space-y-2">
                          <div className="flex items-center justify-between text-xs">
                            <span className="font-medium text-foreground">
                              Account Attractiveness
                            </span>
                            <span className="font-mono font-semibold text-foreground">
                              {draft.audit.supplierPreference.accountAttractivenessScore.toFixed(1)}
                            </span>
                          </div>
                          <PitsSlider
                            min={1}
                            max={5}
                            step={0.1}
                            value={draft.audit.supplierPreference.accountAttractivenessScore}
                            onValueChange={v =>
                              setDraft({
                                ...draft,
                                audit: {
                                  ...draft.audit,
                                  supplierPreference: {
                                    ...draft.audit.supplierPreference,
                                    accountAttractivenessScore: v,
                                  },
                                },
                              })
                            }
                          />
                        </div>
                      </div>
                    </CardContent>
                  </Card>
                </div>

                {/* Right Column: Gate, Save Mode & Unified Diff */}
                <div className="lg:col-span-5">
                  <Card>
                    <CardContent className="space-y-4 p-5">
                      <div className="flex items-center gap-2 text-sm font-semibold text-foreground">
                        <ShieldCheck className="h-4 w-4 text-muted-foreground" />
                        <span>CatalogProcessor Gate &amp; Dual-Mode Persistence</span>
                      </div>

                      <label className="flex cursor-pointer items-start gap-2.5 text-xs text-foreground">
                        <PitsCheckbox
                          checked={writeWorkspaceYaml}
                          onCheckedChange={setWriteWorkspaceYaml}
                        />
                        <span>
                          Also write canonical{' '}
                          <code className="rounded bg-muted px-1 py-0.5 font-mono text-[11px]">
                            catalog/sourcing/&lt;name&gt;.yaml
                          </code>{' '}
                          to workspace repository
                        </span>
                      </label>

                      <Field label="Emergency Override Reason (Mandatory for Save)">
                        <Input
                          required
                          placeholder="e.g., Procurement DB unreachable during incident SEV-2; temporary manual entry"
                          value={overrideReason}
                          onChange={e => setOverrideReason(e.target.value)}
                          disabled={!fallbackUnlocked}
                        />
                      </Field>

                      <div className="flex flex-wrap gap-2">
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => void runProcessorValidation()}
                          disabled={validating || submitting}
                        >
                          {validating ? (
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                          ) : (
                            <ClipboardCheck className="h-3.5 w-3.5" />
                          )}
                          <span>Validate 3 Rules</span>
                        </Button>
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => void handleSubmitWizard(true)}
                          disabled={submitting}
                        >
                          <Eye className="h-3.5 w-3.5" />
                          <span>Dry-Run Diff</span>
                        </Button>
                        <Button
                          variant="warningSolid"
                          size="sm"
                          onClick={() => void handleSubmitWizard(false)}
                          disabled={submitting || !fallbackUnlocked || !overrideReason.trim()}
                        >
                          {submitting ? (
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                          ) : (
                            <Save className="h-3.5 w-3.5" />
                          )}
                          <span>Commit Emergency Override</span>
                        </Button>
                      </div>

                      {wizardError && (
                        <Alert variant="destructive">
                          <AlertCircle />
                          <AlertDescription>{wizardError}</AlertDescription>
                        </Alert>
                      )}

                      {validation && (
                        <div className="space-y-2">
                          {validation.rules.map(r => (
                            <Alert key={r.id} variant={r.passed ? 'success' : 'destructive'}>
                              {r.passed ? <CheckCircle2 /> : <AlertCircle />}
                              <div>
                                <AlertTitle>{r.title}</AlertTitle>
                                <AlertDescription>{r.message}</AlertDescription>
                              </div>
                            </Alert>
                          ))}
                        </div>
                      )}

                      {wizardResult && (
                        <div className="space-y-3">
                          <Separator />
                          <Alert variant={wizardResult.dryRun ? 'info' : 'success'}>
                            {wizardResult.dryRun ? <Info /> : <CheckCircle2 />}
                            <div className="space-y-1.5">
                              <AlertTitle>
                                {wizardResult.dryRun
                                  ? 'Dry-Run Computation Complete'
                                  : 'Persisted & Catalog Refreshed'}{' '}
                                ({wizardResult.persistenceMode})
                              </AlertTitle>
                              <AlertDescription>
                                Quadrant: <strong>{wizardResult.evaluation.facts.quadrant}</strong>{' '}
                                | Y={wizardResult.evaluation.facts.finalYScore.toFixed(2)} | X=
                                {wizardResult.evaluation.facts.finalXScore.toFixed(2)} | Mismatch:{' '}
                                {String(wizardResult.evaluation.facts.windmillMismatch)}
                              </AlertDescription>
                              {!wizardResult.dryRun && (
                                <div>
                                  <Button size="xs" variant="outline" asChild>
                                    <RouterLink
                                      to={`/catalog/default/resource/${encodeURIComponent(
                                        wizardResult.evaluation.record.name,
                                      )}`}
                                    >
                                      Open Entity Scorecard
                                    </RouterLink>
                                  </Button>
                                </div>
                              )}
                            </div>
                          </Alert>

                          <div className="space-y-1.5">
                            <div className="text-xs font-semibold text-foreground">
                              Canonical Unified Diff (catalog-info.yaml)
                            </div>
                            <pre className="pits-scroll m-0 max-h-72 overflow-auto rounded-lg border border-solid border-border bg-zinc-950 p-3 font-mono text-[11px] leading-relaxed text-zinc-200">
                              {wizardResult.unifiedDiff}
                            </pre>
                          </div>
                        </div>
                      )}
                    </CardContent>
                  </Card>
                </div>
              </div>
            </PitsTabPanel>
          </PitsTabs>
        </Card>
      </PageBody>
    </PageShell>
  );
};
