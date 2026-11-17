import {
  CATALOG_FILTER_EXISTS,
  CatalogClient,
  type CatalogRequestOptions,
  type EntityFieldsQuery,
  type EntityFilterQuery,
  type EntityOrderQuery,
  type GetEntitiesByRefsRequest,
  type GetEntitiesByRefsResponse,
  type GetEntitiesRequest,
  type GetEntitiesResponse,
  type GetEntityAncestorsRequest,
  type GetEntityAncestorsResponse,
  type GetEntityFacetsRequest,
  type GetEntityFacetsResponse,
  type QueryEntitiesCursorRequest,
  type QueryEntitiesInitialRequest,
  type QueryEntitiesRequest,
  type QueryEntitiesResponse,
} from '@backstage/catalog-client';
import {
  parseEntityRef,
  stringifyEntityRef,
  type CompoundEntityRef,
  type Entity,
} from '@backstage/catalog-model';
import type { DiscoveryApi, FetchApi } from '@backstage/core-plugin-api';

type AnyPredicate = Record<string, unknown> | string | number | boolean | null;

interface EncodedCursorPayload {
  offset: number;
  filter?: EntityFilterQuery;
  query?: Record<string, unknown>;
  orderFields?: Array<{ field: string; order: 'asc' | 'desc' }>;
  fullTextFilter?: { term: string; fields?: string[] };
}

function buildVirtualPrincipalEntities(): Entity[] {
  return [
    {
      apiVersion: 'backstage.io/v1alpha1',
      kind: 'Group',
      metadata: {
        name: 'operators',
        namespace: 'default',
        uid: 'pits-virtual-group-operators',
        title: 'PITS Infrastructure Operators',
        description:
          'Built-in infrastructure operator group owning enrolled PITS edge, storage, bare-metal, and compute resources.',
        annotations: {
          'backstage.io/managed-by-location':
            'pits-synthesizer:pits://default/pits-infrastructure',
          'backstage.io/managed-by-origin-location':
            'pits-synthesizer:pits://default/pits-infrastructure',
        },
        tags: ['pits', 'operators', 'rbac'],
      },
      spec: {
        type: 'team',
        profile: {
          displayName: 'PITS Operators',
        },
        children: [],
        members: ['pitsadmin'],
      },
      relations: [
        {
          type: 'hasMember',
          targetRef: 'user:default/pitsadmin',
        },
      ],
    },
    {
      apiVersion: 'backstage.io/v1alpha1',
      kind: 'Group',
      metadata: {
        name: 'risk-engineering',
        namespace: 'default',
        uid: 'pits-virtual-group-risk-engineering',
        title: 'Risk Engineering & Fraud Defense',
        description:
          'Transaction security & fraud-defense engineering group owning Core Checkout components and commercial fraud SDK assessments.',
        annotations: {
          'backstage.io/managed-by-location':
            'pits-synthesizer:pits://default/core-checkout',
          'backstage.io/managed-by-origin-location':
            'pits-synthesizer:pits://default/core-checkout',
        },
        tags: ['pits', 'risk-engineering', 'sourcing-governance'],
      },
      spec: {
        type: 'team',
        profile: {
          displayName: 'Risk Engineering',
        },
        children: [],
        members: ['pitsadmin'],
      },
      relations: [
        {
          type: 'hasMember',
          targetRef: 'user:default/pitsadmin',
        },
      ],
    },
    {
      apiVersion: 'backstage.io/v1alpha1',
      kind: 'User',
      metadata: {
        name: 'pitsadmin',
        namespace: 'default',
        uid: 'pits-virtual-user-pitsadmin',
        title: 'PITS Local Operator (pitsadmin)',
        description:
          'Local workstation operator identity bound to PITS CLI and mTLS profile.',
        annotations: {
          'backstage.io/managed-by-location':
            'pits-synthesizer:pits://default/pits-infrastructure',
          'backstage.io/managed-by-origin-location':
            'pits-synthesizer:pits://default/pits-infrastructure',
        },
        tags: ['pits', 'operator'],
      },
      spec: {
        profile: {
          displayName: 'PITS Operator (pitsadmin)',
        },
        memberOf: ['group:default/operators', 'group:default/risk-engineering'],
      },
      relations: [
        {
          type: 'memberOf',
          targetRef: 'group:default/operators',
        },
        {
          type: 'memberOf',
          targetRef: 'group:default/risk-engineering',
        },
      ],
    },
  ];
}

