import type {
  ActionExecutionResult,
  ActionInvokeRequest,
  DiscoveryCandidateRecord,
  GeneratedPkiResult,
  OperatorProfilePatch,
  OperatorProfileState,
  PitsEntityDetail,
  ResourceTypeSummary,
  SourcingDbSyncStatus,
  SourcingEvaluation,
  SourcingOverviewResponse,
  SourcingResourceRecord,
  SourcingValidationReport,
  SourcingWizardUpsertRequest,
  SourcingWizardUpsertResponse,
  TechInsightsCheckEntityResponse,
} from '../types';

const GET_TIMEOUT_MS = 15_000;
const POST_ACTION_TIMEOUT_MS = 60_000;

function resolveApiBase(): string {
  if (typeof window !== 'undefined' && window.location?.origin) {
    return window.location.origin.replace(/\/$/, '');
  }
  return '';
}

async function fetchWithTimeout(
  url: string,
  init: RequestInit = {},
  timeoutMs = GET_TIMEOUT_MS,
): Promise<Response> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    return await fetch(url, {
      ...init,
      signal: controller.signal,
    });
  } catch (err) {
    if (
      (err instanceof DOMException && err.name === 'AbortError') ||
      (err instanceof Error && err.name === 'AbortError')
    ) {
      throw new Error(`Request timed out after ${Math.round(timeoutMs / 1000)}s: ${url}`);
    }
    throw err;
  } finally {
    clearTimeout(timer);
  }
}

