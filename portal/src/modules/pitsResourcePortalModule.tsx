import React from 'react';
import {
  Activity,
  ClipboardCheck,
  FileCode2,
  FolderKanban,
  LayoutGrid,
  Moon,
  SlidersHorizontal,
  Sun,
} from 'lucide-react';
import {
  discoveryApiRef,
  fetchApiRef,
} from '@backstage/core-plugin-api';
import {
  ApiBlueprint,
  PageBlueprint,
  createFrontendModule,
  createFrontendPlugin,
  createRouteRef,
  createTranslationMessages,
} from '@backstage/frontend-plugin-api';
import {
  NavContentBlueprint,
  SignInPageBlueprint,
  ThemeBlueprint,
  TranslationBlueprint,
} from '@backstage/plugin-app-react';
import { catalogTranslationRef } from '@backstage/plugin-catalog/alpha';
import { catalogApiRef } from '@backstage/plugin-catalog-react';
import {
  EntityCardBlueprint,
  EntityContentBlueprint,
} from '@backstage/plugin-catalog-react/alpha';
import { EntitySourcingScorecardCard } from '../components/EntitySourcingScorecardCard';
import { SourcingAuditPage } from '../pages/SourcingAuditPage';
import { PitsCatalogClient } from '../api/pitsCatalogClient';
import {
  AutoGuestSignInPage,
  PitsSidebarNav,
} from '../components/PitsSidebarNav';
import { PitsShadcnThemeProvider } from '../theme/pitsShadcnTheme';

export const resourceTypesRouteRef = createRouteRef();
export const operatorProfileRouteRef = createRouteRef();
export const projectHubRouteRef = createRouteRef();
export const projectHubTaskDetailRouteRef = createRouteRef();

const resourceTypesPage = PageBlueprint.make({
  name: 'resource-types',
  params: {
    path: '/resource-types',
    title: 'Resource Types',
    icon: <LayoutGrid className="h-4 w-4" />,
    noHeader: true,
    routeRef: resourceTypesRouteRef,
    loader: () =>
      import('../pages/ResourceTypeDirectoryPage').then(m => (
        <m.ResourceTypesRouteDispatcher />
      )),
  },
});

const operatorProfilePage = PageBlueprint.make({
  name: 'operator-profile',
  params: {
    path: '/settings/pits-profile',
    title: 'Operator & mTLS',
    icon: <SlidersHorizontal className="h-4 w-4" />,
    noHeader: true,
    routeRef: operatorProfileRouteRef,
    loader: () =>
      import('../pages/OperatorProfilePage').then(m => (
        <m.OperatorProfilePage />
      )),
  },
});

const projectHubPage = PageBlueprint.make({
  name: 'project-hub',
  params: {
    path: '/project-hub',
    title: 'Project Hub',
    icon: <FolderKanban className="h-4 w-4" />,
    noHeader: true,
    routeRef: projectHubRouteRef,
    loader: () =>
      import('../pages/ProjectHubPage').then(m => <m.ProjectHubPage />),
  },
});

const projectHubTaskDetailPage = PageBlueprint.make({
  name: 'project-hub-task-detail',
  params: {
    path: '/project-hub/tasks/:taskKey',
    title: 'Task Detail',
    icon: <ClipboardCheck className="h-4 w-4" />,
    noHeader: true,
    routeRef: projectHubTaskDetailRouteRef,
    loader: () =>
      import('../pages/ProjectHubTaskDetailPage').then(m => (
        <m.ProjectHubTaskDetailPage />
      )),
  },
});

const entityStatusSummaryCard = EntityCardBlueprint.make({
  name: 'pits-status-summary',
  params: {
    type: 'content',
    filter: { kind: 'resource' },
    loader: () =>
      import('../components/EntityOverviewCards').then(m => (
        <m.EntityStatusSummaryCard />
      )),
  },
});

const entityAttributesCard = EntityCardBlueprint.make({
  name: 'pits-attributes',
  params: {
    type: 'content',
    filter: { kind: 'resource' },
    loader: () =>
      import('../components/EntityOverviewCards').then(m => (
        <m.EntityAttributesCard />
      )),
  },
});

const entityContextualActionsCard = EntityCardBlueprint.make({
  name: 'pits-contextual-actions',
  params: {
    type: 'info',
    filter: { kind: 'resource' },
    loader: () =>
      import('../components/EntityOverviewCards').then(m => (
        <m.EntityContextualActionsCard />
      )),
  },
});

const entityManifestsContent = EntityContentBlueprint.make({
  name: 'pits-manifests',
  params: {
    path: 'manifests',
    title: 'Manifests & YAML',
    group: 'deployment',
    icon: <FileCode2 className="h-4 w-4" />,
    filter: { kind: 'resource' },
    loader: () =>
      import('../components/EntityManifestsTab').then(m => (
        <m.EntityManifestsTab />
      )),
  },
});