function canonicalEntityRef(
  ref: string | CompoundEntityRef | Entity,
): string {
  if (typeof ref === 'string') {
    const parsed = parseEntityRef(ref);
    return `${parsed.kind.toLowerCase()}:${parsed.namespace.toLowerCase()}/${parsed.name.toLowerCase()}`;
  }
  if ('apiVersion' in ref && 'metadata' in ref) {
    return `${ref.kind.toLowerCase()}:${(ref.metadata.namespace || 'default').toLowerCase()}/${ref.metadata.name.toLowerCase()}`;
  }
  return `${ref.kind.toLowerCase()}:${(ref.namespace || 'default').toLowerCase()}/${ref.name.toLowerCase()}`;
}

function getValueAtPath(target: unknown, path: string): unknown {
  if (!path || target === null || target === undefined) {
    return undefined;
  }

  if (
    typeof target === 'object' &&
    !Array.isArray(target) &&
    path.toLowerCase().startsWith('relations.')
  ) {
    const relType = path.slice('relations.'.length).toLowerCase();
    const relations = (target as { relations?: Array<{ type?: string; targetRef?: string }> })
      .relations;
    if (!Array.isArray(relations)) {
      return undefined;
    }
    const matched = relations
      .filter(
        r =>
          typeof r?.type === 'string' &&
          r.type.toLowerCase() === relType &&
          typeof r?.targetRef === 'string',
      )
      .map(r => r.targetRef as string);
    return matched.length > 0 ? matched : undefined;
  }

  if (Array.isArray(target)) {
    const results: unknown[] = [];
    for (const elem of target) {
      const sub = getValueAtPath(elem, path);
      if (sub !== undefined) {
        if (Array.isArray(sub)) {
          results.push(...sub);
        } else {
          results.push(sub);
        }
      }
    }
    return results.length > 0 ? results : undefined;
  }

  if (typeof target !== 'object') {
    return undefined;
  }

  const record = target as Record<string, unknown>;
  const lowerPath = path.toLowerCase();

  for (const key of Object.keys(record)) {
    if (key === path || key.toLowerCase() === lowerPath) {
      if (record[key] !== undefined) {
        return record[key];
      }
    }
  }

  for (const key of Object.keys(record)) {
    const prefix = `${key}.`;
    if (
      path.startsWith(prefix) ||
      lowerPath.startsWith(prefix.toLowerCase())
    ) {
      const found = getValueAtPath(record[key], path.slice(prefix.length));
      if (found !== undefined) {
        return found;
      }
    }
  }

  return undefined;
}

function scalarsEqual(a: unknown, b: unknown): boolean {
  if (a === null || b === null || a === undefined || b === undefined) {
    return false;
  }
  if (a === b) {
    return true;
  }
  if (typeof a === 'string' && typeof b === 'string') {
    return a.toUpperCase() === b.toUpperCase();
  }
  if (
    (typeof a === 'number' || typeof a === 'boolean') &&
    (typeof b === 'string' || typeof b === 'number' || typeof b === 'boolean')
  ) {
    return String(a).toUpperCase() === String(b).toUpperCase();
  }
  if (
    typeof a === 'string' &&
    (typeof b === 'number' || typeof b === 'boolean')
  ) {
    return a.toUpperCase() === String(b).toUpperCase();
  }
  return false;
}

