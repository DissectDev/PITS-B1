import React, { useCallback, useEffect, useState } from 'react';
import {
  AlertCircle,
  Check,
  CheckCircle2,
  Copy,
  Info,
  KeyRound,
  Loader2,
  Plus,
  RefreshCw,
  Save,
  Shield,
  Trash2,
  X,
} from 'lucide-react';
import {
  fetchOperatorProfile,
  generateOperatorPki,
  saveOperatorProfile,
} from '../api/pitsClient';
import type { GeneratedPkiResult, OperatorProfileState } from '../types';
import { FormattedAttributeValue, StatusPill } from '../components/StatusPill';
import { PageBody, PageHeader, PageShell } from '../components/ui/page-shell';
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '../components/ui/card';
import { Button } from '../components/ui/button';
import { Input } from '../components/ui/input';
import { Alert, AlertDescription, AlertTitle } from '../components/ui/alert';
import { Separator } from '../components/ui/separator';
import { PitsSwitch } from '../components/ui/pits-primitives';

const FieldRow: React.FC<{
  label: string;
  helper?: string;
  children: React.ReactNode;
}> = ({ label, helper, children }) => (
  <div className="space-y-1.5">
    <label className="block text-xs font-medium text-foreground">{label}</label>
    {children}
    {helper && <p className="m-0 text-[11px] leading-snug text-muted-foreground">{helper}</p>}
  </div>
);

