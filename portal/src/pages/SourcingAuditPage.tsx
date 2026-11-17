import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { Link as RouterLink, useLocation, useSearchParams } from 'react-router-dom';
import {
  Alert,
  Box,
  Button,
  Card,
  CardContent,
  Checkbox,
  Chip,
  Divider,
  FormControl,
  FormControlLabel,
  Grid,
  InputLabel,
  LinearProgress,
  MenuItem,
  Paper,
  Select,
  Slider,
  Stack,
  Switch,
  Tab,
  Table,
  TableBody,
  TableCell,
  TableContainer,
  TableHead,
  TableRow,
  Tabs,
  TextField,
  Typography,
} from '@mui/material';
import AddTaskIcon from '@mui/icons-material/AddTask';
import AssessmentIcon from '@mui/icons-material/Assessment';
import CheckCircleOutlineIcon from '@mui/icons-material/CheckCircleOutline';
import ErrorOutlineIcon from '@mui/icons-material/ErrorOutline';
import HowToVoteIcon from '@mui/icons-material/HowToVote';
import LockOutlinedIcon from '@mui/icons-material/LockOutlined';
import PolicyIcon from '@mui/icons-material/Policy';
import PreviewIcon from '@mui/icons-material/Preview';
import RadarIcon from '@mui/icons-material/Radar';
import RefreshIcon from '@mui/icons-material/Refresh';
import RuleIcon from '@mui/icons-material/Rule';
import SaveIcon from '@mui/icons-material/Save';
import StorageIcon from '@mui/icons-material/Storage';
import SyncIcon from '@mui/icons-material/Sync';
import WarningAmberIcon from '@mui/icons-material/WarningAmber';
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