function evaluatePredicateValue(filter: unknown, value: unknown): boolean {
  if (filter === CATALOG_FILTER_EXISTS) {
    return value !== undefined && value !== null;
  }

  if (
    typeof filter !== 'object' ||
    filter === null ||
    Array.isArray(filter)
  ) {
    if (Array.isArray(value)) {
      if (Array.isArray(filter)) {
        return filter.some(f => value.some(v => scalarsEqual(v, f)));
      }
      return value.some(v => scalarsEqual(v, filter));
    }
    if (Array.isArray(filter)) {
      return filter.some(f => scalarsEqual(value, f));
    }
    return scalarsEqual(value, filter);
  }

  const filterObj = filter as Record<string, unknown>;

  if ('$contains' in filterObj) {
    if (!Array.isArray(value)) {
      return false;
    }
    return value.some(item =>
      evaluatePredicate(filterObj.$contains as AnyPredicate, item),
    );
  }

  if ('$in' in filterObj) {
    const candidates = Array.isArray(filterObj.$in) ? filterObj.$in : [];
    if (Array.isArray(value)) {
      return value.some(v => candidates.some(c => scalarsEqual(v, c)));
    }
    return candidates.some(c => scalarsEqual(value, c));
  }

  if ('$exists' in filterObj) {
    const exists = value !== undefined && value !== null;
    return Boolean(filterObj.$exists) ? exists : !exists;
  }

  if ('$hasPrefix' in filterObj) {
    const prefix = String(filterObj.$hasPrefix ?? '').toUpperCase();
    if (typeof value === 'string') {
      return value.toUpperCase().startsWith(prefix);
    }
    if (Array.isArray(value)) {
      return value.some(
        v => typeof v === 'string' && v.toUpperCase().startsWith(prefix),
      );
    }
    return false;
  }

  return false;
}

function evaluatePredicate(predicate: AnyPredicate, target: unknown): boolean {
  if (
    typeof predicate !== 'object' ||
    predicate === null ||
    Array.isArray(predicate)
  ) {
    return scalarsEqual(target, predicate);
  }

  if ('$all' in predicate) {
    const list = Array.isArray(predicate.$all) ? predicate.$all : [];
    return list.every(sub => evaluatePredicate(sub as AnyPredicate, target));
  }

  if ('$any' in predicate) {
    const list = Array.isArray(predicate.$any) ? predicate.$any : [];
    return list.some(sub => evaluatePredicate(sub as AnyPredicate, target));
  }

  if ('$not' in predicate) {
    return !evaluatePredicate(predicate.$not as AnyPredicate, target);
  }

  for (const [key, expected] of Object.entries(predicate)) {
    if (key.startsWith('$')) {
      return false;
    }
    const actual = getValueAtPath(target, key);
    if (!evaluatePredicateValue(expected, actual)) {
      return false;
    }
  }

  return true;
}

function matchesEntityFilterQuery(
  filter: EntityFilterQuery | undefined,
  entity: Entity,
): boolean {
  if (!filter) {
    return true;
  }
  const sets = Array.isArray(filter) ? filter : [filter];
  if (sets.length === 0) {
    return true;
  }
  return sets.some(filterSet => {
    for (const [key, rawExpected] of Object.entries(filterSet)) {
      const expectedValues = Array.isArray(rawExpected)
        ? rawExpected
        : [rawExpected];
      if (expectedValues.length === 0) {
        continue;
      }
      const actual = getValueAtPath(entity, key);
      const matchedOne = expectedValues.some(exp => {
        if (exp === CATALOG_FILTER_EXISTS) {
          return actual !== undefined && actual !== null;
        }
        return evaluatePredicateValue(exp, actual);
      });
      if (!matchedOne) {
        return false;
      }
    }
    return true;
  });
}

