import React, { useMemo, useState } from 'react';
import {
  AlertTriangle,
  Check,
  Copy,
  Download,
  FileCode2,
  Loader2,
  RefreshCw,
} from 'lucide-react';
import YAML from 'yaml';
import { useEntity } from '@backstage/plugin-catalog-react';
import { usePitsEntityDetail } from '../hooks/usePitsEntityDetail';
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
import { cn } from '../lib/utils';

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
    return manifestEntries.find(([k]) => k === selectedKey) ?? manifestEntries[0];
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
      <Card>
        <CardContent className="flex items-center gap-3 p-6">
          <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
          <span className="text-xs text-muted-foreground">
            Rendering declarative manifests for <strong className="text-foreground">{name}</strong>
            ...
          </span>
        </CardContent>
      </Card>
    );
  }

  const lineCount = activeEntry ? activeEntry[1].split('\n').length : 0;

  return (
    <Card>
      <CardHeader className="flex flex-col gap-3 border-b border-solid border-border sm:flex-row sm:items-center sm:justify-between">
        <div className="space-y-1">
          <CardTitle>Declarative Manifests &amp; Rendered Configurations</CardTitle>
          <CardDescription>
            Backstage Entity Registry YAML and domain-specific Kubernetes, Talos, EdgeRouter, DRA,
            iPXE, or Sourcing Audit artifacts
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
          <span>{refreshing ? 'Refreshing...' : 'Refresh Manifests'}</span>
        </Button>
      </CardHeader>

      <CardContent className="space-y-4 p-5">
        {error && (
          <Alert variant="warning">
            <AlertTriangle />
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        {manifestEntries.length === 0 || !activeEntry ? (
          <div className="text-xs text-muted-foreground">
            No manifests available for this entity.
          </div>
        ) : (
          <div className="grid grid-cols-1 items-start gap-4 md:grid-cols-[270px_1fr]">
            <div className="overflow-hidden rounded-lg border border-solid border-border">
              <div className="border-b border-solid border-border bg-muted/50 px-3.5 py-2 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                Artifacts ({manifestEntries.length})
              </div>
              <div className="divide-y divide-border">
                {manifestEntries.map(([fileKey, content]) => {
                  const selected = activeEntry[0] === fileKey;
                  const lines = content.split('\n').length;
                  return (
                    <button
                      key={fileKey}
                      type="button"
                      onClick={() => setSelectedKey(fileKey)}
                      className={cn(
                        'flex w-full cursor-pointer items-start gap-2.5 border-0 px-3.5 py-2.5 text-left transition-colors',
                        selected
                          ? 'bg-accent text-accent-foreground'
                          : 'bg-card text-foreground hover:bg-muted/50',
                      )}
                    >
                      <FileCode2
                        className={cn(
                          'mt-0.5 h-4 w-4 shrink-0',
                          selected ? 'text-foreground' : 'text-muted-foreground',
                        )}
                      />
                      <div className="min-w-0">
                        <div
                          className={cn(
                            'break-all font-mono text-xs',
                            selected ? 'font-semibold' : 'font-medium',
                          )}
                        >
                          {fileKey}
                        </div>
                        <div className="text-[11px] text-muted-foreground">{lines} lines</div>
                      </div>
                    </button>
                  );
                })}
              </div>
            </div>

            <div className="overflow-hidden rounded-lg border border-solid border-border">
              <div className="flex flex-wrap items-center justify-between gap-2 border-b border-solid border-border bg-muted/50 px-4 py-2">
                <div className="flex items-center gap-2">
                  <span className="font-mono text-xs font-semibold text-foreground">
                    {activeEntry[0]}
                  </span>
                  <Badge variant="outline">{lineCount} lines</Badge>
                </div>
                <div className="flex items-center gap-1.5">
                  <Button size="xs" variant="outline" onClick={() => void handleCopy()}>
                    {copied ? (
                      <Check className="h-3 w-3 text-emerald-600 dark:text-emerald-400" />
                    ) : (
                      <Copy className="h-3 w-3" />
                    )}
                    <span>{copied ? 'Copied!' : 'Copy'}</span>
                  </Button>
                  <Button size="xs" variant="outline" onClick={handleDownload}>
                    <Download className="h-3 w-3" />
                    <span>Download</span>
                  </Button>
                </div>
              </div>

              <pre className="pits-scroll m-0 max-h-[68vh] overflow-auto bg-zinc-950 p-4 font-mono text-xs leading-relaxed text-zinc-200">
                {activeEntry[1]}
              </pre>
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  );
};
