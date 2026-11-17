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
  List,
  ListItemButton,
  ListItemIcon,
  ListItemText,
  Paper,
  Stack,
  Typography,
} from '@mui/material';
import DescriptionOutlinedIcon from '@mui/icons-material/DescriptionOutlined';
import ContentCopyIcon from '@mui/icons-material/ContentCopy';
import CheckIcon from '@mui/icons-material/Check';
import DownloadIcon from '@mui/icons-material/Download';
import RefreshIcon from '@mui/icons-material/Refresh';
import YAML from 'yaml';
import { useEntity } from '@backstage/plugin-catalog-react';
import { usePitsEntityDetail } from '../hooks/usePitsEntityDetail';

export const EntityManifestsTab: React.FC = () => {
  const { entity } = useEntity();
  const namespace = entity.metadata.namespace || 'default';
  const name = entity.metadata.name;
  const { detail, error, loading, refreshing, refreshLive } = usePitsEntityDetail(namespace, name);

  const manifestEntries = useMemo(() => {
    const map = new Map<string, string>();
    const rawManifests = detail?.manifests ?? {};
    for (const [k, v] of Object.entries(rawManifests)) {
      if (typeof v === 'string' && v.trim().length > 0) {
        map.set(k, v);
      }
    }
    if (!map.has('catalog-entity.yaml')) {
      try {
        map.set('catalog-entity.yaml', YAML.stringify(detail?.entity ?? entity));
      } catch {
        // Ignore serialization failure
      }
    }
    return Array.from(map.entries());
  }, [detail, entity]);

  const [selectedKey, setSelectedKey] = useState<string>('');
  const [copied, setCopied] = useState(false);

  const activeEntry = useMemo(() => {
    if (manifestEntries.length === 0) return null;
    return (
      manifestEntries.find(([k]) => k === selectedKey) ?? manifestEntries[0]
    );
  }, [manifestEntries, selectedKey]);

  const handleCopy = async () => {
    if (!activeEntry) return;
    try {
      await navigator.clipboard.writeText(activeEntry[1]);
      setCopied(true);
      setTimeout(() => setCopied(false), 1800);
    } catch {
      // Clipboard unavailable
    }
  };

  const handleDownload = () => {
    if (!activeEntry) return;
    const blob = new Blob([activeEntry[1]], { type: 'text/plain;charset=utf-8' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `${name}-${activeEntry[0]}`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  };

  if (loading && !detail) {
    return (
      <Card variant="outlined">
        <CardContent sx={{ display: 'flex', alignItems: 'center', gap: 2, py: 4 }}>
          <CircularProgress size={24} />
          <Typography variant="body2" color="text.secondary">
            Rendering declarative manifests for <strong>{name}</strong>...
          </Typography>
        </CardContent>
      </Card>
    );
  }

  const lineCount = activeEntry ? activeEntry[1].split('\n').length : 0;

  return (
    <Card variant="outlined">
      <CardHeader
        title="Declarative Manifests & Rendered Configurations"
        subheader="Backstage Entity Registry YAML and domain-specific Kubernetes, Talos, EdgeRouter, DRA, iPXE, or Sourcing Audit artifacts"
        action={
          <Button
            size="small"
            variant="outlined"
            startIcon={refreshing ? <CircularProgress size={15} /> : <RefreshIcon />}
            disabled={refreshing}
            onClick={() => void refreshLive()}
          >
            {refreshing ? 'Refreshing...' : 'Refresh Manifests'}
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

        {manifestEntries.length === 0 || !activeEntry ? (
          <Typography variant="body2" color="text.secondary">
            No manifests available for this entity.
          </Typography>
        ) : (
          <Box
            sx={{
              display: 'grid',
              gridTemplateColumns: { xs: '1fr', md: '270px 1fr' },
              gap: 2.5,
              alignItems: 'start',
            }}
          >
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
                <Typography variant="caption" sx={{ fontWeight: 700, textTransform: 'uppercase' }}>
                  Artifacts ({manifestEntries.length})
                </Typography>
              </Box>
              <List dense disablePadding>
                {manifestEntries.map(([fileKey, content]) => {
                  const selected = activeEntry[0] === fileKey;
                  const lines = content.split('\n').length;
                  return (
                    <ListItemButton
                      key={fileKey}
                      selected={selected}
                      onClick={() => setSelectedKey(fileKey)}
                      sx={{ py: 1 }}
                    >
                      <ListItemIcon sx={{ minWidth: 32 }}>
                        <DescriptionOutlinedIcon fontSize="small" color={selected ? 'primary' : 'inherit'} />
                      </ListItemIcon>
                      <ListItemText
                        primary={fileKey}
                        secondary={`${lines} lines`}
                        primaryTypographyProps={{
                          sx: {
                            fontFamily: 'monospace',
                            fontSize: '0.8rem',
                            fontWeight: selected ? 700 : 500,
                            wordBreak: 'break-all',
                          },
                        }}
                      />
                    </ListItemButton>
                  );
                })}
              </List>
            </Paper>

            <Paper variant="outlined" sx={{ overflow: 'hidden' }}>
              <Stack
                direction="row"
                justifyContent="space-between"
                alignItems="center"
                flexWrap="wrap"
                useFlexGap
                spacing={1}
                sx={{
                  px: 2,
                  py: 1,
                  bgcolor: 'action.hover',
                  borderBottom: '1px solid',
                  borderColor: 'divider',
                }}
              >
                <Stack direction="row" spacing={1} alignItems="center">
                  <Typography
                    variant="subtitle2"
                    sx={{ fontFamily: 'monospace', fontWeight: 700 }}
                  >
                    {activeEntry[0]}
                  </Typography>
                  <Chip size="small" variant="outlined" label={`${lineCount} lines`} />
                </Stack>
                <Stack direction="row" spacing={1}>
                  <Button
                    size="small"
                    variant="outlined"
                    startIcon={copied ? <CheckIcon /> : <ContentCopyIcon />}
                    onClick={handleCopy}
                  >
                    {copied ? 'Copied!' : 'Copy'}
                  </Button>
                  <Button
                    size="small"
                    variant="outlined"
                    startIcon={<DownloadIcon />}
                    onClick={handleDownload}
                  >
                    Download
                  </Button>
                </Stack>
              </Stack>

              <Box
                component="pre"
                sx={{
                  m: 0,
                  p: 2,
                  maxHeight: '68vh',
                  overflow: 'auto',
                  bgcolor: '#0f172a',
                  color: '#e2e8f0',
                  fontFamily: '"JetBrains Mono", "Cascadia Code", Consolas, monospace',
                  fontSize: '0.8rem',
                  lineHeight: 1.55,
                }}
              >
                {activeEntry[1]}
              </Box>
            </Paper>
          </Box>
        )}
      </CardContent>
    </Card>
  );
};