const entityDiagnosticsContent = EntityContentBlueprint.make({
  name: 'pits-diagnostics',
  params: {
    path: 'diagnostics',
    title: 'Diagnostics',
    group: 'observability',
    icon: <Activity className="h-4 w-4" />,
    filter: { kind: 'resource' },
    loader: () =>
      import('../components/EntityDiagnosticsTab').then(m => (
        <m.EntityDiagnosticsTab />
      )),
  },
});

const sourcingAuditRouteRef = createRouteRef();
const sourcingAuditWizardRouteRef = createRouteRef();

const sourcingAuditPageExtension = PageBlueprint.make({
  name: 'sourcing-audit',
  params: {
    path: '/sourcing-audit',
    routeRef: sourcingAuditRouteRef,
    loader: async () => <SourcingAuditPage />,
  },
});

const sourcingAuditWizardPageExtension = PageBlueprint.make({
  name: 'sourcing-audit-wizard',
  params: {
    path: '/sourcing-audit/wizard',
    routeRef: sourcingAuditWizardRouteRef,
    loader: async () => <SourcingAuditPage />,
  },
});

const entitySourcingScorecardCardExtension = EntityCardBlueprint.make({
  name: 'sourcing-scorecard',
  params: {
    type: 'content',
    filter: entity =>
      entity.kind === 'Resource' &&
      ['vendor-saas', 'commercial-sdk', 'managed-infra', 'outsourced-dev'].includes(
        String(entity.spec?.type ?? '').toLowerCase(),
      ),
    loader: async () => <EntitySourcingScorecardCard />,
  },
});

export const pitsResourcePortalPlugin = createFrontendPlugin({
  pluginId: 'pits-portal',
  title: 'PITS Resource & Sourcing Portal',
  icon: <LayoutGrid className="h-4 w-4" />,
  routes: {
    resourceTypes: resourceTypesRouteRef,
    operatorProfile: operatorProfileRouteRef,
    projectHub: projectHubRouteRef,
    projectHubTaskDetail: projectHubTaskDetailRouteRef,
    sourcingAudit: sourcingAuditRouteRef,
    sourcingAuditWizard: sourcingAuditWizardRouteRef,
  },
  extensions: [
    resourceTypesPage,
    operatorProfilePage,
    projectHubPage,
    projectHubTaskDetailPage,
    sourcingAuditPageExtension,
    sourcingAuditWizardPageExtension,
    entityStatusSummaryCard,
    entityAttributesCard,
    entityContextualActionsCard,
    entitySourcingScorecardCardExtension,
    entityManifestsContent,
    entityDiagnosticsContent,
  ],
});

const pitsLightThemeExtension = ThemeBlueprint.make({
  name: 'light',
  params: {
    theme: {
      id: 'light',
      title: 'PITS Light (Zinc)',
      variant: 'light',
      icon: <Sun className="h-4 w-4" />,
      Provider: ({ children }) => (
        <PitsShadcnThemeProvider variant="light">{children}</PitsShadcnThemeProvider>
      ),
    },
  },
});

const pitsDarkThemeExtension = ThemeBlueprint.make({
  name: 'dark',
  params: {
    theme: {
      id: 'dark',
      title: 'PITS Dark (Zinc)',
      variant: 'dark',
      icon: <Moon className="h-4 w-4" />,
      Provider: ({ children }) => (
        <PitsShadcnThemeProvider variant="dark">{children}</PitsShadcnThemeProvider>
      ),
    },
  },
});

const pitsNavContentExtension = NavContentBlueprint.make({
  params: {
    component: PitsSidebarNav,
  },
});

const pitsAutoGuestSignInExtension = SignInPageBlueprint.make({
  params: {
    loader: async () => AutoGuestSignInPage,
  },
});

const pitsCatalogTranslationExtension = TranslationBlueprint.make({
  name: 'pits-entity-registry-i18n',
  params: {
    resource: createTranslationMessages({
      ref: catalogTranslationRef,
      full: false,
      messages: {
        'indexPage.title': 'PITS Entity Registry',
        'indexPage.supportButtonContent':
          'All registered infrastructure & sourcing governance entities across PITS 4-layer catalog',
      },
    }),
  },
});

export const pitsAppOverridesModule = createFrontendModule({
  pluginId: 'app',
  extensions: [
    pitsLightThemeExtension,
    pitsDarkThemeExtension,
    pitsNavContentExtension,
    pitsAutoGuestSignInExtension,
    pitsCatalogTranslationExtension,
  ],
});

const pitsCatalogApiExtension = ApiBlueprint.make({
  params: defineParams =>
    defineParams({
      api: catalogApiRef,
      deps: {
        discoveryApi: discoveryApiRef,
        fetchApi: fetchApiRef,
      },
      factory: ({ discoveryApi, fetchApi }) =>
        new PitsCatalogClient({ discoveryApi, fetchApi }),
    }),
});

export const pitsCatalogOverridesModule = createFrontendModule({
  pluginId: 'catalog',
  extensions: [pitsCatalogApiExtension],
});
