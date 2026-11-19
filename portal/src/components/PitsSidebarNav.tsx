import React, { useEffect, useState } from 'react';
import { Link as RouterLink, useLocation } from 'react-router-dom';
import {
  BarChart3,
  Box,
  Boxes,
  Building2,
  ClipboardCheck,
  Cloud,
  CloudCog,
  Code2,
  Cpu,
  Database,
  Download,
  FolderKanban,
  GitBranch,
  HardDrive,
  History,
  Kanban,
  Layers,
  LayoutGrid,
  Loader2,
  Menu,
  Monitor,
  MonitorSmartphone,
  Moon,
  Network,
  PanelLeftClose,
  PanelLeftOpen,
  Rocket,
  Router,
  Server,
  ShieldCheck,
  SlidersHorizontal,
  Sun,
  Users,
  X,
} from 'lucide-react';
import type { IdentityApi } from '@backstage/frontend-plugin-api';
import type {
  NavContentComponent,
  SignInPageProps,
} from '@backstage/plugin-app-react';
import { cn } from '../lib/utils';
import { PitsTooltip } from './ui/pits-primitives';
import { usePitsThemeMode, type PitsThemeMode } from '../theme/pitsShadcnTheme';

interface NavEntry {
  to: string;
  label: string;
  icon: React.ComponentType<{ className?: string }>;
  matchExact?: boolean;
  badgeKey?: 'hubPending';
}

interface NavGroupSpec {
  label: string;
  items: NavEntry[];
}

const TOP_NAV_ITEMS: NavEntry[] = [
  { to: '/resource-types', label: 'Resource Directory', icon: LayoutGrid, matchExact: true },
  { to: '/catalog', label: 'Entity Registry', icon: Boxes, matchExact: true },
  { to: '/catalog-graph', label: 'Topology Graph', icon: GitBranch },
];

const NAV_GROUPS: NavGroupSpec[] = [
  {
    label: 'Project Hub',
    items: [
      { to: '/project-hub', label: 'Review Queue', icon: ClipboardCheck, matchExact: true, badgeKey: 'hubPending' },
      { to: '/project-hub?tab=board', label: 'Task Board', icon: Kanban },
      { to: '/project-hub?tab=sessions', label: 'Sessions & RFCs', icon: History },
      { to: '/project-hub?tab=projects', label: 'Projects (PITS)', icon: FolderKanban },
    ],
  },
  {
    label: 'Architecture · 4-Layer',
    items: [
      { to: '/catalog?filters%5Bkind%5D=domain', label: 'Domains', icon: Building2 },
      { to: '/catalog?filters%5Bkind%5D=system', label: 'Systems', icon: Layers },
      { to: '/catalog?filters%5Bkind%5D=component', label: 'Components', icon: Box },
    ],
  },
  {
    label: 'Compute',
    items: [
      { to: '/resource-types/baremetal-host', label: 'Baremetal Hosts', icon: Server },
      { to: '/resource-types/npu-accelerator', label: 'AI NPU Accelerators', icon: Cpu },
      { to: '/resource-types/gpu-accelerator', label: 'AI GPU Accelerators', icon: Cpu },
      { to: '/resource-types/virtual-machine', label: 'Virtual Machines', icon: MonitorSmartphone },
    ],
  },
  {
    label: 'Network & Boot',
    items: [
      { to: '/resource-types/edge-router', label: 'Edge Routers', icon: Router },
      { to: '/resource-types/pxe-boot-profile', label: 'PXE Boot Profiles', icon: Download },
    ],
  },
  {
    label: 'Storage',
    items: [
      { to: '/resource-types/storage-pool', label: 'Storage Pools', icon: Database },
      { to: '/resource-types/san-volume', label: 'SAN Volumes', icon: HardDrive },
      { to: '/resource-types/ai-storage-workspace', label: 'AI Storage', icon: CloudCog },
    ],
  },
  {
    label: 'Control Planes',
    items: [
      { to: '/resource-types/kubernetes-cluster', label: 'K8s Clusters', icon: Network },
      { to: '/resource-types/cicd-pipeline', label: 'CI/CD Pipelines', icon: Rocket },
      { to: '/resource-types/auth-flow-bundle', label: 'IAM Auth Flows', icon: ShieldCheck },
    ],
  },
  {
    label: 'Sourcing Governance',
    items: [
      { to: '/sourcing-audit', label: 'Sourcing Matrix & Sync', icon: BarChart3 },
      { to: '/resource-types/vendor-saas', label: 'Vendor SaaS', icon: Cloud },
      { to: '/resource-types/commercial-sdk', label: 'Commercial SDKs', icon: Code2 },
      { to: '/resource-types/managed-infra', label: 'Managed Infra', icon: Router },
      { to: '/resource-types/outsourced-dev', label: 'Outsourced Dev', icon: Users },
    ],
  },
];

