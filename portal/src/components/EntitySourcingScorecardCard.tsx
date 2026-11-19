import React, { useCallback, useEffect, useState } from 'react';
import { Link as RouterLink, useNavigate } from 'react-router-dom';
import {
  AlertCircle,
  AlertTriangle,
  CheckCircle2,
  FileEdit,
  GitBranch,
  Loader2,
  RefreshCw,
  Shield,
  ShieldAlert,
} from 'lucide-react';
import { useEntity } from '@backstage/plugin-catalog-react';
import { fetchTechInsightsChecksResult } from '../api/pitsClient';
import {
  SOURCING_RESOURCE_TYPE_SET,
  SourcingEvaluation,
  TechInsightsCheckEntityResponse,
} from '../types';
import { StatusPill } from './StatusPill';
import { Badge } from './ui/badge';
import { Button } from './ui/button';
import { Card, CardContent } from './ui/card';
import { Alert, AlertDescription } from './ui/alert';
import { Separator } from './ui/separator';
import { PitsProgressBar, PitsTooltip } from './ui/pits-primitives';
import { cn } from '../lib/utils';

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
  core: '#10b981',
  development: '#0ea5e9',
  exploitable: '#f59e0b',
  nuisance: '#ef4444',
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
    <div className="pits-ui w-full overflow-x-auto">
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
            <path d="M0,0 L6,3 L0,6 Z" fill="#a855f7" />
          </marker>
        </defs>

        {/* Quadrant background zones */}
        <rect
          x={padL}
          y={padT}
          width={midX - padL}
          height={midY - padT}
          fill="rgba(14, 165, 233, 0.08)"
        />
        <rect
          x={midX}
          y={padT}
          width={padL + plotW - midX}
          height={midY - padT}
          fill="rgba(239, 68, 68, 0.08)"
        />
        <rect
          x={padL}
          y={midY}
          width={midX - padL}
          height={padT + plotH - midY}
          fill="rgba(16, 185, 129, 0.07)"
        />
        <rect
          x={midX}
          y={midY}
          width={padL + plotW - midX}
          height={padT + plotH - midY}
          fill="rgba(245, 158, 11, 0.08)"
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
        {[1, 2, 3, 4, 5].map(t => (
          <g key={t}>
            <line
              x1={toX(t)}
              y1={padT}
              x2={toX(t)}
              y2={padT + plotH}
              stroke={t === 3 ? 'hsl(var(--muted-foreground))' : 'hsl(var(--border))'}
              strokeWidth={t === 3 ? 1.3 : 0.85}
              strokeDasharray={t === 3 ? '4,3' : undefined}
            />
            <text
              x={toX(t)}
              y={padT + plotH + 14}
              textAnchor="middle"
              fontSize="10"
              fill="hsl(var(--muted-foreground))"
            >
              {t}.0
            </text>
            <line
              x1={padL}
              y1={toY(t)}
              x2={padL + plotW}
              y2={toY(t)}
              stroke={t === 3 ? 'hsl(var(--muted-foreground))' : 'hsl(var(--border))'}
              strokeWidth={t === 3 ? 1.3 : 0.85}
              strokeDasharray={t === 3 ? '4,3' : undefined}
            />
            <text
              x={padL - 8}
              y={toY(t) + 3}
              textAnchor="end"
              fontSize="10"
              fill="hsl(var(--muted-foreground))"
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
          stroke="hsl(var(--border))"
          strokeWidth={1.2}
        />

        {/* Quadrant headings */}
        <text x={padL + 8} y={padT + 14} fontSize="10" fontWeight="700" fill="#0ea5e9">
          LEVERAGE
        </text>
        <text
          x={padL + plotW - 8}
          y={padT + 14}
          textAnchor="end"
          fontSize="10"
          fontWeight="700"
          fill="#ef4444"
        >
          STRATEGIC
        </text>
        <text x={padL + 8} y={padT + plotH - 8} fontSize="10" fontWeight="700" fill="#10b981">
          NON-CRITICAL
        </text>
        <text
          x={padL + plotW - 8}
          y={padT + plotH - 8}
          textAnchor="end"
          fontSize="10"
          fontWeight="700"
          fill="#f59e0b"
        >
          BOTTLENECK
        </text>

        {/* Axis titles */}
        <text
          x={padL + plotW / 2}
          y={height - 4}
          textAnchor="middle"
          fontSize="10"
          fontWeight="600"
          fill="hsl(var(--muted-foreground))"
        >
          Supply Lock-In &amp; Scarcity Risk (X) · Amber Band = 2.7–3.3 Critical Zone
        </text>
        <text
          x={13}
          y={padT + plotH / 2}
          textAnchor="middle"
          fontSize="10"
          fontWeight="600"
          fill="hsl(var(--muted-foreground))"
          transform={`rotate(-90 13 ${padT + plotH / 2})`}
        >
          Business Impact (Y)
        </text>

        {/* Data points & 5-point knockout escalation vectors */}
        {points.map(pt => {
          const cx = toX(pt.finalXScore);
          const cy = toY(pt.finalYScore);
          const rx = toX(pt.rawXScore);
          const ry = toY(pt.rawYScore);
          const isHighlighted = highlightName === pt.name;
          const shifted =
            Math.abs(pt.finalXScore - pt.rawXScore) > 0.05 ||
            Math.abs(pt.finalYScore - pt.rawYScore) > 0.05;
          const color = WINDMILL_COLOR[pt.perceivedStatus] ?? '#71717a';

          return (
            <g
              key={pt.name}
              style={{ cursor: 'pointer' }}
              onClick={() => navigate(`/catalog/default/resource/${encodeURIComponent(pt.name)}`)}
            >
              {shifted && (
                <>
                  <circle cx={rx} cy={ry} r={3.5} fill="none" stroke="#a855f7" strokeWidth={1.3} />
                  <line
                    x1={rx}
                    y1={ry}
                    x2={cx}
                    y2={cy}
                    stroke="#a855f7"
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
                  stroke="#ef4444"
                  strokeWidth={1.8}
                  strokeDasharray="2,2"
                />
              )}
              <circle
                cx={cx}
                cy={cy}
                r={isHighlighted ? 7.5 : 6}
                fill={color}
                stroke={isHighlighted ? 'hsl(var(--foreground))' : 'hsl(var(--background))'}
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
                  fill="hsl(var(--foreground))"
                >
                  {pt.name}
                </text>
              )}
            </g>
          );
        })}
      </svg>

      <div className="mt-2 flex flex-wrap gap-3 text-[11px] text-muted-foreground">
        {Object.entries(WINDMILL_COLOR).map(([k, col]) => (
          <span key={k} className="inline-flex items-center gap-1.5 capitalize">
            <span
              className="inline-block h-2.5 w-2.5 rounded-full"
              style={{ backgroundColor: col }}
            />
            <span>Vendor: {k}</span>
          </span>
        ))}
        <span className="inline-flex items-center gap-1.5">
          <span className="inline-block h-2.5 w-2.5 rounded-full border-2 border-dashed border-red-500" />
          <span>Windmill Mismatch Ring</span>
        </span>
      </div>
    </div>
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
    void loadChecks();
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
        rawXScore:
          data.breakdown?.rawXScore ?? data.facts.breakdown?.rawXScore ?? data.facts.finalXScore,
        rawYScore:
          data.breakdown?.rawYScore ?? data.facts.breakdown?.rawYScore ?? data.facts.finalYScore,
        perceivedStatus: supplierPref.perceivedStatus ?? 'unknown',
        windmillMismatch: data.facts.windmillMismatch,
        knockoutTriggered: data.facts.knockoutTriggered,
        inCriticalZone: data.facts.inCriticalZone,
        reAuditRequired: data.facts.reAuditRequired,
      }
    : null;

  const dimRows = data
    ? [
        { label: 'Y1 Blast Radius (35%, Auto)', val: data.facts.breakdown.y1BlastRadius },
        { label: 'Y2 Core Competitiveness (25%)', val: data.facts.breakdown.y2CoreCompetitiveness },
        { label: 'Y3 FinOps TCO Ratio (25%, Auto)', val: data.facts.breakdown.y3TcoRatio },
        { label: 'Y4 Price Elasticity (15%)', val: data.facts.breakdown.y4PriceElasticity },
        { label: 'X1 Adapter Lock-In (35%, Auto)', val: data.facts.breakdown.x1LockIn },
        { label: 'X2 Market Scarcity (25%)', val: data.facts.breakdown.x2MarketScarcity },
        { label: 'X3 Customization Depth (25%)', val: data.facts.breakdown.x3CustomizationDepth },
        { label: 'X4 Compliance Fragility (15%)', val: data.facts.breakdown.x4ComplianceFragility },
      ]
    : [];

  return (
    <Card>
      <CardContent className="space-y-4 p-5">
        <div className="flex flex-col justify-between gap-3 sm:flex-row sm:items-center">
          <div className="space-y-1">
            <div className="flex flex-wrap items-center gap-2">
              <ShieldAlert
                className={cn(
                  'h-4 w-4',
                  data && data.redlineCount > 0
                    ? 'text-amber-600 dark:text-amber-400'
                    : 'text-foreground',
                )}
              />
              <h3 className="m-0 text-base font-semibold tracking-tight text-foreground">
                Tech Insights · Kraljic × Dutch Windmill Scorecard
              </h3>
              {data && (
                <StatusPill
                  label={
                    data.passedAll
                      ? 'All 3 Redline Checks Clear'
                      : `${data.redlineCount} Redline Alert${data.redlineCount > 1 ? 's' : ''}`
                  }
                  level={
                    data.facts.windmillMismatch ? 'error' : data.redlineCount > 0 ? 'warn' : 'ok'
                  }
                />
              )}
            </div>
            <div className="text-[11px] text-muted-foreground">
              FactRetriever:{' '}
              <code className="rounded bg-muted px-1 py-0.5 font-mono">
                sourcing-kraljic-windmill-retriever
              </code>{' '}
              · Real-time Component topology + blind-review + 8-dim 5pt knockout
            </div>
          </div>

          <div className="flex items-center gap-2">
            <Button size="xs" variant="outline" asChild>
              <RouterLink to={`/sourcing-audit?entity=${encodeURIComponent(name)}`}>
                <FileEdit className="h-3.5 w-3.5" />
                <span>Edit in Wizard</span>
              </RouterLink>
            </Button>
            <PitsTooltip content="Re-fetch Tech Insights facts & BooleanChecks">
              <Button
                size="icon"
                variant="outline"
                disabled={loading}
                onClick={() => void loadChecks()}
                aria-label="Refresh checks"
              >
                {loading ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <RefreshCw className="h-3.5 w-3.5" />
                )}
              </Button>
            </PitsTooltip>
          </div>
        </div>

        {error && (
          <Alert variant="destructive">
            <AlertCircle />
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        {data && (
          <div className="grid grid-cols-1 gap-5 md:grid-cols-2">
            {/* Left column: 3 BooleanCheck Redlines + Scorecard Matrix */}
            <div className="space-y-4">
              <div className="space-y-2">
                <div className="text-xs font-semibold text-foreground">
                  BooleanCheck Governance Redlines
                </div>
                {data.checks.map(chk => {
                  const isCrit = chk.triggered && chk.severity === 'critical';
                  const isWarn = chk.triggered && !isCrit;
                  return (
                    <div
                      key={chk.id}
                      className={cn(
                        'rounded-lg border border-solid p-3',
                        isCrit
                          ? 'border-red-500/40 bg-red-500/[0.06]'
                          : isWarn
                            ? 'border-amber-500/40 bg-amber-500/[0.06]'
                            : 'border-emerald-500/30 bg-emerald-500/[0.04]',
                      )}
                    >
                      <div className="flex items-center justify-between gap-2">
                        <div className="flex items-center gap-1.5">
                          {isCrit ? (
                            <AlertCircle className="h-3.5 w-3.5 text-red-600 dark:text-red-400" />
                          ) : isWarn ? (
                            <AlertTriangle className="h-3.5 w-3.5 text-amber-600 dark:text-amber-400" />
                          ) : (
                            <CheckCircle2 className="h-3.5 w-3.5 text-emerald-600 dark:text-emerald-400" />
                          )}
                          <span className="text-xs font-semibold text-foreground">{chk.name}</span>
                        </div>
                        <StatusPill
                          label={chk.triggered ? chk.severity.toUpperCase() : 'PASS'}
                          level={isCrit ? 'error' : isWarn ? 'warn' : 'ok'}
                        />
                      </div>
                      <p className="mb-0 mt-1 text-[11px] leading-relaxed text-muted-foreground">
                        {chk.message}
                      </p>
                    </div>
                  );
                })}
              </div>

              {pt && (
                <div className="rounded-lg border border-solid border-border p-3.5">
                  <div className="mb-2 text-xs font-semibold text-foreground">
                    Kraljic Coordinate &amp; Knockout Posture
                  </div>
                  <KraljicScatterMatrix points={[pt]} highlightName={name} height={265} />
                </div>
              )}
            </div>

            {/* Right column: 8-dim score bars, 4-Layer Lineage & Playbook */}
            <div className="space-y-4">
              <div className="flex flex-wrap items-center gap-1.5">
                <Badge variant="default" className="font-semibold">
                  Quadrant: {data.facts.quadrant.toUpperCase()}
                </Badge>
                <Badge variant="outline" className="font-mono">
                  Y = {data.facts.finalYScore.toFixed(2)} | X = {data.facts.finalXScore.toFixed(2)}
                </Badge>
                {data.facts.knockoutTriggered && (
                  <Badge variant="purple">
                    5-Pt Knockout: {data.facts.knockoutDimensions.join(', ')}
                  </Badge>
                )}
              </div>

              <div className="space-y-2">
                <div className="text-xs font-semibold text-foreground">
                  Eight-Dimension Scoring Vector
                </div>
                <div className="space-y-2">
                  {dimRows.map(d => (
                    <div key={d.label} className="space-y-1">
                      <div className="flex items-center justify-between text-[11px]">
                        <span className="font-medium text-foreground">{d.label}</span>
                        <span
                          className={cn(
                            'font-mono font-semibold',
                            d.val >= 5
                              ? 'text-red-600 dark:text-red-400'
                              : d.val >= 4
                                ? 'text-amber-600 dark:text-amber-400'
                                : 'text-foreground',
                          )}
                        >
                          {d.val.toFixed(2)} / 5.0
                        </span>
                      </div>
                      <PitsProgressBar
                        value={(d.val / 5) * 100}
                        tone={d.val >= 5 ? 'error' : d.val >= 3.5 ? 'warn' : 'default'}
                      />
                    </div>
                  ))}
                </div>
              </div>

              <Separator />

              <div className="space-y-2">
                <div className="flex items-center gap-1.5 text-xs font-semibold text-foreground">
                  <GitBranch className="h-3.5 w-3.5 text-muted-foreground" />
                  <span>
                    4-Layer Dependency Lineage ({data.facts.prodDependents} Production Components)
                  </span>
                </div>
                <div className="flex flex-wrap gap-1.5">
                  {data.lineage.length === 0 ? (
                    <span className="text-xs text-muted-foreground">
                      No Component dependsOn edges yet (Y1=1.0). Bind Components in the Sourcing
                      Wizard.
                    </span>
                  ) : (
                    data.lineage.map(lin => (
                      <RouterLink
                        key={lin.componentRef}
                        to={`/catalog/default/component/${encodeURIComponent(
                          lin.componentRef.replace(/^component:[^/]+\//i, ''),
                        )}`}
                      >
                        <Badge variant="outline" className="hover:bg-accent">
                          {lin.componentTitle} → {lin.domainRef.replace('domain:default/', '')}
                        </Badge>
                      </RouterLink>
                    ))
                  )}
                </div>
              </div>

              <div className="space-y-2">
                <div className="flex items-center gap-1.5 text-xs font-semibold text-foreground">
                  <Shield className="h-3.5 w-3.5 text-muted-foreground" />
                  <span>Mitigation &amp; Governance Playbook</span>
                </div>
                <div className="space-y-1.5">
                  {data.remediationPlaybook.map((item, idx) => (
                    <div
                      key={idx}
                      className="rounded-md border border-solid border-border bg-muted/40 px-3 py-2 text-xs leading-relaxed text-foreground"
                    >
                      {item}
                    </div>
                  ))}
                </div>
              </div>
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  );
};
