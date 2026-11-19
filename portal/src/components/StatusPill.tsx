import React, { useState } from 'react';
import {
  AlertTriangle,
  Check,
  CheckCircle2,
  Copy,
  Info,
  XCircle,
} from 'lucide-react';
import type { StatusLevel } from '../types';
import { Badge } from './ui/badge';
import { PitsTooltip } from './ui/pits-primitives';
import { cn } from '../lib/utils';

export type ResolvedStatusTone = 'ok' | 'warn' | 'error' | 'info';

export function inferStatusLevel(
  status?: string,
  explicitLevel?: StatusLevel,
): ResolvedStatusTone {
  if (
    explicitLevel === 'ok' ||
    explicitLevel === 'warn' ||
    explicitLevel === 'error' ||
    explicitLevel === 'info'
  ) {
    return explicitLevel;
  }
  if (!status) return 'info';
  const s = status.toLowerCase();
  if (
    s === 'ok' ||
    s.includes('healthy') ||
    s.includes('ready') ||
    s.includes('running') ||
    s.includes('verified') ||
    s.includes('compliant') ||
    s.includes('bound') ||
    s.includes('active') ||
    s.includes('online') ||
    s === 'true'
  ) {
    return 'ok';
  }
  if (
    s === 'warn' ||
    s.includes('pending') ||
    s.includes('paused') ||
    s.includes('degraded') ||
    s.includes('offline') ||
    s.includes('cached') ||
    s.includes('unbound') ||
    s.includes('warning')
  ) {
    return 'warn';
  }
  if (
    s === 'error' ||
    s.includes('fail') ||
    s.includes('error') ||
    s.includes('invalid') ||
    s.includes('missing') ||
    s.includes('broken')
  ) {
    return 'error';
  }
  return 'info';
}

export const StatusPill: React.FC<{
  label: string;
  level?: StatusLevel;
  size?: 'small' | 'medium';
}> = ({ label, level, size = 'small' }) => {
  const resolved = inferStatusLevel(label, level);
  const iconCls = size === 'medium' ? 'h-3.5 w-3.5' : 'h-3 w-3';

  const Icon =
    resolved === 'ok'
      ? CheckCircle2
      : resolved === 'warn'
        ? AlertTriangle
        : resolved === 'error'
          ? XCircle
          : Info;

  return (
    <Badge
      variant={resolved}
      className={cn(
        'font-medium',
        size === 'medium' && 'px-2.5 py-1 text-xs',
      )}
    >
      <Icon className={iconCls} />
      <span>{label}</span>
    </Badge>
  );
};

const TECHNICAL_IDENTIFIER_RE =
  /^([0-9a-f]{2}(:[0-9a-f]{2}){5}|[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-9]|[0-9a-f]{4}:[0-9a-f]{4}|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|(sha256:)?[0-9a-f]{32,64}|nqn\..+|https?:\/\/.+|\d{1,3}(\.\d{1,3}){3}(:\d+)?(\/\d+)?|\/var\/.+|\/dev\/.+)$/i;

export function looksLikeTechnicalIdentifier(val: string, key?: string): boolean {
  if (!val) return false;
  if (TECHNICAL_IDENTIFIER_RE.test(val.trim())) return true;
  if (key) {
    const k = key.toLowerCase();
    if (
      k.includes('wwid') ||
      k.includes('uuid') ||
      k.includes('mac') ||
      k.includes('pci') ||
      k.includes('bdf') ||
      k.includes('sha') ||
      k.includes('hash') ||
      k.includes('thumbprint') ||
      k.includes('nqn') ||
      k.includes('ip') ||
      k.includes('endpoint') ||
      k.includes('path')
    ) {
      return true;
    }
  }
  return false;
}

export const FormattedAttributeValue: React.FC<{
  value: string;
  monospace?: boolean;
  itemKey?: string;
}> = ({ value, monospace, itemKey }) => {
  const [copied, setCopied] = useState(false);
  const useMono = Boolean(monospace) || looksLikeTechnicalIdentifier(value, itemKey);

  const handleCopy = async (e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard may be unavailable in non-secure contexts
    }
  };

  if (!useMono) {
    return (
      <span className="break-words text-[13px] font-medium text-foreground">
        {value || '—'}
      </span>
    );
  }

  return (
    <span className="pits-ui inline-flex max-w-full items-center gap-1.5 rounded-md border border-solid border-border bg-muted/60 px-2 py-0.5">
      <code className="break-all font-mono text-xs leading-snug text-foreground">
        {value || '—'}
      </code>
      {value && value.length >= 6 && (
        <PitsTooltip content={copied ? 'Copied!' : 'Copy value'}>
          <button
            type="button"
            onClick={handleCopy}
            aria-label="Copy value"
            className="inline-flex h-5 w-5 shrink-0 cursor-pointer items-center justify-center rounded border-0 bg-transparent p-0 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          >
            {copied ? (
              <Check className="h-3 w-3 text-emerald-600 dark:text-emerald-400" />
            ) : (
              <Copy className="h-3 w-3" />
            )}
          </button>
        </PitsTooltip>
      )}
    </span>
  );
};
