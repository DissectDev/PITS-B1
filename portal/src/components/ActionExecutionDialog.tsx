import React, { useEffect, useMemo, useState } from 'react';
import {
  Alert,
  AlertTitle,
  Box,
  Button,
  Chip,
  CircularProgress,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  Divider,
  FormControlLabel,
  IconButton,
  MenuItem,
  Paper,
  Stack,
  Switch,
  Table,
  TableBody,
  TableCell,
  TableRow,
  TextField,
  Tooltip,
  Typography,
} from '@mui/material';
import CloseIcon from '@mui/icons-material/Close';
import PlayArrowIcon from '@mui/icons-material/PlayArrow';
import ScienceIcon from '@mui/icons-material/Science';
import ContentCopyIcon from '@mui/icons-material/ContentCopy';
import DownloadIcon from '@mui/icons-material/Download';
import CheckIcon from '@mui/icons-material/Check';
import ShieldOutlinedIcon from '@mui/icons-material/ShieldOutlined';
import YAML from 'yaml';
import { invokeEntityAction } from '../api/pitsClient';
import type {
  ActionExecutionResult,
  ActionParamSpec,
  EntityActionSpec,
} from '../types';
import { FormattedAttributeValue, StatusPill } from './StatusPill';

interface ActionExecutionDialogProps {
  open: boolean;
  onClose: () => void;
  namespace: string;
  entityName: string;
  resourceType: string;
  action: EntityActionSpec | null;
  manifests: Record<string, string>;
  onActionCompleted: () => Promise<unknown>;
}

function buildInitialParams(
  params: ActionParamSpec[] | undefined,
  manifests: Record<string, string>,
): Record<string, unknown> {
  const initial: Record<string, unknown> = {};
  if (!params) return initial;

  const firstYamlManifest = Object.entries(manifests).find(
    ([k]) => k !== 'catalog-entity.yaml' && (k.endsWith('.yaml') || k.endsWith('.yml')),
  )?.[1];

  for (const p of params) {
    if (p.defaultValue !== undefined && p.defaultValue !== null) {
      initial[p.name] = p.defaultValue;
    } else if (p.paramType === 'boolean') {
      initial[p.name] = false;
    } else if (p.paramType === 'select' && p.options && p.options.length > 0) {
      initial[p.name] = p.options[0];
    } else if (p.paramType === 'yaml') {
      initial[p.name] = firstYamlManifest ?? '';
    } else {
      initial[p.name] = '';
    }
  }
  return initial;
}

function summarizeOutputEntries(output: unknown): Array<{ key: string; value: string }> {
  if (!output || typeof output !== 'object' || Array.isArray(output)) {
    return [];
  }
  const rows: Array<{ key: string; value: string }> = [];
  for (const [k, v] of Object.entries(output as Record<string, unknown>)) {
    if (v === null || v === undefined) continue;
    if (typeof v === 'string' || typeof v === 'number' || typeof v === 'boolean') {
      rows.push({ key: k, value: String(v) });
    } else if (Array.isArray(v) && v.every(item => typeof item !== 'object')) {
      rows.push({ key: k, value: v.join(', ') });
    }
  }
  return rows.slice(0, 16);
}

