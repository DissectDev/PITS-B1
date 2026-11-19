import type { HubLogType, HubTaskPriority, HubTaskStatus } from '../../api/hubClient';

export const TASK_STATUS_COLUMNS: Array<{
  id: HubTaskStatus;
  label: string;
  shortLabel: string;
  tone: string;
  dot: string;
}> = [
  {
    id: 'backlog',
    label: 'Backlog',
    shortLabel: 'Backlog',
    tone: 'bg-zinc-500/10 text-zinc-700 dark:text-zinc-300 border-zinc-500/25',
    dot: 'bg-zinc-400',
  },
  {
    id: 'ready',
    label: 'Ready',
    shortLabel: 'Ready',
    tone: 'bg-sky-500/10 text-sky-700 dark:text-sky-300 border-sky-500/25',
    dot: 'bg-sky-500',
  },
  {
    id: 'in_progress',
    label: 'In Progress',
    shortLabel: 'In Progress',
    tone: 'bg-blue-500/10 text-blue-700 dark:text-blue-300 border-blue-500/30',
    dot: 'bg-blue-500',
  },
  {
    id: 'ready_for_review',
    label: 'Ready for Review',
    shortLabel: 'Review',
    tone: 'bg-amber-500/15 text-amber-800 dark:text-amber-300 border-amber-500/35',
    dot: 'bg-amber-500',
  },
  {
    id: 'blocked',
    label: 'Blocked',
    shortLabel: 'Blocked',
    tone: 'bg-red-500/10 text-red-700 dark:text-red-300 border-red-500/30',
    dot: 'bg-red-500',
  },
  {
    id: 'done',
    label: 'Done',
    shortLabel: 'Done',
    tone: 'bg-emerald-500/12 text-emerald-700 dark:text-emerald-300 border-emerald-500/30',
    dot: 'bg-emerald-500',
  },
];

export function statusMeta(status: string) {
  return (
    TASK_STATUS_COLUMNS.find(c => c.id === status) ?? TASK_STATUS_COLUMNS[0]
  );
}

export const PRIORITY_META: Record<
  HubTaskPriority,
  { label: string; className: string }
> = {
  p0_critical: {
    label: 'P0 · Critical',
    className:
      'bg-red-500/15 text-red-700 dark:text-red-300 border-red-500/35',
  },
  p1_high: {
    label: 'P1 · High',
    className:
      'bg-amber-500/15 text-amber-800 dark:text-amber-300 border-amber-500/35',
  },
  p2_medium: {
    label: 'P2 · Medium',
    className:
      'bg-blue-500/10 text-blue-700 dark:text-blue-300 border-blue-500/25',
  },
  p3_low: {
    label: 'P3 · Low',
    className:
      'bg-zinc-500/10 text-zinc-600 dark:text-zinc-400 border-zinc-500/25',
  },
};

export const LOG_TYPE_META: Record<
  HubLogType,
  { label: string; className: string }
> = {
  session_handoff: {
    label: 'Session Handoff',
    className:
      'bg-amber-500/15 text-amber-800 dark:text-amber-300 border-amber-500/35',
  },
  decision_rfc: {
    label: 'Decision / RFC',
    className:
      'bg-violet-500/12 text-violet-700 dark:text-violet-300 border-violet-500/30',
  },
  blocker_pitfall: {
    label: 'Blocker / Pitfall',
    className:
      'bg-red-500/12 text-red-700 dark:text-red-300 border-red-500/30',
  },
  acceptance: {
    label: 'Acceptance',
    className:
      'bg-emerald-500/12 text-emerald-700 dark:text-emerald-300 border-emerald-500/30',
  },
};

export const PRESET_HUB_TAGS = [
  'pits-infra',
  'pits-service',
  'pits-assistant',
  'pits-platform',
  'client',
  'portal',
  'server',
  'cicd',
  'ai-storage',
];

export function formatShortTime(iso?: string | null): string {
  if (!iso) return '—';
  try {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleString(undefined, {
      month: 'short',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
    });
  } catch {
    return iso;
  }
}
