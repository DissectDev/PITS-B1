import React, { useCallback, useEffect, useState } from 'react';
import { Link as RouterLink, useNavigate } from 'react-router-dom';
import {
  Alert,
  Box,
  Button,
  Card,
  CardContent,
  Chip,
  Divider,
  Grid,
  IconButton,
  LinearProgress,
  Paper,
  Stack,
  Tooltip,
  Typography,
} from '@mui/material';
import AccountTreeIcon from '@mui/icons-material/AccountTree';
import CheckCircleOutlineIcon from '@mui/icons-material/CheckCircleOutline';
import EditNoteIcon from '@mui/icons-material/EditNote';
import ErrorOutlineIcon from '@mui/icons-material/ErrorOutline';
import GppMaybeIcon from '@mui/icons-material/GppMaybe';
import RefreshIcon from '@mui/icons-material/Refresh';
import ShieldOutlinedIcon from '@mui/icons-material/ShieldOutlined';
import WarningAmberIcon from '@mui/icons-material/WarningAmber';
import { useEntity } from '@backstage/plugin-catalog-react';
import { fetchTechInsightsChecksResult } from '../api/pitsClient';
import {
  SOURCING_RESOURCE_TYPE_SET,
  SourcingEvaluation,
  TechInsightsCheckEntityResponse,
} from '../types';
import { StatusPill } from './StatusPill';

export interface MatrixPoint {
  name: string;
  vendorName: string;
  quadrant: string;
  finalXScore: number;
  finalYScore: number;
  rawXScore: number;
  rawYScore: number;
  perceivedStatus: string;
  windmillMismatch: boolean;
  knockoutTriggered: boolean;
  inCriticalZone: boolean;
  reAuditRequired: boolean;
}

const WINDMILL_COLOR: Record<string, string> = {
  core: '#2e7d32',
  development: '#0288d1',
  exploitable: '#ed6c02',
  nuisance: '#d32f2f',
};

