import React, { useState } from 'react';
import {
  AlertTriangle,
  Check,
  Copy,
  Info,
  Loader2,
  RefreshCw,
} from 'lucide-react';
import { useEntity } from '@backstage/plugin-catalog-react';
import { usePitsEntityDetail } from '../hooks/usePitsEntityDetail';
import { FormattedAttributeValue, StatusPill } from './StatusPill';
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
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from './ui/table';

function humanizeKey(raw: string): string {
  return raw
    .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
    .replace(/[_-]+/g, ' ')
    .replace(/\b\w/g, c => c.toUpperCase());
}

function isPrimitive(val: unknown): val is string | number | boolean {
  return typeof val === 'string' || typeof val === 'number' || typeof val === 'boolean';
}

const DiagnosticCellValue: React.FC<{ value: unknown; fieldKey?: string }> = ({
  value,
  fieldKey,
}) => {
  if (value === null || value === undefined || value === '') {
    return <span className="text-xs text-muted-foreground">—</span>;
  }
  if (typeof value === 'boolean') {
    return <StatusPill label={value ? 'TRUE' : 'FALSE'} level={value ? 'ok' : 'warn'} />;
  }
  if (typeof value === 'number') {
    return <span className="font-mono text-xs font-semibold">{value.toLocaleString()}</span>;
  }
  if (typeof value === 'string') {
    const lower = value.toLowerCase();
    if (
      ['running', 'healthy', 'ok', 'ready', 'verified', 'bound', 'enabled', 'compliant'].includes(
        lower,
      )
    ) {
      return <StatusPill label={value} level="ok" />;
    }
    if (
      ['pending', 'degraded', 'warn', 'warning', 'paused', 'unbound', 'offline'].includes(lower)
    ) {
      return <StatusPill label={value} level="warn" />;
    }
    if (['failed', 'error', 'invalid', 'missing'].includes(lower)) {
      return <StatusPill label={value} level="error" />;
    }
    return <FormattedAttributeValue value={value} itemKey={fieldKey} />;
  }
  if (Array.isArray(value)) {
    if (value.length === 0) {
      return <span className="text-xs text-muted-foreground">None</span>;
    }
    if (value.every(isPrimitive)) {
      return (
        <div className="flex flex-wrap gap-1">
          {value.map((item, idx) => (
            <Badge key={idx} variant="muted" className="font-mono text-[11px]">
              {String(item)}
            </Badge>
          ))}
        </div>
      );
    }
    return <Badge variant="outline">{value.length} records</Badge>;
  }
  if (typeof value === 'object') {
    const entries = Object.entries(value as Record<string, unknown>).filter(
      ([, v]) => v !== null && v !== undefined,
    );
    return (
      <div className="flex flex-wrap gap-1">
        {entries.slice(0, 6).map(([k, v]) => (
          <Badge key={k} variant="muted" className="font-mono text-[11px]">
            {k}: {isPrimitive(v) ? String(v) : '...'}
          </Badge>
        ))}
      </div>
    );
  }
  return <span className="text-xs">{String(value)}</span>;
};

const TraceTextBlock: React.FC<{ title: string; content: string }> = ({ title, content }) => {
  const [copied, setCopied] = useState(false);
  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(content);
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch {
      // Ignore
    }
  };

  return (
    <div className="overflow-hidden rounded-lg border border-solid border-border">
      <div className="flex items-center justify-between gap-2 border-b border-solid border-border bg-muted/50 px-4 py-2">
        <span className="text-xs font-semibold text-foreground">{title}</span>
        <Button size="xs" variant="outline" onClick={() => void handleCopy()}>
          {copied ? (
            <Check className="h-3 w-3 text-emerald-600 dark:text-emerald-400" />
          ) : (
            <Copy className="h-3 w-3" />
          )}
          <span>{copied ? 'Copied' : 'Copy Trace'}</span>
        </Button>
      </div>
      <pre className="pits-scroll m-0 max-h-[360px] overflow-auto bg-zinc-950 p-4 font-mono text-xs leading-relaxed text-zinc-200">
        {content}
      </pre>
    </div>
  );
};