function isNavEntryActive(
  entry: NavEntry,
  pathname: string,
  search: string,
): boolean {
  const [targetPath, targetSearch] = entry.to.split('?');
  if (targetSearch) {
    return (
      pathname === targetPath &&
      decodeURIComponent(search.replace(/^\?/, '')) === decodeURIComponent(targetSearch)
    );
  }
  if (entry.matchExact) {
    return pathname === targetPath && !search;
  }
  return pathname === targetPath || pathname.startsWith(`${targetPath}/`);
}

const ThemeModeSwitcher: React.FC<{ collapsed: boolean }> = ({ collapsed }) => {
  const { mode, setMode } = usePitsThemeMode();

  const options: Array<{ id: PitsThemeMode; label: string; icon: React.ComponentType<{ className?: string }> }> = [
    { id: 'system', label: 'System', icon: Monitor },
    { id: 'light', label: 'Light', icon: Sun },
    { id: 'dark', label: 'Dark', icon: Moon },
  ];

  if (collapsed) {
    const nextMode: PitsThemeMode =
      mode === 'system' ? 'light' : mode === 'light' ? 'dark' : 'system';
    const CurrentIcon =
      mode === 'light' ? Sun : mode === 'dark' ? Moon : Monitor;
    return (
      <PitsTooltip side="right" content={`Theme: ${mode} (click to cycle)`}>
        <button
          type="button"
          onClick={() => setMode(nextMode)}
          aria-label="Cycle theme mode"
          className="mx-auto flex h-8 w-8 cursor-pointer items-center justify-center rounded-md border border-solid border-sidebar-border bg-background text-sidebar-foreground transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
        >
          <CurrentIcon className="h-3.5 w-3.5" />
        </button>
      </PitsTooltip>
    );
  }

  return (
    <div
      role="group"
      aria-label="Theme preference"
      className="grid grid-cols-3 gap-0.5 rounded-lg border border-solid border-sidebar-border bg-muted/60 p-0.5"
    >
      {options.map(opt => {
        const Icon = opt.icon;
        const active = mode === opt.id;
        return (
          <button
            key={opt.id}
            type="button"
            onClick={() => setMode(opt.id)}
            className={cn(
              'inline-flex h-7 cursor-pointer items-center justify-center gap-1 rounded-md border border-solid px-1.5 text-[11px] font-medium transition-all',
              active
                ? 'border-border bg-background text-foreground shadow-sm'
                : 'border-transparent bg-transparent text-muted-foreground hover:text-foreground',
            )}
          >
            <Icon className="h-3 w-3" />
            <span>{opt.label}</span>
          </button>
        );
      })}
    </div>
  );
};

