#!/usr/bin/env node
// Real Headless Chromium E2E verifier for PITS Backstage v1.56 Portal (`pitsctl serve`).
// Uses Node.js >= 22 built-in `fetch` + `WebSocket` over Chrome DevTools Protocol (CDP)
// so zero external Playwright/Puppeteer npm dependencies are required offline.

import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const baseUrl = process.env.PITS_PORTAL_URL || 'http://127.0.0.1:18090';
const debugPort = Number(process.env.CDP_PORT || 9222);
const startedAt = Date.now();

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function verifyRestContracts() {
  const rtRes = await fetch(`${baseUrl}/api/pits/v1/resource-types`);
  if (!rtRes.ok) {
    throw new Error(`GET /api/pits/v1/resource-types failed: HTTP ${rtRes.status}`);
  }
  const resourceTypes = await rtRes.json();
  if (!Array.isArray(resourceTypes) || resourceTypes.length < 8) {
    throw new Error(`Expected >= 8 resource types, got ${JSON.stringify(resourceTypes).slice(0, 200)}`);
  }

  const catRes = await fetch(`${baseUrl}/api/catalog/entities`);
  if (!catRes.ok) {
    throw new Error(`GET /api/catalog/entities failed: HTTP ${catRes.status}`);
  }
  const entities = await catRes.json();
  if (!Array.isArray(entities) || entities.length < 26) {
    throw new Error(`Expected >= 26 catalog entities, got ${entities?.length}`);
  }

  const authRes = await fetch(`${baseUrl}/api/auth/guest/refresh`, { method: 'POST' });
  if (!authRes.ok) {
    throw new Error(`POST /api/auth/guest/refresh failed: HTTP ${authRes.status}`);
  }

  return {
    resourceTypeCount: resourceTypes.length,
    catalogEntityCount: entities.length,
  };
}

class CdpSession {
  constructor(ws) {
    this.ws = ws;
    this.nextId = 1;
    this.pending = new Map();
    this.exceptions = [];
    this.http5xx = [];

    ws.addEventListener('message', (ev) => {
      const msg = JSON.parse(typeof ev.data === 'string' ? ev.data : ev.data.toString());
      if (msg.id && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        if (msg.error) reject(new Error(msg.error.message || JSON.stringify(msg.error)));
        else resolve(msg.result);
        return;
      }
      if (msg.method === 'Runtime.exceptionThrown') {
        const d = msg.params?.exceptionDetails;
        this.exceptions.push(d?.exception?.description || d?.text || 'Unknown JS exception');
      } else if (msg.method === 'Network.responseReceived') {
        const status = msg.params?.response?.status ?? 0;
        const url = msg.params?.response?.url ?? '';
        if (status >= 500) {
          this.http5xx.push(`${status} ${url}`);
        }
      }
    });
  }

