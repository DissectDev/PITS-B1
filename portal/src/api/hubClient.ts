// Typed REST API client for Self-Managed Project Hub (`/api/pits/v1/hub/*`).

export type HubProjectStatus = 'not_started' | 'in_progress' | 'paused' | 'done';
export type HubProjectPriority = 'high' | 'medium' | 'low';

export type HubTaskStatus =
  | 'backlog'
  | 'ready'
  | 'in_progress'
  | 'ready_for_review'
  | 'blocked'
  | 'done';

export type HubTaskPriority = 'p0_critical' | 'p1_high' | 'p2_medium' | 'p3_low';

export type HubLogType =
  | 'session_handoff'
  | 'decision_rfc'
  | 'blocker_pitfall'
  | 'acceptance';

export interface HubProject {
  id: string;
  name: string;
  status: HubProjectStatus;
  priority: HubProjectPriority;
  workspacePath: string;
  outcome: string;
  targetDate?: string | null;
  lead: string;
  bodyMd: string;
  catalogRefs: string[];
  createdAt: string;
  updatedAt: string;
  syncPending: boolean;
}

export interface AcChecklistItem {
  id: string;
  text: string;
  agentVerified: boolean;
  humanVerified: boolean;
}

export interface HubTask {
  seqId: number;
  taskKey: string;
  projectId: string;
  title: string;
  status: HubTaskStatus;
  priority: HubTaskPriority;
  tags: string[];
  workspacePath: string;
  dispatchTarget: string;
  acceptanceSummary: string;
  acChecklist: AcChecklistItem[];
  owner: string;
  dueDate?: string | null;
  lastSessionAt?: string | null;
  bodyMd: string;
  catalogRefs: string[];
  createdAt: string;
  updatedAt: string;
  syncPending: boolean;
}

export interface HubSessionLog {
  id: string;
  projectId: string;
  summary: string;
  logType: HubLogType;
  tags: string[];
  workspacePath: string;
  conversationId: string;
  commitRefs: string[];
  evidenceRefs: string[];
  bodyMd: string;
  accepted: boolean;
  acceptedBy?: string | null;
  acceptedAt?: string | null;
  reviewNote: string;
  sessionDate: string;
  taskKeys: string[];
  createdAt: string;
  updatedAt: string;
  syncPending: boolean;
}

export interface HubAuditEvent {
  id: string;
  entityType: string;
  entityId: string;
  action: string;
  channel: string;
  actor: string;
  detail: Record<string, unknown>;
  createdAt: string;
}

export interface HubDbSyncStatus {
  sqlitePath: string;
  sqliteVersion: string;
  journalMode: string;
  ftsEnabled: boolean;
  postgresConfigured: boolean;
  postgresUrlRedacted: string;
  postgresConnected: boolean;
  pendingOutboxRows: number;
  lastSyncAt?: string | null;
  lastSyncDirection?: string | null;
  lastSyncError?: string | null;
  projectCount: number;
  taskCount: number;
  sessionCount: number;
  auditCount: number;
}

export interface HubOverviewResponse {
  projects: HubProject[];
  tasks: HubTask[];
  recentSessions: HubSessionLog[];
  pendingReviewCount: number;
  pendingAcceptanceSessions: HubSessionLog[];
  tags: string[];
  statusCounts: Record<string, number>;
  dbSync: HubDbSyncStatus;
}

export interface HubTaskDetailResponse {
  task: HubTask;
  project?: HubProject | null;
  sessions: HubSessionLog[];
  auditEvents: HubAuditEvent[];
  portalUrl: string;
  tailscaleUrl: string;
}

export interface HubSearchResponse {
  query: string;
  engine: string;
  tasks: HubTask[];
  sessions: HubSessionLog[];
}

export interface HumanReviewOutcome {
  session: HubSessionLog;
  updatedTasks: HubTask[];
  authSource: string;
  actor: string;
}

async function hubJson<T>(input: string, init?: RequestInit): Promise<T> {
  const res = await fetch(input, {
    credentials: 'same-origin',
    ...init,
    headers: {
      'Content-Type': 'application/json',
      ...(init?.headers ?? {}),
    },
  });
  if (!res.ok) {
    let msg = `HTTP ${res.status}`;
    try {
      const body = await res.json();
      if (typeof body?.error === 'string' && body.error) {
        msg = body.error;
      }
    } catch {
      // ignore parse failure
    }
    throw new Error(msg);
  }
  return (await res.json()) as T;
}

export async function fetchHubOverview(): Promise<HubOverviewResponse> {
  return hubJson<HubOverviewResponse>('/api/pits/v1/hub/overview');
}

