import React from 'react';
import AppsIcon from '@mui/icons-material/Apps';
import TuneOutlinedIcon from '@mui/icons-material/TuneOutlined';
import DescriptionOutlinedIcon from '@mui/icons-material/DescriptionOutlined';
import MonitorHeartOutlinedIcon from '@mui/icons-material/MonitorHeartOutlined';
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

export const resourceTypesRouteRef = createRouteRef();
export const operatorProfileRouteRef = createRouteRef();

const resourceTypesPage = PageBlueprint.make({
  name: 'resource-types',
  params: {
    path: '/resource-types',
    title: 'Resource Types',
    icon: <AppsIcon />,
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
    icon: <TuneOutlinedIcon />,
    noHeader: true,
    routeRef: operatorProfileRouteRef,
    loader: () =>
      import('../pages/OperatorProfilePage').then(m => (
        <m.OperatorProfilePage />
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
    icon: <DescriptionOutlinedIcon />,
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
    icon: <MonitorHeartOutlinedIcon />,
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
  icon: <AppsIcon />,
  routes: {
    resourceTypes: resourceTypesRouteRef,
    operatorProfile: operatorProfileRouteRef,
    sourcingAudit: sourcingAuditRouteRef,
    sourcingAuditWizard: sourcingAuditWizardRouteRef,
  },
  extensions: [
    resourceTypesPage,
    operatorProfilePage,
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