function matchesFullTextFilter(
  fullTextFilter: { term?: string; fields?: string[] } | undefined,
  entity: Entity,
): boolean {
  const rawTerm = fullTextFilter?.term?.trim();
  if (!rawTerm) {
    return true;
  }
  const words = rawTerm
    .split(/\s+/)
    .map(w => w.trim().toUpperCase())
    .filter(Boolean);
  if (words.length === 0) {
    return true;
  }

  const candidateFields =
    fullTextFilter?.fields && fullTextFilter.fields.length > 0
      ? [
          ...fullTextFilter.fields,
          'metadata.name',
          'metadata.title',
          'metadata.description',
          'metadata.tags',
          'spec.type',
        ]
      : [
          'kind',
          'metadata.name',
          'metadata.namespace',
          'metadata.title',
          'metadata.description',
          'metadata.tags',
          'spec.type',
          'spec.system',
          'spec.owner',
        ];

  const haystack: string[] = [];
  for (const field of candidateFields) {
    const val = getValueAtPath(entity, field);
    if (Array.isArray(val)) {
      for (const item of val) {
        if (item !== null && item !== undefined) {
          haystack.push(String(item).toUpperCase());
        }
      }
    } else if (val !== null && val !== undefined && typeof val !== 'object') {
      haystack.push(String(val).toUpperCase());
    }
  }

  return words.every(word => haystack.some(text => text.includes(word)));
}

function sortEntities(
  entities: Entity[],
  orderFields:
    | Array<{ field: string; order: 'asc' | 'desc' }>
    | { field: string; order: 'asc' | 'desc' }
    | undefined,
): Entity[] {
  const directives = orderFields
    ? Array.isArray(orderFields)
      ? orderFields
      : [orderFields]
    : [{ field: 'metadata.name', order: 'asc' as const }];

  if (directives.length === 0) {
    return [...entities];
  }

  return [...entities].sort((a, b) => {
    for (const { field, order } of directives) {
      const av = getValueAtPath(a, field);
      const bv = getValueAtPath(b, field);
      const aMissing = av === undefined || av === null;
      const bMissing = bv === undefined || bv === null;
      if (aMissing && bMissing) {
        continue;
      }
      if (aMissing) {
        return 1;
      }
      if (bMissing) {
        return -1;
      }
      const aStr = String(Array.isArray(av) ? av[0] ?? '' : av);
      const bStr = String(Array.isArray(bv) ? bv[0] ?? '' : bv);
      const cmp = aStr.localeCompare(bStr, 'en-US', { sensitivity: 'base' });
      if (cmp !== 0) {
        return order === 'desc' ? -cmp : cmp;
      }
    }
    return 0;
  });
}

function projectEntityFields(
  entity: Entity,
  fields: EntityFieldsQuery | undefined,
): Entity {
  if (!fields || fields.length === 0) {
    return entity;
  }
  const projected: Record<string, unknown> = {};
  for (const rawField of fields) {
    const field = rawField.trim();
    if (!field) {
      continue;
    }
    const parts = field.split('.');
    let srcCursor: unknown = entity;
    let dstCursor: Record<string, unknown> = projected;
    for (let i = 0; i < parts.length; i += 1) {
      if (
        srcCursor === null ||
        srcCursor === undefined ||
        typeof srcCursor !== 'object' ||
        Array.isArray(srcCursor)
      ) {
        break;
      }
      const part = parts[i];
      const srcObj = srcCursor as Record<string, unknown>;
      if (!(part in srcObj)) {
        break;
      }
      if (i === parts.length - 1) {
        dstCursor[part] = srcObj[part];
      } else {
        if (
          typeof dstCursor[part] !== 'object' ||
          dstCursor[part] === null ||
          Array.isArray(dstCursor[part])
        ) {
          dstCursor[part] = {};
        }
        dstCursor = dstCursor[part] as Record<string, unknown>;
        srcCursor = srcObj[part];
      }
    }
  }
  return projected as unknown as Entity;
}

function encodeCursor(payload: EncodedCursorPayload): string {
  return btoa(encodeURIComponent(JSON.stringify(payload)));
}