export async function fetchHubTasks(params?: {
  projectId?: string;
  status?: string;
  tag?: string;
  priority?: string;
  q?: string;
}): Promise<HubTask[]> {
  const sp = new URLSearchParams();
  if (params?.projectId) sp.set('projectId', params.projectId);
  if (params?.status) sp.set('status', params.status);
  if (params?.tag) sp.set('tag', params.tag);
  if (params?.priority) sp.set('priority', params.priority);
  if (params?.q) sp.set('q', params.q);
  const qs = sp.toString();
  const data = await hubJson<{ tasks: HubTask[] }>(
    `/api/pits/v1/hub/tasks${qs ? `?${qs}` : ''}`,
  );
  return data.tasks;
}

export async function createHubTask(payload: {
  projectId?: string;
  title: string;
  status?: HubTaskStatus;
  priority?: HubTaskPriority;
  tags?: string[];
  workspacePath?: string;
  dispatchTarget?: string;
  acceptanceSummary?: string;
  acChecklist?: AcChecklistItem[];
  owner?: string;
  dueDate?: string;
  bodyMd?: string;
}): Promise<HubTask> {
  const data = await hubJson<{ task: HubTask }>('/api/pits/v1/hub/tasks', {
    method: 'POST',
    body: JSON.stringify({ projectId: 'pits', ...payload }),
  });
  return data.task;
}

export async function fetchHubTaskDetail(
  taskKey: string,
): Promise<HubTaskDetailResponse> {
  return hubJson<HubTaskDetailResponse>(
    `/api/pits/v1/hub/tasks/${encodeURIComponent(taskKey)}`,
  );
}

export async function updateHubTask(
  taskKey: string,
  patch: Partial<{
    title: string;
    status: HubTaskStatus;
    priority: HubTaskPriority;
    tags: string[];
    workspacePath: string;
    dispatchTarget: string;
    acceptanceSummary: string;
    acChecklist: AcChecklistItem[];
    owner: string;
    dueDate: string | null;
    bodyMd: string;
    catalogRefs: string[];
  }>,
): Promise<HubTask> {
  const data = await hubJson<{ task: HubTask }>(
    `/api/pits/v1/hub/tasks/${encodeURIComponent(taskKey)}`,
    {
      method: 'PATCH',
      body: JSON.stringify(patch),
    },
  );
  return data.task;
}

export async function upsertHubProject(payload: {
  id?: string;
  name: string;
  status?: HubProjectStatus;
  priority?: HubProjectPriority;
  workspacePath?: string;
  outcome?: string;
  lead?: string;
  bodyMd?: string;
}): Promise<HubProject> {
  const data = await hubJson<{ project: HubProject }>('/api/pits/v1/hub/projects', {
    method: 'POST',
    body: JSON.stringify(payload),
  });
  return data.project;
}

export async function createHubSessionLog(payload: {
  projectId?: string;
  summary: string;
  logType: HubLogType;
  tags?: string[];
  workspacePath?: string;
  conversationId?: string;
  commitRefs?: string[];
  evidenceRefs?: string[];
  bodyMd?: string;
  taskKeys?: string[];
}): Promise<HubSessionLog> {
  const data = await hubJson<{ session: HubSessionLog }>('/api/pits/v1/hub/sessions', {
    method: 'POST',
    body: JSON.stringify({ projectId: 'pits', ...payload }),
  });
  return data.session;
}

export async function acceptHubSession(
  sessionId: string,
  payload: {
    markTasksDone?: boolean;
    reviewNote?: string;
    verifiedAcIds?: string[];
  } = { markTasksDone: true },
): Promise<HumanReviewOutcome> {
  return hubJson<HumanReviewOutcome>(
    `/api/pits/v1/hub/sessions/${encodeURIComponent(sessionId)}/accept`,
    {
      method: 'POST',
      body: JSON.stringify(payload),
    },
  );
}

export async function rejectHubSession(
  sessionId: string,
  payload: {
    reviewNote: string;
    targetTaskStatus?: 'in_progress' | 'blocked';
  },
): Promise<HumanReviewOutcome> {
  return hubJson<HumanReviewOutcome>(
    `/api/pits/v1/hub/sessions/${encodeURIComponent(sessionId)}/reject`,
    {
      method: 'POST',
      body: JSON.stringify(payload),
    },
  );
}

export async function searchHub(query: string, limit = 20): Promise<HubSearchResponse> {
  const sp = new URLSearchParams({ q: query, limit: String(limit) });
  return hubJson<HubSearchResponse>(`/api/pits/v1/hub/search?${sp.toString()}`);
}

export async function syncHubDatabase(): Promise<HubDbSyncStatus> {
  return hubJson<HubDbSyncStatus>('/api/pits/v1/hub/db/sync', {
    method: 'POST',
    body: JSON.stringify({}),
  });
}
