import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { Link as RouterLink, useNavigate, useSearchParams } from 'react-router-dom';
import {
  DndContext,
  PointerSensor,
  useDroppable,
  useSensor,
  useSensors,
  type DragEndEvent,
} from '@dnd-kit/core';
import {
  SortableContext,
  useSortable,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import type { ColumnDef } from '@tanstack/react-table';
import {
  CheckCircle2,
  ClipboardCheck,
  Clock,
  Copy,
  Database,
  ExternalLink,
  FolderKanban,
  GitCommitHorizontal,
  GripVertical,
  History,
  Kanban,
  Plus,
  RefreshCw,
  RotateCcw,
  Search,
  ShieldAlert,
  ShieldCheck,
  Table2,
  Tag,
  X,
} from 'lucide-react';
import {
  acceptHubSession,
  createHubSessionLog,
  createHubTask,
  fetchHubOverview,
  rejectHubSession,
  syncHubDatabase,
  updateHubTask,
  upsertHubProject,
  type HubLogType,
  type HubOverviewResponse,
  type HubProject,
  type HubSessionLog,
  type HubTask,
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
import { DataTable } from '../components/ui/data-table';
import { Input } from '../components/ui/input';
import { PageBody, PageHeader, PageShell } from '../components/ui/page-shell';
import {
  PitsCheckbox,
  PitsDialog,
  PitsSelect,
  PitsTabPanel,
  PitsTabs,
} from '../components/ui/pits-primitives';
import { Textarea } from '../components/ui/textarea';
import { BlockNoteMarkdownEditor } from '../components/hub/BlockNoteMarkdownEditor';
import {
  formatShortTime,
  LOG_TYPE_META,
  PRESET_HUB_TAGS,
  PRIORITY_META,
  statusMeta,
  TASK_STATUS_COLUMNS,
} from '../components/hub/hubMeta';

/* ───────────────────────── Draggable Kanban Primitives ───────────────────────── */

const KanbanTaskCard: React.FC<{
  task: HubTask;
  onOpen: (taskKey: string) => void;
}> = ({ task, onOpen }) => {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } =
    useSortable({ id: task.taskKey, data: { status: task.status } });

  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
  };
  const prio = PRIORITY_META[task.priority] ?? PRIORITY_META.p2_medium;
  const acDone = task.acChecklist.filter(a => a.humanVerified).length;
  const acAgent = task.acChecklist.filter(a => a.agentVerified).length;

  return (
    <div
      ref={setNodeRef}
      style={style}
      className={cn(
        'group rounded-lg border border-solid border-border bg-card p-3 shadow-sm transition-shadow hover:shadow-md',
        isDragging && 'opacity-50 ring-2 ring-ring',
      )}
    >
      <div className="mb-1.5 flex items-center justify-between gap-1.5">
        <div className="flex items-center gap-1.5">
          <button
            type="button"
            {...attributes}
            {...listeners}
            aria-label={`Drag ${task.taskKey}`}
            className="inline-flex h-5 w-5 cursor-grab items-center justify-center rounded border-0 bg-transparent p-0 text-muted-foreground hover:bg-accent hover:text-foreground active:cursor-grabbing"
          >
            <GripVertical className="h-3.5 w-3.5" />
          </button>
          <button
            type="button"
            onClick={() => onOpen(task.taskKey)}
            className="cursor-pointer border-0 bg-transparent p-0 font-mono text-xs font-semibold text-foreground hover:underline"
          >
            {task.taskKey}
          </button>
        </div>
        <span
          className={cn(
            'inline-flex items-center rounded border border-solid px-1.5 py-0.5 text-[10px] font-medium',
            prio.className,
          )}
        >
          {prio.label.split(' ')[0]}
        </span>
      </div>

      <button
        type="button"
        onClick={() => onOpen(task.taskKey)}
        className="block w-full cursor-pointer border-0 bg-transparent p-0 text-left text-[13px] font-medium leading-snug text-foreground hover:text-primary"
      >
        {task.title}
      </button>

      {task.tags.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-1">
          {task.tags.slice(0, 4).map(t => (
            <span
              key={t}
              className="rounded bg-secondary px-1.5 py-0.5 font-mono text-[10px] text-secondary-foreground"
            >
              #{t}
            </span>
          ))}
          {task.tags.length > 4 && (
            <span className="text-[10px] text-muted-foreground">
              +{task.tags.length - 4}
            </span>
          )}
        </div>
      )}

      <div className="mt-2.5 flex items-center justify-between border-t border-solid border-border/60 pt-2 text-[11px] text-muted-foreground">
        <span>
          AC {acDone}/{task.acChecklist.length}
          {acAgent > acDone ? ` (${acAgent} agent)` : ''}
        </span>
        <span className="truncate font-mono text-[10px]">
          {task.dispatchTarget || task.owner}
        </span>
      </div>
    </div>
  );
};

const KanbanColumn: React.FC<{
  status: HubTaskStatus;
  label: string;
  dot: string;
  tasks: HubTask[];
  onOpenTask: (taskKey: string) => void;
}> = ({ status, label, dot, tasks, onOpenTask }) => {
  const { setNodeRef, isOver } = useDroppable({ id: `col:${status}` });

  return (
    <div
      ref={setNodeRef}
      className={cn(
        'flex min-h-[380px] min-w-[260px] flex-1 flex-col rounded-xl border border-solid border-border bg-muted/35 p-2.5 transition-colors',
        isOver && 'border-primary/60 bg-accent/40',
      )}
    >
      <div className="mb-2.5 flex items-center justify-between px-1">
        <div className="flex items-center gap-2">
          <span className={cn('h-2 w-2 rounded-full', dot)} />
          <span className="text-xs font-semibold text-foreground">{label}</span>
        </div>
        <span className="rounded-full bg-background px-2 py-0.5 font-mono text-[11px] font-medium text-muted-foreground">
          {tasks.length}
        </span>
      </div>

      <SortableContext
        items={tasks.map(t => t.taskKey)}
        strategy={verticalListSortingStrategy}
      >
        <div className="flex flex-1 flex-col gap-2">
          {tasks.map(task => (
            <KanbanTaskCard key={task.taskKey} task={task} onOpen={onOpenTask} />
          ))}
          {tasks.length === 0 && (
            <div className="flex flex-1 items-center justify-center rounded-lg border border-dashed border-border/80 p-4 text-center text-[11px] text-muted-foreground">
              Drop task here
            </div>
          )}
        </div>
      </SortableContext>
    </div>
  );
};