function decodeCursor(cursor: string): EncodedCursorPayload {
  try {
    const raw = decodeURIComponent(atob(cursor));
    const parsed = JSON.parse(raw) as Partial<EncodedCursorPayload>;
    if (typeof parsed.offset === 'number' && parsed.offset >= 0) {
      return {
        offset: parsed.offset,
        filter: parsed.filter,
        query: parsed.query,
        orderFields: parsed.orderFields,
        fullTextFilter: parsed.fullTextFilter,
      };
    }
  } catch {
    try {
      const legacy = atob(cursor).replace(/^offset:/i, '').trim();
      const n = Number.parseInt(legacy, 10);
      if (!Number.isNaN(n) && n >= 0) {
        return { offset: n };
      }
    } catch {
      const n = Number.parseInt(cursor, 10);
      if (!Number.isNaN(n) && n >= 0) {
        return { offset: n };
      }
    }
  }
  return { offset: 0 };
}

function isCursorRequest(
  request: QueryEntitiesRequest | undefined,
): request is QueryEntitiesCursorRequest {
  return Boolean(request && 'cursor' in request && request.cursor);
}

/**
 * Backstage v1.56 CatalogApi adapter for `pitsctl serve`.
 *
 * Bridges Backstage v1.56 predicate queries (`queryEntities` / `getEntityFacets`
 * which default to POST endpoints in `@backstage/catalog-client@1.16+`) and
 * `/ancestry` vs `/ancestors` path differences onto `pitsctl`'s embedded Axum
 * `/api/catalog/*` GET/POST routes, while providing built-in virtual principal
 * entities (`group:default/operators`, `user:default/pitsadmin`).
 */
export class PitsCatalogClient extends CatalogClient {
  private readonly discoveryApi: DiscoveryApi;
  private readonly fetchApi: FetchApi;
  private readonly virtualPrincipals: Entity[];
  private inflightEntities: Promise<Entity[]> | null = null;
  private cachedEntities: { items: Entity[]; expiresAt: number } | null = null;

  constructor(options: { discoveryApi: DiscoveryApi; fetchApi: FetchApi }) {
    super(options);
    this.discoveryApi = options.discoveryApi;
    this.fetchApi = options.fetchApi;
    this.virtualPrincipals = buildVirtualPrincipalEntities();
  }

  private invalidateLocalCache(): void {
    this.cachedEntities = null;
    this.inflightEntities = null;
  }

  private async fetchAllEntities(
    options?: CatalogRequestOptions,
  ): Promise<Entity[]> {
    const now = Date.now();
    if (this.cachedEntities && this.cachedEntities.expiresAt > now) {
      return this.cachedEntities.items;
    }
    if (this.inflightEntities) {
      return this.inflightEntities;
    }

    const loadPromise = (async () => {
      const baseUrl = await this.discoveryApi.getBaseUrl('catalog');
      const res = await this.fetchApi.fetch(`${baseUrl}/entities`, {
        method: 'GET',
        headers: {
          Accept: 'application/json',
          ...(options?.token
            ? { Authorization: `Bearer ${options.token}` }
            : {}),
        },
      });
      if (!res.ok) {
        throw new Error(
          `Failed to load catalog entities (${res.status} ${res.statusText})`,
        );
      }
      const backendItems = (await res.json()) as Entity[];
      const seenRefs = new Set(
        backendItems.map(entity => canonicalEntityRef(entity)),
      );
      const merged = [...backendItems];
      for (const principal of this.virtualPrincipals) {
        if (!seenRefs.has(canonicalEntityRef(principal))) {
          merged.push(principal);
        }
      }
      this.cachedEntities = {
        items: merged,
        expiresAt: Date.now() + 300,
      };
      return merged;
    })();

    this.inflightEntities = loadPromise;
    try {
      return await loadPromise;
    } finally {
      this.inflightEntities = null;
    }
  }

  private findVirtualPrincipal(
    entityRef: string | CompoundEntityRef,
  ): Entity | undefined {
    const targetRef = canonicalEntityRef(entityRef);
    return this.virtualPrincipals.find(
      item => canonicalEntityRef(item) === targetRef,
    );
  }