  send(method, params = {}) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`CDP timeout on ${method}`));
      }, 15000);
      this.pending.set(id, {
        resolve: (v) => {
          clearTimeout(timer);
          resolve(v);
        },
        reject: (e) => {
          clearTimeout(timer);
          reject(e);
        },
      });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  async evalJs(expression) {
    const res = await this.send('Runtime.evaluate', {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (res?.exceptionDetails) {
      throw new Error(`Eval error: ${res.exceptionDetails.text}`);
    }
    return res?.result?.value;
  }
}

async function main() {
  const apiStats = await verifyRestContracts();

  const userDataDir = mkdtempSync(join(tmpdir(), 'pits-chromium-'));
  const chromeArgs = [
    '--headless=new',
    '--no-sandbox',
    '--disable-setuid-sandbox',
    '--disable-gpu',
    '--disable-dev-shm-usage',
    '--no-first-run',
    '--no-default-browser-check',
    '--log-level=3',
    `--user-data-dir=${userDataDir}`,
    `--remote-debugging-port=${debugPort}`,
    'about:blank',
  ];

  const chrome = spawn('chromium', chromeArgs, {
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, DBUS_SESSION_BUS_ADDRESS: '/dev/null' },
  });
  let chromeStderr = '';
  chrome.stderr.on('data', (chunk) => {
    if (chromeStderr.length < 4096) chromeStderr += chunk.toString();
  });

  try {
    let targetWs = null;
    let browserVersion = 'unknown';
    for (let i = 0; i < 60; i++) {
      try {
        const verRes = await fetch(`http://127.0.0.1:${debugPort}/json/version`);
        if (verRes.ok) {
          const ver = await verRes.json();
          browserVersion = ver.Browser || 'chromium';
        }
        const listRes = await fetch(`http://127.0.0.1:${debugPort}/json/list`);
        if (listRes.ok) {
          const targets = await listRes.json();
          const page = targets.find((t) => t.type === 'page' && t.webSocketDebuggerUrl);
          if (page) {
            targetWs = page.webSocketDebuggerUrl;
            break;
          }
        }
      } catch {
        // wait for Chromium debugging port
      }
      await sleep(150);
    }

    if (!targetWs) {
      throw new Error(`Chromium CDP failed to become ready on :${debugPort}. stderr: ${chromeStderr}`);
    }

    const ws = new WebSocket(targetWs);
    await new Promise((resolve, reject) => {
      ws.addEventListener('open', resolve, { once: true });
      ws.addEventListener('error', (e) => reject(new Error(`WebSocket error: ${e.message || e}`)), { once: true });
    });

    const cdp = new CdpSession(ws);
    await cdp.send('Page.enable');
    await cdp.send('Runtime.enable');
    await cdp.send('Network.enable');

    // 1. Navigate to Portal root and verify redirect -> /resource-types + rendered resource catalog
    await cdp.send('Page.navigate', { url: `${baseUrl}/` });
    let dirSnapshot = null;
    for (let i = 0; i < 60; i++) {
      dirSnapshot = await cdp.evalJs(`(() => ({
        path: window.location.pathname,
        title: document.title,
        text: document.body ? document.body.innerText : ''
      }))()`);
      if (dirSnapshot?.text?.includes('PITS Portal Startup Error')) {
        throw new Error(`PortalRootErrorBoundary triggered: ${dirSnapshot.text}`);
      }
      if (
        dirSnapshot?.path === '/resource-types' &&
        (dirSnapshot.text.includes('baremetal-host') || dirSnapshot.text.includes('Baremetal')) &&
        (dirSnapshot.text.includes('kubernetes-cluster') || dirSnapshot.text.includes('Kubernetes'))
      ) {
        break;
      }
      await sleep(200);
    }

    if (!dirSnapshot || dirSnapshot.path !== '/resource-types') {
      throw new Error(`Expected redirect to /resource-types, got: ${JSON.stringify(dirSnapshot)?.slice(0, 400)}`);
    }

    // 2. Navigate to a concrete Backstage Resource entity page (netboot-001) and verify render
    await cdp.send('Page.navigate', { url: `${baseUrl}/catalog/default/resource/netboot-001` });
    let entitySnapshot = null;
    for (let i = 0; i < 60; i++) {
      entitySnapshot = await cdp.evalJs(`(() => ({
        path: window.location.pathname,
        text: document.body ? document.body.innerText : ''
      }))()`);
      if (entitySnapshot?.text?.includes('PITS Portal Startup Error')) {
        throw new Error(`Entity page crashed: ${entitySnapshot.text}`);
      }
      if (
        entitySnapshot?.text?.includes('netboot-001') &&
        (entitySnapshot.text.includes('ASUS NUC 14 Pro') || entitySnapshot.text.includes('10.89.20.21') || entitySnapshot.text.includes('baremetal-host'))
      ) {
        break;
      }
      await sleep(200);
    }

    if (!entitySnapshot?.text?.includes('netboot-001')) {
      throw new Error(`Entity page /catalog/default/resource/netboot-001 did not render expected details: ${entitySnapshot?.text?.slice(0, 300)}`);
    }

    if (cdp.exceptions.length > 0) {
      throw new Error(`Uncaught frontend JS exceptions (${cdp.exceptions.length}): ${cdp.exceptions.join(' | ')}`);
    }
    if (cdp.http5xx.length > 0) {
      throw new Error(`Backend returned 5xx during browser navigation: ${cdp.http5xx.join(', ')}`);
    }

    ws.close();

    const report = {
      status: 'passed',
      browser: browserVersion,
      resourceTypes: apiStats.resourceTypeCount,
      catalogEntities: apiStats.catalogEntityCount,
      verifiedRoutes: ['/ -> /resource-types', '/catalog/default/resource/netboot-001'],
      durationMs: Date.now() - startedAt,
    };
    process.stdout.write(JSON.stringify(report) + '\n');
  } finally {
    chrome.kill('SIGKILL');
    try {
      rmSync(userDataDir, { recursive: true, force: true });
    } catch {
      // ignore cleanup error
    }
  }
}

main().catch((err) => {
  const report = {
    status: 'failed',
    error: err?.stack || String(err),
    durationMs: Date.now() - startedAt,
  };
  process.stderr.write(JSON.stringify(report) + '\n');
  process.exit(1);
});