export const KraljicScatterMatrix: React.FC<{
  points: MatrixPoint[];
  highlightName?: string;
  height?: number;
}> = ({ points, highlightName, height = 300 }) => {
  const navigate = useNavigate();
  const width = 460;
  const padL = 44;
  const padR = 20;
  const padT = 22;
  const padB = 36;
  const plotW = width - padL - padR;
  const plotH = height - padT - padB;

  const toX = (score: number) =>
    padL + ((Math.max(1, Math.min(5, score)) - 1) / 4) * plotW;
  const toY = (score: number) =>
    padT + ((5 - Math.max(1, Math.min(5, score))) / 4) * plotH;

  const midX = toX(3.0);
  const midY = toY(3.0);
  const critXStart = toX(2.7);
  const critXEnd = toX(3.3);
  const critYTop = toY(3.3);
  const critYBot = toY(2.7);

  return (
    <Box sx={{ width: '100%', overflowX: 'auto' }}>
      <svg
        viewBox={`0 0 ${width} ${height}`}
        style={{ width: '100%', maxWidth: width, height: 'auto', display: 'block' }}
        role="img"
        aria-label="Kraljic Dual-Axis × Dutch Windmill Matrix"
      >
        <defs>
          <marker
            id="knockout-arrow"
            markerWidth="6"
            markerHeight="6"
            refX="5"
            refY="3"
            orient="auto"
          >
            <path d="M0,0 L6,3 L0,6 Z" fill="#9c27b0" />
          </marker>
        </defs>

        {/* Quadrant background zones */}
        <rect
          x={padL}
          y={padT}
          width={midX - padL}
          height={midY - padT}
          fill="rgba(2, 136, 209, 0.06)"
        />
        <rect
          x={midX}
          y={padT}
          width={padL + plotW - midX}
          height={midY - padT}
          fill="rgba(211, 47, 47, 0.07)"
        />
        <rect
          x={padL}
          y={midY}
          width={midX - padL}
          height={padT + plotH - midY}
          fill="rgba(46, 125, 50, 0.05)"
        />
        <rect
          x={midX}
          y={midY}
          width={padL + plotW - midX}
          height={padT + plotH - midY}
          fill="rgba(237, 108, 2, 0.07)"
        />

        {/* [2.70, 3.30] Critical Stress-Test Band */}
        <rect
          x={critXStart}
          y={padT}
          width={critXEnd - critXStart}
          height={plotH}
          fill="rgba(245, 158, 11, 0.14)"
        />
        <rect
          x={padL}
          y={critYTop}
          width={plotW}
          height={critYBot - critYTop}
          fill="rgba(245, 158, 11, 0.14)"
        />

        {/* Grid ticks */}
        {[1, 2, 3, 4, 5].map((t) => (
          <g key={t}>
            <line
              x1={toX(t)}
              y1={padT}
              x2={toX(t)}
              y2={padT + plotH}
              stroke={t === 3 ? '#64748b' : 'rgba(148,163,184,0.32)'}
              strokeWidth={t === 3 ? 1.4 : 0.8}
              strokeDasharray={t === 3 ? '4,3' : undefined}
            />
            <text
              x={toX(t)}
              y={padT + plotH + 14}
              textAnchor="middle"
              fontSize="10"
              fill="#64748b"
            >
              {t}.0
            </text>
            <line
              x1={padL}
              y1={toY(t)}
              x2={padL + plotW}
              y2={toY(t)}
              stroke={t === 3 ? '#64748b' : 'rgba(148,163,184,0.32)'}
              strokeWidth={t === 3 ? 1.4 : 0.8}
              strokeDasharray={t === 3 ? '4,3' : undefined}
            />
            <text
              x={padL - 8}
              y={toY(t) + 3}
              textAnchor="end"
              fontSize="10"
              fill="#64748b"
            >
              {t}.0
            </text>
          </g>
        ))}

        {/* Outer frame */}
        <rect
          x={padL}
          y={padT}
          width={plotW}
          height={plotH}
          fill="none"
          stroke="rgba(100,116,139,0.5)"
          strokeWidth={1.2}
        />

        {/* Quadrant headings */}
        <text x={padL + 8} y={padT + 14} fontSize="10.5" fontWeight="700" fill="#0288d1">
          LEVERAGE
        </text>
        <text
          x={padL + plotW - 8}
          y={padT + 14}
          textAnchor="end"
          fontSize="10.5"
          fontWeight="700"
          fill="#d32f2f"
        >
          STRATEGIC
        </text>
        <text x={padL + 8} y={padT + plotH - 8} fontSize="10.5" fontWeight="700" fill="#2e7d32">
          NON-CRITICAL
        </text>
        <text
          x={padL + plotW - 8}
          y={padT + plotH - 8}
          textAnchor="end"
          fontSize="10.5"
          fontWeight="700"
          fill="#ed6c02"
        >
          BOTTLENECK
        </text>

        {/* Axis titles */}
        <text
          x={padL + plotW / 2}
          y={height - 4}
          textAnchor="middle"
          fontSize="10.5"
          fontWeight="600"
          fill="#475569"
        >
          Supply Lock-In &amp; Scarcity Risk (X) · Amber Band = 2.7–3.3 Critical Zone
        </text>
        <text
          x={13}
          y={padT + plotH / 2}
          textAnchor="middle"
          fontSize="10.5"
          fontWeight="600"
          fill="#475569"
          transform={`rotate(-90 13 ${padT + plotH / 2})`}
        >
          Business Impact (Y)
        </text>

        {/* Data points & 5-point knockout escalation vectors */}
        {points.map((pt) => {
          const cx = toX(pt.finalXScore);
          const cy = toY(pt.finalYScore);
          const rx = toX(pt.rawXScore);
          const ry = toY(pt.rawYScore);
          const isHighlighted = highlightName === pt.name;
          const shifted =
            Math.abs(pt.finalXScore - pt.rawXScore) > 0.05 ||
            Math.abs(pt.finalYScore - pt.rawYScore) > 0.05;
          const color = WINDMILL_COLOR[pt.perceivedStatus] ?? '#475569';

          return (
            <g
              key={pt.name}
              style={{ cursor: 'pointer' }}
              onClick={() => navigate(`/catalog/default/resource/${encodeURIComponent(pt.name)}`)}
            >
              {shifted && (
                <>
                  <circle cx={rx} cy={ry} r={3.5} fill="none" stroke="#9c27b0" strokeWidth={1.3} />
                  <line
                    x1={rx}
                    y1={ry}
                    x2={cx}
                    y2={cy}
                    stroke="#9c27b0"
                    strokeWidth={1.4}
                    strokeDasharray="3,2"
                    markerEnd="url(#knockout-arrow)"
                  />
                </>
              )}
              {pt.windmillMismatch && (
                <circle
                  cx={cx}
                  cy={cy}
                  r={isHighlighted ? 13 : 10}
                  fill="none"
                  stroke="#d32f2f"
                  strokeWidth={1.8}
                  strokeDasharray="2,2"
                />
              )}
              <circle
                cx={cx}
                cy={cy}
                r={isHighlighted ? 7.5 : 6}
                fill={color}
                stroke={isHighlighted ? '#111827' : '#ffffff'}
                strokeWidth={isHighlighted ? 2.2 : 1.5}
              />
              <title>
                {`${pt.name} (${pt.vendorName})\nQuadrant: ${pt.quadrant} | X=${pt.finalXScore.toFixed(
                  2,
                )}, Y=${pt.finalYScore.toFixed(2)}\nWindmill: ${pt.perceivedStatus}${
                  pt.windmillMismatch ? ' [MISMATCH]' : ''
                }`}
              </title>
              {(isHighlighted || points.length <= 8) && (
                <text
                  x={cx + (cx > padL + plotW * 0.65 ? -10 : 10)}
                  y={cy - 8}
                  textAnchor={cx > padL + plotW * 0.65 ? 'end' : 'start'}
                  fontSize="9.5"
                  fontWeight={isHighlighted ? '700' : '600'}
                  fill="#1e293b"
                >
                  {pt.name}
                </text>
              )}
            </g>
          );
        })}
      </svg>

      <Stack direction="row" flexWrap="wrap" gap={1.5} sx={{ mt: 0.75, fontSize: '0.75rem' }}>
        {Object.entries(WINDMILL_COLOR).map(([k, col]) => (
          <Box key={k} sx={{ display: 'inline-flex', alignItems: 'center', gap: 0.5 }}>
            <Box sx={{ width: 9, height: 9, borderRadius: '50%', bgcolor: col }} />
            <Typography variant="caption" sx={{ textTransform: 'capitalize' }}>
              Vendor: {k}
            </Typography>
          </Box>
        ))}
        <Box sx={{ display: 'inline-flex', alignItems: 'center', gap: 0.5 }}>
          <Box
            sx={{
              width: 10,
              height: 10,
              borderRadius: '50%',
              border: '2px dashed #d32f2f',
            }}
          />
          <Typography variant="caption">Windmill Mismatch Ring</Typography>
        </Box>
      </Stack>
    </Box>
  );
};