export const OperatorProfilePage: React.FC = () => {
  const [profile, setProfile] = useState<OperatorProfileState | null>(null);
  const [sourcingDbUrlInput, setSourcingDbUrlInput] = useState('');
  const [clearSourcingDbUrl, setClearSourcingDbUrl] = useState(false);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [generatingPki, setGeneratingPki] = useState(false);
  const [serverSan, setServerSan] = useState('100.95.95.88,192.168.71.35,pits-core');
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
        extraClusters: profile.extraClusters,
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
    <PageShell>
      <PageHeader
        title="Operator Connection & Zero-Trust mTLS Profile"
        description="Machine-local credentials stored exclusively in %LOCALAPPDATA%\PITS\Client (100% isolated from Syncthing)"
        meta={[
          { label: 'Transport', value: 'TLS 1.3 + Ed25519 mTLS' },
          {
            label: 'Client PKI',
            value: profile?.hasClientPki ? 'Configured' : 'Not Configured',
          },
        ]}
      />

      <PageBody className="space-y-6">
        {feedback && (
          <Alert
            variant={
              feedback.severity === 'error'
                ? 'destructive'
                : feedback.severity === 'success'
                  ? 'success'
                  : 'info'
            }
          >
            {feedback.severity === 'error' ? (
              <AlertCircle />
            ) : feedback.severity === 'success' ? (
              <CheckCircle2 />
            ) : (
              <Info />
            )}
            <AlertDescription className="flex-1">{feedback.message}</AlertDescription>
            <button
              type="button"
              onClick={() => setFeedback(null)}
              aria-label="Dismiss message"
              className="inline-flex h-5 w-5 cursor-pointer items-center justify-center rounded border-0 bg-transparent text-current opacity-70 hover:opacity-100"
            >
              <X className="h-3.5 w-3.5" />
            </button>
          </Alert>
        )}

        {loading && !profile ? (
          <div className="flex items-center justify-center py-16">
            <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
          </div>
        ) : profile ? (
          <div className="grid grid-cols-1 gap-6 lg:grid-cols-12">
            {/* Left column: Endpoints & Kubeconfigs */}
            <div className="lg:col-span-7">
              <Card>
                <form onSubmit={handleSave}>
                  <CardHeader className="flex flex-row items-start justify-between gap-4 border-b border-solid border-border">
                    <div className="space-y-1">
                      <CardTitle>Control-Plane Endpoints &amp; Kubeconfig Paths</CardTitle>
                      <CardDescription>
                        Configure pits-agent mTLS bridge, RKE2 pits-core, Sidero Talos pits-lab, and
                        COSI endpoints
                      </CardDescription>
                    </div>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => void loadProfile()}
                    >
                      <RefreshCw className="h-3.5 w-3.5" />
                      <span>Reload</span>
                    </Button>
                  </CardHeader>

                  <CardContent className="space-y-5 p-5">
                    <div className="rounded-lg border border-solid border-border bg-muted/40 p-3.5">
                      <div className="flex flex-wrap items-center justify-between gap-2">
                        <div className="flex items-center gap-2 text-xs font-semibold text-foreground">
                          <Shield className="h-4 w-4 text-foreground" />
                          <span>Operator mTLS Client Certificate</span>
                        </div>
                        <StatusPill
                          label={
                            profile.hasClientPki
                              ? 'Active Ed25519 mTLS Identity'
                              : 'Missing Client PKI'
                          }
                          level={profile.hasClientPki ? 'ok' : 'warn'}
                        />
                      </div>
                      {profile.stateDir && (
                        <div className="mt-2.5 flex flex-wrap items-center gap-2">
                          <span className="text-[11px] font-medium text-muted-foreground">
                            Local State Directory:
                          </span>
                          <FormattedAttributeValue value={profile.stateDir} monospace />
                        </div>
                      )}
                    </div>

                    <FieldRow
                      label="NixOS pits-agent mTLS Endpoint"
                      helper="Default Tailscale mTLS endpoint on pits-core (:18443)"
                    >
                      <Input
                        value={profile.agentEndpoint}
                        onChange={e =>
                          setProfile({ ...profile, agentEndpoint: e.target.value })
                        }
                      />
                    </FieldRow>

                    <FieldRow
                      label="Active Contract Workspace Source (edge-router)"
                      helper="Optional local path to edge-router repository for contract & manifest overlays"
                    >
                      <Input
                        value={profile.activeWorkspaceSource}
                        onChange={e =>
                          setProfile({ ...profile, activeWorkspaceSource: e.target.value })
                        }
                      />
                    </FieldRow>

                    <FieldRow
                      label="Sourcing DB PostgreSQL Sync URL (postgres://...)"
                      helper="Optional PostgreSQL v3 primary DB for bi-directional outbox sync (SQLite WAL remains local SSOT cache). Plaintext URL is never returned on GET."
                    >
                      <Input
                        type="password"
                        placeholder={
                          profile.sourcingDbConfigured
                            ? `Configured (${profile.sourcingDbEndpointRedacted ?? 'redacted'}) — leave blank to keep`
                            : 'postgres://user:pass@host:5432/pits_sourcing'
                        }
                        value={sourcingDbUrlInput}
                        disabled={clearSourcingDbUrl}
                        onChange={e => setSourcingDbUrlInput(e.target.value)}
                      />
                    </FieldRow>

                    {profile.sourcingDbConfigured && (
                      <label className="flex cursor-pointer items-center gap-2.5 text-xs text-foreground">
                        <PitsSwitch
                          checked={clearSourcingDbUrl}
                          onCheckedChange={setClearSourcingDbUrl}
                        />
                        <span>
                          Clear saved PostgreSQL URL on save (revert to local SQLite WAL only)
                        </span>
                      </label>
                    )}

                    <Separator />

                    <div className="grid grid-cols-1 gap-4 sm:grid-cols-12">
                      <div className="sm:col-span-8">
                        <FieldRow label="pits-core RKE2 Kubeconfig Path">
                          <Input
                            value={profile.coreKubeconfigPath}
                            onChange={e =>
                              setProfile({ ...profile, coreKubeconfigPath: e.target.value })
                            }
                          />
                        </FieldRow>
                      </div>
                      <div className="sm:col-span-4">
                        <FieldRow label="pits-core Context">
                          <Input
                            value={profile.coreContext}
                            onChange={e =>
                              setProfile({ ...profile, coreContext: e.target.value })
                            }
                          />
                        </FieldRow>
                      </div>

                      <div className="sm:col-span-8">
                        <FieldRow label="pits-lab Talos K8s Kubeconfig Path">
                          <Input
                            value={profile.labKubeconfigPath}
                            onChange={e =>
                              setProfile({ ...profile, labKubeconfigPath: e.target.value })
                            }
                          />
                        </FieldRow>
                      </div>
                      <div className="sm:col-span-4">
                        <FieldRow label="pits-lab Context">
                          <Input
                            value={profile.labContext}
                            onChange={e =>
                              setProfile({ ...profile, labContext: e.target.value })
                            }
                          />
                        </FieldRow>
                      </div>

                      <div className="sm:col-span-12">
                        <label className="flex cursor-pointer items-center gap-2.5 text-xs text-foreground">
                          <PitsSwitch
                            checked={profile.labViaAgentBridge}
                            onCheckedChange={checked =>
                              setProfile({ ...profile, labViaAgentBridge: checked })
                            }
                          />
                          <span>
                            Tunnel pits-lab K8s API (VLAN 20) via pits-agent mTLS HTTP CONNECT
                            bridge
                          </span>
                        </label>
                      </div>

                      <div className="sm:col-span-8">
                        <FieldRow label="Sidero Talosconfig Path">
                          <Input
                            value={profile.talosconfigPath}
                            onChange={e =>
                              setProfile({ ...profile, talosconfigPath: e.target.value })
                            }
                          />
                        </FieldRow>
                      </div>
                      <div className="sm:col-span-4">
                        <FieldRow label="Talos Context">
                          <Input
                            value={profile.talosContext}
                            onChange={e =>
                              setProfile({ ...profile, talosContext: e.target.value })
                            }
                          />
                        </FieldRow>
                      </div>

                      <div className="sm:col-span-12">
                        <label className="flex cursor-pointer items-center gap-2.5 text-xs text-foreground">
                          <PitsSwitch
                            checked={profile.talosViaAgentBridge}
                            onCheckedChange={checked =>
                              setProfile({ ...profile, talosViaAgentBridge: checked })
                            }
                          />
                          <span>
                            Tunnel Talos COSI/gRPC (:50000 on netboot-001..004) via pits-agent mTLS
                            bridge
                          </span>
                        </label>
                      </div>
                    </div>

                    <Separator />

                    {/* Additional Clusters */}
                    <div className="space-y-3">
                      <div className="flex items-center justify-between gap-2">
                        <div className="text-xs font-semibold text-foreground">
                          Additional Kubernetes / vCluster Endpoints ({profile.extraClusters.length})
                        </div>
                        <Button
                          size="xs"
                          variant="outline"
                          onClick={() =>
                            setProfile({
                              ...profile,
                              extraClusters: [
                                ...profile.extraClusters,
                                {
                                  name: '',
                                  title: '',
                                  role: 'vcluster-gpu-dra',
                                  kubeconfigPath: '',
                                  viaAgentBridge: false,
                                },
                              ],
                            })
                          }
                        >
                          <Plus className="h-3 w-3" />
                          <span>Add Cluster</span>
                        </Button>
                      </div>

                      <div className="space-y-2.5">
                        {profile.extraClusters.map((ec, idx) => (
                          <div
                            key={idx}
                            className="grid grid-cols-1 items-end gap-2.5 rounded-lg border border-solid border-border bg-muted/20 p-3 sm:grid-cols-12"
                          >
                            <div className="sm:col-span-4">
                              <FieldRow label="Cluster Name">
                                <Input
                                  value={ec.name}
                                  onChange={e => {
                                    const next = [...profile.extraClusters];
                                    next[idx] = { ...ec, name: e.target.value };
                                    setProfile({ ...profile, extraClusters: next });
                                  }}
                                />
                              </FieldRow>
                            </div>
                            <div className="sm:col-span-6">
                              <FieldRow label="Kubeconfig Path">
                                <Input
                                  value={ec.kubeconfigPath}
                                  onChange={e => {
                                    const next = [...profile.extraClusters];
                                    next[idx] = { ...ec, kubeconfigPath: e.target.value };
                                    setProfile({ ...profile, extraClusters: next });
                                  }}
                                />
                              </FieldRow>
                            </div>
                            <div className="sm:col-span-2">
                              <Button
                                size="sm"
                                variant="ghost"
                                className="w-full text-red-600 hover:bg-red-500/10 hover:text-red-700 dark:text-red-400"
                                onClick={() =>
                                  setProfile({
                                    ...profile,
                                    extraClusters: profile.extraClusters.filter(
                                      (_, i) => i !== idx,
                                    ),
                                  })
                                }
                              >
                                <Trash2 className="h-3.5 w-3.5" />
                                <span>Remove</span>
                              </Button>
                            </div>
                          </div>
                        ))}
                      </div>
                    </div>

                    <div className="flex justify-end pt-2">
                      <Button type="submit" disabled={saving}>
                        {saving ? (
                          <Loader2 className="h-3.5 w-3.5 animate-spin" />
                        ) : (
                          <Save className="h-3.5 w-3.5" />
                        )}
                        <span>{saving ? 'Saving Profile...' : 'Save Operator Profile'}</span>
                      </Button>
                    </div>
                  </CardContent>
                </form>
              </Card>
            </div>

            {/* Right column: Ed25519 PKI Provisioner */}
            <div className="lg:col-span-5">
              <Card>
                <CardHeader className="border-b border-solid border-border">
                  <CardTitle>Ed25519 mTLS PKI Provisioner</CardTitle>
                  <CardDescription>
                    Generate Root CA + Server/Client Leaf Certificates for zero-trust pits-agent
                    enrollment
                  </CardDescription>
                </CardHeader>

                <CardContent className="space-y-4 p-5">
                  <Alert variant="info">
                    <Info />
                    <div>
                      <AlertTitle>Syncthing Hygiene Guard</AlertTitle>
                      <AlertDescription>
                        Client private keys are saved directly into{' '}
                        <code className="rounded bg-muted px-1 py-0.5 font-mono text-[11px]">
                          %LOCALAPPDATA%\PITS\Client\profile.yaml
                        </code>{' '}
                        and are never written to the repository workspace or returned by GET APIs.
                      </AlertDescription>
                    </div>
                  </Alert>

                  <FieldRow
                    label="Server IP / DNS SANs (comma-separated)"
                    helper="Included in pits-agent X.509 SubjectAltName extension"
                  >
                    <Input
                      value={serverSan}
                      onChange={e => setServerSan(e.target.value)}
                    />
                  </FieldRow>

                  <Button
                    variant="warning"
                    className="w-full"
                    disabled={generatingPki}
                    onClick={() => void handleGeneratePki()}
                  >
                    {generatingPki ? (
                      <Loader2 className="h-3.5 w-3.5 animate-spin" />
                    ) : (
                      <KeyRound className="h-3.5 w-3.5" />
                    )}
                    <span>
                      {generatingPki
                        ? 'Generating Ed25519 PKI...'
                        : 'Rotate & Generate New mTLS PKI'}
                    </span>
                  </Button>

                  {generatedPki && (
                    <div className="space-y-3 pt-1">
                      {[
                        {
                          key: 'ca',
                          label: 'Root CA Certificate (ca.crt)',
                          pem: generatedPki.caCertPem,
                        },
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
                        <div
                          key={item.key}
                          className="overflow-hidden rounded-lg border border-solid border-border"
                        >
                          <div className="flex items-center justify-between gap-2 border-b border-solid border-border bg-muted/50 px-3 py-1.5">
                            <span className="text-[11px] font-semibold text-foreground">
                              {item.label}
                            </span>
                            <Button
                              size="xs"
                              variant="ghost"
                              onClick={() => void copyPem(item.key, item.pem)}
                            >
                              {copiedField === item.key ? (
                                <Check className="h-3 w-3 text-emerald-600 dark:text-emerald-400" />
                              ) : (
                                <Copy className="h-3 w-3" />
                              )}
                              <span>{copiedField === item.key ? 'Copied' : 'Copy'}</span>
                            </Button>
                          </div>
                          <pre className="pits-scroll m-0 max-h-32 overflow-auto bg-zinc-950 p-3 font-mono text-[11px] leading-relaxed text-zinc-200">
                            {item.pem}
                          </pre>
                        </div>
                      ))}
                    </div>
                  )}
                </CardContent>
              </Card>
            </div>
          </div>
        ) : null}
      </PageBody>
    </PageShell>
  );
};
