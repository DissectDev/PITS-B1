import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { Link as RouterLink, useParams } from 'react-router-dom';
import {
  AlertTriangle,
  ArrowLeft,
  ArrowRight,
  BarChart3,
  Cloud,
  CloudCog,
  Code2,
  Cpu,
  Database,
  Download,
  Filter,
  HardDrive,
  Loader2,
  MonitorSmartphone,
  Network,
  RefreshCw,
  Rocket,
  Router,
  Search,
  Server,
  ShieldCheck,
  Users,
} from 'lucide-react';
import { useApi } from '@backstage/core-plugin-api';
import { catalogApiRef } from '@backstage/plugin-catalog-react';
import { parseEntityRef, type Entity } from '@backstage/catalog-model';
import { fetchResourceTypes, triggerCatalogRefresh } from '../api/pitsClient';
import {
  RESOURCE_DOMAIN_ORDER,
  RESOURCE_TYPE_DIRECTORY_META,
  SOURCING_RESOURCE_TYPE_SET,
  type ObservationMode,
  type ResourceDomainGroup,
  type ResourceTypeSummary,
  type SummaryPill,
} from '../types';
import { inferStatusLevel, StatusPill } from '../components/StatusPill';
import { Badge } from '../components/ui/badge';
import { Button } from '../components/ui/button';
import { Card, CardContent, CardFooter } from '../components/ui/card';
import { Input } from '../components/ui/input';
import { Alert, AlertDescription } from '../components/ui/alert';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '../components/ui/table';
import { PageBody, PageHeader, PageShell } from '../components/ui/page-shell';
import { PitsTooltip } from '../components/ui/pits-primitives';
import { cn } from '../lib/utils';

function observationModeBadgeVariant(
  mode?: ObservationMode,
): { label: string; variant: 'purple' | 'info' | 'warn' | 'teal' } {
  switch (mode) {
    case 'policy-first':
      return { label: 'policy-first', variant: 'purple' };
    case 'dynamic-first':
      return { label: 'dynamic-first', variant: 'info' };
    case 'governance-event':
      return { label: 'governance-event', variant: 'warn' };
    default:
      return { label: mode || 'hybrid', variant: 'teal' };
  }
}

const HighlightPillStrip: React.FC<{ highlights?: SummaryPill[] }> = ({ highlights }) => {
  if (!highlights || highlights.length === 0) {
    return null;
  }
  return (
    <div className="mt-3 flex flex-wrap gap-1.5">
      {highlights.map(h => {
        const lvl = inferStatusLevel(h.status, h.status);
        return (
          <Badge key={h.key} variant={lvl} className="font-mono text-[11px]">
            {h.label}: {h.value}
          </Badge>
        );
      })}
    </div>
  );
};

export function getResourceTypeIcon(
  resourceType: string,
  className = 'h-4 w-4 text-foreground',
): React.ReactElement {
  switch (resourceType) {
    case 'baremetal-host':
      return <Server className={className} />;
    case 'npu-accelerator':
    case 'gpu-accelerator':
      return <Cpu className={className} />;
    case 'virtual-machine':
      return <MonitorSmartphone className={className} />;
    case 'edge-router':
      return <Router className={className} />;
    case 'pxe-boot-profile':
      return <Download className={className} />;
    case 'storage-pool':
      return <Database className={className} />;
    case 'san-volume':
      return <HardDrive className={className} />;
    case 'ai-storage-workspace':
      return <CloudCog className={className} />;
    case 'kubernetes-cluster':
      return <Network className={className} />;
    case 'cicd-pipeline':
      return <Rocket className={className} />;
    case 'auth-flow-bundle':
      return <ShieldCheck className={className} />;
    case 'vendor-saas':
      return <Cloud className={className} />;
    case 'commercial-sdk':
      return <Code2 className={className} />;
    case 'managed-infra':
      return <Router className={className} />;
    case 'outsourced-dev':
      return <Users className={className} />;
    default:
      return <HardDrive className={className} />;
  }
}