export const ActionExecutionDialog: React.FC<ActionExecutionDialogProps> = ({
  open,
  onClose,
  namespace,
  entityName,
  resourceType,
  action,
  manifests,
  onActionCompleted,
}) => {
  const [formValues, setFormValues] = useState<Record<string, unknown>>({});
  const [submittingMode, setSubmittingMode] = useState<'dryRun' | 'execute' | null>(null);
  const [result, setResult] = useState<ActionExecutionResult | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [copiedArtifact, setCopiedArtifact] = useState(false);

  useEffect(() => {
    if (action && open) {
      setFormValues(buildInitialParams(action.parameters, manifests));
      setResult(null);
      setErrorMsg(null);
      setSubmittingMode(null);
    }
  }, [action, open, manifests]);

  const yamlValidationErrors = useMemo(() => {
    const errs: Record<string, string> = {};
    if (!action?.parameters) return errs;
    for (const p of action.parameters) {
      if (p.paramType === 'yaml') {
        const raw = String(formValues[p.name] ?? '').trim();
        if (raw) {
          try {
            YAML.parseAllDocuments(raw).forEach(doc => {
              if (doc.errors.length > 0) {
                throw new Error(doc.errors[0].message);
              }
            });
          } catch (e) {
            errs[p.name] = e instanceof Error ? e.message : String(e);
          }
        } else if (p.required) {
          errs[p.name] = 'YAML manifest is required.';
        }
      }
    }
    return errs;
  }, [action, formValues]);

  if (!action) return null;

  const parameters = action.parameters ?? [];
  const hasYamlError = Object.keys(yamlValidationErrors).length > 0;

  const setParamValue = (name: string, val: unknown) => {
    setFormValues(prev => ({ ...prev, [name]: val }));
  };

  const buildPayloadParameters = (): Record<string, unknown> => {
    const payload: Record<string, unknown> = {};
    for (const p of parameters) {
      const raw = formValues[p.name];
      if (p.paramType === 'integer') {
        if (raw === '' || raw === undefined || raw === null) {
          if (p.required) {
            throw new Error(`Parameter "${p.label}" is required.`);
          }
        } else {
          const num = Number(raw);
          if (!Number.isFinite(num)) {
            throw new Error(`Parameter "${p.label}" must be a valid integer.`);
          }
          payload[p.name] = Math.trunc(num);
        }
      } else if (p.paramType === 'number') {
        if (raw === '' || raw === undefined || raw === null) {
          if (p.required) {
            throw new Error(`Parameter "${p.label}" is required.`);
          }
        } else {
          const num = Number(raw);
          if (!Number.isFinite(num)) {
            throw new Error(`Parameter "${p.label}" must be a valid number.`);
          }
          payload[p.name] = num;
        }
      } else if (p.paramType === 'boolean') {
        payload[p.name] = Boolean(raw);
      } else {
        const str = raw === undefined || raw === null ? '' : String(raw);
        if (p.required && !str.trim()) {
          throw new Error(`Parameter "${p.label}" is required.`);
        }
        if (str !== '' || p.required) {
          payload[p.name] = str;
        }
      }
    }
    return payload;
  };

  const handleRun = async (dryRun: boolean) => {
    setErrorMsg(null);
    let normalizedParams: Record<string, unknown>;
    try {
      normalizedParams = buildPayloadParameters();
    } catch (validationErr) {
      setErrorMsg(
        validationErr instanceof Error ? validationErr.message : String(validationErr),
      );
      return;
    }

    setSubmittingMode(dryRun ? 'dryRun' : 'execute');
    try {
      const execResult = await invokeEntityAction(namespace, entityName, action.id, {
        parameters: normalizedParams,
        dryRun,
      });
      setResult(execResult);
      await onActionCompleted();
    } catch (err) {
      setErrorMsg(err instanceof Error ? err.message : String(err));
    } finally {
      setSubmittingMode(null);
    }
  };

  const handleCopyArtifact = async () => {
    if (!result?.renderedArtifact) return;
    try {
      await navigator.clipboard.writeText(result.renderedArtifact);
      setCopiedArtifact(true);
      setTimeout(() => setCopiedArtifact(false), 1800);
    } catch {
      // Ignore clipboard failure
    }
  };

  const handleDownloadRdp = () => {
    if (!result?.renderedArtifact) return;
    const blob = new Blob([result.renderedArtifact], {
      type: 'application/x-rdp;charset=utf-8',
    });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = `${entityName}.rdp`;
    document.body.appendChild(link);
    link.click();
    link.remove();
    URL.revokeObjectURL(url);
  };

  const outputSummaryRows = result ? summarizeOutputEntries(result.output) : [];

  return (
    <Dialog
      open={open}
      onClose={submittingMode ? undefined : onClose}
      maxWidth="md"
      fullWidth
      scroll="paper"
    >
      <DialogTitle sx={{ pb: 1.5 }}>
        <Stack direction="row" justifyContent="space-between" alignItems="flex-start" spacing={2}>
          <Box>
            <Stack direction="row" spacing={1} alignItems="center" flexWrap="wrap" useFlexGap>
              <Typography variant="h6" sx={{ fontWeight: 700 }}>
                {action.title}
              </Typography>
              <Chip
                size="small"
                label={action.id}
                sx={{ fontFamily: 'monospace', fontSize: '0.75rem' }}
              />
              <Chip size="small" variant="outlined" label={action.category} />
            </Stack>
            <Typography variant="caption" color="text.secondary" sx={{ display: 'block', mt: 0.5 }}>
              Target Entity:{' '}
              <Box component="code" sx={{ fontFamily: 'monospace', fontWeight: 600 }}>
                resource:{namespace}/{entityName}
              </Box>{' '}
              ({resourceType})
            </Typography>
          </Box>
          <IconButton onClick={onClose} disabled={Boolean(submittingMode)} size="small">
            <CloseIcon />
          </IconButton>
        </Stack>
      </DialogTitle>

      <Divider />

      <DialogContent sx={{ py: 2.5 }}>
        <Stack spacing={2.5}>
          <Typography variant="body2" color="text.secondary">
            {action.description}
          </Typography>

          {action.dangerous && (
            <Alert severity="warning" icon={<ShieldOutlinedIcon />}>
              <AlertTitle sx={{ fontWeight: 700 }}>
                Guarded Infrastructure Operation
              </AlertTitle>
              This action mutates live infrastructure state and enforces PITS safety invariants
              (such as <code>mgmt0 0000:58:00.0</code> preservation, Talos <code>netboot-guard</code>{' '}
              WWID allowlists, or active workstation session protection).
              {action.supportsDryRun
                ? ' Run a Dry-Run Preview first to inspect rendered changes safely before applying.'
                : ''}
            </Alert>
          )}

          {parameters.length === 0 ? (
            <Paper variant="outlined" sx={{ p: 2, bgcolor: 'action.hover' }}>
              <Typography variant="body2" color="text.secondary">
                No additional input parameters required for <code>{action.id}</code>. Click{' '}
                <strong>{action.supportsDryRun ? 'Dry-Run Preview or Execute Action' : 'Execute Action'}</strong>{' '}
                below to proceed.
              </Typography>
            </Paper>
          ) : (
            <Stack spacing={2}>
              <Typography variant="subtitle2" sx={{ fontWeight: 700 }}>
                Action Parameters
              </Typography>
              {parameters.map(param => {
                const currentVal = formValues[param.name];

                if (param.paramType === 'boolean') {
                  return (
                    <Paper key={param.name} variant="outlined" sx={{ p: 1.5 }}>
                      <FormControlLabel
                        control={
                          <Switch
                            checked={Boolean(currentVal)}
                            onChange={e => setParamValue(param.name, e.target.checked)}
                            color={action.dangerous ? 'warning' : 'primary'}
                          />
                        }
                        label={
                          <Box>
                            <Typography variant="body2" sx={{ fontWeight: 600 }}>
                              {param.label}{' '}
                              <Typography
                                component="span"
                                variant="caption"
                                sx={{ fontFamily: 'monospace', color: 'text.secondary' }}
                              >
                                ({param.name})
                              </Typography>
                            </Typography>
                            {param.description && (
                              <Typography variant="caption" color="text.secondary">
                                {param.description}
                              </Typography>
                            )}
                          </Box>
                        }
                      />
                    </Paper>
                  );
                }

                if (param.paramType === 'select') {
                  return (
                    <TextField
                      key={param.name}
                      select
                      fullWidth
                      size="small"
                      required={param.required}
                      label={`${param.label} (${param.name})`}
                      value={String(currentVal ?? '')}
                      onChange={e => setParamValue(param.name, e.target.value)}
                      helperText={param.description}
                    >
                      {(param.options ?? []).map(opt => (
                        <MenuItem key={opt} value={opt}>
                          {opt}
                        </MenuItem>
                      ))}
                    </TextField>
                  );
                }

                if (param.paramType === 'yaml') {
                  const yamlErr = yamlValidationErrors[param.name];
                  return (
                    <Box key={param.name}>
                      <Stack
                        direction="row"
                        justifyContent="space-between"
                        alignItems="center"
                        sx={{ mb: 0.75 }}
                      >
                        <Typography variant="body2" sx={{ fontWeight: 600 }}>
                          {param.label}{' '}
                          <Typography
                            component="span"
                            variant="caption"
                            sx={{ fontFamily: 'monospace', color: 'text.secondary' }}
                          >
                            ({param.name})
                          </Typography>
                        </Typography>
                        <StatusPill
                          label={yamlErr ? 'YAML Syntax Issue' : 'Valid YAML'}
                          level={yamlErr ? 'error' : 'ok'}
                        />
                      </Stack>
                      <TextField
                        fullWidth
                        multiline
                        minRows={10}
                        maxRows={20}
                        required={param.required}
                        value={String(currentVal ?? '')}
                        onChange={e => setParamValue(param.name, e.target.value)}
                        error={Boolean(yamlErr)}
                        helperText={yamlErr || param.description}
                        InputProps={{
                          sx: {
                            fontFamily: '"JetBrains Mono", "Cascadia Code", Consolas, monospace',
                            fontSize: '0.8rem',
                            lineHeight: 1.5,
                          },
                        }}
                      />
                    </Box>
                  );
                }

                return (
                  <TextField
                    key={param.name}
                    fullWidth
                    size="small"
                    type={
                      param.paramType === 'integer' || param.paramType === 'number'
                        ? 'number'
                        : 'text'
                    }
                    inputProps={param.paramType === 'number' ? { step: '0.1' } : undefined}
                    required={param.required}
                    label={`${param.label} (${param.name})`}
                    value={currentVal === undefined || currentVal === null ? '' : String(currentVal)}
                    onChange={e => setParamValue(param.name, e.target.value)}
                    helperText={param.description}
                  />
                );
              })}
            </Stack>
          )}

          {errorMsg && (
            <Alert severity="error">
              <AlertTitle sx={{ fontWeight: 700 }}>Action Rejected or Failed</AlertTitle>
              {errorMsg}
            </Alert>
          )}

          {result && (
            <Paper
              variant="outlined"
              sx={{
                p: 2,
                borderWidth: 1.5,
                borderColor: result.success ? 'success.main' : 'error.main',
              }}
            >
              <Stack spacing={1.5}>
                <Stack
                  direction="row"
                  justifyContent="space-between"
                  alignItems="center"
                  flexWrap="wrap"
                  useFlexGap
                  spacing={1}
                >
                  <Stack direction="row" spacing={1} alignItems="center">
                    <StatusPill
                      label={result.success ? 'Succeeded' : 'Blocked / Failed'}
                      level={result.success ? 'ok' : 'error'}
                    />
                    <Chip
                      size="small"
                      color={result.dryRun ? 'info' : 'primary'}
                      variant="outlined"
                      label={result.dryRun ? 'DRY-RUN PREVIEW' : 'LIVE EXECUTION'}
                    />
                  </Stack>
                  <Typography variant="caption" color="text.secondary">
                    Executed at {new Date(result.executedAt).toLocaleString()}
                  </Typography>
                </Stack>

                <Typography variant="body2" sx={{ fontWeight: 600 }}>
                  {result.summary}
                </Typography>

                {result.changesOrWarnings.length > 0 && (
                  <Box>
                    <Typography
                      variant="caption"
                      color="text.secondary"
                      sx={{ fontWeight: 700, textTransform: 'uppercase' }}
                    >
                      Precondition Checks, Changes & Warnings
                    </Typography>
                    <Stack component="ul" spacing={0.5} sx={{ m: 0, mt: 0.5, pl: 2.5 }}>
                      {result.changesOrWarnings.map((item, idx) => (
                        <Typography component="li" variant="body2" key={idx}>
                          {item}
                        </Typography>
                      ))}
                    </Stack>
                  </Box>
                )}

                {outputSummaryRows.length > 0 && (
                  <Box>
                    <Typography
                      variant="caption"
                      color="text.secondary"
                      sx={{ fontWeight: 700, textTransform: 'uppercase' }}
                    >
                      Structured Execution Telemetry
                    </Typography>
                    <Table size="small" sx={{ mt: 0.5 }}>
                      <TableBody>
                        {outputSummaryRows.map(row => (
                          <TableRow key={row.key}>
                            <TableCell
                              sx={{ width: '34%', fontWeight: 600, color: 'text.secondary' }}
                            >
                              {row.key}
                            </TableCell>
                            <TableCell>
                              <FormattedAttributeValue value={row.value} itemKey={row.key} />
                            </TableCell>
                          </TableRow>
                        ))}
                      </TableBody>
                    </Table>
                  </Box>
                )}

                {result.renderedArtifact && (
                  <Box>
                    <Stack
                      direction="row"
                      justifyContent="space-between"
                      alignItems="center"
                      sx={{ mb: 0.5 }}
                    >
                      <Typography
                        variant="caption"
                        color="text.secondary"
                        sx={{ fontWeight: 700, textTransform: 'uppercase' }}
                      >
                        Rendered Declarative Artifact (YAML / iPXE / RDP)
                      </Typography>
                      <Stack direction="row" spacing={1}>
                        {action.id === 'connect-pinned-rdp' && (
                          <Tooltip title={`Download ${entityName}.rdp for mstsc.exe`}>
                            <Button
                              size="small"
                              variant="outlined"
                              startIcon={<DownloadIcon />}
                              onClick={handleDownloadRdp}
                            >
                              Download .rdp
                            </Button>
                          </Tooltip>
                        )}
                        <Tooltip title={copiedArtifact ? 'Copied!' : 'Copy artifact'}>
                          <Button
                            size="small"
                            variant="outlined"
                            startIcon={copiedArtifact ? <CheckIcon /> : <ContentCopyIcon />}
                            onClick={handleCopyArtifact}
                          >
                            {copiedArtifact ? 'Copied' : 'Copy'}
                          </Button>
                        </Tooltip>
                      </Stack>
                    </Stack>
                    <Box
                      component="pre"
                      sx={{
                        m: 0,
                        p: 1.5,
                        maxHeight: 320,
                        overflow: 'auto',
                        bgcolor: '#0f172a',
                        color: '#e2e8f0',
                        borderRadius: 1,
                        fontFamily: '"JetBrains Mono", "Cascadia Code", Consolas, monospace',
                        fontSize: '0.78rem',
                        lineHeight: 1.5,
                      }}
                    >
                      {result.renderedArtifact}
                    </Box>
                  </Box>
                )}
              </Stack>
            </Paper>
          )}
        </Stack>
      </DialogContent>

      <Divider />

      <DialogActions sx={{ px: 3, py: 1.75 }}>
        <Button onClick={onClose} disabled={Boolean(submittingMode)}>
          Close
        </Button>
        {action.supportsDryRun && (
          <Button
            variant="outlined"
            color="info"
            disabled={Boolean(submittingMode) || hasYamlError}
            startIcon={
              submittingMode === 'dryRun' ? <CircularProgress size={16} /> : <ScienceIcon />
            }
            onClick={() => handleRun(true)}
          >
            Dry-Run Preview
          </Button>
        )}
        <Button
          variant="contained"
          color={action.dangerous ? 'warning' : 'primary'}
          disabled={Boolean(submittingMode) || hasYamlError}
          startIcon={
            submittingMode === 'execute' ? <CircularProgress size={16} color="inherit" /> : <PlayArrowIcon />
          }
          onClick={() => handleRun(false)}
        >
          Execute Action
        </Button>
      </DialogActions>
    </Dialog>
  );
};