export const PitsSidebarNav: NavContentComponent = () => {
  const location = useLocation();
  const [collapsed, setCollapsed] = useState<boolean>(() => {
    try {
      return window.localStorage.getItem('pits.sidebar.collapsed') === 'true';
    } catch {
      return false;
    }
  });
  const [mobileOpen, setMobileOpen] = useState(false);
  const [hubPendingCount, setHubPendingCount] = useState<number>(0);

  useEffect(() => {
    let active = true;
    fetch('/api/pits/v1/hub/overview', { credentials: 'same-origin' })
      .then(r => (r.ok ? r.json() : null))
      .then(data => {
        if (active && data && typeof data.pendingReviewCount === 'number') {
          setHubPendingCount(data.pendingReviewCount);
        }
      })
      .catch(() => {
        // Non-blocking badge fetch
      });
    return () => {
      active = false;
    };
  }, [location.pathname, location.search]);

  useEffect(() => {
    const width = collapsed ? '64px' : '248px';
    document.documentElement.style.setProperty('--pits-sidebar-w', width);
    try {
      window.localStorage.setItem('pits.sidebar.collapsed', String(collapsed));
    } catch {
      // Ignore storage failures
    }
  }, [collapsed]);

  useEffect(() => {
    setMobileOpen(false);
  }, [location.pathname, location.search]);

  const renderItem = (entry: NavEntry, isCompact: boolean) => {
    const Icon = entry.icon;
    const active = isNavEntryActive(entry, location.pathname, location.search);

    const linkEl = (
      <RouterLink
        key={entry.to}
        to={entry.to}
        className={cn(
          'group relative flex items-center gap-2.5 rounded-md px-2.5 py-1.5 text-[13px] transition-colors',
          isCompact && 'justify-center px-0 py-2',
          active
            ? 'bg-sidebar-accent font-medium text-sidebar-accent-foreground'
            : 'font-normal text-sidebar-foreground/80 hover:bg-sidebar-accent/70 hover:text-sidebar-accent-foreground',
        )}
      >
        {active && (
          <span className="absolute left-0 top-1.5 bottom-1.5 w-[2.5px] rounded-full bg-sidebar-primary" />
        )}
        <Icon
          className={cn(
            'h-4 w-4 shrink-0 transition-colors',
            active ? 'text-sidebar-accent-foreground' : 'text-muted-foreground group-hover:text-foreground',
          )}
        />
        {!isCompact && <span className="truncate">{entry.label}</span>}
        {!isCompact && entry.badgeKey === 'hubPending' && hubPendingCount > 0 && (
          <span className="ml-auto rounded-full bg-amber-500/20 px-1.5 py-0.5 font-mono text-[10px] font-semibold text-amber-800 dark:text-amber-300">
            {hubPendingCount}
          </span>
        )}
      </RouterLink>
    );

    if (isCompact) {
      return (
        <PitsTooltip key={entry.to} side="right" content={entry.label}>
          {linkEl}
        </PitsTooltip>
      );
    }
    return linkEl;
  };

  const renderSidebarBody = (isCompact: boolean) => (
    <div className="flex h-full flex-col">
      {/* Brand header */}
      <div
        className={cn(
          'flex h-14 shrink-0 items-center border-b border-solid border-sidebar-border px-3',
          isCompact ? 'justify-center' : 'justify-between',
        )}
      >
        <RouterLink
          to="/resource-types"
          className={cn('flex min-w-0 items-center gap-2.5', isCompact && 'justify-center')}
        >
          <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-sidebar-primary text-xs font-bold text-sidebar-primary-foreground shadow-sm">
            P
          </span>
          {!isCompact && (
            <div className="min-w-0">
              <div className="truncate text-[13px] font-semibold leading-tight text-foreground">
                PITS Portal
              </div>
              <div className="truncate text-[11px] leading-tight text-muted-foreground">
                Entity Registry · v1.56
              </div>
            </div>
          )}
        </RouterLink>

        {!isCompact && (
          <PitsTooltip side="right" content="Collapse sidebar">
            <button
              type="button"
              onClick={() => setCollapsed(true)}
              aria-label="Collapse sidebar"
              className="hidden h-7 w-7 shrink-0 cursor-pointer items-center justify-center rounded-md border border-solid border-transparent bg-transparent text-muted-foreground transition-colors hover:bg-sidebar-accent hover:text-foreground sm:inline-flex"
            >
              <PanelLeftClose className="h-4 w-4" />
            </button>
          </PitsTooltip>
        )}
      </div>

      {/* Expand button when collapsed */}
      {isCompact && (
        <div className="flex justify-center border-b border-solid border-sidebar-border py-2">
          <PitsTooltip side="right" content="Expand sidebar">
            <button
              type="button"
              onClick={() => setCollapsed(false)}
              aria-label="Expand sidebar"
              className="inline-flex h-7 w-7 cursor-pointer items-center justify-center rounded-md border border-solid border-transparent bg-transparent text-muted-foreground transition-colors hover:bg-sidebar-accent hover:text-foreground"
            >
              <PanelLeftOpen className="h-4 w-4" />
            </button>
          </PitsTooltip>
        </div>
      )}

      {/* Primary Registry items */}
      <div className="shrink-0 space-y-0.5 border-b border-solid border-sidebar-border p-2">
        {TOP_NAV_ITEMS.map(item => renderItem(item, isCompact))}
      </div>

      {/* Scrollable domain navigation */}
      <nav
        aria-label="Infrastructure navigation"
        className="pits-scroll flex-1 space-y-3 overflow-y-auto p-2"
      >
        {NAV_GROUPS.map(group => (
          <div key={group.label} className="space-y-0.5">
            {!isCompact && (
              <div className="px-2.5 pb-1 pt-1.5 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground/80">
                {group.label}
              </div>
            )}
            {group.items.map(item => renderItem(item, isCompact))}
          </div>
        ))}
      </nav>

      {/* Footer: Theme switcher + Settings */}
      <div className="shrink-0 space-y-2 border-t border-solid border-sidebar-border p-2.5">
        <ThemeModeSwitcher collapsed={isCompact} />
        {renderItem(
          {
            to: '/settings/pits-profile',
            label: 'Operator & mTLS',
            icon: SlidersHorizontal,
          },
          isCompact,
        )}
      </div>
    </div>
  );

  return (
    <>
      {/* Desktop fixed sidebar */}
      <aside
        data-pits-sidebar="true"
        className={cn(
          'pits-ui fixed bottom-0 left-0 top-0 z-40 hidden overflow-hidden border-r border-solid border-sidebar-border bg-sidebar text-sidebar-foreground transition-[width] duration-200 sm:block',
          collapsed ? 'w-[64px]' : 'w-[248px]',
        )}
      >
        {renderSidebarBody(collapsed)}
      </aside>

      {/* Mobile trigger button */}
      <button
        type="button"
        onClick={() => setMobileOpen(true)}
        aria-label="Open navigation"
        className="pits-ui fixed bottom-4 right-4 z-40 inline-flex h-11 w-11 cursor-pointer items-center justify-center rounded-full border border-solid border-border bg-primary text-primary-foreground shadow-lg sm:hidden"
      >
        <Menu className="h-5 w-5" />
      </button>

      {/* Mobile slide-over drawer */}
      {mobileOpen && (
        <div className="pits-ui fixed inset-0 z-50 sm:hidden">
          <div
            className="absolute inset-0 bg-black/50 backdrop-blur-[1px]"
            onClick={() => setMobileOpen(false)}
          />
          <aside className="relative z-10 h-full w-[268px] border-r border-solid border-sidebar-border bg-sidebar text-sidebar-foreground shadow-2xl">
            <button
              type="button"
              onClick={() => setMobileOpen(false)}
              aria-label="Close navigation"
              className="absolute right-3 top-3.5 z-20 inline-flex h-7 w-7 cursor-pointer items-center justify-center rounded-md border-0 bg-transparent text-muted-foreground hover:bg-sidebar-accent hover:text-foreground"
            >
              <X className="h-4 w-4" />
            </button>
            {renderSidebarBody(false)}
          </aside>
        </div>
      )}
    </>
  );
};