export const SourcingAuditPage: React.FC = () => {
  const [searchParams, setSearchParams] = useSearchParams();
  const location = useLocation();
  const entityParam = searchParams.get('entity');
  const initialWizard =
    location.pathname.includes('/wizard') || Boolean(entityParam) || searchParams.get('tab') === 'wizard';

  const [tabIndex, setTabIndex] = useState<number>(initialWizard ? 3 : 0);
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
        setBallotUnit((prev) =>
          data.items.some((i) => i.record.name === prev) ? prev : data.items[0].record.name,
        );
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadOverview(false);
  }, [loadOverview]);

  const loadEntityIntoDraft = useCallback(
    (item: SourcingEvaluation) => {
      const recCopy: SourcingResourceRecord = JSON.parse(JSON.stringify(item.record));
      if (!recCopy.dependsOnComponents || recCopy.dependsOnComponents.length === 0) {
        recCopy.dependsOnComponents = item.lineage.map((l) => l.componentRef);
      }
      setDraft(recCopy);
      setValidation(item.validation);
      setWizardResult(null);
      setWizardError(null);
      setTabIndex(3);
    },
    [],
  );

  useEffect(() => {
    if (entityParam && overview?.items) {
      const found = overview.items.find(
        (i) => i.record.name.toLowerCase() === entityParam.toLowerCase(),
      );
      if (found) {
        loadEntityIntoDraft(found);
      }
    }
  }, [entityParam, overview, loadEntityIntoDraft]);

  const filteredItems = useMemo(() => {
    if (!overview) return [];
    return overview.items.filter((ev) => {
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

  const matrixPoints = useMemo(
    () => filteredItems.map(toMatrixPoint),
    [filteredItems],
  );

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
      setTabIndex(1);
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
        setWizardError(
          `CatalogProcessor blocked submission: ${rep.errors.join(' | ')}`,
        );
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
    setDraft((prev) => {
      const exists = prev.dependsOnComponents.includes(compRef);
      return {
        ...prev,
        dependsOnComponents: exists
          ? prev.dependsOnComponents.filter((c) => c !== compRef)
          : [...prev.dependsOnComponents, compRef],
      };
    });
  };

  const updateManualScore = (key: keyof SourcingResourceRecord['audit']['manualScores'], val: number) => {
    setDraft((prev) => ({
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

  return (
    <Box sx={{ p: { xs: 2, md: 3 }, maxWidth: 1460, mx: 'auto' }}>
      <Paper
        elevation={0}
        sx={{
          p: 2.5,
          mb: 2.5,
          borderRadius: 2,
          border: '1px solid',
          borderColor: 'divider',
        }}
      >
        <Stack
          direction={{ xs: 'column', md: 'row' }}
          justifyContent="space-between"
          alignItems={{ xs: 'flex-start', md: 'center' }}
          spacing={2}
        >
          <Box>
            <Stack direction="row" spacing={1} alignItems="center" flexWrap="wrap">
              <PolicyIcon color="primary" />
              <Typography variant="h5" sx={{ fontWeight: 800 }}>
                DB-First &amp; MCP Sourcing Governance
              </Typography>
              <Chip
                size="small"
                color="primary"
                variant="outlined"
                label="SQLite WAL + Postgres v3 + MCP Server"
              />
            </Stack>
            <Typography variant="body2" color="text.secondary" sx={{ mt: 0.5 }}>
              Automated workspace discovery &amp; blind multi-role ballots via <code>pitsctl mcp</code> / <code>POST /mcp</code>, persisted in ACID SQLite WAL with bi-directional PostgreSQL outbox sync.
            </Typography>
          </Box>
          <Stack direction="row" spacing={1} flexWrap="wrap">
            <Button
              variant="outlined"
              startIcon={<RefreshIcon />}
              onClick={() => loadOverview(true)}
              disabled={loading}
            >
              Refresh Facts
            </Button>
            <Button
              variant="outlined"
              startIcon={<SyncIcon />}
              onClick={handleSyncDb}
              disabled={syncingDb}
            >
              {syncingDb ? 'Syncing DB...' : 'Sync PostgreSQL Now'}
            </Button>
            <Button
              variant="contained"
              startIcon={<RadarIcon />}
              onClick={handleScanWorkspace}
              disabled={scanningWs}
            >
              {scanningWs ? 'Scanning...' : 'Scan Workspace (MCP)'}
            </Button>
            <Button
              variant="outlined"
              color="warning"
              startIcon={<AddTaskIcon />}
              onClick={() => {
                setDraft(blankSourcingRecord());
                setValidation(null);
                setWizardResult(null);
                setWizardError(null);
                setSearchParams({ tab: 'wizard' });
                setTabIndex(3);
              }}
            >
              Fallback Override
            </Button>
          </Stack>
        </Stack>

        {overview?.dbSync && (
          <Paper
            variant="outlined"
            sx={{
              p: 1.5,
              mt: 2,
              borderRadius: 1.5,
              bgcolor: 'action.hover',
            }}
          >
            <Stack
              direction={{ xs: 'column', md: 'row' }}
              justifyContent="space-between"
              alignItems={{ xs: 'flex-start', md: 'center' }}
              spacing={1}
            >
              <Stack direction="row" spacing={1} alignItems="center" flexWrap="wrap" useFlexGap>
                <StorageIcon fontSize="small" color="primary" />
                <Chip
                  size="small"
                  color={
                    overview.dbSync.backendMode === 'postgresql+sqlite-wal'
                      ? 'success'
                      : overview.dbSync.backendMode === 'sqlite-wal-outbox-degraded'
                        ? 'warning'
                        : 'info'
                  }
                  label={`DB Mode: ${overview.dbSync.backendMode}`}
                />
                <Chip
                  size="small"
                  variant="outlined"
                  label={`SQLite ${overview.dbSync.sqliteVersion} (${overview.dbSync.sqliteJournalMode.toUpperCase()})`}
                />
                <Chip
                  size="small"
                  variant="outlined"
                  color={overview.dbSync.pendingOutboxRows > 0 ? 'warning' : 'default'}
                  label={`Outbox Pending: ${overview.dbSync.pendingOutboxRows}`}
                />
                {overview.dbSync.postgresConfigured && (
                  <Chip
                    size="small"
                    variant="outlined"
                    label={`Postgres: ${overview.dbSync.postgresEndpointRedacted ?? 'configured'}`}
                  />
                )}
              </Stack>
              <Stack direction="row" spacing={1} alignItems="center" flexWrap="wrap" useFlexGap>
                <Chip size="small" color="secondary" variant="outlined" label="MCP Stdio: pitsctl mcp" />
                <Chip size="small" color="secondary" variant="outlined" label="Streamable HTTP: /mcp" />
              </Stack>
            </Stack>
            <Typography variant="caption" color="text.secondary" sx={{ display: 'block', mt: 0.75, fontFamily: 'monospace' }}>
              SQLite SSOT: {overview.dbSync.sqlitePath} | Units: {overview.dbSync.totalUnits} | Ballots: {overview.dbSync.totalBallots} | Discovery Candidates: {overview.dbSync.totalCandidates} | Events: {overview.dbSync.totalEvents}
              {overview.dbSync.lastSyncAt ? ` | Last Sync: ${overview.dbSync.lastSyncAt} (${overview.dbSync.lastSyncDirection ?? ''})` : ''}
              {overview.dbSync.lastSyncError ? ` | Error: ${overview.dbSync.lastSyncError}` : ''}
            </Typography>
          </Paper>
        )}

        {overview && (
          <Stack direction="row" flexWrap="wrap" gap={1.25} sx={{ mt: 2 }}>
            <Chip label={`Units: ${overview.totalUnits}`} sx={{ fontWeight: 700 }} />
            <Chip
              color="error"
              variant="outlined"
              label={`Strategic: ${overview.quadrantCounts.strategic ?? 0}`}
            />
            <Chip
              color="info"
              variant="outlined"
              label={`Leverage: ${overview.quadrantCounts.leverage ?? 0}`}
            />
            <Chip
              color="warning"
              variant="outlined"
              label={`Bottleneck: ${overview.quadrantCounts.bottleneck ?? 0}`}
            />
            <Chip
              color="success"
              variant="outlined"
              label={`Non-Critical: ${overview.quadrantCounts['non-critical'] ?? 0}`}
            />
            <Chip
              color="error"
              label={`Windmill Mismatch: ${overview.redlineCounts.windmillMismatch ?? 0}`}
            />
            <Chip
              color="warning"
              label={`Re-Audit Due: ${overview.redlineCounts.reAuditRequired ?? 0}`}
            />
            <Chip
              variant="outlined"
              label={`Critical Zone [2.7–3.3]: ${overview.redlineCounts.inCriticalZone ?? 0}`}
            />
          </Stack>
        )}
      </Paper>

      {loading && <LinearProgress sx={{ mb: 2 }} />}
      {error && (
        <Alert severity="error" sx={{ mb: 2 }}>
          {error}
        </Alert>
      )}
      {actionBanner && (
        <Alert
          severity={actionBanner.severity}
          sx={{ mb: 2 }}
          onClose={() => setActionBanner(null)}
        >
          {actionBanner.text}
        </Alert>
      )}

      <Paper variant="outlined" sx={{ borderRadius: 2, mb: 2 }}>
        <Tabs
          value={tabIndex}
          onChange={(_, idx) => setTabIndex(idx)}
          variant="scrollable"
          scrollButtons="auto"
          sx={{ px: 2, borderBottom: '1px solid', borderColor: 'divider' }}
        >
          <Tab
            icon={<AssessmentIcon />}
            iconPosition="start"
            label="Portfolio Audit Matrix & Redlines"
          />
          <Tab
            icon={<RadarIcon />}
            iconPosition="start"
            label={`MCP Discovery Queue (${(overview?.discoveryCandidates ?? []).filter((c) => c.status === 'pending').length})`}
          />
          <Tab
            icon={<HowToVoteIcon />}
            iconPosition="start"
            label={`Blind Ballots & Audit Ledger (${overview?.blindBallots?.length ?? 0})`}
          />
          <Tab
            icon={<LockOutlinedIcon />}
            iconPosition="start"
            label="Emergency Fallback Override (Wizard)"
          />
        </Tabs>

        {tabIndex === 0 && (
          <Box sx={{ p: 2.5 }}>
            <Grid container spacing={2.5}>
              <Grid item xs={12} lg={5}>
                <Card variant="outlined" sx={{ borderRadius: 2 }}>
                  <CardContent>
                    <Typography variant="subtitle1" sx={{ fontWeight: 700, mb: 0.5 }}>
                      2D Kraljic × Windmill Scatter Plot
                    </Typography>
                    <Typography variant="caption" color="text.secondary" sx={{ display: 'block', mb: 1.5 }}>
                      Click any sourcing node to inspect its Backstage Entity scorecard. Purple dashed arrows show 5-point redline knockout escalation.
                    </Typography>
                    <KraljicScatterMatrix points={matrixPoints} height={330} />
                  </CardContent>
                </Card>
              </Grid>

              <Grid item xs={12} lg={7}>
                <Stack
                  direction={{ xs: 'column', sm: 'row' }}
                  spacing={1.5}
                  alignItems={{ xs: 'stretch', sm: 'center' }}
                  sx={{ mb: 1.5 }}
                >
                  <FormControl size="small" sx={{ minWidth: 160 }}>
                    <InputLabel>Quadrant</InputLabel>
                    <Select
                      value={quadrantFilter}
                      label="Quadrant"
                      onChange={(e) => setQuadrantFilter(e.target.value)}
                    >
                      <MenuItem value="all">All Quadrants</MenuItem>
                      <MenuItem value="strategic">Strategic</MenuItem>
                      <MenuItem value="leverage">Leverage</MenuItem>
                      <MenuItem value="bottleneck">Bottleneck</MenuItem>
                      <MenuItem value="non-critical">Non-Critical</MenuItem>
                    </Select>
                  </FormControl>

                  <FormControl size="small" sx={{ minWidth: 170 }}>
                    <InputLabel>Resource Type</InputLabel>
                    <Select
                      value={typeFilter}
                      label="Resource Type"
                      onChange={(e) => setTypeFilter(e.target.value)}
                    >
                      <MenuItem value="all">All 4 Sourcing Types</MenuItem>
                      <MenuItem value="commercial-sdk">commercial-sdk</MenuItem>
                      <MenuItem value="vendor-saas">vendor-saas</MenuItem>
                      <MenuItem value="managed-infra">managed-infra</MenuItem>
                      <MenuItem value="outsourced-dev">outsourced-dev</MenuItem>
                    </Select>
                  </FormControl>

                  <FormControlLabel
                    control={
                      <Switch
                        checked={redlinesOnly}
                        onChange={(e) => setRedlinesOnly(e.target.checked)}
                      />
                    }
                    label="Redlines / Critical Only"
                  />
                </Stack>

                <TableContainer component={Paper} variant="outlined" sx={{ borderRadius: 1.5 }}>
                  <Table size="small">
                    <TableHead>
                      <TableRow>
                        <TableCell sx={{ fontWeight: 700 }}>Sourcing Unit</TableCell>
                        <TableCell sx={{ fontWeight: 700 }}>System → Domain</TableCell>
                        <TableCell sx={{ fontWeight: 700 }}>Y / X Scores</TableCell>
                        <TableCell sx={{ fontWeight: 700 }}>Source / Ballots</TableCell>
                        <TableCell sx={{ fontWeight: 700 }}>Quadrant</TableCell>
                        <TableCell sx={{ fontWeight: 700 }}>Windmill Status</TableCell>
                        <TableCell sx={{ fontWeight: 700 }}>Governance</TableCell>
                        <TableCell align="right" sx={{ fontWeight: 700 }}>
                          Override
                        </TableCell>
                      </TableRow>
                    </TableHead>
                    <TableBody>
                      {filteredItems.map((ev) => {
                        const redlines = ev.checks.filter((c) => c.triggered).length;
                        return (
                          <TableRow key={ev.record.name} hover>
                            <TableCell>
                              <Typography
                                component={RouterLink}
                                to={`/catalog/default/resource/${encodeURIComponent(ev.record.name)}`}
                                variant="body2"
                                sx={{ fontWeight: 700, textDecoration: 'none', color: 'primary.main' }}
                              >
                                {ev.record.name}
                              </Typography>
                              <Typography variant="caption" color="text.secondary" sx={{ display: 'block' }}>
                                {ev.record.resourceType} · {ev.record.audit.contractMeta.vendorName}
                              </Typography>
                            </TableCell>
                            <TableCell>
                              <Typography variant="caption" sx={{ fontFamily: 'monospace', display: 'block' }}>
                                {ev.record.system}
                              </Typography>
                              <Typography variant="caption" color="text.secondary">
                                {ev.resolvedDomain.replace('domain:default/', '')} ({ev.facts.prodDependents} deps)
                              </Typography>
                            </TableCell>
                            <TableCell>
                              <Typography variant="body2" sx={{ fontFamily: 'monospace', fontWeight: 700 }}>
                                Y={ev.facts.finalYScore.toFixed(2)} / X={ev.facts.finalXScore.toFixed(2)}
                              </Typography>
                              {ev.facts.knockoutTriggered && (
                                <Chip size="small" color="secondary" label="5-pt Knockout" sx={{ mt: 0.25 }} />
                              )}
                            </TableCell>
                            <TableCell>
                              <Chip
                                size="small"
                                variant="outlined"
                                color={ev.sourceOrigin === 'wizard-fallback' ? 'warning' : 'default'}
                                label={ev.sourceOrigin ?? 'seed-baseline'}
                              />
                              <Typography variant="caption" color="text.secondary" sx={{ display: 'block', mt: 0.25 }}>
                                {ev.ballotCount ?? 0} ballots · spread {(ev.maxBallotSpread ?? 0).toFixed(2)}
                              </Typography>
                              {ev.record.audit.contractMeta.blindScoreVarianceAlert && (
                                <Chip size="small" color="error" label="Variance >= 1.5" sx={{ mt: 0.25 }} />
                              )}
                            </TableCell>
                            <TableCell>
                              <Chip size="small" label={ev.facts.quadrant} variant="outlined" />
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
                                  redlines === 0 ? 'Healthy' : `${redlines} Redline${redlines > 1 ? 's' : ''}`
                                }
                                level={ev.facts.windmillMismatch ? 'error' : redlines > 0 ? 'warn' : 'ok'}
                              />
                            </TableCell>
                            <TableCell align="right">
                              <Button
                                size="small"
                                variant="outlined"
                                color="warning"
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
                </TableContainer>
              </Grid>
            </Grid>
          </Box>
        )}

        {tabIndex === 1 && (
          <Box sx={{ p: 2.5 }}>
            <Stack
              direction={{ xs: 'column', sm: 'row' }}
              justifyContent="space-between"
              alignItems={{ xs: 'flex-start', sm: 'center' }}
              spacing={1.5}
              sx={{ mb: 2 }}
            >
              <Box>
                <Typography variant="subtitle1" sx={{ fontWeight: 700 }}>
                  MCP Automated Workspace Discovery Queue
                </Typography>
                <Typography variant="caption" color="text.secondary">
                  Scanned from <code>Cargo.toml</code>, <code>package.json</code>, <code>go.mod</code>, Nix flakes &amp; K8s manifests via <code>sourcing_scan_workspace</code>. Promote verified granular units into <code>sourcing_units</code> (FinOps secret &amp; anti-monolith rules enforced automatically).
                </Typography>
              </Box>
              <Button
                variant="contained"
                size="small"
                startIcon={<RadarIcon />}
                onClick={handleScanWorkspace}
                disabled={scanningWs}
              >
                {scanningWs ? 'Scanning...' : 'Run Discovery Scan'}
              </Button>
            </Stack>

            <TableContainer component={Paper} variant="outlined" sx={{ borderRadius: 1.5 }}>
              <Table size="small">
                <TableHead>
                  <TableRow>
                    <TableCell sx={{ fontWeight: 700 }}>Dependency / Package</TableCell>
                    <TableCell sx={{ fontWeight: 700 }}>Ecosystem &amp; Source</TableCell>
                    <TableCell sx={{ fontWeight: 700 }}>Adapter &amp; Granularity</TableCell>
                    <TableCell sx={{ fontWeight: 700 }}>Promotion Parameters</TableCell>
                    <TableCell sx={{ fontWeight: 700 }}>Status</TableCell>
                    <TableCell align="right" sx={{ fontWeight: 700 }}>Disposition</TableCell>
                  </TableRow>
                </TableHead>
                <TableBody>
                  {(overview?.discoveryCandidates ?? []).length === 0 ? (
                    <TableRow>
                      <TableCell colSpan={6} align="center" sx={{ py: 3 }}>
                        <Typography variant="body2" color="text.secondary">
                          No candidates scanned yet. Click <b>Run Discovery Scan</b> or invoke MCP tool <code>sourcing_scan_workspace</code>.
                        </Typography>
                      </TableCell>
                    </TableRow>
                  ) : (
                    (overview?.discoveryCandidates ?? []).map((cand) => {
                      const chosenType = candidateTypeMap[cand.candidateId] ?? cand.suggestedType;
                      const chosenCost = candidateCostMap[cand.candidateId] ?? '';
                      const chosenName = candidateNameMap[cand.candidateId] ?? '';
                      const busy = disposingId === cand.candidateId;
                      return (
                        <TableRow key={cand.candidateId} hover>
                          <TableCell>
                            <Typography variant="body2" sx={{ fontWeight: 700 }}>
                              {cand.packageOrEndpoint}
                            </Typography>
                            <Typography variant="caption" color="text.secondary" sx={{ fontFamily: 'monospace' }}>
                              {cand.candidateId}
                            </Typography>
                          </TableCell>
                          <TableCell>
                            <Chip size="small" label={cand.ecosystem} variant="outlined" sx={{ mr: 0.5 }} />
                            <Typography variant="caption" color="text.secondary" sx={{ display: 'block', mt: 0.25 }}>
                              {cand.detectedInPath} · sys: {cand.inferredSystem}
                            </Typography>
                          </TableCell>
                          <TableCell>
                            <Chip
                              size="small"
                              color={cand.adapterDetected ? 'success' : 'warning'}
                              label={cand.adapterDetected ? 'Adapter Detected' : 'No Adapter'}
                            />
                            <Typography variant="caption" color="text.secondary" sx={{ display: 'block', mt: 0.25 }}>
                              {cand.monolithRiskNote}
                            </Typography>
                          </TableCell>
                          <TableCell>
                            {cand.status === 'pending' ? (
                              <Stack direction="row" spacing={1} flexWrap="wrap" useFlexGap>
                                <Select
                                  size="small"
                                  value={chosenType}
                                  onChange={(e) =>
                                    setCandidateTypeMap((m) => ({
                                      ...m,
                                      [cand.candidateId]: e.target.value,
                                    }))
                                  }
                                  sx={{ minWidth: 135, fontSize: '0.78rem' }}
                                >
                                  <MenuItem value="commercial-sdk">commercial-sdk</MenuItem>
                                  <MenuItem value="vendor-saas">vendor-saas</MenuItem>
                                  <MenuItem value="managed-infra">managed-infra</MenuItem>
                                  <MenuItem value="outsourced-dev">outsourced-dev</MenuItem>
                                </Select>
                                <TextField
                                  size="small"
                                  placeholder="Unit name (optional)"
                                  value={chosenName}
                                  onChange={(e) =>
                                    setCandidateNameMap((m) => ({
                                      ...m,
                                      [cand.candidateId]: e.target.value,
                                    }))
                                  }
                                  sx={{ width: 145 }}
                                />
                                <TextField
                                  size="small"
                                  placeholder="FinOps CC ID (cc-mcp-...)"
                                  value={chosenCost}
                                  onChange={(e) =>
                                    setCandidateCostMap((m) => ({
                                      ...m,
                                      [cand.candidateId]: e.target.value,
                                    }))
                                  }
                                  sx={{ width: 145 }}
                                />
                              </Stack>
                            ) : (
                              <Typography variant="caption" color="text.secondary">
                                {cand.promotedUnitName ? `Unit: ${cand.promotedUnitName}` : '—'}
                              </Typography>
                            )}
                          </TableCell>
                          <TableCell>
                            <Chip
                              size="small"
                              color={
                                cand.status === 'promoted'
                                  ? 'success'
                                  : cand.status === 'ignored'
                                    ? 'default'
                                    : 'info'
                              }
                              label={cand.status}
                            />
                          </TableCell>
                          <TableCell align="right">
                            {cand.status === 'pending' && (
                              <Stack direction="row" spacing={0.75} justifyContent="flex-end">
                                <Button
                                  size="small"
                                  variant="contained"
                                  disabled={busy}
                                  onClick={() =>
                                    handleDisposeCandidate(
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
                                  size="small"
                                  variant="outlined"
                                  color="inherit"
                                  disabled={busy}
                                  onClick={() =>
                                    handleDisposeCandidate(
                                      cand.candidateId,
                                      cand.packageOrEndpoint,
                                      cand.suggestedType,
                                      'ignore',
                                    )
                                  }
                                >
                                  Ignore
                                </Button>
                              </Stack>
                            )}
                          </TableCell>
                        </TableRow>
                      );
                    })
                  )}
                </TableBody>
              </Table>
            </TableContainer>
          </Box>
        )}

        {tabIndex === 2 && (
          <Box sx={{ p: 2.5 }}>
            <Grid container spacing={2.5}>
              <Grid item xs={12} lg={5}>
                <Card variant="outlined" component="form" onSubmit={handleSubmitBlindBallot} sx={{ borderRadius: 2 }}>
                  <CardContent>
                    <Typography variant="subtitle1" sx={{ fontWeight: 700, mb: 0.5 }}>
                      Submit Independent Blind Role Ballot
                    </Typography>
                    <Typography variant="caption" color="text.secondary" sx={{ display: 'block', mb: 1.5 }}>
                      Each role (`architecture`, `procurement`, `security-compliance`, `domain-owner`) submits scores independently into SQLite WAL. Spread &gt;= 1.5 across reviewers automatically sets <code>blindScoreVarianceAlert = true</code>.
                    </Typography>
                    <Stack spacing={1.5}>
                      <FormControl fullWidth size="small">
                        <InputLabel>Target Sourcing Unit</InputLabel>
                        <Select
                          value={ballotUnit}
                          label="Target Sourcing Unit"
                          onChange={(e) => setBallotUnit(e.target.value)}
                        >
                          {(overview?.items ?? []).map((ev) => (
                            <MenuItem key={ev.record.name} value={ev.record.name}>
                              {ev.record.name} ({ev.record.resourceType})
                            </MenuItem>
                          ))}
                        </Select>
                      </FormControl>
                      <Grid container spacing={1.5}>
                        <Grid item xs={12} sm={6}>
                          <FormControl fullWidth size="small">
                            <InputLabel>Reviewer Role</InputLabel>
                            <Select
                              value={ballotRole}
                              label="Reviewer Role"
                              onChange={(e) => setBallotRole(e.target.value)}
                            >
                              <MenuItem value="architecture">architecture</MenuItem>
                              <MenuItem value="procurement">procurement</MenuItem>
                              <MenuItem value="security-compliance">security-compliance</MenuItem>
                              <MenuItem value="domain-owner">domain-owner</MenuItem>
                            </Select>
                          </FormControl>
                        </Grid>
                        <Grid item xs={12} sm={6}>
                          <TextField
                            fullWidth
                            size="small"
                            label="Reviewer ID / Email"
                            value={ballotReviewerId}
                            onChange={(e) => setBallotReviewerId(e.target.value)}
                          />
                        </Grid>
                      </Grid>

                      {(
                        [
                          ['Y2 Core Competitiveness', ballotY2, setBallotY2],
                          ['Y4 Price Elasticity', ballotY4, setBallotY4],
                          ['X2 Market Scarcity', ballotX2, setBallotX2],
                          ['X3 Customization Depth', ballotX3, setBallotX3],
                          ['X4 Compliance Fragility', ballotX4, setBallotX4],
                        ] as const
                      ).map(([lbl, v, setter]) => (
                        <Box key={lbl}>
                          <Stack direction="row" justifyContent="space-between">
                            <Typography variant="caption" sx={{ fontWeight: 600 }}>{lbl}</Typography>
                            <Typography variant="caption" sx={{ fontWeight: 700 }}>{v.toFixed(1)}</Typography>
                          </Stack>
                          <Slider
                            size="small"
                            min={1}
                            max={5}
                            step={0.5}
                            marks
                            value={v}
                            onChange={(_, n) => setter(Number(n))}
                          />
                        </Box>
                      ))}

                      <TextField
                        fullWidth
                        size="small"
                        multiline
                        rows={2}
                        label="Evidence / Justification Notes"
                        value={ballotNotes}
                        onChange={(e) => setBallotNotes(e.target.value)}
                      />

                      <Button
                        type="submit"
                        variant="contained"
                        disabled={submittingBallot}
                        startIcon={<HowToVoteIcon />}
                      >
                        {submittingBallot ? 'Recording Ballot...' : 'Submit Blind Role Ballot'}
                      </Button>
                    </Stack>
                  </CardContent>
                </Card>
              </Grid>

              <Grid item xs={12} lg={7}>
                <Stack spacing={2}>
                  <Card variant="outlined" sx={{ borderRadius: 2 }}>
                    <CardContent>
                      <Typography variant="subtitle1" sx={{ fontWeight: 700, mb: 1 }}>
                        Blind Role Ballots ({overview?.blindBallots?.length ?? 0})
                      </Typography>
                      <TableContainer sx={{ maxHeight: 280 }}>
                        <Table size="small" stickyHeader>
                          <TableHead>
                            <TableRow>
                              <TableCell sx={{ fontWeight: 700 }}>Unit</TableCell>
                              <TableCell sx={{ fontWeight: 700 }}>Role / Reviewer</TableCell>
                              <TableCell sx={{ fontWeight: 700 }}>Y2/Y4/X2/X3/X4</TableCell>
                              <TableCell sx={{ fontWeight: 700 }}>Notes</TableCell>
                            </TableRow>
                          </TableHead>
                          <TableBody>
                            {(overview?.blindBallots ?? []).map((b) => (
                              <TableRow key={b.ballotId} hover>
                                <TableCell sx={{ fontWeight: 600 }}>{b.unitName}</TableCell>
                                <TableCell>
                                  <Chip size="small" label={b.reviewerRole} variant="outlined" sx={{ mr: 0.5 }} />
                                  <Typography variant="caption" color="text.secondary" sx={{ display: 'block' }}>
                                    {b.reviewerId}
                                  </Typography>
                                </TableCell>
                                <TableCell sx={{ fontFamily: 'monospace', fontSize: '0.75rem' }}>
                                  {b.y2_coreCompetitiveness.toFixed(1)} / {b.y4_priceElasticity.toFixed(1)} / {b.x2_marketScarcity.toFixed(1)} / {b.x3_customizationDepth.toFixed(1)} / {b.x4_complianceFragility.toFixed(1)}
                                </TableCell>
                                <TableCell>
                                  <Typography variant="caption" color="text.secondary">
                                    {b.evidenceNotes || '—'}
                                  </Typography>
                                </TableCell>
                              </TableRow>
                            ))}
                          </TableBody>
                        </Table>
                      </TableContainer>
                    </CardContent>
                  </Card>

                  <Card variant="outlined" sx={{ borderRadius: 2 }}>
                    <CardContent>
                      <Typography variant="subtitle1" sx={{ fontWeight: 700, mb: 1 }}>
                        Immutable DB Audit Event Ledger ({overview?.recentEvents?.length ?? 0})
                      </Typography>
                      <TableContainer sx={{ maxHeight: 240 }}>
                        <Table size="small" stickyHeader>
                          <TableHead>
                            <TableRow>
                              <TableCell sx={{ fontWeight: 700 }}>Type / Channel</TableCell>
                              <TableCell sx={{ fontWeight: 700 }}>Unit &amp; Actor</TableCell>
                              <TableCell sx={{ fontWeight: 700 }}>Summary</TableCell>
                            </TableRow>
                          </TableHead>
                          <TableBody>
                            {(overview?.recentEvents ?? []).map((ev) => (
                              <TableRow key={ev.eventId} hover>
                                <TableCell>
                                  <Chip size="small" label={ev.eventType} variant="outlined" />
                                  <Typography variant="caption" color="text.secondary" sx={{ display: 'block' }}>
                                    via {ev.channel}
                                  </Typography>
                                </TableCell>
                                <TableCell>
                                  <Typography variant="caption" sx={{ fontWeight: 700, display: 'block' }}>
                                    {ev.unitName}
                                  </Typography>
                                  <Typography variant="caption" color="text.secondary">
                                    {ev.actor}
                                  </Typography>
                                </TableCell>
                                <TableCell>
                                  <Typography variant="caption">{ev.summary}</Typography>
                                  <Typography variant="caption" color="text.secondary" sx={{ display: 'block', fontFamily: 'monospace' }}>
                                    {ev.createdAt}
                                  </Typography>
                                </TableCell>
                              </TableRow>
                            ))}
                          </TableBody>
                        </Table>
                      </TableContainer>
                    </CardContent>
                  </Card>
                </Stack>
              </Grid>
            </Grid>
          </Box>
        )}

        {tabIndex === 3 && (
          <Box sx={{ p: 2.5 }}>
            <Alert
              severity="warning"
              icon={<WarningAmberIcon />}
              sx={{ mb: 2 }}
              action={
                <FormControlLabel
                  control={
                    <Switch
                      color="warning"
                      checked={fallbackUnlocked}
                      onChange={(e) => setFallbackUnlocked(e.target.checked)}
                    />
                  }
                  label={fallbackUnlocked ? 'Override UNLOCKED' : 'Locked (Enable Override)'}
                />
              }
            >
              <Typography variant="body2" sx={{ fontWeight: 700 }}>
                Emergency Fallback Override Only — Primary Intake Uses MCP &amp; DB Sync
              </Typography>
              <Typography variant="caption">
                Standard onboarding uses <code>pitsctl mcp</code> / <code>POST /mcp</code> (<code>sourcing_scan_workspace</code> → <code>sourcing_promote_candidate</code> → <code>sourcing_submit_blind_ballot</code>). Unlocking and committing here stamps <code>source_origin = &quot;wizard-fallback&quot;</code> and logs your mandatory override reason in the SQLite WAL + PostgreSQL audit ledger.
              </Typography>
            </Alert>
            <Grid container spacing={2.5}>
              <Grid item xs={12} lg={7}>
                <Stack spacing={2}>
                  {overview && overview.items.length > 0 && (
                    <Paper variant="outlined" sx={{ p: 1.5, borderRadius: 1.5 }}>
                      <Stack direction="row" spacing={1.5} alignItems="center" flexWrap="wrap">
                        <Typography variant="subtitle2" sx={{ fontWeight: 700 }}>
                          Load Existing Sourcing Unit:
                        </Typography>
                        {overview.items.map((ev) => (
                          <Chip
                            key={ev.record.name}
                            size="small"
                            clickable
                            color={draft.name === ev.record.name ? 'primary' : 'default'}
                            label={ev.record.name}
                            onClick={() => loadEntityIntoDraft(ev)}
                          />
                        ))}
                      </Stack>
                    </Paper>
                  )}

                  {/* Section 1: CatalogProcessor Unpacking */}
                  <Card variant="outlined" sx={{ borderRadius: 2 }}>
                    <CardContent>
                      <Typography variant="subtitle1" sx={{ fontWeight: 700, mb: 1.5 }}>
                        1 · 4-Layer Catalog Unpacking &amp; FinOps Cost Attribution (CatalogProcessor)
                      </Typography>
                      <Grid container spacing={1.5}>
                        <Grid item xs={12} sm={6}>
                          <TextField
                            fullWidth
                            size="small"
                            label="Entity Name (DNS-safe sourcing unit)"
                            placeholder="e.g. fraud-rule-engine-sdk"
                            value={draft.name}
                            onChange={(e) => setDraft({ ...draft, name: e.target.value })}
                            helperText="Must avoid vendor-level catch-alls like '-all' or bare vendor name"
                          />
                        </Grid>
                        <Grid item xs={12} sm={6}>
                          <FormControl fullWidth size="small">
                            <InputLabel>Granular Resource Type (spec.type)</InputLabel>
                            <Select
                              value={draft.resourceType}
                              label="Granular Resource Type (spec.type)"
                              onChange={(e) => setDraft({ ...draft, resourceType: e.target.value })}
                            >
                              <MenuItem value="commercial-sdk">commercial-sdk</MenuItem>
                              <MenuItem value="vendor-saas">vendor-saas</MenuItem>
                              <MenuItem value="managed-infra">managed-infra</MenuItem>
                              <MenuItem value="outsourced-dev">outsourced-dev</MenuItem>
                            </Select>
                          </FormControl>
                        </Grid>
                        <Grid item xs={12}>
                          <TextField
                            fullWidth
                            size="small"
                            label="Description"
                            value={draft.description}
                            onChange={(e) => setDraft({ ...draft, description: e.target.value })}
                          />
                        </Grid>
                        <Grid item xs={12} sm={4}>
                          <FormControl fullWidth size="small">
                            <InputLabel>Parent System (spec.system)</InputLabel>
                            <Select
                              value={draft.system}
                              label="Parent System (spec.system)"
                              onChange={(e) => setDraft({ ...draft, system: e.target.value })}
                            >
                              {(overview?.availableSystems ?? [
                                { name: 'core-checkout', title: 'Core Checkout', domain: 'core-commerce' },
                                { name: 'pits-infrastructure', title: 'PITS Infrastructure', domain: 'platform-infra' },
                                { name: 'pits-city-pipeline', title: 'PITS City Pipeline', domain: 'spatial-simulation' },
                              ]).map((sys) => (
                                <MenuItem key={sys.name} value={sys.name}>
                                  {sys.name} ({sys.domain})
                                </MenuItem>
                              ))}
                            </Select>
                          </FormControl>
                        </Grid>
                        <Grid item xs={12} sm={4}>
                          <TextField
                            fullWidth
                            size="small"
                            label="Owner (spec.owner)"
                            value={draft.owner}
                            onChange={(e) => setDraft({ ...draft, owner: e.target.value })}
                          />
                        </Grid>
                        <Grid item xs={12} sm={4}>
                          <TextField
                            fullWidth
                            size="small"
                            label="FinOps Cost Allocation ID"
                            value={draft.costAllocationId}
                            onChange={(e) => setDraft({ ...draft, costAllocationId: e.target.value })}
                            helperText="Never enter plaintext contract price"
                          />
                        </Grid>
                        <Grid item xs={12}>
                          <FormControlLabel
                            control={
                              <Switch
                                checked={draft.adapterAbstracted}
                                onChange={(e) =>
                                  setDraft({ ...draft, adapterAbstracted: e.target.checked })
                                }
                              />
                            }
                            label={`sourcing.company.io/adapter-abstracted = "${draft.adapterAbstracted}" (reduces architectural lock-in X1)`}
                          />
                        </Grid>
                        <Grid item xs={12}>
                          <Typography variant="caption" sx={{ fontWeight: 700, display: 'block', mb: 0.5 }}>
                            Upstream Dependent Business Components (drives Y1 Blast Radius):
                          </Typography>
                          <Stack direction="row" flexWrap="wrap" gap={0.5}>
                            {(overview?.availableComponents ?? []).map((comp) => (
                              <FormControlLabel
                                key={comp.ref}
                                control={
                                  <Checkbox
                                    size="small"
                                    checked={draft.dependsOnComponents.includes(comp.ref)}
                                    onChange={() => toggleComponentDep(comp.ref)}
                                  />
                                }
                                label={
                                  <Typography variant="caption">
                                    {comp.name} ({comp.system})
                                  </Typography>
                                }
                              />
                            ))}
                          </Stack>
                        </Grid>
                      </Grid>
                    </CardContent>
                  </Card>

                  {/* Section 2: Blind Review Scores */}
                  <Card variant="outlined" sx={{ borderRadius: 2 }}>
                    <CardContent>
                      <Typography variant="subtitle1" sx={{ fontWeight: 700, mb: 1.5 }}>
                        2 · Contract Meta &amp; Eight-Dimension Blind Review (5-Pt Knockout Rule)
                      </Typography>
                      <Grid container spacing={1.5} sx={{ mb: 1.5 }}>
                        <Grid item xs={12} sm={4}>
                          <TextField
                            fullWidth
                            size="small"
                            label="Vendor Legal Name"
                            value={draft.audit.contractMeta.vendorName}
                            onChange={(e) =>
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
                        </Grid>
                        <Grid item xs={12} sm={4}>
                          <TextField
                            fullWidth
                            size="small"
                            type="date"
                            label="Renewal Date (YYYY-MM-DD)"
                            InputLabelProps={{ shrink: true }}
                            value={draft.audit.contractMeta.renewalDate}
                            onChange={(e) =>
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
                        </Grid>
                        <Grid item xs={12} sm={4}>
                          <TextField
                            fullWidth
                            size="small"
                            type="number"
                            label="Baseline Prod Dependents"
                            value={draft.audit.contractMeta.baselineProdDependents}
                            onChange={(e) =>
                              setDraft({
                                ...draft,
                                audit: {
                                  ...draft.audit,
                                  contractMeta: {
                                    ...draft.audit.contractMeta,
                                    baselineProdDependents: Math.max(1, Number(e.target.value) || 1),
                                  },
                                },
                              })
                            }
                          />
                        </Grid>
                      </Grid>

                      <Grid container spacing={2}>
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
                            <Grid item xs={12} sm={6} key={fieldKey}>
                              <Stack direction="row" justifyContent="space-between">
                                <Typography variant="caption" sx={{ fontWeight: 600 }}>
                                  {label}
                                </Typography>
                                <Typography
                                  variant="caption"
                                  sx={{
                                    fontWeight: 700,
                                    color: val >= 5 ? 'error.main' : 'primary.main',
                                  }}
                                >
                                  {val.toFixed(1)} {val >= 5 ? '(KNOCKOUT)' : ''}
                                </Typography>
                              </Stack>
                              <Slider
                                size="small"
                                min={1}
                                max={5}
                                step={0.5}
                                marks
                                value={val}
                                onChange={(_, v) => updateManualScore(fieldKey, Number(v))}
                              />
                            </Grid>
                          );
                        })}
                      </Grid>
                    </CardContent>
                  </Card>

                  {/* Section 3: Dutch Windmill */}
                  <Card variant="outlined" sx={{ borderRadius: 2 }}>
                    <CardContent>
                      <Typography variant="subtitle1" sx={{ fontWeight: 700, mb: 1.5 }}>
                        3 · Dutch Windmill Reverse Audit (Supplier View of PITS)
                      </Typography>
                      <Grid container spacing={1.5}>
                        <Grid item xs={12} sm={4}>
                          <FormControl fullWidth size="small">
                            <InputLabel>Supplier Perceived Status</InputLabel>
                            <Select
                              value={draft.audit.supplierPreference.perceivedStatus}
                              label="Supplier Perceived Status"
                              onChange={(e) =>
                                setDraft({
                                  ...draft,
                                  audit: {
                                    ...draft.audit,
                                    supplierPreference: {
                                      ...draft.audit.supplierPreference,
                                      perceivedStatus: e.target.value,
                                    },
                                  },
                                })
                              }
                            >
                              <MenuItem value="core">core (Strategic Partner)</MenuItem>
                              <MenuItem value="development">development (Growth)</MenuItem>
                              <MenuItem value="exploitable">exploitable (Captive)</MenuItem>
                              <MenuItem value="nuisance">nuisance (Low Priority)</MenuItem>
                            </Select>
                          </FormControl>
                        </Grid>
                        <Grid item xs={12} sm={4}>
                          <FormControl fullWidth size="small">
                            <InputLabel>Estimated Wallet Share</InputLabel>
                            <Select
                              value={draft.audit.supplierPreference.estimatedWalletShareTier}
                              label="Estimated Wallet Share"
                              onChange={(e) =>
                                setDraft({
                                  ...draft,
                                  audit: {
                                    ...draft.audit,
                                    supplierPreference: {
                                      ...draft.audit.supplierPreference,
                                      estimatedWalletShareTier: e.target.value,
                                    },
                                  },
                                })
                              }
                            >
                              <MenuItem value="under-1pct">under-1pct</MenuItem>
                              <MenuItem value="1-to-5pct">1-to-5pct</MenuItem>
                              <MenuItem value="5-to-15pct">5-to-15pct</MenuItem>
                              <MenuItem value="over-15pct">over-15pct</MenuItem>
                            </Select>
                          </FormControl>
                        </Grid>
                        <Grid item xs={12} sm={4}>
                          <Typography variant="caption" sx={{ fontWeight: 600 }}>
                            Account Attractiveness ({draft.audit.supplierPreference.accountAttractivenessScore.toFixed(1)})
                          </Typography>
                          <Slider
                            size="small"
                            min={1}
                            max={5}
                            step={0.1}
                            value={draft.audit.supplierPreference.accountAttractivenessScore}
                            onChange={(_, v) =>
                              setDraft({
                                ...draft,
                                audit: {
                                  ...draft.audit,
                                  supplierPreference: {
                                    ...draft.audit.supplierPreference,
                                    accountAttractivenessScore: Number(v),
                                  },
                                },
                              })
                            }
                          />
                        </Grid>
                      </Grid>
                    </CardContent>
                  </Card>
                </Stack>
              </Grid>

              {/* Right Column: Live CatalogProcessor Gate, Save Mode & Unified Diff Preview */}
              <Grid item xs={12} lg={5}>
                <Stack spacing={2}>
                  <Card variant="outlined" sx={{ borderRadius: 2 }}>
                    <CardContent>
                      <Typography variant="subtitle1" sx={{ fontWeight: 700, mb: 1 }}>
                        CatalogProcessor Gate &amp; Dual-Mode Persistence
                      </Typography>
                      <FormControlLabel
                        sx={{ mb: 1 }}
                        control={
                          <Checkbox
                            checked={writeWorkspaceYaml}
                            onChange={(e) => setWriteWorkspaceYaml(e.target.checked)}
                          />
                        }
                        label={
                          <Typography variant="body2">
                            Also write canonical <code>catalog/sourcing/&lt;name&gt;.yaml</code> to workspace repository
                          </Typography>
                        }
                      />

                      <TextField
                        fullWidth
                        size="small"
                        required
                        label="Emergency Override Reason (Mandatory for Save)"
                        placeholder="e.g., Procurement DB unreachable during incident SEV-2; temporary manual entry"
                        value={overrideReason}
                        onChange={(e) => setOverrideReason(e.target.value)}
                        disabled={!fallbackUnlocked}
                        sx={{ mb: 1.5 }}
                      />

                      <Stack direction="row" flexWrap="wrap" gap={1} sx={{ mb: 1.5 }}>
                        <Button
                          variant="outlined"
                          size="small"
                          startIcon={<RuleIcon />}
                          onClick={runProcessorValidation}
                          disabled={validating || submitting}
                        >
                          Validate 3 Rules
                        </Button>
                        <Button
                          variant="outlined"
                          size="small"
                          startIcon={<PreviewIcon />}
                          onClick={() => handleSubmitWizard(true)}
                          disabled={submitting}
                        >
                          Dry-Run Diff
                        </Button>
                        <Button
                          variant="contained"
                          color="warning"
                          size="small"
                          startIcon={<SaveIcon />}
                          onClick={() => handleSubmitWizard(false)}
                          disabled={submitting || !fallbackUnlocked || !overrideReason.trim()}
                        >
                          Commit Emergency Override
                        </Button>
                      </Stack>

                      {(validating || submitting) && <LinearProgress sx={{ mb: 1 }} />}

                      {wizardError && (
                        <Alert severity="error" sx={{ mb: 1.5 }}>
                          {wizardError}
                        </Alert>
                      )}

                      {validation && (
                        <Stack spacing={0.75} sx={{ mb: 1.5 }}>
                          {validation.rules.map((r) => (
                            <Alert
                              key={r.id}
                              severity={r.passed ? 'success' : 'error'}
                              icon={r.passed ? <CheckCircleOutlineIcon /> : <ErrorOutlineIcon />}
                              sx={{ py: 0.25 }}
                            >
                              <Typography variant="caption" sx={{ fontWeight: 700, display: 'block' }}>
                                {r.title}
                              </Typography>
                              <Typography variant="caption">{r.message}</Typography>
                            </Alert>
                          ))}
                        </Stack>
                      )}

                      {wizardResult && (
                        <Box>
                          <Divider sx={{ my: 1.5 }} />
                          <Alert severity={wizardResult.dryRun ? 'info' : 'success'} sx={{ mb: 1.5 }}>
                            <Typography variant="body2" sx={{ fontWeight: 700 }}>
                              {wizardResult.dryRun ? 'Dry-Run Computation Complete' : 'Persisted & Catalog Refreshed'} ({wizardResult.persistenceMode})
                            </Typography>
                            <Typography variant="caption" sx={{ display: 'block' }}>
                              Quadrant: <b>{wizardResult.evaluation.facts.quadrant}</b> | Y=
                              {wizardResult.evaluation.facts.finalYScore.toFixed(2)} | X=
                              {wizardResult.evaluation.facts.finalXScore.toFixed(2)} | Mismatch:{' '}
                              {String(wizardResult.evaluation.facts.windmillMismatch)}
                            </Typography>
                            {!wizardResult.dryRun && (
                              <Button
                                size="small"
                                sx={{ mt: 0.75 }}
                                variant="outlined"
                                component={RouterLink}
                                to={`/catalog/default/resource/${encodeURIComponent(
                                  wizardResult.evaluation.record.name,
                                )}`}
                              >
                                Open Entity Scorecard
                              </Button>
                            )}
                          </Alert>

                          <Typography variant="subtitle2" sx={{ fontWeight: 700, mb: 0.5 }}>
                            Canonical Unified Diff (catalog-info.yaml)
                          </Typography>
                          <Paper
                            variant="outlined"
                            sx={{
                              p: 1.25,
                              bgcolor: '#0f172a',
                              color: '#e2e8f0',
                              maxHeight: 280,
                              overflow: 'auto',
                              fontFamily: 'monospace',
                              fontSize: '0.74rem',
                              whiteSpace: 'pre',
                            }}
                          >
                            {wizardResult.unifiedDiff}
                          </Paper>
                        </Box>
                      )}
                    </CardContent>
                  </Card>
                </Stack>
              </Grid>
            </Grid>
          </Box>
        )}
      </Paper>
    </Box>
  );
};