async function handleJsonResponse<T>(res: Response): Promise<T> {
  const text = await res.text();
  let parsed: unknown = undefined;
  if (text) {
    try {
      parsed = JSON.parse(text);
    } catch {
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}: ${text}`);
      }
      throw new Error(`Invalid JSON response from server: ${text.slice(0, 200)}`);
    }
  }
  if (!res.ok) {
    const errObj = parsed as Record<string, unknown> | undefined;
    const nestedErr =
      errObj?.error && typeof errObj.error === 'object'
        ? (errObj.error as Record<string, unknown>)
        : undefined;
    const msg =
      (typeof nestedErr?.message === 'string' && nestedErr.message) ||
      (typeof errObj?.error === 'string' && errObj.error) ||
      (typeof errObj?.message === 'string' && errObj.message) ||
      `HTTP ${res.status} ${res.statusText}`;
    throw new Error(msg);
  }
  return parsed as T;
}

export async function fetchResourceTypes(refresh = false): Promise<ResourceTypeSummary[]> {
  const base = resolveApiBase();
  const qs = refresh ? '?refresh=true' : '';
  const res = await fetchWithTimeout(
    `${base}/api/pits/v1/resource-types${qs}`,
    {
      headers: { Accept: 'application/json' },
    },
    refresh ? POST_ACTION_TIMEOUT_MS : GET_TIMEOUT_MS,
  );
  const data = await handleJsonResponse<ResourceTypeSummary[] | { items: ResourceTypeSummary[] }>(
    res,
  );
  return Array.isArray(data) ? data : data.items ?? [];
}

export async function triggerCatalogRefresh(): Promise<ResourceTypeSummary[]> {
  const base = resolveApiBase();
  const res = await fetchWithTimeout(
    `${base}/api/pits/v1/catalog/refresh`,
    {
      method: 'POST',
      headers: { Accept: 'application/json', 'Content-Type': 'application/json' },
      body: JSON.stringify({}),
    },
    POST_ACTION_TIMEOUT_MS,
  );
  const data = await handleJsonResponse<
    ResourceTypeSummary[] | { items?: ResourceTypeSummary[]; resourceTypes?: ResourceTypeSummary[] }
  >(res);
  if (Array.isArray(data)) {
    return data;
  }
  return data.items ?? data.resourceTypes ?? [];
}

export async function fetchPitsEntityDetail(
  namespace: string,
  name: string,
  refresh = false,
): Promise<PitsEntityDetail> {
  const base = resolveApiBase();
  const qs = refresh ? '?refresh=true' : '';
  const res = await fetchWithTimeout(
    `${base}/api/pits/v1/entities/${encodeURIComponent(namespace)}/${encodeURIComponent(name)}${qs}`,
    {
      headers: { Accept: 'application/json' },
    },
    refresh ? POST_ACTION_TIMEOUT_MS : GET_TIMEOUT_MS,
  );
  return handleJsonResponse<PitsEntityDetail>(res);
}

export async function invokeEntityAction(
  namespace: string,
  name: string,
  actionId: string,
  request: ActionInvokeRequest,
): Promise<ActionExecutionResult> {
  const base = resolveApiBase();
  const res = await fetchWithTimeout(
    `${base}/api/pits/v1/entities/${encodeURIComponent(namespace)}/${encodeURIComponent(
      name,
    )}/actions/${encodeURIComponent(actionId)}`,
    {
      method: 'POST',
      headers: {
        Accept: 'application/json',
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(request),
    },
    POST_ACTION_TIMEOUT_MS,
  );
  return handleJsonResponse<ActionExecutionResult>(res);
}

export async function fetchOperatorProfile(): Promise<OperatorProfileState> {
  const base = resolveApiBase();
  const res = await fetchWithTimeout(
    `${base}/api/pits/v1/profile`,
    {
      headers: { Accept: 'application/json' },
    },
    GET_TIMEOUT_MS,
  );
  const raw = await handleJsonResponse<Record<string, unknown>>(res);
  const inner =
    raw.profile && typeof raw.profile === 'object'
      ? (raw.profile as Record<string, unknown>)
      : raw;

  const str = (k: string, fallback = ''): string => {
    const v = inner[k] ?? raw[k];
    return typeof v === 'string' ? v : fallback;
  };
  const bool = (k: string, fallback = false): boolean => {
    const v = inner[k] ?? raw[k];
    return typeof v === 'boolean' ? v : fallback;
  };

  return {
    activeWorkspaceSource: str('activeWorkspaceSource'),
    agentEndpoint: str('agentEndpoint', 'https://100.95.95.88:18443'),
    hasClientPki:
      bool('hasClientPki') ||
      Boolean(inner.agentClientCertPem || raw.hasClientPki),
    hasBearerToken:
      bool('hasBearerToken') || Boolean(inner.agentBearerToken || raw.hasBearerToken),
    coreKubeconfigPath: str('coreKubeconfigPath'),
    coreContext: str('coreContext'),
    labKubeconfigPath: str('labKubeconfigPath'),
    labContext: str('labContext'),
    labViaAgentBridge: bool('labViaAgentBridge', true),
    talosconfigPath: str('talosconfigPath'),
    talosContext: str('talosContext'),
    talosViaAgentBridge: bool('talosViaAgentBridge', true),
    extraClusters: Array.isArray(inner.extraClusters)
      ? (inner.extraClusters as OperatorProfileState['extraClusters'])
      : Array.isArray(raw.extraClusters)
        ? (raw.extraClusters as OperatorProfileState['extraClusters'])
        : [],
    stateDir: str('stateDir') || undefined,
    profilePath: str('profilePath') || str('path') || undefined,
  };
}

export async function saveOperatorProfile(
  patch: OperatorProfilePatch,
): Promise<{ saved: boolean; path?: string }> {
  const base = resolveApiBase();
  const res = await fetchWithTimeout(
    `${base}/api/pits/v1/profile`,
    {
      method: 'POST',
      headers: {
        Accept: 'application/json',
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(patch),
    },
    POST_ACTION_TIMEOUT_MS,
  );
  return handleJsonResponse<{ saved: boolean; path?: string }>(res);
}

export async function generateOperatorPki(
  serverSan: string,
): Promise<GeneratedPkiResult> {
  const base = resolveApiBase();
  const res = await fetchWithTimeout(
    `${base}/api/pits/v1/profile/generate-pki`,
    {
      method: 'POST',
      headers: {
        Accept: 'application/json',
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({ serverSan }),
    },
    POST_ACTION_TIMEOUT_MS,
  );
  return handleJsonResponse<GeneratedPkiResult>(res);
}

export async function fetchSourcingOverview(
  forceLive = false,
): Promise<SourcingOverviewResponse> {
  const base = resolveApiBase();
  const qs = forceLive ? '?forceLive=true' : '';
  const res = await fetchWithTimeout(
    `${base}/api/pits/v1/sourcing/overview${qs}`,
    {
      headers: { Accept: 'application/json' },
    },
    forceLive ? POST_ACTION_TIMEOUT_MS : GET_TIMEOUT_MS,
  );
  return handleJsonResponse<SourcingOverviewResponse>(res);
}

export async function validateSourcingManifest(payload: {
  yaml?: string;
  record?: SourcingResourceRecord;
  rawPayload?: unknown;
}): Promise<SourcingValidationReport> {
  const base = resolveApiBase();
  const res = await fetchWithTimeout(
    `${base}/api/pits/v1/sourcing/validate`,
    {
      method: 'POST',
      headers: {
        Accept: 'application/json',
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(payload),
    },
    GET_TIMEOUT_MS,
  );
  return handleJsonResponse<SourcingValidationReport>(res);
}

export async function upsertSourcingAssessment(
  req: SourcingWizardUpsertRequest,
): Promise<SourcingWizardUpsertResponse> {
  const base = resolveApiBase();
  const res = await fetchWithTimeout(
    `${base}/api/pits/v1/sourcing/entities`,
    {
      method: 'POST',
      headers: {
        Accept: 'application/json',
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(req),
    },
    POST_ACTION_TIMEOUT_MS,
  );
  return handleJsonResponse<SourcingWizardUpsertResponse>(res);
}

export async function fetchTechInsightsChecksResult(
  namespace: string,
  kind: string,
  name: string,
): Promise<TechInsightsCheckEntityResponse> {
  const base = resolveApiBase();
  const res = await fetchWithTimeout(
    `${base}/api/tech-insights/checks/result/${encodeURIComponent(namespace)}/${encodeURIComponent(
      kind,
    )}/${encodeURIComponent(name)}`,
    {
      headers: { Accept: 'application/json' },
    },
    GET_TIMEOUT_MS,
  );
  return handleJsonResponse<TechInsightsCheckEntityResponse>(res);
}

export async function syncSourcingDb(
  pgUrlOverride?: string,
): Promise<SourcingDbSyncStatus> {
  const base = resolveApiBase();
  const res = await fetchWithTimeout(
    `${base}/api/pits/v1/sourcing/db/sync`,
    {
      method: 'POST',
      headers: {
        Accept: 'application/json',
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(pgUrlOverride ? { pgUrlOverride } : {}),
    },
    POST_ACTION_TIMEOUT_MS,
  );
  return handleJsonResponse<SourcingDbSyncStatus>(res);
}

export async function discoverWorkspaceDependencies(payload?: {
  rootPath?: string;
  actor?: string;
}): Promise<{
  ok: boolean;
  totalCandidates: number;
  pendingCount: number;
  candidates: DiscoveryCandidateRecord[];
  dbSync: SourcingDbSyncStatus;
}> {
  const base = resolveApiBase();
  const res = await fetchWithTimeout(
    `${base}/api/pits/v1/sourcing/discover`,
    {
      method: 'POST',
      headers: {
        Accept: 'application/json',
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(payload ?? {}),
    },
    POST_ACTION_TIMEOUT_MS,
  );
  return handleJsonResponse(res);
}

export async function disposeDiscoveryCandidate(payload: {
  candidateId: string;
  action: 'promote' | 'ignore';
  name?: string;
  resourceType?: string;
  system?: string;
  owner?: string;
  costAllocationId?: string;
  renewalDate?: string;
  vendorName?: string;
  adapterAbstracted?: boolean;
  description?: string;
  dependsOnComponents?: string[];
  actor?: string;
}): Promise<{
  ok: boolean;
  candidate: DiscoveryCandidateRecord;
  evaluation?: SourcingEvaluation;
}> {
  const base = resolveApiBase();
  const res = await fetchWithTimeout(
    `${base}/api/pits/v1/sourcing/candidates/disposition`,
    {
      method: 'POST',
      headers: {
        Accept: 'application/json',
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(payload),
    },
    POST_ACTION_TIMEOUT_MS,
  );
  return handleJsonResponse(res);
}

export async function submitSourcingBlindBallot(payload: {
  unitName: string;
  reviewerRole: string;
  reviewerId: string;
  y2_coreCompetitiveness: number;
  y4_priceElasticity: number;
  x2_marketScarcity: number;
  x3_customizationDepth: number;
  x4_complianceFragility: number;
  evidenceNotes?: string;
}): Promise<{
  ok: boolean;
  unitName: string;
  maxSpread: number;
  varianceDimensions: string[];
  varianceAlert: boolean;
  evaluation?: SourcingEvaluation;
}> {
  const base = resolveApiBase();
  const res = await fetchWithTimeout(
    `${base}/api/pits/v1/sourcing/ballots`,
    {
      method: 'POST',
      headers: {
        Accept: 'application/json',
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(payload),
    },
    POST_ACTION_TIMEOUT_MS,
  );
  return handleJsonResponse(res);
}