function extractEntityStatus(entity: Entity): { status: string; level?: string } {
  const annotations = entity.metadata.annotations ?? {};
  const labels = entity.metadata.labels ?? {};
  const spec = (entity.spec ?? {}) as Record<string, unknown>;

  const status =
    annotations['pits.io/status'] ||
    labels['pits.io/status'] ||
    (typeof spec.status === 'string' ? spec.status : '') ||
    (typeof spec.lifecycle === 'string' ? spec.lifecycle : 'Healthy');

  const level =
    annotations['pits.io/status-level'] ||
    labels['pits.io/status-level'] ||
    undefined;

  return { status, level };
}

function entityRefToCatalogUrl(targetRef: string): string {
  try {
    const parsed = parseEntityRef(targetRef, {
      defaultKind: 'resource',
      defaultNamespace: 'default',
    });
    return `/catalog/${encodeURIComponent(parsed.namespace)}/${encodeURIComponent(
      parsed.kind.toLowerCase(),
    )}/${encodeURIComponent(parsed.name)}`;
  } catch {
    return '/catalog';
  }
}

export const ResourceTypeDirectoryPage: React.FC = () => {
  const [summaries, setSummaries] = useState<ResourceTypeSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadSummaries = useCallback(async (forceRefresh = false) => {
    if (forceRefresh) {
      setRefreshing(true);
    } else {
      setLoading(true);
    }
    setError(null);
    try {
      const items = forceRefresh
        ? await triggerCatalogRefresh()
        : await fetchResourceTypes(false);
      setSummaries(items);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
  }, []);

  const silentRefresh = useCallback(async () => {
    try {
      const items = await fetchResourceTypes(false);
      if (items.length > 0) {
        setSummaries(items);
        setError(null);
      }
    } catch {
      // Swallow transient background revalidation errors without disturbing rendered state
    }
  }, []);

  useEffect(() => {
    void loadSummaries(false);
  }, [loadSummaries]);

  useEffect(() => {
    let cancelled = false;
    const run = async () => {
      if (cancelled) return;
      await silentRefresh();
    };
    const t1 = window.setTimeout(() => void run(), 3_500);
    const t2 = window.setTimeout(() => void run(), 8_000);
    const t3 = window.setTimeout(() => void run(), 16_000);
    const iv = window.setInterval(() => void run(), 30_000);
    return () => {
      cancelled = true;
      window.clearTimeout(t1);
      window.clearTimeout(t2);
      window.clearTimeout(t3);
      window.clearInterval(iv);
    };
  }, [silentRefresh]);

  const mergedByDomain = useMemo(() => {
    const summaryMap = new Map(summaries.map(s => [s.resourceType, s]));
    const groups = new Map<ResourceDomainGroup, ResourceTypeSummary[]>();
    for (const domain of RESOURCE_DOMAIN_ORDER) {
      groups.set(domain, []);
    }

    for (const [typeKey, meta] of Object.entries(RESOURCE_TYPE_DIRECTORY_META)) {
      const live = summaryMap.get(typeKey);
      const item: ResourceTypeSummary = live ?? {
        resourceType: typeKey,
        category: meta.domain,
        title: meta.shortTitle,
        description: meta.description,
        entityCount: 0,
        healthyCount: 0,
        warningCount: 0,
        actionIds: [],
      };
      groups.get(meta.domain)?.push(item);
      summaryMap.delete(typeKey);
    }

    for (const extra of summaryMap.values()) {
      const targetGroup: ResourceDomainGroup = RESOURCE_DOMAIN_ORDER.includes(
        extra.category as ResourceDomainGroup,
      )
        ? (extra.category as ResourceDomainGroup)
        : 'Control Planes';
      groups.get(targetGroup)?.push(extra);
    }

    return Array.from(groups.entries());
  }, [summaries]);

  const totals = useMemo(() => {
    let entities = 0;
    let healthy = 0;
    let warnings = 0;
    let drifted = 0;
    for (const s of summaries) {
      entities += s.entityCount;
      healthy += s.healthyCount;
      warnings += s.warningCount;
      drifted += s.driftedCount ?? 0;
    }
    return {
      types: Math.max(Object.keys(RESOURCE_TYPE_DIRECTORY_META).length, summaries.length),
      entities,
      healthy,
      warnings,
      drifted,
    };
  }, [summaries]);

  return (
    <PageShell>
      <PageHeader
        title="PITS Resource & Sourcing Directory"
        description="Resource-Type-Centric Entity Registry, Guarded Fleet Operations & Sourcing Governance (Backstage v1.56)"
        meta={[
          { label: 'System', value: 'pits-infrastructure · 3 Domains' },
          { label: 'Resource Types', value: String(totals.types) },
          { label: 'Catalog Entities', value: String(totals.entities || 33) },
        ]}
      />

      <PageBody className="space-y-6">
        {/* KPI Strip & Primary Action Bar */}
        <Card>
          <CardContent className="flex flex-col justify-between gap-4 p-5 lg:flex-row lg:items-center">
            <div className="grid grid-cols-2 gap-4 sm:grid-cols-5 sm:gap-6">
              <div>
                <div className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                  Resource Types
                </div>
                <div className="mt-0.5 text-2xl font-bold tracking-tight text-foreground">
                  {totals.types}
                </div>
              </div>
              <div className="sm:border-l sm:border-solid sm:border-border sm:pl-5">
                <div className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                  Enrolled Entities
                </div>
                <div className="mt-0.5 text-2xl font-bold tracking-tight text-foreground">
                  {loading ? '…' : totals.entities}
                </div>
              </div>
              <div className="sm:border-l sm:border-solid sm:border-border sm:pl-5">
                <div className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                  Healthy / Ready
                </div>
                <div className="mt-0.5 text-2xl font-bold tracking-tight text-emerald-600 dark:text-emerald-400">
                  {loading ? '…' : totals.healthy}
                </div>
              </div>
              <div className="sm:border-l sm:border-solid sm:border-border sm:pl-5">
                <div className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                  Warning / Gated
                </div>
                <div
                  className={cn(
                    'mt-0.5 text-2xl font-bold tracking-tight',
                    totals.warnings > 0
                      ? 'text-amber-600 dark:text-amber-400'
                      : 'text-foreground',
                  )}
                >
                  {loading ? '…' : totals.warnings}
                </div>
              </div>
              <div className="sm:border-l sm:border-solid sm:border-border sm:pl-5">
                <div className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                  Drifted
                </div>
                <div
                  className={cn(
                    'mt-0.5 text-2xl font-bold tracking-tight',
                    totals.drifted > 0 ? 'text-red-600 dark:text-red-400' : 'text-foreground',
                  )}
                >
                  {loading ? '…' : totals.drifted}
                </div>
              </div>
            </div>

            <div className="flex flex-wrap items-center gap-2">
              <Button
                variant="outline"
                size="sm"
                disabled={refreshing}
                onClick={() => void loadSummaries(true)}
              >
                {refreshing ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <RefreshCw className="h-3.5 w-3.5" />
                )}
                <span>{refreshing ? 'Refreshing Fleet...' : 'Refresh Live Fleet'}</span>
              </Button>
              <Button variant="outline" size="sm" asChild>
                <a href="/api/pits/v1/catalog/export" download="catalog-info.yaml">
                  <Download className="h-3.5 w-3.5" />
                  <span>Export YAML</span>
                </a>
              </Button>
              <Button variant="outline" size="sm" asChild>
                <RouterLink to="/sourcing-audit">
                  <BarChart3 className="h-3.5 w-3.5" />
                  <span>Sourcing Audit &amp; Wizard</span>
                </RouterLink>
              </Button>
              <Button size="sm" asChild>
                <RouterLink to="/catalog?filters%5Bkind%5D=resource">
                  <span>Entity Registry Index</span>
                  <ArrowRight className="h-3.5 w-3.5" />
                </RouterLink>
              </Button>
            </div>
          </CardContent>
        </Card>

        {error && (
          <Alert variant="warning">
            <AlertTriangle />
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        {loading && summaries.length === 0 ? (
          <div className="flex items-center justify-center py-16">
            <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
          </div>
        ) : (
          <div className="space-y-8">
            {mergedByDomain.map(([domainName, domainItems]) => (
              <section key={domainName} className="space-y-3.5">
                <div className="flex items-center gap-2.5">
                  <h2 className="m-0 text-base font-semibold tracking-tight text-foreground">
                    {domainName}
                  </h2>
                  <Badge variant="muted">
                    {domainItems.length} resource {domainItems.length === 1 ? 'type' : 'types'}
                  </Badge>
                </div>

                <div className="grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-3">
                  {domainItems.map(item => {
                    const meta = RESOURCE_TYPE_DIRECTORY_META[item.resourceType];
                    return (
                      <Card
                        key={item.resourceType}
                        className="flex h-full flex-col transition-colors hover:border-foreground/30"
                      >
                        <CardContent className="flex-1 p-5">
                          <div className="flex items-start justify-between gap-3">
                            <div className="flex min-w-0 items-center gap-3">
                              <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border border-solid border-border bg-muted/60">
                                {getResourceTypeIcon(item.resourceType)}
                              </div>
                              <div className="min-w-0">
                                <div className="truncate text-sm font-semibold leading-snug text-foreground">
                                  {meta?.shortTitle || item.title}
                                </div>
                                <div className="truncate font-mono text-[11px] text-muted-foreground">
                                  spec.type: {item.resourceType}
                                </div>
                              </div>
                            </div>

                            <Badge variant="default" className="shrink-0 font-semibold">
                              {item.entityCount} {item.entityCount === 1 ? 'Entity' : 'Entities'}
                            </Badge>
                          </div>

                          <p className="mb-0 mt-3 min-h-[40px] text-xs leading-relaxed text-muted-foreground">
                            {item.description || meta?.description}
                          </p>

                          <div className="mt-3.5 flex flex-wrap items-center gap-1.5">
                            {item.observationMode &&
                              (() => {
                                const badge = observationModeBadgeVariant(item.observationMode);
                                return (
                                  <PitsTooltip
                                    content={`Refresh cadence: ${item.cadenceTier || 'standard-60s'}`}
                                  >
                                    <Badge variant={badge.variant} className="font-mono text-[10.5px]">
                                      {item.cadenceTier
                                        ? `${badge.label} · ${item.cadenceTier}`
                                        : badge.label}
                                    </Badge>
                                  </PitsTooltip>
                                );
                              })()}
                            <StatusPill label={`${item.healthyCount} Healthy`} level="ok" />
                            {item.warningCount > 0 && (
                              <StatusPill
                                label={`${item.warningCount} Warning / Pending`}
                                level="warn"
                              />
                            )}
                            {(item.driftedCount ?? 0) > 0 && (
                              <StatusPill label={`${item.driftedCount} Drifted`} level="error" />
                            )}
                          </div>

                          <HighlightPillStrip highlights={item.highlights} />

                          {item.actionIds.length > 0 && (
                            <div className="mt-4">
                              <div className="mb-1.5 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                                Contextual Entity Actions
                              </div>
                              <div className="flex flex-wrap gap-1">
                                {item.actionIds.slice(0, 5).map(actId => (
                                  <Badge
                                    key={actId}
                                    variant="muted"
                                    className="px-1.5 py-0 font-mono text-[10.5px]"
                                  >
                                    {actId}
                                  </Badge>
                                ))}
                                {item.actionIds.length > 5 && (
                                  <Badge variant="muted" className="px-1.5 py-0 text-[10.5px]">
                                    +{item.actionIds.length - 5} more
                                  </Badge>
                                )}
                              </div>
                            </div>
                          )}
                        </CardContent>

                        <CardFooter className="bg-muted/20 px-5 py-3">
                          <Button variant="ghost" size="xs" asChild>
                            <RouterLink
                              to={`/catalog?filters%5Bkind%5D=resource&filters%5Btype%5D=${encodeURIComponent(
                                item.resourceType,
                              )}`}
                            >
                              <Filter className="h-3 w-3" />
                              <span>Catalog Facet</span>
                            </RouterLink>
                          </Button>
                          <Button size="xs" asChild>
                            <RouterLink
                              to={`/resource-types/${encodeURIComponent(item.resourceType)}`}
                            >
                              <span>Explore Entities</span>
                              <ArrowRight className="h-3 w-3" />
                            </RouterLink>
                          </Button>
                        </CardFooter>
                      </Card>
                    );
                  })}
                </div>
              </section>
            ))}
          </div>
        )}
      </PageBody>
    </PageShell>
  );
};

export const ResourceTypeEntitiesPage: React.FC<{ fixedResourceType?: string }> = ({
  fixedResourceType,
}) => {
  const params = useParams<{ type?: string }>();
  const resourceType = fixedResourceType || params.type || 'baremetal-host';
  const catalogApi = useApi(catalogApiRef);

  const [entities, setEntities] = useState<Entity[]>([]);
  const [typeSummary, setTypeSummary] = useState<ResourceTypeSummary | undefined>(undefined);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [searchTerm, setSearchTerm] = useState('');
  const [error, setError] = useState<string | null>(null);

  const meta = RESOURCE_TYPE_DIRECTORY_META[resourceType];

  const loadEntities = useCallback(
    async (forceRefresh = false) => {
      if (forceRefresh) {
        setRefreshing(true);
      } else {
        setLoading(true);
      }
      setError(null);
      try {
        if (forceRefresh) {
          await triggerCatalogRefresh();
        }
        const [queryRes, typesRes] = await Promise.all([
          catalogApi.queryEntities({
            filter: {
              kind: 'resource',
              'spec.type': resourceType,
            },
          }),
          fetchResourceTypes(false).catch(() => [] as ResourceTypeSummary[]),
        ]);
        setEntities(queryRes.items);
        setTypeSummary(typesRes.find(t => t.resourceType === resourceType));
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
        setRefreshing(false);
      }
    },
    [catalogApi, resourceType],
  );

  const silentRefresh = useCallback(async () => {
    try {
      const [queryRes, typesRes] = await Promise.all([
        catalogApi.queryEntities({
          filter: {
            kind: 'resource',
            'spec.type': resourceType,
          },
        }),
        fetchResourceTypes(false).catch(() => [] as ResourceTypeSummary[]),
      ]);
      setEntities(queryRes.items);
      const matched = typesRes.find(t => t.resourceType === resourceType);
      if (matched) {
        setTypeSummary(matched);
      }
      setError(null);
    } catch {
      // Swallow transient background revalidation errors without disturbing rendered state
    }
  }, [catalogApi, resourceType]);

  useEffect(() => {
    void loadEntities(false);
  }, [loadEntities]);

  useEffect(() => {
    let cancelled = false;
    const run = async () => {
      if (cancelled) return;
      await silentRefresh();
    };
    const t1 = window.setTimeout(() => void run(), 3_500);
    const t2 = window.setTimeout(() => void run(), 8_000);
    const t3 = window.setTimeout(() => void run(), 16_000);
    const iv = window.setInterval(() => void run(), 30_000);
    return () => {
      cancelled = true;
      window.clearTimeout(t1);
      window.clearTimeout(t2);
      window.clearTimeout(t3);
      window.clearInterval(iv);
    };
  }, [silentRefresh]);

  const filteredEntities = useMemo(() => {
    const q = searchTerm.trim().toLowerCase();
    if (!q) return entities;
    return entities.filter(ent => {
      const hay = [
        ent.metadata.name,
        ent.metadata.title,
        ent.metadata.description,
        ...(ent.metadata.tags ?? []),
        ...Object.values(ent.metadata.labels ?? {}),
        ...Object.values(ent.metadata.annotations ?? {}),
      ]
        .filter(Boolean)
        .join(' ')
        .toLowerCase();
      return hay.includes(q);
    });
  }, [entities, searchTerm]);

  return (
    <PageShell>
      <PageHeader
        breadcrumb={{ label: 'Resource Types', to: '/resource-types' }}
        title={meta?.fullTitle || `Resource Type: ${resourceType}`}
        description={
          typeSummary?.description || meta?.description || 'Aggregated infrastructure entities'
        }
        badge={
          <Badge variant="muted" className="font-mono">
            {resourceType}
          </Badge>
        }
        meta={[
          { label: 'Domain', value: meta?.domain || typeSummary?.category || 'Infrastructure' },
          { label: 'Entities', value: String(entities.length) },
        ]}
      />

      <PageBody className="space-y-5">
        {/* Quick resource-type switcher strip */}
        <div className="flex flex-wrap items-center gap-1.5">
          <Button variant="outline" size="xs" asChild>
            <RouterLink to="/resource-types">
              <ArrowLeft className="h-3 w-3" />
              <span>All Types</span>
            </RouterLink>
          </Button>
          <span className="mx-1 h-4 w-px bg-border" />
          {Object.values(RESOURCE_TYPE_DIRECTORY_META).map(m => {
            const active = m.type === resourceType;
            return (
              <RouterLink
                key={m.type}
                to={`/resource-types/${encodeURIComponent(m.type)}`}
                className={cn(
                  'inline-flex items-center rounded-md border border-solid px-2.5 py-1 text-xs transition-colors',
                  active
                    ? 'border-primary bg-primary font-medium text-primary-foreground'
                    : 'border-border bg-background text-muted-foreground hover:bg-accent hover:text-foreground',
                )}
              >
                {m.shortTitle}
              </RouterLink>
            );
          })}
        </div>

        {/* Search & status toolbar */}
        <Card>
          <CardContent className="flex flex-col justify-between gap-3 p-4 lg:flex-row lg:items-center">
            <div className="relative w-full lg:max-w-md">
              <Search className="pointer-events-none absolute left-3 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={searchTerm}
                onChange={e => setSearchTerm(e.target.value)}
                placeholder={`Filter ${resourceType} entities by name, model, IP, WWID, PCI ID, or status...`}
                className="pl-8"
              />
            </div>

            <div className="flex flex-wrap items-center gap-2">
              {typeSummary && (
                <>
                  {typeSummary.observationMode &&
                    (() => {
                      const badge = observationModeBadgeVariant(typeSummary.observationMode);
                      return (
                        <Badge variant={badge.variant} className="font-mono text-[11px]">
                          {typeSummary.cadenceTier
                            ? `${badge.label} · ${typeSummary.cadenceTier}`
                            : badge.label}
                        </Badge>
                      );
                    })()}
                  <StatusPill label={`${typeSummary.healthyCount} Healthy`} level="ok" />
                  {typeSummary.warningCount > 0 && (
                    <StatusPill
                      label={`${typeSummary.warningCount} Warning / Gated`}
                      level="warn"
                    />
                  )}
                  {(typeSummary.driftedCount ?? 0) > 0 && (
                    <StatusPill label={`${typeSummary.driftedCount} Drifted`} level="error" />
                  )}
                </>
              )}

              <Button
                size="sm"
                variant="outline"
                disabled={refreshing}
                onClick={() => void loadEntities(true)}
              >
                {refreshing ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <RefreshCw className="h-3.5 w-3.5" />
                )}
                <span>{refreshing ? 'Refreshing...' : 'Refresh Live'}</span>
              </Button>

              {SOURCING_RESOURCE_TYPE_SET.has(resourceType) && (
                <Button size="sm" asChild>
                  <RouterLink to="/sourcing-audit?tab=wizard">
                    <BarChart3 className="h-3.5 w-3.5" />
                    <span>Sourcing Audit &amp; Wizard</span>
                  </RouterLink>
                </Button>
              )}

              <Button size="sm" variant="outline" asChild>
                <RouterLink
                  to={`/catalog?filters%5Bkind%5D=resource&filters%5Btype%5D=${encodeURIComponent(
                    resourceType,
                  )}`}
                >
                  <Filter className="h-3.5 w-3.5" />
                  <span>View in Entity Registry</span>
                </RouterLink>
              </Button>
            </div>
          </CardContent>
        </Card>

        {error && (
          <Alert variant="destructive">
            <AlertTriangle />
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        {loading ? (
          <div className="flex items-center justify-center py-16">
            <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
          </div>
        ) : (
          <Card className="overflow-hidden">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Entity Instance</TableHead>
                  <TableHead>Observed Status</TableHead>
                  <TableHead>Summary &amp; Hardware / Spec Identity</TableHead>
                  <TableHead>Topology Relations</TableHead>
                  <TableHead className="text-right">Landing Page</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {filteredEntities.length === 0 ? (
                  <TableRow>
                    <TableCell colSpan={5} className="py-10 text-center text-muted-foreground">
                      No entities matched for resource type{' '}
                      <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">
                        {resourceType}
                      </code>
                      .
                    </TableCell>
                  </TableRow>
                ) : (
                  filteredEntities.map(ent => {
                    const ns = ent.metadata.namespace || 'default';
                    const entityUrl = `/catalog/${encodeURIComponent(ns)}/resource/${encodeURIComponent(
                      ent.metadata.name,
                    )}`;
                    const { status, level } = extractEntityStatus(ent);
                    const relations = (ent.relations ?? []).filter(
                      r =>
                        r.type === 'dependsOn' ||
                        r.type === 'partOf' ||
                        r.type === 'hasPart',
                    );

                    return (
                      <TableRow key={ent.metadata.uid || ent.metadata.name}>
                        <TableCell className="min-w-[210px]">
                          <RouterLink
                            to={entityUrl}
                            className="font-mono text-[13px] font-semibold text-foreground hover:underline"
                          >
                            {ent.metadata.name}
                          </RouterLink>
                          {ent.metadata.title && ent.metadata.title !== ent.metadata.name && (
                            <div className="mt-0.5 text-xs text-muted-foreground">
                              {ent.metadata.title}
                            </div>
                          )}
                        </TableCell>

                        <TableCell className="whitespace-nowrap">
                          <StatusPill label={status} level={level} />
                        </TableCell>

                        <TableCell className="max-w-[460px]">
                          <div className="text-[13px] text-foreground">
                            {ent.metadata.description || '—'}
                          </div>
                          {ent.metadata.tags && ent.metadata.tags.length > 0 && (
                            <div className="mt-1.5 flex flex-wrap gap-1">
                              {ent.metadata.tags.slice(0, 5).map(t => (
                                <Badge key={t} variant="muted" className="px-1.5 py-0 text-[10.5px]">
                                  {t}
                                </Badge>
                              ))}
                            </div>
                          )}
                        </TableCell>

                        <TableCell className="max-w-[300px]">
                          <div className="flex flex-wrap gap-1">
                            {relations.slice(0, 4).map((rel, idx) => {
                              const shortTarget = rel.targetRef.replace(
                                /^(resource|system):default\//i,
                                '',
                              );
                              return (
                                <RouterLink
                                  key={`${rel.type}-${rel.targetRef}-${idx}`}
                                  to={entityRefToCatalogUrl(rel.targetRef)}
                                  className="inline-flex items-center rounded border border-solid border-border bg-muted/40 px-1.5 py-0.5 font-mono text-[10.5px] text-foreground transition-colors hover:bg-accent"
                                >
                                  {rel.type}: {shortTarget}
                                </RouterLink>
                              );
                            })}
                            {relations.length > 4 && (
                              <Badge variant="muted" className="px-1.5 py-0 text-[10.5px]">
                                +{relations.length - 4}
                              </Badge>
                            )}
                          </div>
                        </TableCell>

                        <TableCell className="whitespace-nowrap text-right">
                          <Button size="xs" asChild>
                            <RouterLink to={entityUrl}>
                              <span>Inspect &amp; Act</span>
                              <ArrowRight className="h-3 w-3" />
                            </RouterLink>
                          </Button>
                        </TableCell>
                      </TableRow>
                    );
                  })
                )}
              </TableBody>
            </Table>
          </Card>
        )}
      </PageBody>
    </PageShell>
  );
};

export const ResourceTypesRouteDispatcher: React.FC = () => {
  const params = useParams<{ '*': string }>();
  const subPath = (params['*'] ?? '').replace(/^\/+|\/+$/g, '');
  if (subPath) {
    return <ResourceTypeEntitiesPage fixedResourceType={subPath} />;
  }
  return <ResourceTypeDirectoryPage />;
};
