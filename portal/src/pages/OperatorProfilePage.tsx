import React, { useCallback, useEffect, useState } from 'react';
import {
  Alert,
  AlertTitle,
  Box,
  Button,
  Card,
  CardContent,
  CardHeader,
  CircularProgress,
  Divider,
  FormControlLabel,
  Grid,
  Paper,
  Stack,
  Switch,
  TextField,
  Typography,
} from '@mui/material';
import SaveIcon from '@mui/icons-material/Save';
import VpnKeyIcon from '@mui/icons-material/VpnKey';
import RefreshIcon from '@mui/icons-material/Refresh';
import ContentCopyIcon from '@mui/icons-material/ContentCopy';
import CheckIcon from '@mui/icons-material/Check';
import SecurityIcon from '@mui/icons-material/Security';
import { Content, Header, HeaderLabel, Page } from '@backstage/core-components';
import {
  fetchOperatorProfile,
  generateOperatorPki,
  saveOperatorProfile,
} from '../api/pitsClient';
import type { GeneratedPkiResult, OperatorProfileState } from '../types';
import { FormattedAttributeValue, StatusPill } from '../components/StatusPill';

export const OperatorProfilePage: React.FC = () => {
  const [profile, setProfile] = useState<OperatorProfileState | null>(null);
  const [sourcingDbUrlInput, setSourcingDbUrlInput] = useState('');
  const [clearSourcingDbUrl, setClearSourcingDbUrl] = useState(false);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [generatingPki, setGeneratingPki] = useState(false);
  const [serverSan, setServerSan] = useState('100.74.25.42,192.168.71.35,pits-core');
  const [generatedPki, setGeneratedPki] = useState<GeneratedPkiResult | null>(null);
  const [copiedField, setCopiedField] = useState<string | null>(null);
  const [feedback, setFeedback] = useState<{
    severity: 'success' | 'error' | 'info';
    message: string;
  } | null>(null);

  const loadProfile = useCallback(async () => {
    setLoading(true);
    try {
      const state = await fetchOperatorProfile();
      setProfile(state);
    } catch (err) {
      setFeedback({
        severity: 'error',
        message: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadProfile();
  }, [loadProfile]);

  const handleSave = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!profile) return;
    setSaving(true);
    setFeedback(null);
    try {
      const res = await saveOperatorProfile({
        activeWorkspaceSource: profile.activeWorkspaceSource,
        ...(clearSourcingDbUrl
          ? { sourcingDbUrl: '' }
          : sourcingDbUrlInput.trim()
            ? { sourcingDbUrl: sourcingDbUrlInput.trim() }
            : {}),
        agentEndpoint: profile.agentEndpoint,
        coreKubeconfigPath: profile.coreKubeconfigPath,
        coreContext: profile.coreContext,
        labKubeconfigPath: profile.labKubeconfigPath,
        labContext: profile.labContext,
        labViaAgentBridge: profile.labViaAgentBridge,
        talosconfigPath: profile.talosconfigPath,
        talosContext: profile.talosContext,
        talosViaAgentBridge: profile.talosViaAgentBridge,
      });
      setSourcingDbUrlInput('');
      setClearSourcingDbUrl(false);
      setFeedback({
        severity: 'success',
        message: `Operator profile saved cleanly${res.path ? ` to ${res.path}` : ''} (outside Syncthing workspace).`,
      });
      await loadProfile();
    } catch (err) {
      setFeedback({
        severity: 'error',
        message: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setSaving(false);
    }
  };

  const handleGeneratePki = async () => {
    setGeneratingPki(true);
    setFeedback(null);
    try {
      const pki = await generateOperatorPki(serverSan);
      setGeneratedPki(pki);
      setFeedback({
        severity: 'success',
        message:
          'Generated Ed25519 Root CA, pits-agent Server Certificate, and pitsctl Operator Client Certificate. Client identity stored securely in local state directory.',
      });
      await loadProfile();
    } catch (err) {
      setFeedback({
        severity: 'error',
        message: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setGeneratingPki(false);
    }
  };

  const copyPem = async (field: string, pem: string) => {
    try {
      await navigator.clipboard.writeText(pem);
      setCopiedField(field);
      setTimeout(() => setCopiedField(null), 1800);
    } catch {
      // Ignore
    }
  };

  return (
    <Page themeId="tool">
      <Header
        title="Operator Connection & Zero-Trust mTLS Profile"
        subtitle="Machine-local credentials stored exclusively in %LOCALAPPDATA%\\PITS\\Client (100% isolated from Syncthing)"
      >
        <HeaderLabel label="Transport" value="TLS 1.3 + Ed25519 mTLS" />
        <HeaderLabel
          label="Client PKI"
          value={profile?.hasClientPki ? 'Configured' : 'Not Configured'}
        />
      </Header>

      <Content>
        <Stack spacing={3}>
          {feedback && (
            <Alert severity={feedback.severity} onClose={() => setFeedback(null)}>
              {feedback.message}
            </Alert>
          )}

          {loading && !profile ? (
            <Box sx={{ display: 'flex', justifyContent: 'center', py: 6 }}>
              <CircularProgress />
            </Box>
          ) : profile ? (
            <Grid container spacing={3}>
              <Grid item xs={12} lg={7}>
                <Card variant="outlined" component="form" onSubmit={handleSave}>
                  <CardHeader
                    title="Control-Plane Endpoints & Kubeconfig Paths"
                    subheader="Configure pits-agent mTLS bridge, RKE2 pits-core, Sidero Talos pits-lab, and COSI endpoints"
                    action={
                      <Button
                        size="small"
                        startIcon={<RefreshIcon />}
                        onClick={() => void loadProfile()}
                      >
                        Reload
                      </Button>
                    }
                  />
                  <Divider />
                  <CardContent>
                    <Stack spacing={2.25}>
                      <Paper variant="outlined" sx={{ p: 1.75, bgcolor: 'action.hover' }}>
                        <Stack
                          direction="row"
                          justifyContent="space-between"
                          alignItems="center"
                          flexWrap="wrap"
                          useFlexGap
                          spacing={1}
                        >
                          <Stack direction="row" spacing={1} alignItems="center">
                            <SecurityIcon color="primary" fontSize="small" />
                            <Typography variant="body2" sx={{ fontWeight: 600 }}>
                              Operator mTLS Client Certificate
                            </Typography>
                          </Stack>
                          <StatusPill
                            label={
                              profile.hasClientPki
                                ? 'Active Ed25519 mTLS Identity'
                                : 'Missing Client PKI'
                            }
                            level={profile.hasClientPki ? 'ok' : 'warn'}
                          />
                        </Stack>
                        {profile.stateDir && (
                          <Box sx={{ mt: 1 }}>
                            <Typography variant="caption" color="text.secondary">
                              Local State Directory:{' '}
                            </Typography>
                            <FormattedAttributeValue value={profile.stateDir} monospace />
                          </Box>
                        )}
                      </Paper>

                      <TextField
                        fullWidth
                        size="small"
                        label="NixOS pits-agent mTLS Endpoint"
                        value={profile.agentEndpoint}
                        onChange={e =>
                          setProfile({ ...profile, agentEndpoint: e.target.value })
                        }
                        helperText="Default Tailscale mTLS endpoint on pits-core (:18443)"
                      />

                      <TextField
                        fullWidth
                        size="small"
                        label="Active Contract Workspace Source (edge-router)"
                        value={profile.activeWorkspaceSource}
                        onChange={e =>
                          setProfile({ ...profile, activeWorkspaceSource: e.target.value })
                        }
                        helperText="Optional local path to edge-router repository for contract & manifest overlays"
                      />

                      <TextField
                        fullWidth
                        size="small"
                        type="password"
                        label="Sourcing DB PostgreSQL Sync URL (postgres://...)"
                        placeholder={
                          profile.sourcingDbConfigured
                            ? `Configured (${profile.sourcingDbEndpointRedacted ?? 'redacted'}) — leave blank to keep`
                            : 'postgres://user:pass@host:5432/pits_sourcing'
                        }
                        value={sourcingDbUrlInput}
                        disabled={clearSourcingDbUrl}
                        onChange={e => setSourcingDbUrlInput(e.target.value)}
                        helperText="Optional PostgreSQL v3 primary DB for bi-directional outbox sync (SQLite WAL remains local SSOT cache). Plaintext URL is never returned on GET."
                      />
                      {profile.sourcingDbConfigured && (
                        <FormControlLabel
                          control={
                            <Switch
                              size="small"
                              checked={clearSourcingDbUrl}
                              onChange={e => setClearSourcingDbUrl(e.target.checked)}
                            />
                          }
                          label="Clear saved PostgreSQL URL on save (revert to local SQLite WAL only)"
                        />
                      )}

                      <Divider />

                      <Grid container spacing={2}>
                        <Grid item xs={12} sm={8}>
                          <TextField
                            fullWidth
                            size="small"
                            label="pits-core RKE2 Kubeconfig Path"
                            value={profile.coreKubeconfigPath}
                            onChange={e =>
                              setProfile({ ...profile, coreKubeconfigPath: e.target.value })
                            }
                          />
                        </Grid>
                        <Grid item xs={12} sm={4}>
                          <TextField
                            fullWidth
                            size="small"
                            label="pits-core Context"
                            value={profile.coreContext}
                            onChange={e =>
                              setProfile({ ...profile, coreContext: e.target.value })
                            }
                          />
                        </Grid>

                        <Grid item xs={12} sm={8}>
                          <TextField
                            fullWidth
                            size="small"
                            label="pits-lab Talos K8s Kubeconfig Path"
                            value={profile.labKubeconfigPath}
                            onChange={e =>
                              setProfile({ ...profile, labKubeconfigPath: e.target.value })
                            }
                          />
                        </Grid>
                        <Grid item xs={12} sm={4}>
                          <TextField
                            fullWidth
                            size="small"
                            label="pits-lab Context"
                            value={profile.labContext}
                            onChange={e =>
                              setProfile({ ...profile, labContext: e.target.value })
                            }
                          />
                        </Grid>

                        <Grid item xs={12}>
                          <FormControlLabel
                            control={
                              <Switch
                                checked={profile.labViaAgentBridge}
                                onChange={e =>
                                  setProfile({
                                    ...profile,
                                    labViaAgentBridge: e.target.checked,
                                  })
                                }
                              />
                            }
                            label="Tunnel pits-lab K8s API (VLAN 20) via pits-agent mTLS HTTP CONNECT bridge"
                          />
                        </Grid>

                        <Grid item xs={12} sm={8}>
                          <TextField
                            fullWidth
                            size="small"
                            label="Sidero Talosconfig Path"
                            value={profile.talosconfigPath}
                            onChange={e =>
                              setProfile({ ...profile, talosconfigPath: e.target.value })
                            }
                          />
                        </Grid>
                        <Grid item xs={12} sm={4}>
                          <TextField
                            fullWidth
                            size="small"
                            label="Talos Context"
                            value={profile.talosContext}
                            onChange={e =>
                              setProfile({ ...profile, talosContext: e.target.value })
                            }
                          />
                        </Grid>

                        <Grid item xs={12}>
                          <FormControlLabel
                            control={
                              <Switch
                                checked={profile.talosViaAgentBridge}
                                onChange={e =>
                                  setProfile({
                                    ...profile,
                                    talosViaAgentBridge: e.target.checked,
                                  })
                                }
                              />
                            }
                            label="Tunnel Talos COSI/gRPC (:50000 on netboot-001..004) via pits-agent mTLS bridge"
                          />
                        </Grid>
                      </Grid>

                      <Box sx={{ display: 'flex', justifyContent: 'flex-end', pt: 1 }}>
                        <Button
                          type="submit"
                          variant="contained"
                          disabled={saving}
                          startIcon={
                            saving ? <CircularProgress size={16} color="inherit" /> : <SaveIcon />
                          }
                        >
                          {saving ? 'Saving Profile...' : 'Save Operator Profile'}
                        </Button>
                      </Box>
                    </Stack>
                  </CardContent>
                </Card>
              </Grid>

              <Grid item xs={12} lg={5}>
                <Card variant="outlined">
                  <CardHeader
                    title="Ed25519 mTLS PKI Provisioner"
                    subheader="Generate Root CA + Server/Client Leaf Certificates for zero-trust pits-agent enrollment"
                  />
                  <Divider />
                  <CardContent>
                    <Stack spacing={2}>
                      <Alert severity="info">
                        <AlertTitle sx={{ fontWeight: 700 }}>Syncthing Hygiene Guard</AlertTitle>
                        Client private keys are saved directly into{' '}
                        <code>%LOCALAPPDATA%\PITS\Client\profile.yaml</code> and are never written
                        to the repository workspace or returned by GET APIs.
                      </Alert>

                      <TextField
                        fullWidth
                        size="small"
                        label="Server IP / DNS SANs (comma-separated)"
                        value={serverSan}
                        onChange={e => setServerSan(e.target.value)}
                        helperText="Included in pits-agent X.509 SubjectAltName extension"
                      />

                      <Button
                        variant="outlined"
                        color="warning"
                        disabled={generatingPki}
                        startIcon={
                          generatingPki ? <CircularProgress size={16} /> : <VpnKeyIcon />
                        }
                        onClick={() => void handleGeneratePki()}
                      >
                        {generatingPki
                          ? 'Generating Ed25519 PKI...'
                          : 'Rotate & Generate New mTLS PKI'}
                      </Button>

                      {generatedPki && (
                        <Stack spacing={1.5} sx={{ mt: 1 }}>
                          {[
                            { key: 'ca', label: 'Root CA Certificate (ca.crt)', pem: generatedPki.caCertPem },
                            {
                              key: 'srvCert',
                              label: 'pits-agent Server Certificate (server.crt)',
                              pem: generatedPki.serverCertPem,
                            },
                            {
                              key: 'srvKey',
                              label: 'pits-agent Server Key (server.key — copy once to pits-core)',
                              pem: generatedPki.serverKeyPem,
                            },
                          ].map(item => (
                            <Paper key={item.key} variant="outlined" sx={{ overflow: 'hidden' }}>
                              <Stack
                                direction="row"
                                justifyContent="space-between"
                                alignItems="center"
                                sx={{ px: 1.5, py: 0.75, bgcolor: 'action.hover' }}
                              >
                                <Typography variant="caption" sx={{ fontWeight: 700 }}>
                                  {item.label}
                                </Typography>
                                <Button
                                  size="small"
                                  startIcon={
                                    copiedField === item.key ? <CheckIcon /> : <ContentCopyIcon />
                                  }
                                  onClick={() => void copyPem(item.key, item.pem)}
                                >
                                  {copiedField === item.key ? 'Copied' : 'Copy'}
                                </Button>
                              </Stack>
                              <Box
                                component="pre"
                                sx={{
                                  m: 0,
                                  p: 1.25,
                                  maxHeight: 120,
                                  overflow: 'auto',
                                  bgcolor: '#0f172a',
                                  color: '#e2e8f0',
                                  fontFamily: 'monospace',
                                  fontSize: '0.72rem',
                                }}
                              >
                                {item.pem}
                              </Box>
                            </Paper>
                          ))}
                        </Stack>
                      )}
                    </Stack>
                  </CardContent>
                </Card>
              </Grid>
            </Grid>
          ) : null}
        </Stack>
      </Content>
    </Page>
  );
};
