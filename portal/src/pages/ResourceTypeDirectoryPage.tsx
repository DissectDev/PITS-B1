import React, { useCallback, useEffect, useMemo, useState } from 'react';
import {
  Alert,
  Box,
  Button,
  Card,
  CardActions,
  CardContent,
  Chip,
  CircularProgress,
  Divider,
  Grid,
  InputAdornment,
  Paper,
  Stack,
  Table,
  TableBody,
  TableCell,
  TableContainer,
  TableHead,
  TableRow,
  TextField,
  Typography,
} from '@mui/material';
import DnsOutlinedIcon from '@mui/icons-material/DnsOutlined';
import MemoryOutlinedIcon from '@mui/icons-material/MemoryOutlined';
import DesktopWindowsOutlinedIcon from '@mui/icons-material/DesktopWindowsOutlined';
import RouterOutlinedIcon from '@mui/icons-material/RouterOutlined';
import SystemUpdateAltOutlinedIcon from '@mui/icons-material/SystemUpdateAltOutlined';
import StorageOutlinedIcon from '@mui/icons-material/StorageOutlined';
import ViewInArOutlinedIcon from '@mui/icons-material/ViewInArOutlined';
import HubOutlinedIcon from '@mui/icons-material/HubOutlined';
import AssessmentOutlinedIcon from '@mui/icons-material/AssessmentOutlined';
import CloudOutlinedIcon from '@mui/icons-material/CloudOutlined';
import CodeOutlinedIcon from '@mui/icons-material/CodeOutlined';
import GroupsOutlinedIcon from '@mui/icons-material/GroupsOutlined';
import RefreshIcon from '@mui/icons-material/Refresh';
import DownloadIcon from '@mui/icons-material/Download';
import ArrowForwardIcon from '@mui/icons-material/ArrowForward';
import FilterListIcon from '@mui/icons-material/FilterList';
import SearchIcon from '@mui/icons-material/Search';
import ArrowBackIcon from '@mui/icons-material/ArrowBack';
import { Link as RouterLink, useParams } from 'react-router-dom';
import { Content, Header, HeaderLabel, Page } from '@backstage/core-components';
import { useApi } from '@backstage/core-plugin-api';
import { catalogApiRef } from '@backstage/plugin-catalog-react';
import { parseEntityRef, type Entity } from '@backstage/catalog-model';
import { fetchResourceTypes, triggerCatalogRefresh } from '../api/pitsClient';
import {
  RESOURCE_DOMAIN_ORDER,
  RESOURCE_TYPE_DIRECTORY_META,
  SOURCING_RESOURCE_TYPE_SET,
  type ResourceDomainGroup,
  type ResourceTypeSummary,
} from '../types';
import { StatusPill } from '../components/StatusPill';

