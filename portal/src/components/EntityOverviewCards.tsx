import React, { useMemo, useState } from 'react';
import { Link as RouterLink } from 'react-router-dom';
import {
  AlertTriangle,
  Loader2,
  Lock,
  Network,
  PlayCircle,
  Radio,
  RefreshCw,
  Shield,
} from 'lucide-react';
import { useEntity } from '@backstage/plugin-catalog-react';
import { usePitsEntityDetail } from '../hooks/usePitsEntityDetail';
import {
  RESOURCE_TYPE_DIRECTORY_META,
  type AttributeSection,
  type DynamicSignal,
  type EntityActionSpec,
  type ObservationMode,
  type SummaryPill,
} from '../types';
import { FormattedAttributeValue, inferStatusLevel, StatusPill } from './StatusPill';
import { ActionExecutionDialog } from './ActionExecutionDialog';
import { Badge } from './ui/badge';
import { Button } from './ui/button';
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from './ui/card';
import { Alert, AlertDescription } from './ui/alert';
import { Table, TableBody, TableCell, TableRow } from './ui/table';
import { PitsTooltip } from './ui/pits-primitives';
import { cn } from '../lib/utils';

function observationModeLabel(mode?: ObservationMode): string {
  switch (mode) {
    case 'policy-first':
      return 'Policy-First';
    case 'dynamic-first':
      return 'Dynamic-First';
    case 'governance-event':
      return 'Governance-Event';
    default:
      return 'Hybrid';
  }
}

function signalToneClasses(status?: string): { box: string; value: string } {
  const lvl = inferStatusLevel(status, status);
  if (lvl === 'error') {
    return {
      box: 'border-red-500/35 bg-red-500/[0.07]',
      value: 'text-red-700 dark:text-red-400',
    };
  }
  if (lvl === 'warn') {
    return {
      box: 'border-amber-500/35 bg-amber-500/[0.07]',
      value: 'text-amber-700 dark:text-amber-400',
    };
  }
  if (lvl === 'ok') {
    return {
      box: 'border-emerald-500/30 bg-emerald-500/[0.07]',
      value: 'text-emerald-700 dark:text-emerald-400',
    };
  }
  return {
    box: 'border-sky-500/30 bg-sky-500/[0.06]',
    value: 'text-sky-700 dark:text-sky-400',
  };
}

function trendGlyph(trend?: string): string {
  switch (trend) {
    case 'up':
      return '▲';
    case 'down':
      return '▼';
    case 'changed':
      return '◆';
    case 'alert':
      return '!';
    default:
      return '';
  }
}