  override async getEntities(
    request?: GetEntitiesRequest,
    options?: CatalogRequestOptions,
  ): Promise<GetEntitiesResponse> {
    const all = await this.fetchAllEntities(options);
    let matched = all.filter(entity =>
      matchesEntityFilterQuery(request?.filter, entity),
    );
    matched = sortEntities(matched, request?.order);

    const startOffset =
      request?.offset ??
      (request?.after ? decodeCursor(request.after).offset : 0);
    const sliced =
      request?.limit !== undefined
        ? matched.slice(startOffset, startOffset + request.limit)
        : startOffset > 0
          ? matched.slice(startOffset)
          : matched;

    return {
      items: sliced.map(entity => projectEntityFields(entity, request?.fields)),
    };
  }

  override async queryEntities(
    request?: QueryEntitiesRequest,
    options?: CatalogRequestOptions,
  ): Promise<QueryEntitiesResponse> {
    const all = await this.fetchAllEntities(options);

    let filter: EntityFilterQuery | undefined;
    let query: Record<string, unknown> | undefined;
    let orderFields: Array<{ field: string; order: 'asc' | 'desc' }> | undefined;
    let fullTextFilter: { term: string; fields?: string[] } | undefined;
    let startOffset = 0;

    if (isCursorRequest(request)) {
      const decoded = decodeCursor(request.cursor);
      startOffset = decoded.offset;
      filter = decoded.filter;
      query = decoded.query;
      orderFields = decoded.orderFields;
      fullTextFilter = decoded.fullTextFilter;
    } else {
      const initReq = (request ?? {}) as QueryEntitiesInitialRequest;
      filter = initReq.filter;
      query = initReq.query as Record<string, unknown> | undefined;
      orderFields = initReq.orderFields
        ? Array.isArray(initReq.orderFields)
          ? initReq.orderFields
          : [initReq.orderFields]
        : undefined;
      fullTextFilter = initReq.fullTextFilter;
      startOffset = initReq.offset ?? 0;
    }

    let matched = all.filter(entity => {
      if (!matchesEntityFilterQuery(filter, entity)) {
        return false;
      }
      if (query && !evaluatePredicate(query, entity)) {
        return false;
      }
      if (!matchesFullTextFilter(fullTextFilter, entity)) {
        return false;
      }
      return true;
    });

    matched = sortEntities(matched, orderFields);
    const totalItems = matched.length;
    const limit = request?.limit;

    if (limit === 0) {
      return {
        items: [],
        totalItems,
        pageInfo: {},
      };
    }

    const effectiveLimit = limit ?? 20;
    const pageSlice = matched.slice(startOffset, startOffset + effectiveLimit);
    const nextOffset = startOffset + effectiveLimit;
    const prevOffset =
      startOffset > 0 ? Math.max(0, startOffset - effectiveLimit) : undefined;

    const pageInfo: QueryEntitiesResponse['pageInfo'] = {};
    if (nextOffset < totalItems) {
      pageInfo.nextCursor = encodeCursor({
        offset: nextOffset,
        filter,
        query,
        orderFields,
        fullTextFilter,
      });
    }
    if (prevOffset !== undefined) {
      pageInfo.prevCursor = encodeCursor({
        offset: prevOffset,
        filter,
        query,
        orderFields,
        fullTextFilter,
      });
    }

    return {
      items: pageSlice.map(entity =>
        projectEntityFields(entity, request?.fields),
      ),
      totalItems,
      pageInfo,
    };
  }

