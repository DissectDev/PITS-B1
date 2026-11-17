import React, { useState } from 'react';
import { Box, Chip, IconButton, Tooltip, Typography } from '@mui/material';
import CheckCircleOutlineIcon from '@mui/icons-material/CheckCircleOutline';
import WarningAmberIcon from '@mui/icons-material/WarningAmber';
import ErrorOutlineIcon from '@mui/icons-material/ErrorOutline';
import InfoOutlinedIcon from '@mui/icons-material/InfoOutlined';
import ContentCopyIcon from '@mui/icons-material/ContentCopy';
import CheckIcon from '@mui/icons-material/Check';
import type { StatusLevel } from '../types';

export function inferStatusLevel(status?: string, explicitLevel?: StatusLevel): StatusLevel {
  if (explicitLevel && ['ok', 'warn', 'error', 'info'].includes(explicitLevel)) {
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

  if (resolved === 'ok') {
    return (
      <Chip
        size={size}
        icon={<CheckCircleOutlineIcon />}
        label={label}
        sx={{
          fontWeight: 600,
          bgcolor: 'rgba(46, 125, 50, 0.14)',
          color: '#2e7d32',
          border: '1px solid rgba(46, 125, 50, 0.35)',
          '& .MuiChip-icon': { color: '#2e7d32' },
        }}
      />
    );
  }
  if (resolved === 'warn') {
    return (
      <Chip
        size={size}
        icon={<WarningAmberIcon />}
        label={label}
        sx={{
          fontWeight: 600,
          bgcolor: 'rgba(237, 108, 2, 0.14)',
          color: '#ed6c02',
          border: '1px solid rgba(237, 108, 2, 0.38)',
          '& .MuiChip-icon': { color: '#ed6c02' },
        }}
      />
    );
  }
  if (resolved === 'error') {
    return (
      <Chip
        size={size}
        icon={<ErrorOutlineIcon />}
        label={label}
        sx={{
          fontWeight: 600,
          bgcolor: 'rgba(211, 47, 47, 0.14)',
          color: '#d32f2f',
          border: '1px solid rgba(211, 47, 47, 0.38)',
          '& .MuiChip-icon': { color: '#d32f2f' },
        }}
      />
    );
  }
  return (
    <Chip
      size={size}
      icon={<InfoOutlinedIcon />}
      label={label}
      sx={{
        fontWeight: 600,
        bgcolor: 'rgba(2, 136, 209, 0.12)',
        color: '#0288d1',
        border: '1px solid rgba(2, 136, 209, 0.32)',
        '& .MuiChip-icon': { color: '#0288d1' },
      }}
    />
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
      <Typography variant="body2" sx={{ wordBreak: 'break-word', fontWeight: 500 }}>
        {value || '—'}
      </Typography>
    );
  }

  return (
    <Box
      sx={{
        display: 'inline-flex',
        alignItems: 'center',
        gap: 0.5,
        maxWidth: '100%',
        bgcolor: 'action.hover',
        px: 1,
        py: 0.25,
        borderRadius: 1,
        border: '1px solid',
        borderColor: 'divider',
      }}
    >
      <Typography
        component="code"
        sx={{
          fontFamily: '"JetBrains Mono", "Cascadia Code", Consolas, monospace',
          fontSize: '0.79rem',
          wordBreak: 'break-all',
          lineHeight: 1.45,
        }}
      >
        {value || '—'}
      </Typography>
      {value && value.length >= 6 && (
        <Tooltip title={copied ? 'Copied!' : 'Copy value'}>
          <IconButton size="small" onClick={handleCopy} sx={{ p: 0.25 }}>
            {copied ? (
              <CheckIcon sx={{ fontSize: 13, color: 'success.main' }} />
            ) : (
              <ContentCopyIcon sx={{ fontSize: 13, opacity: 0.65 }} />
            )}
          </IconButton>
        </Tooltip>
      )}
    </Box>
  );
};
