import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { useParams } from 'react-router-dom';
import {
  CheckCircle2,
  Clock,
  Code2,
  Copy,
  FileText,
  GitCommitHorizontal,
  Plus,
  RotateCcw,
  Save,
  ShieldAlert,
  ShieldCheck,
  Tag,
} from 'lucide-react';
import {
  acceptHubSession,
  fetchHubTaskDetail,
  rejectHubSession,
  updateHubTask,
  type AcChecklistItem,
  type HubTaskDetailResponse,
  type HubTaskPriority,
  type HubTaskStatus,
} from '../api/hubClient';
import { cn } from '../lib/utils';
import { Alert, AlertDescription, AlertTitle } from '../components/ui/alert';
import { Badge } from '../components/ui/badge';
import { Button } from '../components/ui/button';
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '../components/ui/card';
import { Input } from '../components/ui/input';
import { PageBody, PageHeader, PageShell } from '../components/ui/page-shell';
import { PitsCheckbox, PitsSelect } from '../components/ui/pits-primitives';
import { Textarea } from '../components/ui/textarea';
import { BlockNoteMarkdownEditor } from '../components/hub/BlockNoteMarkdownEditor';
import {
  formatShortTime,
  LOG_TYPE_META,
  PRIORITY_META,
  statusMeta,
  TASK_STATUS_COLUMNS,
} from '../components/hub/hubMeta';

