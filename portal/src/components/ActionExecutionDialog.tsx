import React, { useEffect, useMemo, useState } from 'react';
import {
  AlertCircle,
  Check,
  Copy,
  Download,
  FlaskConical,
  Loader2,
  Play,
  Shield,
} from 'lucide-react';
import YAML from 'yaml';
import { invokeEntityAction } from '../api/pitsClient';
import type {
  ActionExecutionResult,
  ActionParamSpec,
  EntityActionSpec,
} from '../types';
import { FormattedAttributeValue, StatusPill } from './StatusPill';
import { Badge } from './ui/badge';
import { Button } from './ui/button';
import { Input } from './ui/input';
import { Textarea } from './ui/textarea';
import { Alert, AlertDescription, AlertTitle } from './ui/alert';
import { Table, TableBody, TableCell, TableRow } from './ui/table';
import {
  PitsDialog,
  PitsSelect,
  PitsSwitch,
  PitsTooltip,
} from './ui/pits-primitives';
import { cn } from '../lib/utils';

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
    <PitsDialog
      open={open}
      onOpenChange={next => {
        if (!next) onClose();
      }}
      disableClose={Boolean(submittingMode)}
      title={
        <span className="flex flex-wrap items-center gap-2">
          <span>{action.title}</span>
          <Badge variant="muted" className="font-mono">
            {action.id}
          </Badge>
          <Badge variant="outline">{action.category}</Badge>
        </span>
      }
      subtitle={
        <span>
          Target Entity:{' '}
          <code className="font-mono font-semibold text-foreground">
            resource:{namespace}/{entityName}
          </code>{' '}
          ({resourceType})
        </span>
      }
      footer={
        <>
          <Button variant="ghost" size="sm" onClick={onClose} disabled={Boolean(submittingMode)}>
            Close
          </Button>
          {action.supportsDryRun && (
            <Button
              variant="outline"
              size="sm"
              disabled={Boolean(submittingMode) || hasYamlError}
              onClick={() => void handleRun(true)}
            >
              {submittingMode === 'dryRun' ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <FlaskConical className="h-3.5 w-3.5" />
              )}
              <span>Dry-Run Preview</span>
            </Button>
          )}
          <Button
            variant={action.dangerous ? 'warningSolid' : 'default'}
            size="sm"
            disabled={Boolean(submittingMode) || hasYamlError}
            onClick={() => void handleRun(false)}
          >
            {submittingMode === 'execute' ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <Play className="h-3.5 w-3.5" />
            )}
            <span>Execute Action</span>
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <p className="m-0 text-[13px] leading-relaxed text-muted-foreground">
          {action.description}
        </p>

        {action.dangerous && (
          <Alert variant="warning">
            <Shield />
            <div>
              <AlertTitle>Guarded Infrastructure Operation</AlertTitle>
              <AlertDescription>
                This action mutates live infrastructure state and enforces PITS safety invariants
                (such as{' '}
                <code className="rounded bg-black/10 px-1 py-0.5 font-mono text-[11px] dark:bg-white/10">
                  mgmt0 0000:58:00.0
                </code>{' '}
                preservation, Talos{' '}
                <code className="rounded bg-black/10 px-1 py-0.5 font-mono text-[11px] dark:bg-white/10">
                  netboot-guard
                </code>{' '}
                WWID allowlists, or active workstation session protection).
                {action.supportsDryRun
                  ? ' Run a Dry-Run Preview first to inspect rendered changes safely before applying.'
                  : ''}
              </AlertDescription>
            </div>
          </Alert>
        )}

        {parameters.length === 0 ? (
          <div className="rounded-lg border border-solid border-border bg-muted/40 p-3.5 text-xs leading-relaxed text-muted-foreground">
            No additional input parameters required for{' '}
            <code className="font-mono text-foreground">{action.id}</code>. Click{' '}
            <strong className="text-foreground">
              {action.supportsDryRun ? 'Dry-Run Preview or Execute Action' : 'Execute Action'}
            </strong>{' '}
            below to proceed.
          </div>
        ) : (
          <div className="space-y-3.5">
            <div className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
              Action Parameters
            </div>

            {parameters.map(param => {
              const currentVal = formValues[param.name];

              if (param.paramType === 'boolean') {
                return (
                  <label
                    key={param.name}
                    className="flex cursor-pointer items-start justify-between gap-4 rounded-lg border border-solid border-border bg-muted/20 p-3.5"
                  >
                    <div className="space-y-0.5">
                      <div className="text-xs font-semibold text-foreground">
                        {param.label}{' '}
                        <span className="font-mono text-[11px] font-normal text-muted-foreground">
                          ({param.name})
                        </span>
                      </div>
                      {param.description && (
                        <div className="text-[11px] text-muted-foreground">
                          {param.description}
                        </div>
                      )}
                    </div>
                    <PitsSwitch
                      checked={Boolean(currentVal)}
                      onCheckedChange={checked => setParamValue(param.name, checked)}
                      tone={action.dangerous ? 'warning' : 'default'}
                    />
                  </label>
                );
              }

              if (param.paramType === 'select') {
                return (
                  <div key={param.name} className="space-y-1.5">
                    <label className="block text-xs font-medium text-foreground">
                      {param.label}{' '}
                      <span className="font-mono text-[11px] text-muted-foreground">
                        ({param.name})
                      </span>
                      {param.required && <span className="ml-0.5 text-red-500">*</span>}
                    </label>
                    <PitsSelect
                      value={String(currentVal ?? '')}
                      onValueChange={val => setParamValue(param.name, val)}
                      options={(param.options ?? []).map(opt => ({ value: opt, label: opt }))}
                    />
                    {param.description && (
                      <p className="m-0 text-[11px] text-muted-foreground">{param.description}</p>
                    )}
                  </div>
                );
              }

              if (param.paramType === 'yaml') {
                const yamlErr = yamlValidationErrors[param.name];
                return (
                  <div key={param.name} className="space-y-1.5">
                    <div className="flex items-center justify-between gap-2">
                      <label className="text-xs font-medium text-foreground">
                        {param.label}{' '}
                        <span className="font-mono text-[11px] text-muted-foreground">
                          ({param.name})
                        </span>
                        {param.required && <span className="ml-0.5 text-red-500">*</span>}
                      </label>
                      <StatusPill
                        label={yamlErr ? 'YAML Syntax Issue' : 'Valid YAML'}
                        level={yamlErr ? 'error' : 'ok'}
                      />
                    </div>
                    <Textarea
                      rows={11}
                      value={String(currentVal ?? '')}
                      onChange={e => setParamValue(param.name, e.target.value)}
                      className={cn(
                        'font-mono text-xs leading-relaxed',
                        yamlErr && 'border-red-500 focus-visible:ring-red-500',
                      )}
                    />
                    {(yamlErr || param.description) && (
                      <p
                        className={cn(
                          'm-0 text-[11px]',
                          yamlErr ? 'text-red-600 dark:text-red-400' : 'text-muted-foreground',
                        )}
                      >
                        {yamlErr || param.description}
                      </p>
                    )}
                  </div>
                );
              }

              return (
                <div key={param.name} className="space-y-1.5">
                  <label className="block text-xs font-medium text-foreground">
                    {param.label}{' '}
                    <span className="font-mono text-[11px] text-muted-foreground">
                      ({param.name})
                    </span>
                    {param.required && <span className="ml-0.5 text-red-500">*</span>}
                  </label>
                  <Input
                    type={
                      param.paramType === 'integer' || param.paramType === 'number'
                        ? 'number'
                        : 'text'
                    }
                    step={param.paramType === 'number' ? '0.1' : undefined}
                    required={param.required}
                    value={
                      currentVal === undefined || currentVal === null ? '' : String(currentVal)
                    }
                    onChange={e => setParamValue(param.name, e.target.value)}
                  />
                  {param.description && (
                    <p className="m-0 text-[11px] text-muted-foreground">{param.description}</p>
                  )}
                </div>
              );
            })}
          </div>
        )}

        {errorMsg && (
          <Alert variant="destructive">
            <AlertCircle />
            <div>
              <AlertTitle>Action Rejected or Failed</AlertTitle>
              <AlertDescription>{errorMsg}</AlertDescription>
            </div>
          </Alert>
        )}

        {result && (
          <div
            className={cn(
              'space-y-3.5 rounded-lg border border-solid p-4',
              result.success
                ? 'border-emerald-500/40 bg-emerald-500/[0.04]'
                : 'border-red-500/40 bg-red-500/[0.04]',
            )}
          >
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div className="flex items-center gap-2">
                <StatusPill
                  label={result.success ? 'Succeeded' : 'Blocked / Failed'}
                  level={result.success ? 'ok' : 'error'}
                />
                <Badge variant={result.dryRun ? 'info' : 'default'}>
                  {result.dryRun ? 'DRY-RUN PREVIEW' : 'LIVE EXECUTION'}
                </Badge>
              </div>
              <span className="text-[11px] text-muted-foreground">
                Executed at {new Date(result.executedAt).toLocaleString()}
              </span>
            </div>

            <div className="text-[13px] font-semibold text-foreground">{result.summary}</div>

            {result.changesOrWarnings.length > 0 && (
              <div className="space-y-1">
                <div className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                  Precondition Checks, Changes &amp; Warnings
                </div>
                <ul className="m-0 list-disc space-y-1 pl-5 text-xs text-foreground">
                  {result.changesOrWarnings.map((item, idx) => (
                    <li key={idx}>{item}</li>
                  ))}
                </ul>
              </div>
            )}

            {outputSummaryRows.length > 0 && (
              <div className="space-y-1.5">
                <div className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                  Structured Execution Telemetry
                </div>
                <div className="overflow-hidden rounded-md border border-solid border-border bg-card">
                  <Table>
                    <TableBody>
                      {outputSummaryRows.map(row => (
                        <TableRow key={row.key}>
                          <TableCell className="w-[34%] font-medium text-muted-foreground">
                            {row.key}
                          </TableCell>
                          <TableCell>
                            <FormattedAttributeValue value={row.value} itemKey={row.key} />
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </div>
              </div>
            )}

            {result.renderedArtifact && (
              <div className="space-y-1.5">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <div className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                    Rendered Declarative Artifact (YAML / iPXE / RDP)
                  </div>
                  <div className="flex items-center gap-1.5">
                    {action.id === 'connect-pinned-rdp' && (
                      <PitsTooltip content={`Download ${entityName}.rdp for mstsc.exe`}>
                        <Button size="xs" variant="outline" onClick={handleDownloadRdp}>
                          <Download className="h-3 w-3" />
                          <span>Download .rdp</span>
                        </Button>
                      </PitsTooltip>
                    )}
                    <PitsTooltip content={copiedArtifact ? 'Copied!' : 'Copy artifact'}>
                      <Button size="xs" variant="outline" onClick={() => void handleCopyArtifact()}>
                        {copiedArtifact ? (
                          <Check className="h-3 w-3 text-emerald-600 dark:text-emerald-400" />
                        ) : (
                          <Copy className="h-3 w-3" />
                        )}
                        <span>{copiedArtifact ? 'Copied' : 'Copy'}</span>
                      </Button>
                    </PitsTooltip>
                  </div>
                </div>
                <pre className="pits-scroll m-0 max-h-80 overflow-auto rounded-md border border-solid border-border bg-zinc-950 p-3.5 font-mono text-xs leading-relaxed text-zinc-200">
                  {result.renderedArtifact}
                </pre>
              </div>
            )}
          </div>
        )}
      </div>
    </PitsDialog>
  );
};