  override async getEntityFacets(
    request: GetEntityFacetsRequest,
    options?: CatalogRequestOptions,
  ): Promise<GetEntityFacetsResponse> {
    const all = await this.fetchAllEntities(options);
    const query = request.query as Record<string, unknown> | undefined;
    const filtered = all.filter(entity => {
      if (!matchesEntityFilterQuery(request.filter, entity)) {
        return false;
      }
      if (query && !evaluatePredicate(query, entity)) {
        return false;
      }
      return true;
    });

    const facets: GetEntityFacetsResponse['facets'] = {};
    for (const facetPath of request.facets) {
      const counts = new Map<string, { value: string; count: number }>();
      for (const entity of filtered) {
        const rawVal = getValueAtPath(entity, facetPath);
        if (rawVal === undefined || rawVal === null) {
          continue;
        }
        const values = Array.isArray(rawVal) ? rawVal : [rawVal];
        const seenOnEntity = new Set<string>();
        for (const item of values) {
          if (item === undefined || item === null || typeof item === 'object') {
            continue;
          }
          const strVal = String(item);
          const dedupKey = strVal.toLowerCase();
          if (seenOnEntity.has(dedupKey)) {
            continue;
          }
          seenOnEntity.add(dedupKey);
          const existing = counts.get(dedupKey);
          if (existing) {
            existing.count += 1;
          } else {
            counts.set(dedupKey, { value: strVal, count: 1 });
          }
        }
      }
      facets[facetPath] = Array.from(counts.values());
    }

    return { facets };
  }

  override async getEntityByRef(
    entityRef: string | CompoundEntityRef,
    options?: CatalogRequestOptions,
  ): Promise<Entity | undefined> {
    const fromBackend = await super.getEntityByRef(entityRef, options);
    if (fromBackend) {
      return fromBackend;
    }
    return this.findVirtualPrincipal(entityRef);
  }

  override async getEntityByName(
    compoundName: CompoundEntityRef,
    options?: CatalogRequestOptions,
  ): Promise<Entity | undefined> {
    const fromBackend = await super.getEntityByName(compoundName, options);
    if (fromBackend) {
      return fromBackend;
    }
    return this.findVirtualPrincipal(compoundName);
  }

  override async getEntitiesByRefs(
    request: GetEntitiesByRefsRequest,
    options?: CatalogRequestOptions,
  ): Promise<GetEntitiesByRefsResponse> {
    if (request.entityRefs.length === 0) {
      return { items: [] };
    }
    const all = await this.fetchAllEntities(options);
    const byRef = new Map<string, Entity>();
    for (const entity of all) {
      byRef.set(canonicalEntityRef(entity), entity);
    }

    const query = request.query as Record<string, unknown> | undefined;
    const items = request.entityRefs.map(ref => {
      let key: string;
      try {
        key = canonicalEntityRef(ref);
      } catch {
        return undefined;
      }
      const entity = byRef.get(key);
      if (!entity) {
        return undefined;
      }
      if (!matchesEntityFilterQuery(request.filter, entity)) {
        return undefined;
      }
      if (query && !evaluatePredicate(query, entity)) {
        return undefined;
      }
      return projectEntityFields(entity, request.fields);
    });

    return { items };
  }

  override async getEntityAncestors(
    request: GetEntityAncestorsRequest,
    options?: CatalogRequestOptions,
  ): Promise<GetEntityAncestorsResponse> {
    const compound = parseEntityRef(request.entityRef);
    const rootEntityRef = stringifyEntityRef(compound);
    const baseUrl = await this.discoveryApi.getBaseUrl('catalog');
    const url = `${baseUrl}/entities/by-name/${encodeURIComponent(compound.kind)}/${encodeURIComponent(compound.namespace)}/${encodeURIComponent(compound.name)}/ancestors`;

    try {
      const res = await this.fetchApi.fetch(url, {
        method: 'GET',
        headers: {
          Accept: 'application/json',
          ...(options?.token
            ? { Authorization: `Bearer ${options.token}` }
            : {}),
        },
      });
      if (res.ok) {
        return (await res.json()) as GetEntityAncestorsResponse;
      }
    } catch {
      // Fall back to synthesized single-node ancestry below.
    }

    const entity = await this.getEntityByRef(compound, options);
    return {
      rootEntityRef,
      items: entity ? [{ entity, parentEntityRefs: [] }] : [],
    };
  }

  override async refreshEntity(
    entityRef: string,
    options?: CatalogRequestOptions,
  ): Promise<void> {
    this.invalidateLocalCache();
    await super.refreshEntity(entityRef, options);
  }
}