interface GuestSessionPayload {
  profile?: {
    displayName?: string;
    email?: string;
  };
  backstageIdentity?: {
    token?: string;
    identity?: {
      type?: 'user';
      userEntityRef?: string;
      ownershipEntityRefs?: string[];
    };
  };
  token?: string;
  identity?: {
    type?: 'user';
    userEntityRef?: string;
    ownershipEntityRefs?: string[];
  };
}

async function fetchGuestSession(): Promise<{
  token?: string;
  displayName: string;
  email: string;
  userEntityRef: string;
  ownershipEntityRefs: string[];
}> {
  const origin =
    typeof window !== 'undefined' && window.location?.origin
      ? window.location.origin.replace(/\/$/, '')
      : '';
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), 2500);
  try {
    const res = await fetch(`${origin}/api/auth/guest/refresh`, {
      method: 'POST',
      headers: {
        Accept: 'application/json',
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({}),
      signal: controller.signal,
    });
    if (res.ok) {
      const data = (await res.json()) as GuestSessionPayload;
      const token = data.backstageIdentity?.token || data.token;
      const idObj = data.backstageIdentity?.identity || data.identity;
      return {
        token,
        displayName: data.profile?.displayName || 'PITS Operator',
        email: data.profile?.email || 'pitsadmin@pits.home.arpa',
        userEntityRef: idObj?.userEntityRef || 'user:default/pitsadmin',
        ownershipEntityRefs: idObj?.ownershipEntityRefs || [
          'user:default/pitsadmin',
          'group:default/operators',
          'group:default/risk-engineering',
        ],
      };
    }
  } catch {
    // Fallback to local guest identity when running standalone preview
  } finally {
    clearTimeout(timeoutId);
  }
  return {
    token: undefined,
    displayName: 'PITS Operator',
    email: 'pitsadmin@pits.home.arpa',
    userEntityRef: 'user:default/pitsadmin',
    ownershipEntityRefs: [
      'user:default/pitsadmin',
      'group:default/operators',
      'group:default/risk-engineering',
    ],
  };
}