export function toMatrixPoint(ev: SourcingEvaluation): MatrixPoint {
  return {
    name: ev.record.name,
    vendorName: ev.record.audit.contractMeta.vendorName,
    quadrant: ev.facts.quadrant,
    finalXScore: ev.facts.finalXScore,
    finalYScore: ev.facts.finalYScore,
    rawXScore: ev.facts.breakdown.rawXScore,
    rawYScore: ev.facts.breakdown.rawYScore,
    perceivedStatus: ev.record.audit.supplierPreference.perceivedStatus,
    windmillMismatch: ev.facts.windmillMismatch,
    knockoutTriggered: ev.facts.knockoutTriggered,
    inCriticalZone: ev.facts.inCriticalZone,
    reAuditRequired: ev.facts.reAuditRequired,
  };
}

export const EntitySourcingScorecardCard: React.FC = () => {
  const { entity } = useEntity();
  const specType = String(entity.spec?.type ?? '').toLowerCase();
  const isSourcing =
    entity.kind === 'Resource' && SOURCING_RESOURCE_TYPE_SET.has(specType);

  const namespace = entity.metadata.namespace || 'default';
  const name = entity.metadata.name;

  const [data, setData] = useState<TechInsightsCheckEntityResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadChecks = useCallback(async () => {
    if (!isSourcing) return;
    setLoading(true);
    setError(null);
    try {
      const res = await fetchTechInsightsChecksResult(namespace, 'resource', name);
      setData(res);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [isSourcing, namespace, name]);

  useEffect(() => {
    loadChecks();
  }, [loadChecks]);

  if (!isSourcing) {
    return null;
  }

  const audit: any = (entity.spec as any)?.sourcingAudit ?? {};
  const contractMeta = audit.contractMeta ?? {};
  const supplierPref = audit.supplierPreference ?? {};

  const pt: MatrixPoint | null = data
    ? {
        name,
        vendorName: contractMeta.vendorName ?? name,
        quadrant: data.facts.quadrant,
        finalXScore: data.facts.finalXScore,
        finalYScore: data.facts.finalYScore,
        rawXScore: data.breakdown?.rawXScore ?? data.facts.breakdown?.rawXScore ?? data.facts.finalXScore,
        rawYScore: data.breakdown?.rawYScore ?? data.facts.breakdown?.rawYScore ?? data.facts.finalYScore,
        perceivedStatus: supplierPref.perceivedStatus ?? 'unknown',
        windmillMismatch: data.facts.windmillMismatch,
        knockoutTriggered: data.facts.knockoutTriggered,
        inCriticalZone: data.facts.inCriticalZone,
        reAuditRequired: data.facts.reAuditRequired,
      }
    : null;

  const dimRows = data
    ? [
        { label: 'Y1 Blast Radius (35%, Auto)', val: data.facts.breakdown.y1BlastRadius, weight: '35%' },
        { label: 'Y2 Core Competitiveness (25%)', val: data.facts.breakdown.y2CoreCompetitiveness, weight: '25%' },
        { label: 'Y3 FinOps TCO Ratio (25%, Auto)', val: data.facts.breakdown.y3TcoRatio, weight: '25%' },
        { label: 'Y4 Price Elasticity (15%)', val: data.facts.breakdown.y4PriceElasticity, weight: '15%' },
        { label: 'X1 Adapter Lock-In (35%, Auto)', val: data.facts.breakdown.x1LockIn, weight: '35%' },
        { label: 'X2 Market Scarcity (25%)', val: data.facts.breakdown.x2MarketScarcity, weight: '25%' },
        { label: 'X3 Customization Depth (25%)', val: data.facts.breakdown.x3CustomizationDepth, weight: '25%' },
        { label: 'X4 Compliance Fragility (15%)', val: data.facts.breakdown.x4ComplianceFragility, weight: '15%' },
      ]
    : [];

  return (
    <Card variant="outlined" sx={{ borderRadius: 2 }}>
      <CardContent>
        <Stack
          direction={{ xs: 'column', sm: 'row' }}
          justifyContent="space-between"
          alignItems={{ xs: 'flex-start', sm: 'center' }}
          spacing={1.5}
          sx={{ mb: 1.5 }}
        >
          <Box>
            <Stack direction="row" alignItems="center" spacing={1}>
              <GppMaybeIcon color={data && data.redlineCount > 0 ? 'warning' : 'primary'} />
              <Typography variant="h6" sx={{ fontWeight: 700 }}>
                Tech Insights · Kraljic × Dutch Windmill Scorecard
              </Typography>
              {data && (
                <StatusPill
                  label={
                    data.passedAll
                      ? 'All 3 Redline Checks Clear'
                      : `${data.redlineCount} Redline Alert${data.redlineCount > 1 ? 's' : ''}`
                  }
                  level={data.facts.windmillMismatch ? 'error' : data.redlineCount > 0 ? 'warn' : 'ok'}
                />
              )}
            </Stack>
            <Typography variant="caption" color="text.secondary">
              FactRetriever: <code>sourcing-kraljic-windmill-retriever</code> · Real-time Component topology + blind-review + 8-dim 5pt knockout
            </Typography>
          </Box>
          <Stack direction="row" spacing={1} alignItems="center">
            <Button
              size="small"
              variant="outlined"
              component={RouterLink}
              to={`/sourcing-audit?entity=${encodeURIComponent(name)}`}
              startIcon={<EditNoteIcon />}
            >
              Edit in Wizard
            </Button>
            <Tooltip title="Re-fetch Tech Insights facts & BooleanChecks">
              <IconButton size="small" onClick={loadChecks} disabled={loading}>
                <RefreshIcon fontSize="small" />
              </IconButton>
            </Tooltip>
          </Stack>
        </Stack>

        {loading && <LinearProgress sx={{ mb: 1.5 }} />}
        {error && (
          <Alert severity="error" sx={{ mb: 1.5 }}>
            {error}
          </Alert>
        )}

        {data && (
          <Grid container spacing={2.5}>
            {/* Left column: 3 BooleanCheck Redlines + Scorecard Matrix */}
            <Grid item xs={12} md={6}>
              <Typography variant="subtitle2" sx={{ fontWeight: 700, mb: 1 }}>
                BooleanCheck Governance Redlines
              </Typography>
              <Stack spacing={1} sx={{ mb: 2 }}>
                {data.checks.map((chk) => {
                  const isCrit = chk.triggered && chk.severity === 'critical';
                  const isWarn = chk.triggered && !isCrit;
                  return (
                    <Paper
                      key={chk.id}
                      variant="outlined"
                      sx={{
                        p: 1.25,
                        borderRadius: 1.5,
                        borderColor: isCrit
                          ? 'error.main'
                          : isWarn
                            ? 'warning.main'
                            : 'rgba(46,125,50,0.35)',
                        bgcolor: isCrit
                          ? 'rgba(211,47,47,0.05)'
                          : isWarn
                            ? 'rgba(237,108,2,0.05)'
                            : 'rgba(46,125,50,0.03)',
                      }}
                    >
                      <Stack direction="row" justifyContent="space-between" alignItems="center" spacing={1}>
                        <Stack direction="row" alignItems="center" spacing={0.75}>
                          {isCrit ? (
                            <ErrorOutlineIcon fontSize="small" color="error" />
                          ) : isWarn ? (
                            <WarningAmberIcon fontSize="small" color="warning" />
                          ) : (
                            <CheckCircleOutlineIcon fontSize="small" color="success" />
                          )}
                          <Typography variant="body2" sx={{ fontWeight: 700 }}>
                            {chk.name}
                          </Typography>
                        </Stack>
                        <StatusPill
                          label={chk.triggered ? chk.severity.toUpperCase() : 'PASS'}
                          level={isCrit ? 'error' : isWarn ? 'warn' : 'ok'}
                        />
                      </Stack>
                      <Typography variant="caption" color="text.secondary" sx={{ display: 'block', mt: 0.5 }}>
                        {chk.message}
                      </Typography>
                    </Paper>
                  );
                })}
              </Stack>

              {pt && (
                <Paper variant="outlined" sx={{ p: 1.5, borderRadius: 1.5 }}>
                  <Typography variant="subtitle2" sx={{ fontWeight: 700, mb: 0.5 }}>
                    Kraljic Coordinate &amp; Knockout Posture
                  </Typography>
                  <KraljicScatterMatrix points={[pt]} highlightName={name} height={265} />
                </Paper>
              )}
            </Grid>

            {/* Right column: 8-dim score bars, 4-Layer Lineage & Playbook */}
            <Grid item xs={12} md={6}>
              <Stack direction="row" spacing={1} flexWrap="wrap" useFlexGap sx={{ mb: 1.5 }}>
                <Chip
                  size="small"
                  color="primary"
                  label={`Quadrant: ${data.facts.quadrant.toUpperCase()}`}
                  sx={{ fontWeight: 700 }}
                />
                <Chip
                  size="small"
                  variant="outlined"
                  label={`Y = ${data.facts.finalYScore.toFixed(2)} | X = ${data.facts.finalXScore.toFixed(2)}`}
                />
                {data.facts.knockoutTriggered && (
                  <Chip
                    size="small"
                    color="secondary"
                    label={`5-Pt Knockout: ${data.facts.knockoutDimensions.join(', ')}`}
                  />
                )}
              </Stack>

              <Typography variant="subtitle2" sx={{ fontWeight: 700, mb: 0.75 }}>
                Eight-Dimension Scoring Vector
              </Typography>
              <Stack spacing={0.75} sx={{ mb: 2 }}>
                {dimRows.map((d) => (
                  <Box key={d.label}>
                    <Stack direction="row" justifyContent="space-between">
                      <Typography variant="caption" sx={{ fontWeight: 600 }}>
                        {d.label}
                      </Typography>
                      <Typography
                        variant="caption"
                        sx={{
                          fontWeight: 700,
                          color: d.val >= 5 ? 'error.main' : d.val >= 4 ? 'warning.main' : 'text.primary',
                        }}
                      >
                        {d.val.toFixed(2)} / 5.0
                      </Typography>
                    </Stack>
                    <LinearProgress
                      variant="determinate"
                      value={Math.min(100, (d.val / 5) * 100)}
                      color={d.val >= 5 ? 'error' : d.val >= 3.5 ? 'warning' : 'primary'}
                      sx={{ height: 5, borderRadius: 3 }}
                    />
                  </Box>
                ))}
              </Stack>

              <Divider sx={{ my: 1.25 }} />

              <Stack direction="row" alignItems="center" spacing={0.75} sx={{ mb: 0.75 }}>
                <AccountTreeIcon fontSize="small" color="action" />
                <Typography variant="subtitle2" sx={{ fontWeight: 700 }}>
                  4-Layer Dependency Lineage ({data.facts.prodDependents} Production Components)
                </Typography>
              </Stack>
              <Stack direction="row" flexWrap="wrap" gap={0.75} sx={{ mb: 1.5 }}>
                {data.lineage.length === 0 ? (
                  <Typography variant="caption" color="text.secondary">
                    No Component dependsOn edges yet (Y1=1.0). Bind Components in the Sourcing Wizard.
                  </Typography>
                ) : (
                  data.lineage.map((lin) => (
                    <Chip
                      key={lin.componentRef}
                      size="small"
                      variant="outlined"
                      component={RouterLink}
                      clickable
                      to={`/catalog/default/component/${encodeURIComponent(
                        lin.componentRef.replace(/^component:[^/]+\//i, ''),
                      )}`}
                      label={`${lin.componentTitle} → ${lin.domainRef.replace('domain:default/', '')}`}
                    />
                  ))
                )}
              </Stack>

              <Stack direction="row" alignItems="center" spacing={0.75} sx={{ mb: 0.75 }}>
                <ShieldOutlinedIcon fontSize="small" color="primary" />
                <Typography variant="subtitle2" sx={{ fontWeight: 700 }}>
                  Mitigation &amp; Governance Playbook
                </Typography>
              </Stack>
              <Stack spacing={0.75}>
                {data.remediationPlaybook.map((item, idx) => (
                  <Paper
                    key={idx}
                    variant="outlined"
                    sx={{ p: 1, borderRadius: 1, bgcolor: 'background.default' }}
                  >
                    <Typography variant="caption" sx={{ display: 'block', lineHeight: 1.45 }}>
                      {item}
                    </Typography>
                  </Paper>
                ))}
              </Stack>
            </Grid>
          </Grid>
        )}
      </CardContent>
    </Card>
  );
};
