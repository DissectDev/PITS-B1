import React, { useMemo, useState } from 'react';
import {
  Alert,
  Box,
  Button,
  Card,
  CardContent,
  CardHeader,
  Chip,
  CircularProgress,
  Divider,
  Grid,
  Paper,
  Stack,
  Table,
  TableBody,
  TableCell,
  TableContainer,
  TableRow,
  Tooltip,
  Typography,
} from '@mui/material';
import RefreshIcon from '@mui/icons-material/Refresh';
import PlayCircleOutlineIcon from '@mui/icons-material/PlayCircleOutline';
import LockOutlinedIcon from '@mui/icons-material/LockOutlined';
import ShieldOutlinedIcon from '@mui/icons-material/ShieldOutlined';
import SensorsIcon from '@mui/icons-material/Sensors';
import HubIcon from '@mui/icons-material/Hub';
import { Link as RouterLink } from 'react-router-dom';
import { useEntity } from '@backstage/plugin-catalog-react';
import { usePitsEntityDetail } from '../hooks/usePitsEntityDetail';
import {
  RESOURCE_TYPE_DIRECTORY_META,
  type AttributeSection,
  type EntityActionSpec,
} from '../types';
import { FormattedAttributeValue, StatusPill } from './StatusPill';
import { ActionExecutionDialog } from './ActionExecutionDialog';

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
      <Card variant="outlined">
        <CardContent sx={{ display: 'flex', alignItems: 'center', gap: 2, py: 3 }}>
          <CircularProgress size={24} />
          <Typography variant="body2" color="text.secondary">
            Synthesizing live entity state for <strong>{name}</strong>...
          </Typography>
        </CardContent>
      </Card>
    );
  }

  return (
    <Card variant="outlined">
      <CardHeader
        title={
          <Stack direction="row" spacing={1.25} alignItems="center" flexWrap="wrap" useFlexGap>
            <Typography variant="h6" sx={{ fontWeight: 700 }}>
              {entity.metadata.title || entity.metadata.name}
            </Typography>
            <Chip
              size="small"
              color="primary"
              variant="outlined"
              component={RouterLink}
              to={`/resource-types/${encodeURIComponent(resolvedType)}`}
              clickable
              label={resolvedType}
              sx={{ fontFamily: 'monospace', fontWeight: 600 }}
            />
            {typeMeta && (
              <Chip size="small" variant="outlined" label={typeMeta.domain} />
            )}
            {detail && (
              <StatusPill label={detail.status} level={detail.statusLevel} size="medium" />
            )}
          </Stack>
        }
        action={
          <Button
            variant="outlined"
            size="small"
            startIcon={refreshing ? <CircularProgress size={15} /> : <RefreshIcon />}
            disabled={refreshing}
            onClick={() => void refreshLive()}
          >
            {refreshing ? 'Refreshing...' : 'Refresh Live Telemetry'}
          </Button>
        }
      />
      <Divider />
      <CardContent>
        <Stack spacing={2}>
          {error && <Alert severity="warning">{error}</Alert>}

          <Paper
            variant="outlined"
            sx={{
              p: 1.75,
              bgcolor: 'action.hover',
              display: 'flex',
              flexDirection: { xs: 'column', sm: 'row' },
              justifyContent: 'space-between',
              alignItems: { xs: 'flex-start', sm: 'center' },
              gap: 1.5,
            }}
          >
            <Box>
              <Typography variant="body1" sx={{ fontWeight: 600 }}>
                {detail?.summaryLine || entity.metadata.description || 'PITS Infrastructure Resource Entity'}
              </Typography>
              {entity.metadata.description &&
                detail?.summaryLine &&
                entity.metadata.description !== detail.summaryLine && (
                  <Typography variant="caption" color="text.secondary" sx={{ display: 'block', mt: 0.5 }}>
                    {entity.metadata.description}
                  </Typography>
                )}
            </Box>

            <Stack direction="row" spacing={1} alignItems="center" sx={{ flexShrink: 0 }}>
              <Chip
                size="small"
                icon={<SensorsIcon />}
                color={detail?.liveConnected ? 'success' : 'default'}
                variant={detail?.liveConnected ? 'filled' : 'outlined'}
                label={detail?.liveConnected ? 'Live Telemetry Connected' : 'Baseline / Cached State'}
              />
            </Stack>
          </Paper>

          <Grid container spacing={2}>
            <Grid item xs={12} sm={6} md={3}>
              <Typography variant="caption" color="text.secondary" sx={{ fontWeight: 600 }}>
                ENTITY REF
              </Typography>
              <Box sx={{ mt: 0.4 }}>
                <FormattedAttributeValue
                  value={`resource:${namespace}/${name}`}
                  monospace
                />
              </Box>
            </Grid>
            <Grid item xs={12} sm={6} md={3}>
              <Typography variant="caption" color="text.secondary" sx={{ fontWeight: 600 }}>
                SYSTEM
              </Typography>
              <Box sx={{ mt: 0.4 }}>
                <Chip
                  size="small"
                  icon={<HubIcon />}
                  label={String((entity.spec as Record<string, unknown> | undefined)?.system ?? 'pits-infrastructure')}
                  component={RouterLink}
                  to="/catalog?filters%5Bkind%5D=resource"
                  clickable
                  variant="outlined"
                />
              </Box>
            </Grid>
            <Grid item xs={12} sm={6} md={3}>
              <Typography variant="caption" color="text.secondary" sx={{ fontWeight: 600 }}>
                OWNER & LIFECYCLE
              </Typography>
              <Typography variant="body2" sx={{ mt: 0.4, fontWeight: 500 }}>
                {String((entity.spec as Record<string, unknown> | undefined)?.owner ?? 'group:default/operators')}
                {' · '}
                {String((entity.spec as Record<string, unknown> | undefined)?.lifecycle ?? 'production')}
              </Typography>
            </Grid>
            <Grid item xs={12} sm={6} md={3}>
              <Typography variant="caption" color="text.secondary" sx={{ fontWeight: 600 }}>
                LAST SYNTHESIZED
              </Typography>
              <Typography variant="body2" sx={{ mt: 0.4, fontWeight: 500 }}>
                {detail?.refreshedAt
                  ? new Date(detail.refreshedAt).toLocaleTimeString()
                  : 'Just now'}
              </Typography>
            </Grid>
          </Grid>

          {entity.metadata.tags && entity.metadata.tags.length > 0 && (
            <Stack direction="row" spacing={0.75} flexWrap="wrap" useFlexGap>
              {entity.metadata.tags.map(tag => (
                <Chip key={tag} size="small" label={`#${tag}`} variant="outlined" />
              ))}
            </Stack>
          )}
        </Stack>
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
      <Card variant="outlined">
        <CardHeader title="Desired vs Observed Attributes" />
        <Divider />
        <CardContent sx={{ display: 'flex', justifyContent: 'center', py: 4 }}>
          <CircularProgress size={26} />
        </CardContent>
      </Card>
    );
  }

  const sections: AttributeSection[] = detail?.attributeSections ?? [];

  return (
    <Card variant="outlined">
      <CardHeader
        title="Structured Entity Attributes"
        subheader="Desired contract specification reconciled against live hardware, kernel, and control-plane state"
      />
      <Divider />
      <CardContent>
        {sections.length === 0 ? (
          <Typography variant="body2" color="text.secondary">
            No structured attribute sections reported for this entity.
          </Typography>
        ) : (
          <Stack spacing={2.5}>
            {sections.map(section => (
              <Paper key={section.id} variant="outlined" sx={{ overflow: 'hidden' }}>
                <Box
                  sx={{
                    px: 2,
                    py: 1,
                    bgcolor: 'action.hover',
                    borderBottom: '1px solid',
                    borderColor: 'divider',
                  }}
                >
                  <Typography variant="subtitle2" sx={{ fontWeight: 700 }}>
                    {section.title}
                  </Typography>
                </Box>
                <TableContainer>
                  <Table size="small">
                    <TableBody>
                      {section.items.map(item => (
                        <TableRow key={item.key} hover>
                          <TableCell
                            sx={{
                              width: '36%',
                              fontWeight: 600,
                              color: 'text.secondary',
                              verticalAlign: 'middle',
                            }}
                          >
                            <Tooltip title={item.key} placement="top-start">
                              <span>{item.label}</span>
                            </Tooltip>
                          </TableCell>
                          <TableCell sx={{ verticalAlign: 'middle' }}>
                            <Stack
                              direction="row"
                              spacing={1}
                              alignItems="center"
                              justifyContent="space-between"
                              flexWrap="wrap"
                              useFlexGap
                            >
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
                            </Stack>
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </TableContainer>
              </Paper>
            ))}
          </Stack>
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
      <Card variant="outlined">
        <CardHeader title="Contextual Entity Actions" />
        <Divider />
        <CardContent sx={{ display: 'flex', justifyContent: 'center', py: 4 }}>
          <CircularProgress size={26} />
        </CardContent>
      </Card>
    );
  }

  return (
    <>
      <Card variant="outlined">
        <CardHeader
          title="Contextual Entity Actions"
          subheader={`Precondition-gated operations scoped strictly to ${name}`}
        />
        <Divider />
        <CardContent>
          {groupedActions.length === 0 ? (
            <Typography variant="body2" color="text.secondary">
              No contextual actions registered for this entity.
            </Typography>
          ) : (
            <Stack spacing={2.25}>
              {groupedActions.map(([category, actions]) => (
                <Box key={category}>
                  <Typography
                    variant="caption"
                    color="text.secondary"
                    sx={{
                      fontWeight: 700,
                      textTransform: 'uppercase',
                      letterSpacing: 0.6,
                      display: 'block',
                      mb: 1,
                    }}
                  >
                    {category}
                  </Typography>
                  <Stack spacing={1.25}>
                    {actions.map(act => {
                      const isDisabled = !act.enabled;
                      return (
                        <Paper
                          key={act.id}
                          variant="outlined"
                          sx={{
                            p: 1.5,
                            opacity: isDisabled ? 0.85 : 1,
                            bgcolor: isDisabled ? 'action.hover' : 'background.paper',
                          }}
                        >
                          <Stack spacing={1}>
                            <Stack
                              direction="row"
                              justifyContent="space-between"
                              alignItems="flex-start"
                              spacing={1.5}
                            >
                              <Box sx={{ minWidth: 0 }}>
                                <Stack
                                  direction="row"
                                  spacing={0.75}
                                  alignItems="center"
                                  flexWrap="wrap"
                                  useFlexGap
                                >
                                  <Typography variant="subtitle2" sx={{ fontWeight: 700 }}>
                                    {act.title}
                                  </Typography>
                                  <Chip
                                    size="small"
                                    label={act.id}
                                    sx={{
                                      fontFamily: 'monospace',
                                      fontSize: '0.7rem',
                                      height: 20,
                                    }}
                                  />
                                  {act.dangerous && (
                                    <Chip
                                      size="small"
                                      color="warning"
                                      variant="outlined"
                                      icon={<ShieldOutlinedIcon />}
                                      label="Guarded"
                                      sx={{ height: 20, fontSize: '0.7rem' }}
                                    />
                                  )}
                                  {act.supportsDryRun && (
                                    <Chip
                                      size="small"
                                      color="info"
                                      variant="outlined"
                                      label="Dry-Run"
                                      sx={{ height: 20, fontSize: '0.7rem' }}
                                    />
                                  )}
                                </Stack>
                                <Typography
                                  variant="body2"
                                  color="text.secondary"
                                  sx={{ mt: 0.5, fontSize: '0.82rem' }}
                                >
                                  {act.description}
                                </Typography>
                              </Box>

                              <Tooltip
                                title={
                                  isDisabled
                                    ? act.disabledReason || 'Precondition gate not satisfied'
                                    : `Open ${act.title}`
                                }
                              >
                                <span>
                                  <Button
                                    size="small"
                                    variant={act.dangerous ? 'outlined' : 'contained'}
                                    color={act.dangerous ? 'warning' : 'primary'}
                                    disabled={isDisabled}
                                    startIcon={
                                      isDisabled ? (
                                        <LockOutlinedIcon />
                                      ) : (
                                        <PlayCircleOutlineIcon />
                                      )
                                    }
                                    onClick={() => setSelectedAction(act)}
                                    sx={{ whiteSpace: 'nowrap', flexShrink: 0 }}
                                  >
                                    {isDisabled ? 'Gated' : 'Run'}
                                  </Button>
                                </span>
                              </Tooltip>
                            </Stack>

                            {isDisabled && act.disabledReason && (
                              <Alert
                                severity="warning"
                                icon={<LockOutlinedIcon fontSize="small" />}
                                sx={{ py: 0.25, px: 1.25, '& .MuiAlert-message': { fontSize: '0.78rem' } }}
                              >
                                <strong>Precondition Blocked:</strong> {act.disabledReason}
                              </Alert>
                            )}
                          </Stack>
                        </Paper>
                      );
                    })}
                  </Stack>
                </Box>
              ))}
            </Stack>
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