/* ───────────────────────── Main ProjectHubPage ───────────────────────── */

export const ProjectHubPage: React.FC = () => {
  const navigate = useNavigate();
  const [searchParams, setSearchParams] = useSearchParams();
  const initialTab = searchParams.get('tab') || 'review';
  const [activeTab, setActiveTab] = useState<string>(initialTab);

  useEffect(() => {
    const nextTab = searchParams.get('tab') || 'review';
    setActiveTab(nextTab);
  }, [searchParams]);

  const [overview, setOverview] = useState<HubOverviewResponse | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [syncing, setSyncing] = useState<boolean>(false);

  // Filters
  const [boardView, setBoardView] = useState<'kanban' | 'table'>('kanban');
  const [selectedTags, setSelectedTags] = useState<string[]>([]);
  const [priorityFilter, setPriorityFilter] = useState<string>('all');
  const [searchQuery, setSearchQuery] = useState<string>('');
  const [logTypeFilter, setLogTypeFilter] = useState<string>('all');

  // Review Queue inline state
  const [expandedSessionId, setExpandedSessionId] = useState<string | null>(null);
  const [reviewNotes, setReviewNotes] = useState<Record<string, string>>({});
  const [rejectTargets, setRejectTargets] = useState<
    Record<string, 'in_progress' | 'blocked'>
  >({});
  const [busySessionId, setBusySessionId] = useState<string | null>(null);

  // Create Task Dialog
  const [createTaskOpen, setCreateTaskOpen] = useState(false);
  const [newTitle, setNewTitle] = useState('');
  const [newStatus, setNewStatus] = useState<HubTaskStatus>('ready');
  const [newPriority, setNewPriority] = useState<HubTaskPriority>('p1_high');
  const [newTagsText, setNewTagsText] = useState('pits-infra');
  const [newWorkspace, setNewWorkspace] = useState('c:/Project/PITS/Infra');
  const [newDispatch, setNewDispatch] = useState('pits-infra');
  const [newSummary, setNewSummary] = useState('');
  const [newAcLines, setNewAcLines] = useState('');
  const [creatingTask, setCreatingTask] = useState(false);

  // Create Session Log Dialog
  const [createLogOpen, setCreateLogOpen] = useState(false);
  const [newLogType, setNewLogType] = useState<HubLogType>('decision_rfc');
  const [newLogSummary, setNewLogSummary] = useState('');
  const [newLogTags, setNewLogTags] = useState('pits-infra');
  const [newLogTaskKeys, setNewLogTaskKeys] = useState('');
  const [newLogBody, setNewLogBody] = useState('');
  const [creatingLog, setCreatingLog] = useState(false);

  // Project inline editor
  const [projectDraftMd, setProjectDraftMd] = useState<string>('');
  const [savingProject, setSavingProject] = useState(false);

  const [copiedId, setCopiedId] = useState<string | null>(null);

  const loadOverview = useCallback(async (silent = false) => {
    if (!silent) setLoading(true);
    setError(null);
    try {
      const data = await fetchHubOverview();
      setOverview(data);
      if (data.projects[0]) {
        setProjectDraftMd(data.projects[0].bodyMd);
      }
      if (!expandedSessionId && data.pendingAcceptanceSessions[0]) {
        setExpandedSessionId(data.pendingAcceptanceSessions[0].id);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [expandedSessionId]);

  useEffect(() => {
    void loadOverview();
  }, [loadOverview]);

  const handleTabChange = (next: string) => {
    setActiveTab(next);
    setSearchParams(prev => {
      const sp = new URLSearchParams(prev);
      sp.set('tab', next);
      return sp;
    });
  };

  const toggleTag = (tag: string) => {
    setSelectedTags(prev =>
      prev.includes(tag) ? prev.filter(t => t !== tag) : [...prev, tag],
    );
  };

  const copyText = async (text: string, id: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopiedId(id);
      setTimeout(() => setCopiedId(prev => (prev === id ? null : prev)), 1800);
    } catch {
      // ignore clipboard restrictions
    }
  };

  const handleManualSync = async () => {
    setSyncing(true);
    setNotice(null);
    setError(null);
    try {
      const st = await syncHubDatabase();
      setNotice(
        st.postgresConnected
          ? `CNPG sync succeeded (${st.lastSyncDirection || 'up-to-date'}, outbox=${st.pendingOutboxRows}).`
          : st.postgresConfigured
            ? `CNPG sync warning: ${st.lastSyncError || 'disconnected'}`
            : 'SQLite WAL local mode active (PITS_HUB_DB_URL not configured).',
      );
      await loadOverview(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSyncing(false);
    }
  };

  const handleAcceptSession = async (session: HubSessionLog) => {
    setBusySessionId(session.id);
    setError(null);
    setNotice(null);
    try {
      const outcome = await acceptHubSession(session.id, {
        markTasksDone: true,
        reviewNote: reviewNotes[session.id] ?? '',
      });
      setNotice(
        `Accepted "${session.summary}" as ${outcome.actor} (${outcome.authSource}); marked ${outcome.updatedTasks.map(t => t.taskKey).join(', ') || 'session'} Done.`,
      );
      await loadOverview(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusySessionId(null);
    }
  };

  const handleRejectSession = async (session: HubSessionLog) => {
    setBusySessionId(session.id);
    setError(null);
    setNotice(null);
    try {
      const target = rejectTargets[session.id] ?? 'in_progress';
      const outcome = await rejectHubSession(session.id, {
        reviewNote: reviewNotes[session.id] || 'Returned for rework via Project Hub',
        targetTaskStatus: target,
      });
      setNotice(
        `Returned ${outcome.updatedTasks.map(t => t.taskKey).join(', ')} to ${target}.`,
      );
      await loadOverview(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusySessionId(null);
    }
  };

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 6 } }),
  );

  const handleKanbanDragEnd = async (event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || !overview) return;
    const taskKey = String(active.id);
    const overId = String(over.id);

    let targetStatus: HubTaskStatus | null = null;
    if (overId.startsWith('col:')) {
      targetStatus = overId.slice(4) as HubTaskStatus;
    } else {
      const overTask = overview.tasks.find(t => t.taskKey === overId);
      if (overTask) targetStatus = overTask.status;
    }
    const currentTask = overview.tasks.find(t => t.taskKey === taskKey);
    if (!currentTask || !targetStatus || currentTask.status === targetStatus) {
      return;
    }

    // Optimistic UI update
    setOverview(prev =>
      prev
        ? {
            ...prev,
            tasks: prev.tasks.map(t =>
              t.taskKey === taskKey ? { ...t, status: targetStatus! } : t,
            ),
          }
        : prev,
    );

    try {
      await updateHubTask(taskKey, { status: targetStatus });
      await loadOverview(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      await loadOverview(true);
    }
  };

  const handleCreateTask = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newTitle.trim()) return;
    setCreatingTask(true);
    setError(null);
    try {
      const tags = newTagsText
        .split(/[,\s]+/)
        .map(s => s.trim())
        .filter(Boolean);
      const acChecklist = newAcLines
        .split('\n')
        .map(s => s.trim())
        .filter(Boolean)
        .map((text, idx) => ({
          id: `ac-${idx + 1}`,
          text,
          agentVerified: false,
          humanVerified: false,
        }));
      const created = await createHubTask({
        projectId: 'pits',
        title: newTitle.trim(),
        status: newStatus,
        priority: newPriority,
        tags,
        workspacePath: newWorkspace.trim(),
        dispatchTarget: newDispatch.trim(),
        acceptanceSummary: newSummary.trim(),
        acChecklist,
        bodyMd: `## Overview\n\n${newSummary.trim() || newTitle.trim()}\n`,
      });
      setCreateTaskOpen(false);
      setNewTitle('');
      setNewSummary('');
      setNewAcLines('');
      setNotice(`Created ${created.taskKey}: ${created.title}`);
      await loadOverview(true);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setCreatingTask(false);
    }
  };

  const handleCreateSessionLog = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newLogSummary.trim()) return;
    setCreatingLog(true);
    setError(null);
    try {
      const tags = newLogTags
        .split(/[,\s]+/)
        .map(s => s.trim())
        .filter(Boolean);
      const taskKeys = newLogTaskKeys
        .split(/[,\s]+/)
        .map(s => s.trim().toUpperCase())
        .filter(Boolean);
      await createHubSessionLog({
        projectId: 'pits',
        logType: newLogType,
        summary: newLogSummary.trim(),
        tags,
        taskKeys,
        bodyMd: newLogBody,
      });
      setCreateLogOpen(false);
      setNewLogSummary('');
      setNewLogTaskKeys('');
      setNewLogBody('');
      setNotice('Recorded session decision / blocker entry.');
      await loadOverview(true);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setCreatingLog(false);
    }
  };

  const handleSaveProjectMd = async (project: HubProject) => {
    setSavingProject(true);
    setError(null);
    try {
      await upsertHubProject({
        id: project.id,
        name: project.name,
        status: project.status,
        priority: project.priority,
        workspacePath: project.workspacePath,
        outcome: project.outcome,
        lead: project.lead,
        bodyMd: projectDraftMd,
      });
      setNotice(`Saved ${project.name} project documentation.`);
      await loadOverview(true);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingProject(false);
    }
  };

  const allTags = useMemo(() => {
    const set = new Set<string>(PRESET_HUB_TAGS);
    overview?.tags.forEach(t => set.add(t));
    return Array.from(set).sort();
  }, [overview]);

  const filteredTasks = useMemo(() => {
    if (!overview) return [];
    const q = searchQuery.trim().toLowerCase();
    return overview.tasks.filter(t => {
      if (priorityFilter !== 'all' && t.priority !== priorityFilter) return false;
      if (
        selectedTags.length > 0 &&
        !selectedTags.every(tag => t.tags.includes(tag))
      ) {
        return false;
      }
      if (q) {
        const hay = `${t.taskKey} ${t.title} ${t.acceptanceSummary} ${t.tags.join(' ')} ${t.workspacePath}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    });
  }, [overview, priorityFilter, selectedTags, searchQuery]);

  const filteredSessions = useMemo(() => {
    if (!overview) return [];
    const q = searchQuery.trim().toLowerCase();
    return overview.recentSessions.filter(s => {
      if (logTypeFilter !== 'all' && s.logType !== logTypeFilter) return false;
      if (
        selectedTags.length > 0 &&
        !selectedTags.every(tag => s.tags.includes(tag))
      ) {
        return false;
      }
      if (q) {
        const hay = `${s.summary} ${s.conversationId} ${s.commitRefs.join(' ')} ${s.taskKeys.join(' ')} ${s.tags.join(' ')}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    });
  }, [overview, logTypeFilter, selectedTags, searchQuery]);

  const taskMap = useMemo(() => {
    const map = new Map<string, HubTask>();
    overview?.tasks.forEach(t => map.set(t.taskKey, t));
    return map;
  }, [overview]);

  const tableColumns = useMemo<ColumnDef<HubTask>[]>(
    () => [
      {
        accessorKey: 'taskKey',
        header: 'Key',
        cell: ({ row }) => (
          <RouterLink
            to={`/project-hub/tasks/${row.original.taskKey}`}
            onClick={e => e.stopPropagation()}
            className="font-mono text-xs font-semibold text-foreground hover:underline"
          >
            {row.original.taskKey}
          </RouterLink>
        ),
      },
      {
        accessorKey: 'title',
        header: 'Task Title',
        cell: ({ row }) => (
          <div className="max-w-md">
            <div className="truncate text-[13px] font-medium text-foreground">
              {row.original.title}
            </div>
            {row.original.acceptanceSummary && (
              <div className="truncate text-[11px] text-muted-foreground">
                {row.original.acceptanceSummary}
              </div>
            )}
          </div>
        ),
      },
      {
        accessorKey: 'status',
        header: 'Status',
        cell: ({ row }) => {
          const meta = statusMeta(row.original.status);
          return (
            <span
              className={cn(
                'inline-flex items-center gap-1.5 rounded-full border border-solid px-2 py-0.5 text-[11px] font-medium',
                meta.tone,
              )}
            >
              <span className={cn('h-1.5 w-1.5 rounded-full', meta.dot)} />
              {meta.label}
            </span>
          );
        },
      },
      {
        accessorKey: 'priority',
        header: 'Priority',
        cell: ({ row }) => {
          const prio =
            PRIORITY_META[row.original.priority] ?? PRIORITY_META.p2_medium;
          return (
            <span
              className={cn(
                'inline-flex rounded border border-solid px-1.5 py-0.5 text-[11px] font-medium',
                prio.className,
              )}
            >
              {prio.label}
            </span>
          );
        },
      },
      {
        accessorKey: 'tags',
        header: 'Tags',
        enableSorting: false,
        cell: ({ row }) => (
          <div className="flex max-w-[220px] flex-wrap gap-1">
            {row.original.tags.map(tag => (
              <span
                key={tag}
                className="rounded bg-secondary px-1.5 py-0.5 font-mono text-[10px] text-secondary-foreground"
              >
                #{tag}
              </span>
            ))}
          </div>
        ),
      },
      {
        accessorKey: 'updatedAt',
        header: 'Updated',
        cell: ({ row }) => (
          <span className="whitespace-nowrap font-mono text-[11px] text-muted-foreground">
            {formatShortTime(row.original.updatedAt)}
          </span>
        ),
      },
    ],
    [],
  );

  const dbSync = overview?.dbSync;
  const pendingCount = overview?.pendingReviewCount ?? 0;

  return (
    <PageShell>
      <PageHeader
        title="Project Hub"
        description="Self-managed PITS cross-workspace task & handoff control plane with protocol-enforced human acceptance gate and SQLite WAL ↔ CNPG PostgreSQL bi-directional sync."
        badge={
          <div className="flex flex-wrap items-center gap-1.5">
            <Badge variant="secondary" className="font-mono text-[11px]">
              Project: PITS
            </Badge>
            {pendingCount > 0 && (
              <Badge className="border-amber-500/40 bg-amber-500/15 text-amber-800 dark:text-amber-300">
                {pendingCount} Pending Acceptance
              </Badge>
            )}
          </div>
        }
        meta={[
          {
            label: 'Tasks',
            value: String(overview?.tasks.length ?? 0),
          },
          {
            label: 'CNPG SSOT',
            value: dbSync?.postgresConnected
              ? 'Connected'
              : dbSync?.postgresConfigured
                ? 'Outbox Queued'
                : 'SQLite WAL',
          },
          {
            label: 'Search Engine',
            value: dbSync?.ftsEnabled ? 'FTS5' : 'LIKE',
          },
        ]}
        actions={
          <>
            <Button
              variant="outline"
              size="sm"
              onClick={handleManualSync}
              disabled={syncing}
            >
              <Database className={cn('h-3.5 w-3.5', syncing && 'animate-spin')} />
              <span>{syncing ? 'Syncing…' : 'Sync CNPG'}</span>
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => void loadOverview(true)}
              disabled={loading}
            >
              <RefreshCw className={cn('h-3.5 w-3.5', loading && 'animate-spin')} />
              <span>Refresh</span>
            </Button>
            <Button size="sm" onClick={() => setCreateTaskOpen(true)}>
              <Plus className="h-3.5 w-3.5" />
              <span>New AGY Task</span>
            </Button>
          </>
        }
      />

      <PageBody className="space-y-5">
        {error && (
          <Alert variant="destructive">
            <ShieldAlert className="h-4 w-4" />
            <AlertTitle>Project Hub Error</AlertTitle>
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        {notice && (
          <Alert>
            <CheckCircle2 className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
            <AlertTitle>Operation Complete</AlertTitle>
            <AlertDescription>{notice}</AlertDescription>
          </Alert>
        )}

        {/* Global Search + First-Class Tag Chips */}
        <Card>
          <CardContent className="flex flex-col gap-3 p-4">
            <div className="flex flex-wrap items-center gap-2.5">
              <div className="relative min-w-[240px] flex-1">
                <Search className="pointer-events-none absolute left-3 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
                <Input
                  value={searchQuery}
                  onChange={e => setSearchQuery(e.target.value)}
                  placeholder="Search AGY-<n>, titles, commits (d19c0e6), conversations, or tags..."
                  className="pl-8"
                />
              </div>
              <div className="w-40">
                <PitsSelect
                  size="sm"
                  value={priorityFilter}
                  onValueChange={setPriorityFilter}
                  options={[
                    { value: 'all', label: 'All Priorities' },
                    { value: 'p0_critical', label: 'P0 · Critical' },
                    { value: 'p1_high', label: 'P1 · High' },
                    { value: 'p2_medium', label: 'P2 · Medium' },
                    { value: 'p3_low', label: 'P3 · Low' },
                  ]}
                />
              </div>
              {selectedTags.length > 0 && (
                <Button
                  variant="ghost"
                  size="xs"
                  onClick={() => setSelectedTags([])}
                >
                  <X className="h-3 w-3" />
                  <span>Clear Tags ({selectedTags.length})</span>
                </Button>
              )}
            </div>

            <div className="flex flex-wrap items-center gap-1.5">
              <span className="mr-1 inline-flex items-center gap-1 text-[11px] font-medium text-muted-foreground">
                <Tag className="h-3 w-3" />
                Tags:
              </span>
              {allTags.map(tag => {
                const active = selectedTags.includes(tag);
                return (
                  <button
                    key={tag}
                    type="button"
                    onClick={() => toggleTag(tag)}
                    className={cn(
                      'cursor-pointer rounded-full border border-solid px-2.5 py-0.5 font-mono text-[11px] transition-colors',
                      active
                        ? 'border-primary bg-primary text-primary-foreground'
                        : 'border-border bg-secondary/60 text-secondary-foreground hover:bg-secondary',
                    )}
                  >
                    #{tag}
                  </button>
                );
              })}
            </div>
          </CardContent>
        </Card>

        {/* Main 4-Tab Workspace */}
        <Card className="overflow-hidden">
          <PitsTabs
            value={activeTab}
            onValueChange={handleTabChange}
            items={[
              {
                value: 'review',
                icon: <ClipboardCheck className="h-3.5 w-3.5" />,
                label: (
                  <span className="inline-flex items-center gap-1.5">
                    <span>Review Queue</span>
                    <span
                      className={cn(
                        'rounded-full px-1.5 py-0.2 font-mono text-[10px] font-semibold',
                        pendingCount > 0
                          ? 'bg-amber-500/20 text-amber-800 dark:text-amber-300'
                          : 'bg-muted text-muted-foreground',
                      )}
                    >
                      {pendingCount}
                    </span>
                  </span>
                ),
              },
              {
                value: 'board',
                icon: <Kanban className="h-3.5 w-3.5" />,
                label: `Task Board (${filteredTasks.length})`,
              },
              {
                value: 'sessions',
                icon: <History className="h-3.5 w-3.5" />,
                label: `Sessions & Decisions (${filteredSessions.length})`,
              },
              {
                value: 'projects',
                icon: <FolderKanban className="h-3.5 w-3.5" />,
                label: `Projects (${overview?.projects.length ?? 0})`,
              },
            ]}
          >
            {/* ─────────── TAB 1: REVIEW QUEUE (DEFAULT) ─────────── */}
            <PitsTabPanel value="review" className="space-y-4">
              <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-solid border-amber-500/30 bg-amber-500/5 px-4 py-3">
                <div className="flex items-center gap-2.5">
                  <ShieldCheck className="h-4 w-4 shrink-0 text-amber-600 dark:text-amber-400" />
                  <div className="text-xs text-foreground">
                    <span className="font-semibold">Protocol-Enforced Human Gate:</span>{' '}
                    Cross-workspace agents via <code className="font-mono">/mcp/hub</code> can only submit{' '}
                    <code className="font-mono">ready_for_review</code> handoffs (HTTP 403 on self-accept/Done). Only operators here can accept or reject.
                  </div>
                </div>
              </div>

              {loading && !overview ? (
                <div className="py-12 text-center text-xs text-muted-foreground">
                  Loading pending review queue…
                </div>
              ) : (overview?.pendingAcceptanceSessions.length ?? 0) === 0 ? (
                <div className="rounded-xl border border-dashed border-border py-12 text-center">
                  <CheckCircle2 className="mx-auto mb-2 h-8 w-8 text-emerald-500/70" />
                  <div className="text-sm font-medium text-foreground">
                    All handoffs have been reviewed
                  </div>
                  <p className="mt-1 text-xs text-muted-foreground">
                    Switch to Task Board to inspect active work or create a new AGY task.
                  </p>
                </div>
              ) : (
                <div className="space-y-3">
                  {overview!.pendingAcceptanceSessions.map(session => {
                    const isExpanded = expandedSessionId === session.id;
                    const linkedTasks = session.taskKeys
                      .map(k => taskMap.get(k))
                      .filter((t): t is HubTask => Boolean(t));
                    const isBusy = busySessionId === session.id;

                    return (
                      <div
                        key={session.id}
                        className="rounded-xl border border-solid border-border bg-card shadow-sm"
                      >
                        <div className="flex flex-wrap items-start justify-between gap-3 p-4">
                          <div className="min-w-0 flex-1 space-y-1.5">
                            <div className="flex flex-wrap items-center gap-2">
                              {session.taskKeys.map(k => (
                                <RouterLink
                                  key={k}
                                  to={`/project-hub/tasks/${k}`}
                                  className="inline-flex items-center gap-1 rounded-md border border-solid border-amber-500/40 bg-amber-500/15 px-2 py-0.5 font-mono text-xs font-semibold text-amber-800 hover:bg-amber-500/25 dark:text-amber-300"
                                >
                                  <span>{k}</span>
                                  <ExternalLink className="h-3 w-3" />
                                </RouterLink>
                              ))}
                              <span className="rounded-full border border-solid border-amber-500/35 bg-amber-500/10 px-2 py-0.5 text-[11px] font-medium text-amber-800 dark:text-amber-300">
                                Pending Human Acceptance
                              </span>
                              <span className="font-mono text-[11px] text-muted-foreground">
                                {session.sessionDate}
                              </span>
                            </div>

                            <h3 className="m-0 text-sm font-semibold text-foreground">
                              {session.summary}
                            </h3>

                            <div className="flex flex-wrap items-center gap-2 text-[11px] text-muted-foreground">
                              {session.commitRefs.map(c => (
                                <span
                                  key={c}
                                  className="inline-flex items-center gap-1 rounded bg-secondary px-1.5 py-0.5 font-mono text-foreground"
                                >
                                  <GitCommitHorizontal className="h-3 w-3" />
                                  {c}
                                </span>
                              ))}
                              {session.conversationId && (
                                <button
                                  type="button"
                                  onClick={() =>
                                    void copyText(
                                      `conversation://${session.conversationId}`,
                                      session.id,
                                    )
                                  }
                                  className="inline-flex cursor-pointer items-center gap-1 rounded border border-solid border-border bg-background px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground hover:text-foreground"
                                >
                                  <Copy className="h-3 w-3" />
                                  <span>
                                    {copiedId === session.id
                                      ? 'Copied conversation://'
                                      : `conversation://${session.conversationId.slice(0, 8)}…`}
                                  </span>
                                </button>
                              )}
                              {session.tags.map(t => (
                                <span
                                  key={t}
                                  className="rounded bg-muted px-1.5 py-0.5 font-mono text-[10px]"
                                >
                                  #{t}
                                </span>
                              ))}
                            </div>
                          </div>

                          <div className="flex items-center gap-2">
                            <Button
                              variant="outline"
                              size="sm"
                              onClick={() =>
                                setExpandedSessionId(isExpanded ? null : session.id)
                              }
                            >
                              {isExpanded ? 'Hide Details' : 'Review & Accept'}
                            </Button>
                          </div>
                        </div>

                        {isExpanded && (
                          <div className="space-y-4 border-t border-solid border-border bg-muted/20 p-4">
                            {session.bodyMd && (
                              <div className="rounded-lg border border-solid border-border bg-background p-3.5">
                                <div className="mb-1.5 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                                  Handoff Summary & Evidence
                                </div>
                                <pre className="m-0 whitespace-pre-wrap font-sans text-xs leading-relaxed text-foreground">
                                  {session.bodyMd}
                                </pre>
                                {session.evidenceRefs.length > 0 && (
                                  <div className="mt-2.5 flex flex-wrap gap-1.5 border-t border-solid border-border pt-2">
                                    {session.evidenceRefs.map(ev => (
                                      <span
                                        key={ev}
                                        className="rounded bg-secondary px-2 py-0.5 font-mono text-[10px] text-secondary-foreground"
                                      >
                                        {ev}
                                      </span>
                                    ))}
                                  </div>
                                )}
                              </div>
                            )}

                            {linkedTasks.map(lt => (
                              <div
                                key={lt.taskKey}
                                className="rounded-lg border border-solid border-border bg-background p-3.5"
                              >
                                <div className="mb-2 flex items-center justify-between">
                                  <div className="text-xs font-semibold text-foreground">
                                    {lt.taskKey} Acceptance Criteria ({lt.acChecklist.length})
                                  </div>
                                  <RouterLink
                                    to={`/project-hub/tasks/${lt.taskKey}`}
                                    className="text-xs text-primary hover:underline"
                                  >
                                    Open Block Editor →
                                  </RouterLink>
                                </div>
                                <div className="space-y-1.5">
                                  {lt.acChecklist.map(ac => (
                                    <div
                                      key={ac.id}
                                      className="flex items-start justify-between gap-3 rounded-md bg-muted/40 px-2.5 py-1.5 text-xs"
                                    >
                                      <span className="text-foreground">{ac.text}</span>
                                      <div className="flex shrink-0 items-center gap-1.5">
                                        <span
                                          className={cn(
                                            'rounded px-1.5 py-0.5 text-[10px] font-medium',
                                            ac.agentVerified
                                              ? 'bg-sky-500/15 text-sky-700 dark:text-sky-300'
                                              : 'bg-muted text-muted-foreground',
                                          )}
                                        >
                                          {ac.agentVerified ? 'Agent ✓' : 'Agent Pending'}
                                        </span>
                                        <span
                                          className={cn(
                                            'rounded px-1.5 py-0.5 text-[10px] font-medium',
                                            ac.humanVerified
                                              ? 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-300'
                                              : 'bg-amber-500/15 text-amber-800 dark:text-amber-300',
                                          )}
                                        >
                                          {ac.humanVerified ? 'Human ✓' : 'Awaiting Human'}
                                        </span>
                                      </div>
                                    </div>
                                  ))}
                                </div>
                              </div>
                            ))}

                            <div className="space-y-2.5 rounded-lg border border-solid border-border bg-background p-3.5">
                              <label className="block text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                                Operator Review Note (recorded in immutable audit log)
                              </label>
                              <Textarea
                                rows={2}
                                value={reviewNotes[session.id] ?? ''}
                                onChange={e =>
                                  setReviewNotes(prev => ({
                                    ...prev,
                                    [session.id]: e.target.value,
                                  }))
                                }
                                placeholder="Optional verification observations, live URLs checked, or rework instructions..."
                              />
                              <div className="flex flex-wrap items-center justify-between gap-2 pt-1">
                                <div className="flex items-center gap-2">
                                  <div className="w-40">
                                    <PitsSelect
                                      size="sm"
                                      value={rejectTargets[session.id] ?? 'in_progress'}
                                      onValueChange={v =>
                                        setRejectTargets(prev => ({
                                          ...prev,
                                          [session.id]: v as 'in_progress' | 'blocked',
                                        }))
                                      }
                                      options={[
                                        { value: 'in_progress', label: 'Reject → In Progress' },
                                        { value: 'blocked', label: 'Reject → Blocked' },
                                      ]}
                                    />
                                  </div>
                                  <Button
                                    variant="destructive"
                                    size="sm"
                                    disabled={isBusy}
                                    onClick={() => void handleRejectSession(session)}
                                  >
                                    <RotateCcw className="h-3.5 w-3.5" />
                                    <span>打回修改</span>
                                  </Button>
                                </div>

                                <Button
                                  size="sm"
                                  disabled={isBusy}
                                  className="bg-emerald-600 text-white hover:bg-emerald-700 dark:bg-emerald-500 dark:text-zinc-950 dark:hover:bg-emerald-400"
                                  onClick={() => void handleAcceptSession(session)}
                                >
                                  <CheckCircle2 className="h-3.5 w-3.5" />
                                  <span>一键验收并标记 Done</span>
                                </Button>
                              </div>
                            </div>
                          </div>
                        )}
                      </div>
                    );
                  })}
                </div>
              )}
            </PitsTabPanel>

            {/* ─────────── TAB 2: TASK BOARD (KANBAN <-> TABLE) ─────────── */}
            <PitsTabPanel value="board" className="space-y-4">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div className="text-xs text-muted-foreground">
                  Drag cards between columns to update status, or switch to sortable Data Table.
                </div>
                <div className="inline-flex items-center rounded-lg border border-solid border-border bg-muted/50 p-0.5">
                  <button
                    type="button"
                    onClick={() => setBoardView('kanban')}
                    className={cn(
                      'inline-flex cursor-pointer items-center gap-1.5 rounded-md border-0 px-2.5 py-1 text-xs font-medium transition-colors',
                      boardView === 'kanban'
                        ? 'bg-background text-foreground shadow-sm'
                        : 'bg-transparent text-muted-foreground hover:text-foreground',
                    )}
                  >
                    <Kanban className="h-3.5 w-3.5" />
                    <span>Kanban</span>
                  </button>
                  <button
                    type="button"
                    onClick={() => setBoardView('table')}
                    className={cn(
                      'inline-flex cursor-pointer items-center gap-1.5 rounded-md border-0 px-2.5 py-1 text-xs font-medium transition-colors',
                      boardView === 'table'
                        ? 'bg-background text-foreground shadow-sm'
                        : 'bg-transparent text-muted-foreground hover:text-foreground',
                    )}
                  >
                    <Table2 className="h-3.5 w-3.5" />
                    <span>Table</span>
                  </button>
                </div>
              </div>

              {boardView === 'kanban' ? (
                <DndContext sensors={sensors} onDragEnd={handleKanbanDragEnd}>
                  <div className="pits-scroll flex gap-3 overflow-x-auto pb-2">
                    {TASK_STATUS_COLUMNS.map(col => (
                      <KanbanColumn
                        key={col.id}
                        status={col.id}
                        label={col.label}
                        dot={col.dot}
                        tasks={filteredTasks.filter(t => t.status === col.id)}
                        onOpenTask={key => navigate(`/project-hub/tasks/${key}`)}
                      />
                    ))}
                  </div>
                </DndContext>
              ) : (
                <DataTable
                  columns={tableColumns}
                  data={filteredTasks}
                  onRowClick={row => navigate(`/project-hub/tasks/${row.taskKey}`)}
                />
              )}
            </PitsTabPanel>

            {/* ─────────── TAB 3: SESSIONS & DECISIONS ─────────── */}
            <PitsTabPanel value="sessions" className="space-y-4">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div className="w-52">
                  <PitsSelect
                    size="sm"
                    value={logTypeFilter}
                    onValueChange={setLogTypeFilter}
                    options={[
                      { value: 'all', label: 'All Log Types' },
                      { value: 'session_handoff', label: 'Session Handoffs' },
                      { value: 'decision_rfc', label: 'Decisions / RFCs' },
                      { value: 'blocker_pitfall', label: 'Blockers & Pitfalls' },
                      { value: 'acceptance', label: 'Acceptance Records' },
                    ]}
                  />
                </div>
                <Button size="sm" variant="outline" onClick={() => setCreateLogOpen(true)}>
                  <Plus className="h-3.5 w-3.5" />
                  <span>Log Decision / Blocker</span>
                </Button>
              </div>

              <div className="space-y-3">
                {filteredSessions.map(s => {
                  const meta = LOG_TYPE_META[s.logType] ?? LOG_TYPE_META.session_handoff;
                  return (
                    <div
                      key={s.id}
                      className="rounded-xl border border-solid border-border bg-card p-4"
                    >
                      <div className="flex flex-wrap items-start justify-between gap-2">
                        <div className="space-y-1">
                          <div className="flex flex-wrap items-center gap-1.5">
                            <span
                              className={cn(
                                'rounded border border-solid px-2 py-0.5 text-[10px] font-semibold',
                                meta.className,
                              )}
                            >
                              {meta.label}
                            </span>
                            {s.logType === 'session_handoff' &&
                              (s.accepted ? (
                                <span className="inline-flex items-center gap-1 rounded border border-solid border-emerald-500/35 bg-emerald-500/12 px-2 py-0.5 text-[10px] font-semibold text-emerald-700 dark:text-emerald-300">
                                  <CheckCircle2 className="h-3 w-3" />
                                  Accepted by {s.acceptedBy || 'human'}
                                </span>
                              ) : (
                                <span className="inline-flex items-center gap-1 rounded border border-solid border-amber-500/35 bg-amber-500/12 px-2 py-0.5 text-[10px] font-semibold text-amber-800 dark:text-amber-300">
                                  <Clock className="h-3 w-3" />
                                  Pending Acceptance
                                </span>
                              ))}
                            {s.taskKeys.map(k => (
                              <RouterLink
                                key={k}
                                to={`/project-hub/tasks/${k}`}
                                className="rounded bg-secondary px-1.5 py-0.5 font-mono text-[11px] font-semibold text-foreground hover:underline"
                              >
                                {k}
                              </RouterLink>
                            ))}
                          </div>
                          <div className="text-sm font-semibold text-foreground">
                            {s.summary}
                          </div>
                        </div>

                        <div className="flex items-center gap-2 font-mono text-[11px] text-muted-foreground">
                          <span>{s.sessionDate}</span>
                          {s.conversationId && (
                            <Button
                              variant="outline"
                              size="xs"
                              onClick={() =>
                                void copyText(
                                  `conversation://${s.conversationId}`,
                                  `sess-${s.id}`,
                                )
                              }
                            >
                              <Copy className="h-3 w-3" />
                              <span>
                                {copiedId === `sess-${s.id}`
                                  ? 'Copied'
                                  : s.conversationId.slice(0, 8)}
                              </span>
                            </Button>
                          )}
                        </div>
                      </div>

                      {s.bodyMd && (
                        <pre className="mt-2.5 mb-0 whitespace-pre-wrap rounded-lg bg-muted/40 p-3 font-sans text-xs leading-relaxed text-foreground">
                          {s.bodyMd}
                        </pre>
                      )}

                      <div className="mt-2.5 flex flex-wrap items-center gap-1.5 text-[11px]">
                        {s.commitRefs.map(c => (
                          <span
                            key={c}
                            className="inline-flex items-center gap-1 rounded bg-secondary px-1.5 py-0.5 font-mono text-foreground"
                          >
                            <GitCommitHorizontal className="h-3 w-3" />
                            {c}
                          </span>
                        ))}
                        {s.tags.map(t => (
                          <span
                            key={t}
                            className="rounded bg-muted px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground"
                          >
                            #{t}
                          </span>
                        ))}
                      </div>
                    </div>
                  );
                })}
                {filteredSessions.length === 0 && (
                  <div className="py-10 text-center text-xs text-muted-foreground">
                    No session logs match the selected filters.
                  </div>
                )}
              </div>
            </PitsTabPanel>

            {/* ─────────── TAB 4: PROJECTS ─────────── */}
            <PitsTabPanel value="projects" className="space-y-4">
              {(overview?.projects ?? []).map(proj => (
                <Card key={proj.id}>
                  <CardHeader>
                    <div className="flex flex-wrap items-start justify-between gap-2">
                      <div>
                        <CardTitle className="flex items-center gap-2 text-base">
                          <span>{proj.name}</span>
                          <Badge variant="outline" className="font-mono text-[11px]">
                            id: {proj.id}
                          </Badge>
                          <Badge variant="secondary">{proj.status}</Badge>
                        </CardTitle>
                        <CardDescription className="mt-1">
                          {proj.outcome}
                        </CardDescription>
                      </div>
                      <Button
                        size="sm"
                        disabled={savingProject}
                        onClick={() => void handleSaveProjectMd(proj)}
                      >
                        {savingProject ? 'Saving…' : 'Save Project Doc'}
                      </Button>
                    </div>
                    <div className="mt-2 flex flex-wrap items-center gap-3 font-mono text-xs text-muted-foreground">
                      <span>Workspace: {proj.workspacePath}</span>
                      <span>Lead: {proj.lead}</span>
                    </div>
                  </CardHeader>
                  <CardContent className="space-y-3">
                    <BlockNoteMarkdownEditor
                      docKey={`project-${proj.id}`}
                      initialMarkdown={proj.bodyMd}
                      onMarkdownChange={setProjectDraftMd}
                    />
                  </CardContent>
                </Card>
              ))}
            </PitsTabPanel>
          </PitsTabs>
        </Card>
      </PageBody>

      {/* ─────────── Create Task Dialog ─────────── */}
      <PitsDialog
        open={createTaskOpen}
        onOpenChange={setCreateTaskOpen}
        title="Create New AGY Task"
        subtitle="Automatically allocates the next globally monotonic AGY-<n> identifier under project PITS."
        footer={
          <>
            <Button variant="outline" size="sm" onClick={() => setCreateTaskOpen(false)}>
              Cancel
            </Button>
            <Button
              size="sm"
              disabled={creatingTask || !newTitle.trim()}
              onClick={handleCreateTask}
            >
              {creatingTask ? 'Creating…' : 'Create Task'}
            </Button>
          </>
        }
      >
        <form className="space-y-3.5" onSubmit={handleCreateTask}>
          <div>
            <label className="mb-1 block text-xs font-medium text-foreground">
              Task Title *
            </label>
            <Input
              required
              value={newTitle}
              onChange={e => setNewTitle(e.target.value)}
              placeholder="e.g. Deploy Phase 2 cross-workspace pits-project-hub plugin"
            />
          </div>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <div>
              <label className="mb-1 block text-xs font-medium text-foreground">
                Initial Status
              </label>
              <PitsSelect
                value={newStatus}
                onValueChange={v => setNewStatus(v as HubTaskStatus)}
                options={[
                  { value: 'backlog', label: 'Backlog' },
                  { value: 'ready', label: 'Ready' },
                  { value: 'in_progress', label: 'In Progress' },
                ]}
              />
            </div>
            <div>
              <label className="mb-1 block text-xs font-medium text-foreground">
                Priority
              </label>
              <PitsSelect
                value={newPriority}
                onValueChange={v => setNewPriority(v as HubTaskPriority)}
                options={[
                  { value: 'p0_critical', label: 'P0 · Critical' },
                  { value: 'p1_high', label: 'P1 · High' },
                  { value: 'p2_medium', label: 'P2 · Medium' },
                  { value: 'p3_low', label: 'P3 · Low' },
                ]}
              />
            </div>
          </div>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <div>
              <label className="mb-1 block text-xs font-medium text-foreground">
                Tags (comma-separated)
              </label>
              <Input
                value={newTagsText}
                onChange={e => setNewTagsText(e.target.value)}
                placeholder="pits-infra, portal, client"
              />
            </div>
            <div>
              <label className="mb-1 block text-xs font-medium text-foreground">
                Dispatch Target
              </label>
              <Input
                value={newDispatch}
                onChange={e => setNewDispatch(e.target.value)}
                placeholder="pits-infra | pits-service | pits-assistant"
              />
            </div>
          </div>
          <div>
            <label className="mb-1 block text-xs font-medium text-foreground">
              Workspace Path
            </label>
            <Input
              value={newWorkspace}
              onChange={e => setNewWorkspace(e.target.value)}
              placeholder="c:/Project/PITS/Infra"
            />
          </div>
          <div>
            <label className="mb-1 block text-xs font-medium text-foreground">
              Acceptance Summary
            </label>
            <Input
              value={newSummary}
              onChange={e => setNewSummary(e.target.value)}
              placeholder="Concise definition of done"
            />
          </div>
          <div>
            <label className="mb-1 block text-xs font-medium text-foreground">
              AC Checklist (one criterion per line)
            </label>
            <Textarea
              rows={3}
              value={newAcLines}
              onChange={e => setNewAcLines(e.target.value)}
              placeholder={'Rust unit tests pass\nPortal E2E zero errors'}
            />
          </div>
        </form>
      </PitsDialog>

      {/* ─────────── Create Decision/Blocker Log Dialog ─────────── */}
      <PitsDialog
        open={createLogOpen}
        onOpenChange={setCreateLogOpen}
        title="Record Decision / Blocker"
        subtitle="Creates a durable entry searchable by all cross-workspace agents via hub_get_task_context & hub_search."
        footer={
          <>
            <Button variant="outline" size="sm" onClick={() => setCreateLogOpen(false)}>
              Cancel
            </Button>
            <Button
              size="sm"
              disabled={creatingLog || !newLogSummary.trim()}
              onClick={handleCreateSessionLog}
            >
              {creatingLog ? 'Saving…' : 'Save Entry'}
            </Button>
          </>
        }
      >
        <form className="space-y-3.5" onSubmit={handleCreateSessionLog}>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <div>
              <label className="mb-1 block text-xs font-medium text-foreground">
                Entry Type
              </label>
              <PitsSelect
                value={newLogType}
                onValueChange={v => setNewLogType(v as HubLogType)}
                options={[
                  { value: 'decision_rfc', label: 'Decision / RFC' },
                  { value: 'blocker_pitfall', label: 'Blocker / Pitfall' },
                ]}
              />
            </div>
            <div>
              <label className="mb-1 block text-xs font-medium text-foreground">
                Linked Task Keys
              </label>
              <Input
                value={newLogTaskKeys}
                onChange={e => setNewLogTaskKeys(e.target.value)}
                placeholder="AGY-2, AGY-3"
              />
            </div>
          </div>
          <div>
            <label className="mb-1 block text-xs font-medium text-foreground">
              Summary *
            </label>
            <Input
              required
              value={newLogSummary}
              onChange={e => setNewLogSummary(e.target.value)}
              placeholder="[PITS] Architecture decision..."
            />
          </div>
          <div>
            <label className="mb-1 block text-xs font-medium text-foreground">
              Tags
            </label>
            <Input
              value={newLogTags}
              onChange={e => setNewLogTags(e.target.value)}
              placeholder="pits-infra, cicd"
            />
          </div>
          <div>
            <label className="mb-1 block text-xs font-medium text-foreground">
              Markdown Body
            </label>
            <Textarea
              rows={5}
              value={newLogBody}
              onChange={e => setNewLogBody(e.target.value)}
              placeholder="Context, decision rationale, and mitigation details..."
            />
          </div>
        </form>
      </PitsDialog>
    </PageShell>
  );
};

// Keep PitsCheckbox imported for shared UI check consistency
void PitsCheckbox;