export const EntityStatusSummaryCard: React.FC = () => {
  const { entity } = useEntity();
  const namespace = entity.metadata.namespace || 'default';
  const name = entity.metadata.name;
  const { detail, error, loading, refreshing, refreshLive } = usePitsEntityDetail(namespace, name);

  const specType = String((entity.spec as Record<string, unknown> | undefined)?.type ?? '');
  const resolvedType = detail?.resourceType || specType || 'resource';
  const typeMeta = RESOURCE_TYPE_DIRECTORY_META[resolvedType];

  if (loading && !detail) {
    return (
      <Card>
        <CardContent className="flex items-center gap-3 p-6">
          <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
          <span className="text-xs text-muted-foreground">
            Synthesizing live entity state for <strong className="text-foreground">{name}</strong>
            ...
          </span>
        </CardContent>
      </Card>
    );
  }

  return (
    <Card>
      <CardHeader className="flex flex-col gap-3 border-b border-solid border-border sm:flex-row sm:items-center sm:justify-between">
        <div className="flex flex-wrap items-center gap-2">
          <CardTitle className="text-base">
            {entity.metadata.title || entity.metadata.name}
          </CardTitle>
          <RouterLink to={`/resource-types/${encodeURIComponent(resolvedType)}`}>
            <Badge variant="outline" className="font-mono hover:bg-accent">
              {resolvedType}
            </Badge>
          </RouterLink>
          {typeMeta && <Badge variant="muted">{typeMeta.domain}</Badge>}
          {detail?.summary?.observationMode && (
            <PitsTooltip
              content={`Refresh cadence: ${detail.summary.cadenceTier || 'standard-60s'}`}
            >
              <Badge variant="outline" className="font-mono">
                {observationModeLabel(detail.summary.observationMode)} ·{' '}
                {detail.summary.cadenceTier}
              </Badge>
            </PitsTooltip>
          )}
          {detail && <StatusPill label={detail.status} level={detail.statusLevel} size="medium" />}
        </div>

        <Button
          variant="outline"
          size="sm"
          disabled={refreshing}
          onClick={() => void refreshLive()}
          className="shrink-0"
        >
          {refreshing ? (
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
          ) : (
            <RefreshCw className="h-3.5 w-3.5" />
          )}
          <span>{refreshing ? 'Refreshing...' : 'Refresh Live Telemetry'}</span>
        </Button>
      </CardHeader>

      <CardContent className="space-y-4 p-5">
        {error && (
          <Alert variant="warning">
            <AlertTriangle />
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        {detail?.summary?.driftDetected && (
          <Alert variant={detail.statusLevel === 'error' ? 'destructive' : 'warning'}>
            <AlertTriangle />
            <AlertDescription>
              <strong>Policy / Runtime Drift Detected:</strong>{' '}
              {detail.summary.driftSummary ||
                'Observed state diverges from declared static contract.'}
            </AlertDescription>
          </Alert>
        )}

        <div className="flex flex-col justify-between gap-3 rounded-lg border border-solid border-border bg-muted/40 p-4 sm:flex-row sm:items-center">
          <div className="min-w-0 space-y-1">
            <div className="text-sm font-semibold text-foreground">
              {detail?.summary?.headline ||
                detail?.summaryLine ||
                entity.metadata.description ||
                'PITS Infrastructure Resource Entity'}
            </div>
            {entity.metadata.description &&
              detail?.summaryLine &&
              entity.metadata.description !== detail.summaryLine && (
                <div className="text-xs text-muted-foreground">{entity.metadata.description}</div>
              )}
          </div>

          <Badge
            variant={detail?.liveConnected ? 'ok' : 'muted'}
            className="shrink-0 self-start sm:self-center"
          >
            <Radio className="h-3 w-3" />
            <span>{detail?.liveConnected ? 'Live Telemetry Connected' : 'Baseline / Cached State'}</span>
          </Badge>
        </div>

        {detail?.summary &&
          (detail.summary.staticPolicy.length > 0 || detail.summary.dynamicSignals.length > 0) && (
            <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
              {detail.summary.staticPolicy.length > 0 && (
                <div className="rounded-lg border border-solid border-border p-3.5">
                  <div className="mb-2.5 flex items-center justify-between gap-2">
                    <span className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                      Static Policy &amp; Contract Band
                    </span>
                    <Badge variant={detail.summary.driftDetected ? 'warn' : 'ok'}>
                      {detail.summary.driftDetected ? 'Drift Guard Alert' : 'Compliant'}
                    </Badge>
                  </div>
                  <div className="flex flex-wrap gap-2">
                    {detail.summary.staticPolicy.map((p: SummaryPill) => {
                      const tone = signalToneClasses(p.status);
                      return (
                        <div
                          key={p.key}
                          className={cn(
                            'rounded-md border border-solid px-2.5 py-1.5',
                            tone.box,
                          )}
                        >
                          <div className="text-[10px] font-semibold text-muted-foreground">
                            {p.label}
                          </div>
                          <div className={cn('font-mono text-xs font-bold', tone.value)}>
                            {p.value}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </div>
              )}

              {detail.summary.dynamicSignals.length > 0 && (
                <div className="rounded-lg border border-solid border-border p-3.5">
                  <div className="mb-2.5 flex items-center justify-between gap-2">
                    <span className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                      Live Dynamic Signals &amp; Occupancy Band
                    </span>
                    {detail.summary.lastChangedAt && (
                      <span className="text-[11px] text-muted-foreground">
                        Changed: {new Date(detail.summary.lastChangedAt).toLocaleTimeString()}
                      </span>
                    )}
                  </div>
                  <div className="flex flex-wrap gap-2">
                    {detail.summary.dynamicSignals.map((s: DynamicSignal) => {
                      const tone = signalToneClasses(s.status);
                      const glyph = trendGlyph(s.trend);
                      return (
                        <div
                          key={s.key}
                          className={cn(
                            'min-w-[120px] rounded-md border border-solid px-2.5 py-1.5',
                            tone.box,
                          )}
                        >
                          <div className="flex items-center justify-between gap-2">
                            <span className="text-[10px] font-semibold text-muted-foreground">
                              {s.label}
                            </span>
                            {typeof s.observedAgeSecs === 'number' && s.observedAgeSecs > 0 && (
                              <span className="font-mono text-[10px] text-muted-foreground">
                                {s.observedAgeSecs}s
                              </span>
                            )}
                          </div>
                          <div className="mt-0.5 flex flex-wrap items-baseline gap-1.5">
                            <span className={cn('font-mono text-xs font-bold', tone.value)}>
                              {glyph ? `${glyph} ` : ''}
                              {s.value}
                            </span>
                            {s.delta && (
                              <Badge variant="muted" className="px-1 py-0 font-mono text-[10px]">
                                Δ {s.delta}
                              </Badge>
                            )}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </div>
              )}
            </div>
          )}

        <div className="grid grid-cols-1 gap-4 pt-1 sm:grid-cols-2 lg:grid-cols-4">
          <div className="space-y-1">
            <div className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
              Entity Ref
            </div>
            <FormattedAttributeValue value={`resource:${namespace}/${name}`} monospace />
          </div>
          <div className="space-y-1">
            <div className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
              System
            </div>
            <div>
              <RouterLink to="/catalog?filters%5Bkind%5D=resource">
                <Badge variant="outline" className="hover:bg-accent">
                  <Network className="h-3 w-3" />
                  <span>
                    {String(
                      (entity.spec as Record<string, unknown> | undefined)?.system ??
                        'pits-infrastructure',
                    )}
                  </span>
                </Badge>
              </RouterLink>
            </div>
          </div>
          <div className="space-y-1">
            <div className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
              Owner &amp; Lifecycle
            </div>
            <div className="text-xs font-medium text-foreground">
              {String(
                (entity.spec as Record<string, unknown> | undefined)?.owner ??
                  'group:default/operators',
              )}
              {' · '}
              {String(
                (entity.spec as Record<string, unknown> | undefined)?.lifecycle ?? 'production',
              )}
            </div>
          </div>
          <div className="space-y-1">
            <div className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
              Last Synthesized
            </div>
            <div className="text-xs font-medium text-foreground">
              {detail?.refreshedAt
                ? new Date(detail.refreshedAt).toLocaleTimeString()
                : 'Just now'}
            </div>
          </div>
        </div>

        {entity.metadata.tags && entity.metadata.tags.length > 0 && (
          <div className="flex flex-wrap gap-1.5 pt-1">
            {entity.metadata.tags.map(tag => (
              <Badge key={tag} variant="muted">
                #{tag}
              </Badge>
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  );
};

export const EntityAttributesCard: React.FC = () => {
  const { entity } = useEntity();
  const namespace = entity.metadata.namespace || 'default';
  const name = entity.metadata.name;
  const { detail, loading } = usePitsEntityDetail(namespace, name);

  if (loading && !detail) {
    return (
      <Card>
        <CardHeader className="border-b border-solid border-border">
          <CardTitle>Desired vs Observed Attributes</CardTitle>
        </CardHeader>
        <CardContent className="flex justify-center p-8">
          <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
        </CardContent>
      </Card>
    );
  }

  const sections: AttributeSection[] = detail?.attributeSections ?? [];

  return (
    <Card>
      <CardHeader className="border-b border-solid border-border">
        <CardTitle>Structured Entity Attributes</CardTitle>
        <CardDescription>
          Desired contract specification reconciled against live hardware, kernel, and
          control-plane state
        </CardDescription>
      </CardHeader>
      <CardContent className="p-5">
        {sections.length === 0 ? (
          <div className="text-xs text-muted-foreground">
            No structured attribute sections reported for this entity.
          </div>
        ) : (
          <div className="space-y-4">
            {sections.map(section => (
              <div
                key={section.id}
                className="overflow-hidden rounded-lg border border-solid border-border"
              >
                <div className="border-b border-solid border-border bg-muted/50 px-3.5 py-2 text-xs font-semibold text-foreground">
                  {section.title}
                </div>
                <Table>
                  <TableBody>
                    {section.items.map(item => (
                      <TableRow key={item.key}>
                        <TableCell className="w-[36%] font-medium text-muted-foreground">
                          <PitsTooltip content={item.key}>
                            <span className="cursor-help border-b border-dashed border-border">
                              {item.label}
                            </span>
                          </PitsTooltip>
                        </TableCell>
                        <TableCell>
                          <div className="flex flex-wrap items-center justify-between gap-2">
                            <FormattedAttributeValue
                              value={item.value}
                              monospace={item.monospace}
                              itemKey={item.key}
                            />
                            {item.status && (
                              <StatusPill
                                label={item.status.toUpperCase()}
                                level={item.status}
                              />
                            )}
                          </div>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  );
};

export const EntityContextualActionsCard: React.FC = () => {
  const { entity } = useEntity();
  const namespace = entity.metadata.namespace || 'default';
  const name = entity.metadata.name;
  const { detail, loading, refreshLive } = usePitsEntityDetail(namespace, name);
  const [selectedAction, setSelectedAction] = useState<EntityActionSpec | null>(null);

  const groupedActions = useMemo(() => {
    const groups = new Map<string, EntityActionSpec[]>();
    for (const act of detail?.actions ?? []) {
      const cat = act.category || 'Operations';
      const list = groups.get(cat) ?? [];
      list.push(act);
      groups.set(cat, list);
    }
    return Array.from(groups.entries());
  }, [detail?.actions]);

  if (loading && !detail) {
    return (
      <Card>
        <CardHeader className="border-b border-solid border-border">
          <CardTitle>Contextual Entity Actions</CardTitle>
        </CardHeader>
        <CardContent className="flex justify-center p-8">
          <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
        </CardContent>
      </Card>
    );
  }

  return (
    <>
      <Card>
        <CardHeader className="border-b border-solid border-border">
          <CardTitle>Contextual Entity Actions</CardTitle>
          <CardDescription>
            Precondition-gated operations scoped strictly to {name}
          </CardDescription>
        </CardHeader>
        <CardContent className="p-5">
          {groupedActions.length === 0 ? (
            <div className="text-xs text-muted-foreground">
              No contextual actions registered for this entity.
            </div>
          ) : (
            <div className="space-y-5">
              {groupedActions.map(([category, actions]) => (
                <div key={category} className="space-y-2.5">
                  <div className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                    {category}
                  </div>
                  <div className="space-y-2">
                    {actions.map(act => {
                      const isDisabled = !act.enabled;
                      return (
                        <div
                          key={act.id}
                          className={cn(
                            'space-y-2 rounded-lg border border-solid border-border p-3.5 transition-colors',
                            isDisabled ? 'bg-muted/40 opacity-85' : 'bg-card hover:border-foreground/25',
                          )}
                        >
                          <div className="flex items-start justify-between gap-3">
                            <div className="min-w-0 space-y-1">
                              <div className="flex flex-wrap items-center gap-1.5">
                                <span className="text-[13px] font-semibold text-foreground">
                                  {act.title}
                                </span>
                                <Badge variant="muted" className="font-mono text-[10.5px]">
                                  {act.id}
                                </Badge>
                                {act.dangerous && (
                                  <Badge variant="warn" className="text-[10.5px]">
                                    <Shield className="h-3 w-3" />
                                    <span>Guarded</span>
                                  </Badge>
                                )}
                                {act.supportsDryRun && (
                                  <Badge variant="info" className="text-[10.5px]">
                                    Dry-Run
                                  </Badge>
                                )}
                              </div>
                              <p className="m-0 text-xs leading-relaxed text-muted-foreground">
                                {act.description}
                              </p>
                            </div>

                            <PitsTooltip
                              content={
                                isDisabled
                                  ? act.disabledReason || 'Precondition gate not satisfied'
                                  : `Open ${act.title}`
                              }
                            >
                              <span className="shrink-0">
                                <Button
                                  size="xs"
                                  variant={act.dangerous ? 'warning' : 'default'}
                                  disabled={isDisabled}
                                  onClick={() => setSelectedAction(act)}
                                >
                                  {isDisabled ? (
                                    <Lock className="h-3 w-3" />
                                  ) : (
                                    <PlayCircle className="h-3 w-3" />
                                  )}
                                  <span>{isDisabled ? 'Gated' : 'Run'}</span>
                                </Button>
                              </span>
                            </PitsTooltip>
                          </div>

                          {isDisabled && act.disabledReason && (
                            <Alert variant="warning" className="py-2">
                              <Lock />
                              <AlertDescription>
                                <strong>Precondition Blocked:</strong> {act.disabledReason}
                              </AlertDescription>
                            </Alert>
                          )}
                        </div>
                      );
                    })}
                  </div>
                </div>
              ))}
            </div>
          )}
        </CardContent>
      </Card>

      <ActionExecutionDialog
        open={Boolean(selectedAction)}
        onClose={() => setSelectedAction(null)}
        namespace={namespace}
        entityName={name}
        resourceType={detail?.resourceType || 'resource'}
        action={selectedAction}
        manifests={detail?.manifests ?? {}}
        onActionCompleted={refreshLive}
      />
    </>
  );
};