export function getResourceTypeIcon(resourceType: string): React.ReactElement {
  switch (resourceType) {
    case 'baremetal-host':
      return <DnsOutlinedIcon color="primary" />;
    case 'npu-accelerator':
      return <MemoryOutlinedIcon color="primary" />;
    case 'virtual-machine':
      return <DesktopWindowsOutlinedIcon color="primary" />;
    case 'edge-router':
      return <RouterOutlinedIcon color="primary" />;
    case 'pxe-boot-profile':
      return <SystemUpdateAltOutlinedIcon color="primary" />;
    case 'storage-pool':
      return <StorageOutlinedIcon color="primary" />;
    case 'san-volume':
      return <ViewInArOutlinedIcon color="primary" />;
    case 'kubernetes-cluster':
      return <HubOutlinedIcon color="primary" />;
    case 'vendor-saas':
      return <CloudOutlinedIcon color="primary" />;
    case 'commercial-sdk':
      return <CodeOutlinedIcon color="primary" />;
    case 'managed-infra':
      return <RouterOutlinedIcon color="primary" />;
    case 'outsourced-dev':
      return <GroupsOutlinedIcon color="primary" />;
    default:
      return <ViewInArOutlinedIcon color="primary" />;
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
      const targetGroup: ResourceDomainGroup =
        extra.category === 'Sourcing Governance'
          ? 'Sourcing Governance'
          : 'Compute & Accelerators';
      groups.get(targetGroup)?.push(extra);
    }

    return Array.from(groups.entries());
  }, [summaries]);

  const totals = useMemo(() => {
    let entities = 0;
    let healthy = 0;
    let warnings = 0;
    for (const s of summaries) {
      entities += s.entityCount;
      healthy += s.healthyCount;
      warnings += s.warningCount;
    }
    return {
      types: Object.keys(RESOURCE_TYPE_DIRECTORY_META).length,
      entities,
      healthy,
      warnings,
    };
  }, [summaries]);

  return (
    <Page themeId="home">
      <Header
        title="PITS Resource & Sourcing Directory"
        subtitle="Resource-Type-Centric Entity Registry, Guarded Fleet Operations & Sourcing Governance (Backstage v1.56)"
      >
        <HeaderLabel label="System" value="pits-infrastructure · 3 Domains" />
        <HeaderLabel label="Resource Types" value={String(totals.types)} />
        <HeaderLabel label="Catalog Entities" value={String(totals.entities || 31)} />
      </Header>

      <Content>
        <Stack spacing={3}>
          <Paper
            variant="outlined"
            sx={{
              p: 2.25,
              display: 'flex',
              flexDirection: { xs: 'column', md: 'row' },
              justifyContent: 'space-between',
              alignItems: { xs: 'flex-start', md: 'center' },
              gap: 2,
            }}
          >
            <Stack direction="row" spacing={3} flexWrap="wrap" useFlexGap>
              <Box>
                <Typography variant="caption" color="text.secondary" sx={{ fontWeight: 700 }}>
                  RESOURCE TYPES
                </Typography>
                <Typography variant="h5" sx={{ fontWeight: 800 }}>
                  {totals.types}
                </Typography>
              </Box>
              <Divider orientation="vertical" flexItem />
              <Box>
                <Typography variant="caption" color="text.secondary" sx={{ fontWeight: 700 }}>
                  ENROLLED ENTITIES
                </Typography>
                <Typography variant="h5" sx={{ fontWeight: 800 }}>
                  {loading ? '…' : totals.entities}
                </Typography>
              </Box>
              <Divider orientation="vertical" flexItem />
              <Box>
                <Typography variant="caption" color="text.secondary" sx={{ fontWeight: 700 }}>
                  HEALTHY / READY
                </Typography>
                <Typography variant="h5" sx={{ fontWeight: 800, color: 'success.main' }}>
                  {loading ? '…' : totals.healthy}
                </Typography>
              </Box>
              <Divider orientation="vertical" flexItem />
              <Box>
                <Typography variant="caption" color="text.secondary" sx={{ fontWeight: 700 }}>
                  WARNING / GATED
                </Typography>
                <Typography
                  variant="h5"
                  sx={{
                    fontWeight: 800,
                    color: totals.warnings > 0 ? 'warning.main' : 'text.primary',
                  }}
                >
                  {loading ? '…' : totals.warnings}
                </Typography>
              </Box>
            </Stack>

            <Stack direction="row" spacing={1.25} flexWrap="wrap" useFlexGap>
              <Button
                variant="outlined"
                startIcon={refreshing ? <CircularProgress size={16} /> : <RefreshIcon />}
                disabled={refreshing}
                onClick={() => void loadSummaries(true)}
              >
                {refreshing ? 'Refreshing Fleet...' : 'Refresh Live Fleet'}
              </Button>
              <Button
                variant="outlined"
                component="a"
                href="/api/pits/v1/catalog/export"
                download="catalog-info.yaml"
                startIcon={<DownloadIcon />}
              >
                Export YAML
              </Button>
              <Button
                variant="outlined"
                component={RouterLink}
                to="/sourcing-audit"
                startIcon={<AssessmentOutlinedIcon />}
              >
                Sourcing Audit &amp; Wizard
              </Button>
              <Button
                variant="contained"
                component={RouterLink}
                to="/catalog?filters%5Bkind%5D=resource"
                endIcon={<ArrowForwardIcon />}
              >
                Entity Registry Index
              </Button>
            </Stack>
          </Paper>

          {error && <Alert severity="warning">{error}</Alert>}

          {loading && summaries.length === 0 ? (
            <Box sx={{ display: 'flex', justifyContent: 'center', py: 6 }}>
              <CircularProgress />
            </Box>
          ) : (
            mergedByDomain.map(([domainName, domainItems]) => (
              <Box key={domainName}>
                <Stack
                  direction="row"
                  spacing={1.25}
                  alignItems="center"
                  sx={{ mb: 1.5 }}
                >
                  <Typography variant="h6" sx={{ fontWeight: 700 }}>
                    {domainName}
                  </Typography>
                  <Chip
                    size="small"
                    variant="outlined"
                    label={`${domainItems.length} resource ${
                      domainItems.length === 1 ? 'type' : 'types'
                    }`}
                  />
                </Stack>

                <Grid container spacing={2.25}>
                  {domainItems.map(item => {
                    const meta = RESOURCE_TYPE_DIRECTORY_META[item.resourceType];
                    return (
                      <Grid item xs={12} md={6} lg={4} key={item.resourceType}>
                        <Card
                          variant="outlined"
                          sx={{
                            height: '100%',
                            display: 'flex',
                            flexDirection: 'column',
                            transition: 'border-color 0.15s ease, box-shadow 0.15s ease',
                            '&:hover': {
                              borderColor: 'primary.main',
                              boxShadow: 2,
                            },
                          }}
                        >
                          <CardContent sx={{ flexGrow: 1 }}>
                            <Stack
                              direction="row"
                              justifyContent="space-between"
                              alignItems="flex-start"
                              spacing={1}
                            >
                              <Stack direction="row" spacing={1.25} alignItems="center">
                                {getResourceTypeIcon(item.resourceType)}
                                <Box>
                                  <Typography variant="subtitle1" sx={{ fontWeight: 700, lineHeight: 1.25 }}>
                                    {meta?.shortTitle || item.title}
                                  </Typography>
                                  <Typography
                                    variant="caption"
                                    sx={{ fontFamily: 'monospace', color: 'text.secondary' }}
                                  >
                                    spec.type: {item.resourceType}
                                  </Typography>
                                </Box>
                              </Stack>

                              <Chip
                                size="small"
                                color="primary"
                                label={`${item.entityCount} ${
                                  item.entityCount === 1 ? 'Entity' : 'Entities'
                                }`}
                                sx={{ fontWeight: 700 }}
                              />
                            </Stack>

                            <Typography
                              variant="body2"
                              color="text.secondary"
                              sx={{ mt: 1.5, minHeight: 44 }}
                            >
                              {item.description || meta?.description}
                            </Typography>

                            <Stack
                              direction="row"
                              spacing={1}
                              alignItems="center"
                              flexWrap="wrap"
                              useFlexGap
                              sx={{ mt: 1.75 }}
                            >
                              <StatusPill
                                label={`${item.healthyCount} Healthy`}
                                level="ok"
                              />
                              {item.warningCount > 0 && (
                                <StatusPill
                                  label={`${item.warningCount} Warning / Pending`}
                                  level="warn"
                                />
                              )}
                            </Stack>

                            {item.actionIds.length > 0 && (
                              <Box sx={{ mt: 1.75 }}>
                                <Typography
                                  variant="caption"
                                  color="text.secondary"
                                  sx={{ fontWeight: 700, display: 'block', mb: 0.5 }}
                                >
                                  CONTEXTUAL ENTITY ACTIONS
                                </Typography>
                                <Stack direction="row" spacing={0.5} flexWrap="wrap" useFlexGap>
                                  {item.actionIds.slice(0, 5).map(actId => (
                                    <Chip
                                      key={actId}
                                      size="small"
                                      variant="outlined"
                                      label={actId}
                                      sx={{
                                        fontFamily: 'monospace',
                                        fontSize: '0.69rem',
                                        height: 20,
                                      }}
                                    />
                                  ))}
                                  {item.actionIds.length > 5 && (
                                    <Chip
                                      size="small"
                                      variant="outlined"
                                      label={`+${item.actionIds.length - 5} more`}
                                      sx={{ height: 20, fontSize: '0.69rem' }}
                                    />
                                  )}
                                </Stack>
                              </Box>
                            )}
                          </CardContent>

                          <Divider />

                          <CardActions sx={{ px: 2, py: 1.25, justifyContent: 'space-between' }}>
                            <Button
                              size="small"
                              component={RouterLink}
                              to={`/catalog?filters%5Bkind%5D=resource&filters%5Btype%5D=${encodeURIComponent(
                                item.resourceType,
                              )}`}
                              startIcon={<FilterListIcon />}
                            >
                              Catalog Facet
                            </Button>
                            <Button
                              size="small"
                              variant="contained"
                              component={RouterLink}
                              to={`/resource-types/${encodeURIComponent(item.resourceType)}`}
                              endIcon={<ArrowForwardIcon />}
                            >
                              Explore Entities
                            </Button>
                          </CardActions>
                        </Card>
                      </Grid>
                    );
                  })}
                </Grid>
              </Box>
            ))
          )}
        </Stack>
      </Content>
    </Page>
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
    <Page themeId="service">
      <Header
        title={meta?.fullTitle || `Resource Type: ${resourceType}`}
        subtitle={typeSummary?.description || meta?.description || 'Aggregated infrastructure entities'}
        type="Resource Type"
        typeLink="/resource-types"
      >
        <HeaderLabel label="Domain" value={meta?.domain || typeSummary?.category || 'Infrastructure'} />
        <HeaderLabel label="Entities" value={String(entities.length)} />
      </Header>

      <Content>
        <Stack spacing={2.5}>
          <Stack
            direction="row"
            spacing={1}
            alignItems="center"
            flexWrap="wrap"
            useFlexGap
          >
            <Button
              size="small"
              variant="outlined"
              component={RouterLink}
              to="/resource-types"
              startIcon={<ArrowBackIcon />}
            >
              All Types
            </Button>
            <Divider orientation="vertical" flexItem />
            {Object.values(RESOURCE_TYPE_DIRECTORY_META).map(m => (
              <Chip
                key={m.type}
                label={m.shortTitle}
                component={RouterLink}
                to={`/resource-types/${encodeURIComponent(m.type)}`}
                clickable
                color={m.type === resourceType ? 'primary' : 'default'}
                variant={m.type === resourceType ? 'filled' : 'outlined'}
                sx={{ fontWeight: m.type === resourceType ? 700 : 500 }}
              />
            ))}
          </Stack>

          <Paper
            variant="outlined"
            sx={{
              p: 2,
              display: 'flex',
              flexDirection: { xs: 'column', md: 'row' },
              justifyContent: 'space-between',
              alignItems: { xs: 'stretch', md: 'center' },
              gap: 2,
            }}
          >
            <TextField
              size="small"
              placeholder={`Filter ${resourceType} entities by name, model, IP, WWID, PCI ID, or status...`}
              value={searchTerm}
              onChange={e => setSearchTerm(e.target.value)}
              sx={{ minWidth: { xs: '100%', md: 420 } }}
              InputProps={{
                startAdornment: (
                  <InputAdornment position="start">
                    <SearchIcon fontSize="small" />
                  </InputAdornment>
                ),
              }}
            />

            <Stack direction="row" spacing={1.25} alignItems="center" flexWrap="wrap" useFlexGap>
              {typeSummary && (
                <>
                  <StatusPill label={`${typeSummary.healthyCount} Healthy`} level="ok" />
                  {typeSummary.warningCount > 0 && (
                    <StatusPill
                      label={`${typeSummary.warningCount} Warning / Gated`}
                      level="warn"
                    />
                  )}
                </>
              )}
              <Button
                size="small"
                variant="outlined"
                startIcon={refreshing ? <CircularProgress size={15} /> : <RefreshIcon />}
                disabled={refreshing}
                onClick={() => void loadEntities(true)}
              >
                {refreshing ? 'Refreshing...' : 'Refresh Live'}
              </Button>
              {SOURCING_RESOURCE_TYPE_SET.has(resourceType) && (
                <Button
                  size="small"
                  variant="contained"
                  component={RouterLink}
                  to="/sourcing-audit?tab=wizard"
                  startIcon={<AssessmentOutlinedIcon />}
                >
                  Sourcing Audit &amp; Wizard
                </Button>
              )}
              <Button
                size="small"
                variant="outlined"
                component={RouterLink}
                to={`/catalog?filters%5Bkind%5D=resource&filters%5Btype%5D=${encodeURIComponent(
                  resourceType,
                )}`}
                startIcon={<FilterListIcon />}
              >
                View in Entity Registry
              </Button>
            </Stack>
          </Paper>

          {error && <Alert severity="error">{error}</Alert>}

          {loading ? (
            <Box sx={{ display: 'flex', justifyContent: 'center', py: 6 }}>
              <CircularProgress />
            </Box>
          ) : (
            <TableContainer component={Paper} variant="outlined">
              <Table size="medium">
                <TableHead>
                  <TableRow sx={{ bgcolor: 'action.hover' }}>
                    <TableCell sx={{ fontWeight: 700 }}>Entity Instance</TableCell>
                    <TableCell sx={{ fontWeight: 700 }}>Observed Status</TableCell>
                    <TableCell sx={{ fontWeight: 700 }}>Summary & Hardware / Spec Identity</TableCell>
                    <TableCell sx={{ fontWeight: 700 }}>Topology Relations</TableCell>
                    <TableCell align="right" sx={{ fontWeight: 700 }}>
                      Landing Page
                    </TableCell>
                  </TableRow>
                </TableHead>
                <TableBody>
                  {filteredEntities.length === 0 ? (
                    <TableRow>
                      <TableCell colSpan={5} align="center" sx={{ py: 4 }}>
                        <Typography variant="body2" color="text.secondary">
                          No entities matched for resource type <code>{resourceType}</code>.
                        </Typography>
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
                        <TableRow key={ent.metadata.uid || ent.metadata.name} hover>
                          <TableCell sx={{ minWidth: 210 }}>
                            <Typography
                              component={RouterLink}
                              to={entityUrl}
                              variant="subtitle2"
                              sx={{
                                fontWeight: 700,
                                fontFamily: 'monospace',
                                color: 'primary.main',
                                textDecoration: 'none',
                                '&:hover': { textDecoration: 'underline' },
                              }}
                            >
                              {ent.metadata.name}
                            </Typography>
                            {ent.metadata.title && ent.metadata.title !== ent.metadata.name && (
                              <Typography
                                variant="caption"
                                color="text.secondary"
                                sx={{ display: 'block' }}
                              >
                                {ent.metadata.title}
                              </Typography>
                            )}
                          </TableCell>

                          <TableCell sx={{ whiteSpace: 'nowrap' }}>
                            <StatusPill label={status} level={level} />
                          </TableCell>

                          <TableCell sx={{ maxWidth: 460 }}>
                            <Typography variant="body2">
                              {ent.metadata.description || '—'}
                            </Typography>
                            {ent.metadata.tags && ent.metadata.tags.length > 0 && (
                              <Stack
                                direction="row"
                                spacing={0.5}
                                flexWrap="wrap"
                                useFlexGap
                                sx={{ mt: 0.75 }}
                              >
                                {ent.metadata.tags.slice(0, 5).map(t => (
                                  <Chip
                                    key={t}
                                    size="small"
                                    variant="outlined"
                                    label={t}
                                    sx={{ height: 19, fontSize: '0.68rem' }}
                                  />
                                ))}
                              </Stack>
                            )}
                          </TableCell>

                          <TableCell sx={{ maxWidth: 300 }}>
                            <Stack direction="row" spacing={0.5} flexWrap="wrap" useFlexGap>
                              {relations.slice(0, 4).map((rel, idx) => {
                                const shortTarget = rel.targetRef.replace(
                                  /^(resource|system):default\//i,
                                  '',
                                );
                                return (
                                  <Chip
                                    key={`${rel.type}-${rel.targetRef}-${idx}`}
                                    size="small"
                                    variant="outlined"
                                    component={RouterLink}
                                    to={entityRefToCatalogUrl(rel.targetRef)}
                                    clickable
                                    label={`${rel.type}: ${shortTarget}`}
                                    sx={{
                                      fontFamily: 'monospace',
                                      fontSize: '0.69rem',
                                      height: 21,
                                    }}
                                  />
                                );
                              })}
                              {relations.length > 4 && (
                                <Chip
                                  size="small"
                                  label={`+${relations.length - 4}`}
                                  sx={{ height: 21, fontSize: '0.69rem' }}
                                />
                              )}
                            </Stack>
                          </TableCell>

                          <TableCell align="right" sx={{ whiteSpace: 'nowrap' }}>
                            <Button
                              size="small"
                              variant="contained"
                              component={RouterLink}
                              to={entityUrl}
                              endIcon={<ArrowForwardIcon />}
                            >
                              Inspect & Act
                            </Button>
                          </TableCell>
                        </TableRow>
                      );
                    })
                  )}
                </TableBody>
              </Table>
            </TableContainer>
          )}
        </Stack>
      </Content>
    </Page>
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
