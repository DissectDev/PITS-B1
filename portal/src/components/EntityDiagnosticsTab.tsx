import React, { useState } from 'react';
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
  Paper,
  Stack,
  Table,
  TableBody,
  TableCell,
  TableContainer,
  TableHead,
  TableRow,
  Typography,
} from '@mui/material';
import RefreshIcon from '@mui/icons-material/Refresh';
import ContentCopyIcon from '@mui/icons-material/ContentCopy';
import CheckIcon from '@mui/icons-material/Check';
import { useEntity } from '@backstage/plugin-catalog-react';
import { usePitsEntityDetail } from '../hooks/usePitsEntityDetail';
import { FormattedAttributeValue, StatusPill } from './StatusPill';

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
    return (
      <Typography variant="body2" color="text.secondary">
        —
      </Typography>
    );
  }
  if (typeof value === 'boolean') {
    return (
      <StatusPill
        label={value ? 'TRUE' : 'FALSE'}
        level={value ? 'ok' : 'warn'}
      />
    );
  }
  if (typeof value === 'number') {
    return (
      <Typography variant="body2" sx={{ fontFamily: 'monospace', fontWeight: 600 }}>
        {value.toLocaleString()}
      </Typography>
    );
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
      return (
        <Typography variant="body2" color="text.secondary">
          None
        </Typography>
      );
    }
    if (value.every(isPrimitive)) {
      return (
        <Stack direction="row" spacing={0.5} flexWrap="wrap" useFlexGap>
          {value.map((item, idx) => (
            <Chip
              key={idx}
              size="small"
              variant="outlined"
              label={String(item)}
              sx={{ fontFamily: 'monospace', fontSize: '0.73rem' }}
            />
          ))}
        </Stack>
      );
    }
    return <Chip size="small" label={`${value.length} records`} variant="outlined" />;
  }
  if (typeof value === 'object') {
    const entries = Object.entries(value as Record<string, unknown>).filter(
      ([, v]) => v !== null && v !== undefined,
    );
    return (
      <Stack direction="row" spacing={0.75} flexWrap="wrap" useFlexGap>
        {entries.slice(0, 6).map(([k, v]) => (
          <Chip
            key={k}
            size="small"
            variant="outlined"
            label={`${k}: ${isPrimitive(v) ? String(v) : '...'}`}
            sx={{ fontFamily: 'monospace', fontSize: '0.72rem' }}
          />
        ))}
      </Stack>
    );
  }
  return <Typography variant="body2">{String(value)}</Typography>;
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
    <Paper variant="outlined" sx={{ overflow: 'hidden' }}>
      <Stack
        direction="row"
        justifyContent="space-between"
        alignItems="center"
        sx={{
          px: 2,
          py: 1,
          bgcolor: 'action.hover',
          borderBottom: '1px solid',
          borderColor: 'divider',
        }}
      >
        <Typography variant="subtitle2" sx={{ fontWeight: 700 }}>
          {title}
        </Typography>
        <Button
          size="small"
          variant="outlined"
          startIcon={copied ? <CheckIcon /> : <ContentCopyIcon />}
          onClick={handleCopy}
        >
          {copied ? 'Copied' : 'Copy Trace'}
        </Button>
      </Stack>
      <Box
        component="pre"
        sx={{
          m: 0,
          p: 2,
          maxHeight: 360,
          overflow: 'auto',
          bgcolor: '#0f172a',
          color: '#e2e8f0',
          fontFamily: '"JetBrains Mono", "Cascadia Code", Consolas, monospace',
          fontSize: '0.78rem',
          lineHeight: 1.5,
        }}
      >
        {content}
      </Box>
    </Paper>
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
    <Paper variant="outlined" sx={{ overflow: 'hidden' }}>
      <Stack
        direction="row"
        justifyContent="space-between"
        alignItems="center"
        sx={{
          px: 2,
          py: 1,
          bgcolor: 'action.hover',
          borderBottom: '1px solid',
          borderColor: 'divider',
        }}
      >
        <Typography variant="subtitle2" sx={{ fontWeight: 700 }}>
          {title}
        </Typography>
        <Chip size="small" variant="outlined" label={`${rows.length} entries`} />
      </Stack>
      <TableContainer sx={{ maxHeight: 420 }}>
        <Table size="small" stickyHeader>
          <TableHead>
            <TableRow>
              {columns.map(col => (
                <TableCell key={col} sx={{ fontWeight: 700, whiteSpace: 'nowrap' }}>
                  {humanizeKey(col)}
                </TableCell>
              ))}
            </TableRow>
          </TableHead>
          <TableBody>
            {rows.map((row, idx) => (
              <TableRow key={idx} hover>
                {columns.map(col => (
                  <TableCell key={col}>
                    <DiagnosticCellValue value={row[col]} fieldKey={col} />
                  </TableCell>
                ))}
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </TableContainer>
    </Paper>
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
    <Stack spacing={2.5}>
      {scalarEntries.length > 0 && (
        <Paper variant="outlined" sx={{ overflow: 'hidden' }}>
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
              {title}
            </Typography>
          </Box>
          <TableContainer>
            <Table size="small">
              <TableBody>
                {scalarEntries.map(([k, v]) => (
                  <TableRow key={k} hover>
                    <TableCell sx={{ width: '35%', fontWeight: 600, color: 'text.secondary' }}>
                      {humanizeKey(k)}
                    </TableCell>
                    <TableCell>
                      <DiagnosticCellValue value={v} fieldKey={k} />
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </TableContainer>
        </Paper>
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
    </Stack>
  );
};

export const EntityDiagnosticsTab: React.FC = () => {
  const { entity } = useEntity();
  const namespace = entity.metadata.namespace || 'default';
  const name = entity.metadata.name;
  const { detail, error, loading, refreshing, refreshLive } = usePitsEntityDetail(namespace, name);

  if (loading && !detail) {
    return (
      <Card variant="outlined">
        <CardContent sx={{ display: 'flex', alignItems: 'center', gap: 2, py: 4 }}>
          <CircularProgress size={24} />
          <Typography variant="body2" color="text.secondary">
            Collecting deep protocol diagnostics for <strong>{name}</strong>...
          </Typography>
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
    <Card variant="outlined">
      <CardHeader
        title="Deep Protocol & Kernel Diagnostics"
        subheader="Structured COSI service telemetry, Hook PCI/IOMMU inventory, GoVPP dataplane counters, NVMe/TCP GPT layout, and X.509 thumbprints"
        action={
          <Button
            size="small"
            variant="outlined"
            startIcon={refreshing ? <CircularProgress size={15} /> : <RefreshIcon />}
            disabled={refreshing}
            onClick={() => void refreshLive()}
          >
            {refreshing ? 'Probing Live...' : 'Probe Live Telemetry'}
          </Button>
        }
      />
      <Divider />
      <CardContent>
        {error && (
          <Alert severity="warning" sx={{ mb: 2 }}>
            {error}
          </Alert>
        )}

        {!hasDiagnostics ? (
          <Alert severity="info">
            No additional deep diagnostic payload reported for <strong>{name}</strong> in current
            snapshot. Click <strong>Probe Live Telemetry</strong> to query the live fleet endpoints.
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