const ObjectArrayTableCard: React.FC<{
  title: string;
  rows: Array<Record<string, unknown>>;
}> = ({ title, rows }) => {
  const columns = Array.from(
    rows.reduce((acc, row) => {
      Object.keys(row).forEach(k => acc.add(k));
      return acc;
    }, new Set<string>()),
  ).slice(0, 10);

  return (
    <div className="overflow-hidden rounded-lg border border-solid border-border">
      <div className="flex items-center justify-between gap-2 border-b border-solid border-border bg-muted/50 px-4 py-2">
        <span className="text-xs font-semibold text-foreground">{title}</span>
        <Badge variant="outline">{rows.length} entries</Badge>
      </div>
      <div className="pits-scroll max-h-[420px] overflow-auto">
        <Table>
          <TableHeader>
            <TableRow>
              {columns.map(col => (
                <TableHead key={col}>{humanizeKey(col)}</TableHead>
              ))}
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((row, idx) => (
              <TableRow key={idx}>
                {columns.map(col => (
                  <TableCell key={col}>
                    <DiagnosticCellValue value={row[col]} fieldKey={col} />
                  </TableCell>
                ))}
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
    </div>
  );
};

const StructuredDiagnosticsSection: React.FC<{
  title: string;
  data: Record<string, unknown>;
}> = ({ title, data }) => {
  const scalarEntries: Array<[string, unknown]> = [];
  const nestedSections: Array<[string, unknown]> = [];

  for (const [k, v] of Object.entries(data)) {
    if (v === null || v === undefined) continue;
    if (typeof v === 'string' && v.includes('\n') && v.length > 80) {
      nestedSections.push([k, v]);
    } else if (Array.isArray(v) && v.length > 0 && typeof v[0] === 'object' && v[0] !== null) {
      nestedSections.push([k, v]);
    } else if (typeof v === 'object' && !Array.isArray(v) && Object.keys(v as object).length > 0) {
      nestedSections.push([k, v]);
    } else {
      scalarEntries.push([k, v]);
    }
  }

  return (
    <div className="space-y-4">
      {scalarEntries.length > 0 && (
        <div className="overflow-hidden rounded-lg border border-solid border-border">
          <div className="border-b border-solid border-border bg-muted/50 px-4 py-2 text-xs font-semibold text-foreground">
            {title}
          </div>
          <Table>
            <TableBody>
              {scalarEntries.map(([k, v]) => (
                <TableRow key={k}>
                  <TableCell className="w-[35%] font-medium text-muted-foreground">
                    {humanizeKey(k)}
                  </TableCell>
                  <TableCell>
                    <DiagnosticCellValue value={v} fieldKey={k} />
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}

      {nestedSections.map(([k, v]) => {
        const subTitle = humanizeKey(k);
        if (typeof v === 'string') {
          return <TraceTextBlock key={k} title={subTitle} content={v} />;
        }
        if (Array.isArray(v)) {
          return (
            <ObjectArrayTableCard
              key={k}
              title={subTitle}
              rows={v as Array<Record<string, unknown>>}
            />
          );
        }
        if (typeof v === 'object' && v !== null) {
          return (
            <StructuredDiagnosticsSection
              key={k}
              title={subTitle}
              data={v as Record<string, unknown>}
            />
          );
        }
        return null;
      })}
    </div>
  );
};

export const EntityDiagnosticsTab: React.FC = () => {
  const { entity } = useEntity();
  const namespace = entity.metadata.namespace || 'default';
  const name = entity.metadata.name;
  const { detail, error, loading, refreshing, refreshLive } = usePitsEntityDetail(namespace, name);

  if (loading && !detail) {
    return (
      <Card>
        <CardContent className="flex items-center gap-3 p-6">
          <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
          <span className="text-xs text-muted-foreground">
            Collecting deep protocol diagnostics for <strong className="text-foreground">{name}</strong>
            ...
          </span>
        </CardContent>
      </Card>
    );
  }

  const diagnostics = detail?.diagnostics;
  const hasDiagnostics =
    diagnostics !== null &&
    diagnostics !== undefined &&
    (typeof diagnostics !== 'object' || Object.keys(diagnostics as object).length > 0);

  return (
    <Card>
      <CardHeader className="flex flex-col gap-3 border-b border-solid border-border sm:flex-row sm:items-center sm:justify-between">
        <div className="space-y-1">
          <CardTitle>Deep Protocol &amp; Kernel Diagnostics</CardTitle>
          <CardDescription>
            Structured COSI service telemetry, Hook PCI/IOMMU inventory, GoVPP dataplane counters,
            NVMe/TCP GPT layout, and X.509 thumbprints
          </CardDescription>
        </div>
        <Button
          size="sm"
          variant="outline"
          disabled={refreshing}
          onClick={() => void refreshLive()}
          className="shrink-0"
        >
          {refreshing ? (
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
          ) : (
            <RefreshCw className="h-3.5 w-3.5" />
          )}
          <span>{refreshing ? 'Probing Live...' : 'Probe Live Telemetry'}</span>
        </Button>
      </CardHeader>

      <CardContent className="space-y-4 p-5">
        {error && (
          <Alert variant="warning">
            <AlertTriangle />
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        {!hasDiagnostics ? (
          <Alert variant="info">
            <Info />
            <AlertDescription>
              No additional deep diagnostic payload reported for <strong>{name}</strong> in current
              snapshot. Click <strong>Probe Live Telemetry</strong> to query the live fleet
              endpoints.
            </AlertDescription>
          </Alert>
        ) : typeof diagnostics === 'string' ? (
          <TraceTextBlock title="Diagnostic Output" content={diagnostics} />
        ) : Array.isArray(diagnostics) ? (
          <ObjectArrayTableCard
            title="Diagnostic Records"
            rows={diagnostics as Array<Record<string, unknown>>}
          />
        ) : (
          <StructuredDiagnosticsSection
            title="Diagnostic Telemetry Summary"
            data={diagnostics as Record<string, unknown>}
          />
        )}
      </CardContent>
    </Card>
  );
};