export const ProjectHubTaskDetailPage: React.FC = () => {
  const { taskKey = '' } = useParams<{ taskKey: string }>();

  const [detail, setDetail] = useState<HubTaskDetailResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  // Editable task state
  const [title, setTitle] = useState('');
  const [status, setStatus] = useState<HubTaskStatus>('backlog');
  const [priority, setPriority] = useState<HubTaskPriority>('p2_medium');
  const [tagsText, setTagsText] = useState('');
  const [workspacePath, setWorkspacePath] = useState('');
  const [dispatchTarget, setDispatchTarget] = useState('');
  const [acceptanceSummary, setAcceptanceSummary] = useState('');
  const [acChecklist, setAcChecklist] = useState<AcChecklistItem[]>([]);
  const [bodyMd, setBodyMd] = useState('');
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [editorMode, setEditorMode] = useState<'blocknote' | 'markdown'>('blocknote');

  // New AC item
  const [newAcText, setNewAcText] = useState('');

  // Human gate review state
  const [reviewNote, setReviewNote] = useState('');
  const [rejectTarget, setRejectTarget] = useState<'in_progress' | 'blocked'>(
    'in_progress',
  );
  const [selectedAcIds, setSelectedAcIds] = useState<string[]>([]);
  const [gateBusy, setGateBusy] = useState(false);
  const [copiedId, setCopiedId] = useState<string | null>(null);

  const loadDetail = useCallback(async () => {
    if (!taskKey) return;
    setLoading(true);
    setError(null);
    try {
      const data = await fetchHubTaskDetail(taskKey);
      setDetail(data);
      setTitle(data.task.title);
      setStatus(data.task.status);
      setPriority(data.task.priority);
      setTagsText(data.task.tags.join(', '));
      setWorkspacePath(data.task.workspacePath);
      setDispatchTarget(data.task.dispatchTarget);
      setAcceptanceSummary(data.task.acceptanceSummary);
      setAcChecklist(data.task.acChecklist);
      setBodyMd(data.task.bodyMd);
      setSelectedAcIds(data.task.acChecklist.map(a => a.id));
      setDirty(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [taskKey]);

  useEffect(() => {
    void loadDetail();
  }, [loadDetail]);

  const handleSaveTask = async () => {
    if (!detail) return;
    setSaving(true);
    setError(null);
    setNotice(null);
    try {
      const tags = tagsText
        .split(/[,\s]+/)
        .map(s => s.trim())
        .filter(Boolean);
      const updated = await updateHubTask(detail.task.taskKey, {
        title: title.trim() || detail.task.title,
        status,
        priority,
        tags,
        workspacePath: workspacePath.trim(),
        dispatchTarget: dispatchTarget.trim(),
        acceptanceSummary: acceptanceSummary.trim(),
        acChecklist,
        bodyMd,
      });
      setDetail(prev => (prev ? { ...prev, task: updated } : prev));
      setDirty(false);
      setNotice(`Saved changes to ${updated.taskKey}.`);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  const handleAddAc = () => {
    const text = newAcText.trim();
    if (!text) return;
    const nextItem: AcChecklistItem = {
      id: `ac-${acChecklist.length + 1}`,
      text,
      agentVerified: false,
      humanVerified: false,
    };
    setAcChecklist(prev => [...prev, nextItem]);
    setSelectedAcIds(prev => [...prev, nextItem.id]);
    setNewAcText('');
    setDirty(true);
  };

  const toggleAcHuman = (id: string, checked: boolean) => {
    setAcChecklist(prev =>
      prev.map(item =>
        item.id === id ? { ...item, humanVerified: checked } : item,
      ),
    );
    setDirty(true);
  };

  const pendingHandoff = useMemo(
    () =>
      detail?.sessions.find(
        s => s.logType === 'session_handoff' && !s.accepted,
      ) ?? null,
    [detail],
  );

  const latestAcceptedHandoff = useMemo(
    () =>
      detail?.sessions.find(
        s => s.logType === 'session_handoff' && s.accepted,
      ) ?? null,
    [detail],
  );

  const handleAcceptGate = async () => {
    if (!pendingHandoff) return;
    setGateBusy(true);
    setError(null);
    setNotice(null);
    try {
      const outcome = await acceptHubSession(pendingHandoff.id, {
        markTasksDone: true,
        reviewNote,
        verifiedAcIds: selectedAcIds,
      });
      setNotice(
        `Accepted ${detail?.task.taskKey} by ${outcome.actor} (${outcome.authSource}). Task transitioned to Done.`,
      );
      setReviewNote('');
      await loadDetail();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setGateBusy(false);
    }
  };

  const handleRejectGate = async () => {
    if (!pendingHandoff) return;
    setGateBusy(true);
    setError(null);
    setNotice(null);
    try {
      await rejectHubSession(pendingHandoff.id, {
        reviewNote: reviewNote || 'Returned for rework via Task Detail page',
        targetTaskStatus: rejectTarget,
      });
      setNotice(
        `Returned ${detail?.task.taskKey} to ${rejectTarget}. Audit event recorded.`,
      );
      setReviewNote('');
      await loadDetail();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setGateBusy(false);
    }
  };

  const copyText = async (text: string, id: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopiedId(id);
      setTimeout(() => setCopiedId(prev => (prev === id ? null : prev)), 1800);
    } catch {
      // ignore
    }
  };

  const stMeta = statusMeta(status);
  const prMeta = PRIORITY_META[priority] ?? PRIORITY_META.p2_medium;

  return (
    <PageShell>
      <PageHeader
        breadcrumb={{ label: 'Project Hub', to: '/project-hub' }}
        title={
          <span className="inline-flex items-center gap-2.5">
            <span className="font-mono text-lg text-muted-foreground">
              {detail?.task.taskKey || taskKey}
            </span>
            <span>{title || 'Loading task…'}</span>
          </span>
        }
        badge={
          <div className="flex flex-wrap items-center gap-1.5">
            <span
              className={cn(
                'inline-flex items-center gap-1.5 rounded-full border border-solid px-2.5 py-0.5 text-xs font-medium',
                stMeta.tone,
              )}
            >
              <span className={cn('h-1.5 w-1.5 rounded-full', stMeta.dot)} />
              {stMeta.label}
            </span>
            <span
              className={cn(
                'inline-flex rounded border border-solid px-2 py-0.5 text-xs font-medium',
                prMeta.className,
              )}
            >
              {prMeta.label}
            </span>
          </div>
        }
        actions={
          <>
            <Button
              size="sm"
              disabled={saving || !dirty}
              onClick={() => void handleSaveTask()}
            >
              <Save className="h-3.5 w-3.5" />
              <span>{saving ? 'Saving…' : dirty ? 'Save Changes' : 'Saved'}</span>
            </Button>
          </>
        }
      />

      <PageBody className="space-y-5">
        {error && (
          <Alert variant="destructive">
            <ShieldAlert className="h-4 w-4" />
            <AlertTitle>Error</AlertTitle>
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        {notice && (
          <Alert>
            <CheckCircle2 className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
            <AlertTitle>Updated</AlertTitle>
            <AlertDescription>{notice}</AlertDescription>
          </Alert>
        )}

        {loading && !detail ? (
          <div className="py-12 text-center text-xs text-muted-foreground">
            Loading {taskKey}…
          </div>
        ) : detail ? (
          <div className="grid grid-cols-1 gap-5 lg:grid-cols-12">
            {/* ─────────── LEFT COLUMN: METADATA + @blocknote/shadcn EDITOR ─────────── */}
            <div className="space-y-5 lg:col-span-8">
              <Card>
                <CardHeader className="pb-3">
                  <CardTitle className="text-sm">Task Properties</CardTitle>
                </CardHeader>
                <CardContent className="space-y-3.5">
                  <div>
                    <label className="mb-1 block text-xs font-medium text-muted-foreground">
                      Title
                    </label>
                    <Input
                      value={title}
                      onChange={e => {
                        setTitle(e.target.value);
                        setDirty(true);
                      }}
                    />
                  </div>
                  <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
                    <div>
                      <label className="mb-1 block text-xs font-medium text-muted-foreground">
                        Status
                      </label>
                      <PitsSelect
                        size="sm"
                        value={status}
                        onValueChange={v => {
                          setStatus(v as HubTaskStatus);
                          setDirty(true);
                        }}
                        options={TASK_STATUS_COLUMNS.map(c => ({
                          value: c.id,
                          label: c.label,
                        }))}
                      />
                    </div>
                    <div>
                      <label className="mb-1 block text-xs font-medium text-muted-foreground">
                        Priority
                      </label>
                      <PitsSelect
                        size="sm"
                        value={priority}
                        onValueChange={v => {
                          setPriority(v as HubTaskPriority);
                          setDirty(true);
                        }}
                        options={[
                          { value: 'p0_critical', label: 'P0 · Critical' },
                          { value: 'p1_high', label: 'P1 · High' },
                          { value: 'p2_medium', label: 'P2 · Medium' },
                          { value: 'p3_low', label: 'P3 · Low' },
                        ]}
                      />
                    </div>
                    <div>
                      <label className="mb-1 block text-xs font-medium text-muted-foreground">
                        Dispatch Target
                      </label>
                      <Input
                        value={dispatchTarget}
                        onChange={e => {
                          setDispatchTarget(e.target.value);
                          setDirty(true);
                        }}
                        className="h-8 text-xs"
                      />
                    </div>
                  </div>
                  <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                    <div>
                      <label className="mb-1 inline-flex items-center gap-1 text-xs font-medium text-muted-foreground">
                        <Tag className="h-3 w-3" />
                        First-Class Tags
                      </label>
                      <Input
                        value={tagsText}
                        onChange={e => {
                          setTagsText(e.target.value);
                          setDirty(true);
                        }}
                        className="h-8 font-mono text-xs"
                        placeholder="pits-infra, portal, client"
                      />
                    </div>
                    <div>
                      <label className="mb-1 block text-xs font-medium text-muted-foreground">
                        Workspace Path
                      </label>
                      <Input
                        value={workspacePath}
                        onChange={e => {
                          setWorkspacePath(e.target.value);
                          setDirty(true);
                        }}
                        className="h-8 font-mono text-xs"
                      />
                    </div>
                  </div>
                  <div>
                    <label className="mb-1 block text-xs font-medium text-muted-foreground">
                      Acceptance Summary
                    </label>
                    <Input
                      value={acceptanceSummary}
                      onChange={e => {
                        setAcceptanceSummary(e.target.value);
                        setDirty(true);
                      }}
                      className="h-8 text-xs"
                    />
                  </div>
                </CardContent>
              </Card>

              {/* BlockNote Notion-Style Document Editor */}
              <Card>
                <CardHeader className="pb-3">
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <div>
                      <CardTitle className="text-sm">
                        Specification & Notes (`body_md`)
                      </CardTitle>
                      <CardDescription className="text-xs">
                        Powered by @blocknote/shadcn — live bidirectional Markdown sync with SQLite WAL & CNPG.
                      </CardDescription>
                    </div>
                    <div className="inline-flex rounded-lg border border-solid border-border bg-muted/50 p-0.5">
                      <button
                        type="button"
                        onClick={() => setEditorMode('blocknote')}
                        className={cn(
                          'inline-flex cursor-pointer items-center gap-1 rounded-md border-0 px-2.5 py-1 text-xs font-medium',
                          editorMode === 'blocknote'
                            ? 'bg-background text-foreground shadow-sm'
                            : 'bg-transparent text-muted-foreground',
                        )}
                      >
                        <FileText className="h-3.5 w-3.5" />
                        <span>Block Editor</span>
                      </button>
                      <button
                        type="button"
                        onClick={() => setEditorMode('markdown')}
                        className={cn(
                          'inline-flex cursor-pointer items-center gap-1 rounded-md border-0 px-2.5 py-1 text-xs font-medium',
                          editorMode === 'markdown'
                            ? 'bg-background text-foreground shadow-sm'
                            : 'bg-transparent text-muted-foreground',
                        )}
                      >
                        <Code2 className="h-3.5 w-3.5" />
                        <span>Raw Markdown</span>
                      </button>
                    </div>
                  </div>
                </CardHeader>
                <CardContent>
                  {editorMode === 'blocknote' ? (
                    <BlockNoteMarkdownEditor
                      docKey={detail.task.taskKey}
                      initialMarkdown={bodyMd}
                      onMarkdownChange={md => {
                        setBodyMd(md);
                        setDirty(true);
                      }}
                    />
                  ) : (
                    <Textarea
                      rows={18}
                      value={bodyMd}
                      onChange={e => {
                        setBodyMd(e.target.value);
                        setDirty(true);
                      }}
                      className="font-mono text-xs leading-relaxed"
                    />
                  )}
                </CardContent>
              </Card>
            </div>

            {/* ─────────── RIGHT COLUMN (STICKY): HUMAN GATE + AC + SESSIONS + AUDIT ─────────── */}
            <div className="space-y-5 lg:col-span-4">
              {/* Sticky Human Acceptance Gate Panel */}
              <div className="lg:sticky lg:top-5 space-y-5">
                <Card
                  className={cn(
                    pendingHandoff
                      ? 'border-amber-500/50 bg-amber-500/[0.04]'
                      : latestAcceptedHandoff
                        ? 'border-emerald-500/40 bg-emerald-500/[0.04]'
                        : '',
                  )}
                >
                  <CardHeader className="pb-3">
                    <CardTitle className="flex items-center gap-2 text-sm">
                      <ShieldCheck
                        className={cn(
                          'h-4 w-4',
                          pendingHandoff
                            ? 'text-amber-600 dark:text-amber-400'
                            : 'text-emerald-600 dark:text-emerald-400',
                        )}
                      />
                      <span>Human Acceptance Gate</span>
                    </CardTitle>
                    <CardDescription className="text-xs">
                      {pendingHandoff
                        ? 'Pending Session Handoff awaiting your final verification.'
                        : latestAcceptedHandoff
                          ? `Accepted by ${latestAcceptedHandoff.acceptedBy || 'operator'} at ${formatShortTime(latestAcceptedHandoff.acceptedAt)}.`
                          : 'No pending handoff yet. Agents submit handoffs via hub_submit_handoff.'}
                    </CardDescription>
                  </CardHeader>
                  <CardContent className="space-y-3">
                    {pendingHandoff ? (
                      <>
                        <div className="rounded-lg border border-solid border-border bg-background p-2.5 text-xs">
                          <div className="font-semibold text-foreground">
                            {pendingHandoff.summary}
                          </div>
                          <div className="mt-1 flex flex-wrap items-center gap-1.5 font-mono text-[10px] text-muted-foreground">
                            <span>{pendingHandoff.sessionDate}</span>
                            {pendingHandoff.commitRefs.map(c => (
                              <span
                                key={c}
                                className="rounded bg-secondary px-1.5 py-0.5 text-foreground"
                              >
                                {c}
                              </span>
                            ))}
                          </div>
                        </div>

                        <div>
                          <label className="mb-1 block text-[11px] font-medium text-muted-foreground">
                            Review Note
                          </label>
                          <Textarea
                            rows={2}
                            value={reviewNote}
                            onChange={e => setReviewNote(e.target.value)}
                            placeholder="Verification note or rejection feedback..."
                            className="text-xs"
                          />
                        </div>

                        <div className="flex flex-col gap-2">
                          <Button
                            size="sm"
                            disabled={gateBusy}
                            className="w-full bg-emerald-600 text-white hover:bg-emerald-700 dark:bg-emerald-500 dark:text-zinc-950 dark:hover:bg-emerald-400"
                            onClick={() => void handleAcceptGate()}
                          >
                            <CheckCircle2 className="h-3.5 w-3.5" />
                            <span>一键验收并标记 Done</span>
                          </Button>
                          <div className="flex items-center gap-2">
                            <div className="flex-1">
                              <PitsSelect
                                size="sm"
                                value={rejectTarget}
                                onValueChange={v =>
                                  setRejectTarget(v as 'in_progress' | 'blocked')
                                }
                                options={[
                                  { value: 'in_progress', label: '→ In Progress' },
                                  { value: 'blocked', label: '→ Blocked' },
                                ]}
                              />
                            </div>
                            <Button
                              variant="destructive"
                              size="sm"
                              disabled={gateBusy}
                              onClick={() => void handleRejectGate()}
                            >
                              <RotateCcw className="h-3.5 w-3.5" />
                              <span>打回修改</span>
                            </Button>
                          </div>
                        </div>
                      </>
                    ) : latestAcceptedHandoff ? (
                      <div className="rounded-lg border border-solid border-emerald-500/30 bg-background p-3 text-xs">
                        <div className="font-semibold text-emerald-700 dark:text-emerald-300">
                          Verified & Accepted
                        </div>
                        {latestAcceptedHandoff.reviewNote && (
                          <p className="mt-1 mb-0 text-muted-foreground">
                            {latestAcceptedHandoff.reviewNote}
                          </p>
                        )}
                      </div>
                    ) : (
                      <div className="text-xs text-muted-foreground">
                        Waiting for agent handoff submission.
                      </div>
                    )}
                  </CardContent>
                </Card>

                {/* AC Checklist Card */}
                <Card>
                  <CardHeader className="pb-3">
                    <CardTitle className="text-sm">
                      AC Checklist ({acChecklist.filter(a => a.humanVerified).length}/
                      {acChecklist.length})
                    </CardTitle>
                  </CardHeader>
                  <CardContent className="space-y-2.5">
                    {acChecklist.map(item => (
                      <div
                        key={item.id}
                        className="flex items-start gap-2.5 rounded-lg border border-solid border-border bg-muted/25 p-2.5"
                      >
                        <div className="pt-0.5">
                          <PitsCheckbox
                            checked={item.humanVerified}
                            onCheckedChange={checked =>
                              toggleAcHuman(item.id, checked)
                            }
                          />
                        </div>
                        <div className="min-w-0 flex-1 space-y-1">
                          <div className="text-xs leading-snug text-foreground">
                            {item.text}
                          </div>
                          <div className="flex flex-wrap items-center gap-1.5">
                            <span className="font-mono text-[10px] text-muted-foreground">
                              {item.id}
                            </span>
                            <span
                              className={cn(
                                'rounded px-1.5 py-0.2 text-[10px]',
                                item.agentVerified
                                  ? 'bg-sky-500/15 text-sky-700 dark:text-sky-300'
                                  : 'bg-muted text-muted-foreground',
                              )}
                            >
                              {item.agentVerified ? 'Agent Verified' : 'Agent Pending'}
                            </span>
                            <span
                              className={cn(
                                'rounded px-1.5 py-0.2 text-[10px]',
                                item.humanVerified
                                  ? 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-300'
                                  : 'bg-amber-500/15 text-amber-800 dark:text-amber-300',
                              )}
                            >
                              {item.humanVerified ? 'Human Verified' : 'Unverified'}
                            </span>
                          </div>
                        </div>
                      </div>
                    ))}

                    <div className="flex items-center gap-1.5 pt-1">
                      <Input
                        value={newAcText}
                        onChange={e => setNewAcText(e.target.value)}
                        onKeyDown={e => {
                          if (e.key === 'Enter') {
                            e.preventDefault();
                            handleAddAc();
                          }
                        }}
                        placeholder="Add acceptance criterion..."
                        className="h-8 text-xs"
                      />
                      <Button variant="outline" size="xs" onClick={handleAddAc}>
                        <Plus className="h-3.5 w-3.5" />
                      </Button>
                    </div>
                  </CardContent>
                </Card>

                {/* Linked Session Logs */}
                <Card>
                  <CardHeader className="pb-3">
                    <CardTitle className="text-sm">
                      Linked Sessions ({detail.sessions.length})
                    </CardTitle>
                  </CardHeader>
                  <CardContent className="space-y-2.5">
                    {detail.sessions.map(s => {
                      const lmeta =
                        LOG_TYPE_META[s.logType] ?? LOG_TYPE_META.session_handoff;
                      return (
                        <div
                          key={s.id}
                          className="rounded-lg border border-solid border-border bg-muted/20 p-2.5 text-xs"
                        >
                          <div className="flex items-center justify-between gap-1.5">
                            <span
                              className={cn(
                                'rounded border border-solid px-1.5 py-0.5 text-[10px] font-medium',
                                lmeta.className,
                              )}
                            >
                              {lmeta.label}
                            </span>
                            <span className="font-mono text-[10px] text-muted-foreground">
                              {s.sessionDate}
                            </span>
                          </div>
                          <div className="mt-1 font-medium text-foreground">
                            {s.summary}
                          </div>
                          <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
                            {s.commitRefs.map(c => (
                              <span
                                key={c}
                                className="inline-flex items-center gap-0.5 rounded bg-secondary px-1.5 py-0.5 font-mono text-[10px]"
                              >
                                <GitCommitHorizontal className="h-2.5 w-2.5" />
                                {c}
                              </span>
                            ))}
                            {s.conversationId && (
                              <button
                                type="button"
                                onClick={() =>
                                  void copyText(
                                    `conversation://${s.conversationId}`,
                                    s.id,
                                  )
                                }
                                className="inline-flex cursor-pointer items-center gap-1 rounded border border-solid border-border bg-background px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground hover:text-foreground"
                              >
                                <Copy className="h-2.5 w-2.5" />
                                {copiedId === s.id
                                  ? 'Copied'
                                  : s.conversationId.slice(0, 8)}
                              </button>
                            )}
                          </div>
                        </div>
                      );
                    })}
                    {detail.sessions.length === 0 && (
                      <div className="text-xs text-muted-foreground">
                        No linked sessions yet.
                      </div>
                    )}
                  </CardContent>
                </Card>

                {/* Immutable Audit Trail */}
                <Card>
                  <CardHeader className="pb-2">
                    <CardTitle className="text-sm">
                      Audit Trail ({detail.auditEvents.length})
                    </CardTitle>
                  </CardHeader>
                  <CardContent className="space-y-1.5">
                    {detail.auditEvents.slice(0, 12).map(ev => (
                      <div
                        key={ev.id}
                        className="flex items-center justify-between gap-2 rounded bg-muted/35 px-2.5 py-1.5 text-[11px]"
                      >
                        <div className="min-w-0">
                          <Badge
                            variant={
                              ev.action === 'forbidden_attempt'
                                ? 'error'
                                : 'secondary'
                            }
                            className="mr-1.5 text-[10px]"
                          >
                            {ev.action}
                          </Badge>
                          <span className="font-mono text-foreground">
                            {ev.actor}
                          </span>
                          <span className="ml-1 text-muted-foreground">
                            via {ev.channel}
                          </span>
                        </div>
                        <span className="inline-flex shrink-0 items-center gap-1 font-mono text-[10px] text-muted-foreground">
                          <Clock className="h-2.5 w-2.5" />
                          {formatShortTime(ev.createdAt)}
                        </span>
                      </div>
                    ))}
                  </CardContent>
                </Card>
              </div>
            </div>
          </div>
        ) : null}
      </PageBody>
    </PageShell>
  );
};
