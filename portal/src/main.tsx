import '@backstage/ui/css/styles.css';
import './global.css';
import React from 'react';
import ReactDOM from 'react-dom/client';
import { ConfigReader } from '@backstage/config';
import { createApp } from '@backstage/frontend-defaults';
import catalogPlugin from '@backstage/plugin-catalog/alpha';
import catalogGraphPlugin from '@backstage/plugin-catalog-graph/alpha';
import {
  pitsAppOverridesModule,
  pitsCatalogOverridesModule,
  pitsResourcePortalPlugin,
} from './modules/pitsResourcePortalModule';

const runtimeOrigin =
  typeof window !== 'undefined' && window.location?.origin
    ? window.location.origin.replace(/\/$/, '')
    : 'http://127.0.0.1:18090';

const app = createApp({
  features: [
    catalogPlugin,
    catalogGraphPlugin,
    pitsResourcePortalPlugin,
    pitsAppOverridesModule,
    pitsCatalogOverridesModule,
  ],
  bindRoutes({ bind }) {
    bind(catalogGraphPlugin.externalRoutes, {
      catalogEntity: catalogPlugin.routes.catalogEntity,
    });
  },
  advanced: {
    configLoader: async () => ({
      config: new ConfigReader(
        {
          app: {
            title: 'PITS Entity Registry & Sourcing Portal (Backstage)',
            baseUrl: runtimeOrigin,
            extensions: [
              {
                'app/routes': {
                  config: {
                    redirects: [{ from: '/', to: '/resource-types' }],
                  },
                },
              },
              {
                'catalog-filter:catalog/kind': {
                  config: {
                    initialFilter: 'resource',
                  },
                },
              },
              {
                'catalog-filter:catalog/list': {
                  config: {
                    initialFilter: 'all',
                  },
                },
              },
              {
                'entity-card:catalog-graph/relations': {
                  config: {
                    title: 'Entity Dependency Topology',
                    maxDepth: 2,
                    height: 360,
                    zoom: 'enable-on-click',
                  },
                },
              },
              {
                'page:catalog': {
                  config: {
                    title: 'PITS Entity Registry',
                  },
                },
              },
              {
                'page:catalog-graph': {
                  config: {
                    title: 'PITS 4-Layer Entity Topology (Domain → System → Component → Resource)',
                    rootEntityRefs: [
                      'domain:default/core-commerce',
                      'domain:default/platform-infra',
                      'domain:default/spatial-simulation',
                    ],
                    maxDepth: 4,
                    unidirectional: true,
                    mergeRelations: true,
                  },
                },
              },
              {
                'entity-context-menu-item:catalog/unregister-entity': false,
              },
            ],
          },
          organization: {
            name: 'PITS Infrastructure',
          },
          backend: {
            baseUrl: runtimeOrigin,
          },
        },
        'pits-runtime-config',
      ),
    }),
  },
});

class PortalRootErrorBoundary extends React.Component<
  { children: React.ReactNode },
  { error: Error | null }
> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  render() {
    if (this.state.error) {
      return (
        <div style={{ padding: 24, fontFamily: 'monospace' }}>
          <h2>PITS Portal Startup Error</h2>
          <pre style={{ whiteSpace: 'pre-wrap', color: '#b91c1c' }}>
            {this.state.error.stack || this.state.error.message}
          </pre>
        </div>
      );
    }
    return this.props.children;
  }
}

const rootEl = document.getElementById('root');
if (!rootEl) {
  throw new Error('Missing #root container in index.html');
}

ReactDOM.createRoot(rootEl).render(
  <PortalRootErrorBoundary>{app.createRoot()}</PortalRootErrorBoundary>,
);