export const AutoGuestSignInPage: React.FC<SignInPageProps> = ({ onSignInSuccess }) => {
  useEffect(() => {
    let cancelled = false;

    void (async () => {
      const session = await fetchGuestSession();
      if (cancelled) return;
      let cachedToken = session.token;
      const cookieAuthMode = !session.token;

      const identityApi: IdentityApi & {
        getUserId: () => string;
        getProfile: () => { displayName: string; email: string };
        getIdToken: () => Promise<string | undefined>;
      } = {
        getUserId: () =>
          session.userEntityRef.split('/').pop() || 'pitsadmin',
        getProfile: () => ({
          displayName: session.displayName,
          email: session.email,
        }),
        getIdToken: async () => cachedToken,
        getProfileInfo: async () => ({
          displayName: session.displayName,
          email: session.email,
        }),
        getBackstageIdentity: async () => ({
          type: 'user',
          userEntityRef: session.userEntityRef,
          ownershipEntityRefs: session.ownershipEntityRefs,
        }),
        getCredentials: async () => {
          if (!cachedToken && !cookieAuthMode) {
            const refreshed = await fetchGuestSession();
            cachedToken = refreshed.token;
          }
          return cachedToken ? { token: cachedToken } : {};
        },
        signOut: async () => {
          cachedToken = undefined;
        },
      };

      onSignInSuccess(identityApi);
    })();

    return () => {
      cancelled = true;
    };
  }, [onSignInSuccess]);

  return (
    <div className="pits-ui flex min-h-screen flex-col items-center justify-center gap-3 bg-background text-foreground">
      <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
      <p className="m-0 text-xs font-medium text-muted-foreground">
        Initializing PITS Operator Guest Session...
      </p>
    </div>
  );
};
