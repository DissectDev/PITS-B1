# OpenDAL 5-Layer AI Storage: User Journeys & Hands-On Tutorials

> Companion guide to [ai-storage-opendal-mcp-crossplane-design.md](./ai-storage-opendal-mcp-crossplane-design.md), [ai-storage/README.md](../ai-storage/README.md), and the thin consumer plugin in [`.agents/plugins/pits-ai-storage/`](../.agents/plugins/pits-ai-storage/plugin.json).

---

## 1. Personas & System Roles at a Glance

The PITS AI Storage platform cleanly separates the **Producer** (`ai-store`, backed by Apache OpenDAL, RustFS S3, PostgreSQL `pg_trgm`/`pgvector`, and Valkey) from **Thin Consumers** (Antigravity, Claude Code, Codex CLI, Cursor, and `pits-test-pool` / sandbox workers):

| Persona | Primary Goal | Entry Point | Core Archetypes Used |
| :--- | :--- | :--- | :--- |
| **Local Poly-Agent Developer** | Share project memory & workspace snapshots across IDEs/CLIs with zero cloud secrets or `sidecar.json` coupling | `ai-store mcp` (Singleton Daemon + Stdio Proxy) | `ws://`, `mem://`, `kb://` |
| **Refactor / Experimentation Agent** | Safely attempt risky code edits, compare before/after diffs, roll back, or fork isolated branches | `ws_snapshot`, `ws_diff`, `ws_rollback`, `ws_fork` | `ws://` |
| **Research & Synthesis Workflow** | Search indexed corpuses without reading full dumps, persist durable findings with TTL, and emit verifiable deliverables | `kb_search`, `mem_commit`, `artifact_publish`, `traces_query` | `kb://`, `mem://`, `artifacts://`, `traces://` |
| **Multi-Sandbox Swarm Coordinator** | Synchronize handoffs between workstation agents and ephemeral Linux test pods without race conditions | `shared_lock`, `state_cas`, `state_lease` against `https://mcp.lab.martensite.dev/mcp` | `shared://`, `state://`, `data://` |
| **Platform Engineer / Operator** | Provision team AI storage stacks declaratively and observe live health across clusters | Crossplane `AIStorageWorkspace` CR + `pitsctl` / `pits-agent` | Crossplane XRD, `profile://active`, `/metrics` |

---

## 2. End-to-End User Journeys

### Journey 1 — Zero-Config Local Multi-Harness Memory & Storage
**Scenario**: A developer switches frequently between **Antigravity** (`win11-001`), **Claude Code**, **Codex CLI**, and **Cursor** while iterating on `c:\Project\PITS\Infra`. Instead of re-explaining architectural constraints in every session or managing per-tool sidecar processes:
1. Each harness runs a thin stdio configuration pointing at `ai-store mcp` (`ai-storage/bin/ai-store.exe mcp`).
2. The first harness invocation spawns a detached local **Singleton Daemon** pinned to `.ai-store.yaml`; subsequent harnesses reuse the same daemon socket/HTTP loopback endpoint automatically.
3. All harnesses discover capabilities dynamically by reading the MCP resources `mounts://tree` and `profile://active`.
4. Memories written under `mem://pits/<user>/<session>/...` in Antigravity are immediately searchable via `mem_recall` inside Claude Code or Codex CLI.

### Journey 2 — Time-Travel Refactoring & Parallel A/B Forking (`ws://`)
**Scenario**: An autonomous coding agent needs to test an aggressive storage-driver refactor without risking uncommitted local edits:
1. Before editing, the agent calls `ws_snapshot(workspacePrefix="refactor", label="pre-nvmet-edit")` to archive a clean immutable snapshot ID (e.g. `snap-xxxxxxxx`).
2. The agent writes trial code via `vfs_write(uri="ws://refactor/driver.rs", ...)`.
3. It runs `ws_diff(fromSnapshot="snap-xxxxxxxx", toTarget="ws://refactor")` to inspect added, modified, and removed paths.
4. Either:
   - The experiment fails -> `ws_rollback(snapshotId="snap-xxxxxxxx", workspacePrefix="refactor")` restores the exact baseline state.
   - Or it wants an A/B comparison -> `ws_fork(snapshotId="snap-xxxxxxxx", targetPrefix="refactor-alt")` clones the snapshot to an isolated prefix for a second worker agent.

### Journey 3 — Grounded Research-to-Immutable-Artifact Pipeline
**Scenario**: A research subagent audits cross-cluster documentation, saves reusable conclusions, and produces a signed report:
1. **Read-Only Discovery**: Instead of pulling entire directories into context, it queries `kb://` (enforced `mode: read-only`) using `kb_search`, `vfs_glob`, and `vfs_grep`, then fetches bounded ranges with `vfs_read(uri, offset, limit)`.
2. **Scoped Memory**: Key architectural invariants are saved via `mem_commit` under `<project>/<user>/<session>` with `ttlDays: 90` (`requireScope: true` prevents unscoped pollution).
3. **Immutable Deliverable**: The final markdown report or JSON audit is published to `artifacts://` (enforced `mode: append-only`) using `artifact_publish` with mandatory `runId`, `agentId`, and optional `sourceWs` / `promptHash` lineage metadata.
4. **Traceability**: Tool operations and explicit checkpoints via `traces_emit` stream into rolling JSONL logs under `traces://` and can be queried via `traces_query` or `artifact_lineage`.

### Journey 4 — Cross-Sandbox Collaboration (`win11-001` <-> `pits-test-pool`)
**Scenario**: Antigravity on `win11-001` prepares a verification payload and coordinates with an ephemeral sandbox pod running inside `cluster-pits-lab` (`pits-test-pool`):
1. The remote team workspace is served over **Streamable HTTP** behind Istio Ambient & Mode-A Keycloak JWT at `https://mcp.lab.martensite.dev/mcp`.
2. Both agents coordinate workflow ownership with `state_lease` (`state://`) and atomic step transitions via `state_cas(key=..., value=..., expectedVersion=...)`.
3. Before updating a shared handoff file, the producer acquires a fenced path lease with `shared_lock(path="tutorial/handoff.json", owner="win11-agent", ttlSecs=120)`, writes via `vfs_write(uri="shared://tutorial/handoff.json", ...)`, and calls `shared_unlock`.
4. Read-heavy training shards or model weights under `data://` and `models://` leverage byte-range streaming + NVMe LRU read-through caching (`data_cache_status`).

### Journey 5 — Platform Self-Service via Crossplane & Unified Control-Plane Discovery
**Scenario**: A platform engineer provisions a shared AI Storage stack on `cluster-pits-lab` and verifies its end-to-end health without switching tools:
1. Declare an `AIStorageWorkspace` claim (such as [CICD/platform/instances/pits-ai-storage.yaml](../CICD/platform/instances/pits-ai-storage.yaml)) reconciled by Crossplane Composition `aistorage-workspace-composition`.
2. Crossplane creates CloudNativePG (`ai-pg`), RustFS S3 + bucket init job, Valkey, ConfigMap `pits-profile`, Secret `pits-opendal-creds`, and Deployment `pits-ai-store`, populating status fields (`mcpEndpoint`, `internalEndpoint`, `profileConfigMap`, `readyArchetypes`).
3. Both **`pitsctl`** and **`pits-agent`** automatically discover all Crossplane XRDs, Claims, Composites (`XAIStorageWorkspace`, `XAuthFlowBundle`, `XLabDevStack`), Providers, Functions, and Compositions:
   - Query `pitsctl get crossplane --live` or `pitsctl describe pits-agent-space --live` on your workstation.
   - Query `GET /v1/crossplane` or `GET /v1/clusters` on `pits-agent` (`:18443`).

---

## 3. Hands-On Tutorials

> **Prerequisites (Windows workstation `win11-001`)**:
> - Binary: `c:\Project\PITS\Infra\ai-storage\bin\ai-store.exe`
> - Example config: `c:\Project\PITS\Infra\ai-storage\.ai-store.example.yaml`
> - Optional remote Lab endpoint: `https://mcp.lab.martensite.dev/mcp`

### Tutorial 1: Validate Local Profile & Boot the Singleton MCP Daemon

1. Inspect and validate your `ProjectProfile` (`Layer 4`) against `ai-storage/.ai-store.example.yaml`:

```powershell
Set-Location c:\Project\PITS\Infra\ai-storage
.\bin\ai-store.exe --profile .\.ai-store.example.yaml profile validate
.\bin\ai-store.exe --profile .\.ai-store.example.yaml profile show
```

2. Check or start the local **Singleton Daemon** (automatically started on demand by `ai-store mcp`, or inspectable via `daemon status`):

```powershell
.\bin\ai-store.exe --profile .\.ai-store.example.yaml daemon status
```

3. Connect your preferred AI harness using the zero-sidecar consumer configs already shipped in `ai-storage/consumers/`:
   - **Claude Code**: [`ai-storage/consumers/claude-code/.mcp.json`](../ai-storage/consumers/claude-code/.mcp.json) (`command: "ai-store", args: ["mcp"]` for local, or `https://mcp.lab.martensite.dev/mcp` for shared Lab).
   - **Codex CLI**: [`ai-storage/consumers/codex/config.toml`](../ai-storage/consumers/codex/config.toml).
   - **Cursor**: [`ai-storage/consumers/cursor/mcp.json`](../ai-storage/consumers/cursor/mcp.json).
   - **Antigravity**: Ready out of the box via `.agents/plugins/pits-ai-storage/.mcp.json` (`pits-ai-storage-local` + `pits-ai-storage-remote`).

4. Verify Policy Guard & Streamable HTTP endpoints locally:

```powershell
# Start Streamable HTTP Producer locally on 127.0.0.1:8787
.\bin\ai-store.exe --profile .\.ai-store.example.yaml serve --host 127.0.0.1 --port 8787
```

In a second PowerShell terminal, check health and Prometheus metrics:

```powershell
Invoke-RestMethod http://127.0.0.1:8787/healthz
Invoke-RestMethod http://127.0.0.1:8787/readyz
Invoke-WebRequest http://127.0.0.1:8787/metrics | Select-Object -ExpandProperty Content
```

---

### Tutorial 2: Snapshot, Diff, Rollback & Fork in `ws://`

Using MCP JSON-RPC calls (via your connected Agent or `POST http://127.0.0.1:8787/mcp` with `Content-Type: application/json`):

1. **Discover active mounts** (always do this first in a new session):
   - Read MCP Resource `mounts://tree` and `profile://active`.
2. **Seed a baseline workspace file and create a snapshot**:
   - Call `vfs_write`:
     ```json
     { "uri": "ws://demo/app.rs", "content": "fn version() -> u32 { 1 }\n", "agentId": "dev-1" }
     ```
   - Call `ws_snapshot`:
     ```json
     { "workspacePrefix": "demo", "label": "v1-baseline" }
     ```
     Record the returned `snapshotId` (e.g. `snap-1a2b3c4d`).
3. **Make a risky modification & inspect the diff**:
   - Call `vfs_write`:
     ```json
     { "uri": "ws://demo/app.rs", "content": "fn version() -> u32 { panic!(\"broken\") }\n", "agentId": "dev-1" }
     ```
   - Call `ws_diff`:
     ```json
     { "fromSnapshot": "snap-1a2b3c4d", "toTarget": "ws://demo" }
     ```
     Verify `changed` lists `demo/app.rs`.
4. **Roll back cleanly or fork an experimental copy**:
   - Roll back live workspace:
     ```json
     { "snapshotId": "snap-1a2b3c4d", "workspacePrefix": "demo" }
     ```
   - Or fork into an isolated prefix for a parallel worker:
     ```json
     { "snapshotId": "snap-1a2b3c4d", "targetPrefix": "demo-experiment" }
     ```

---

### Tutorial 3: Research (`kb://`), Scoped Memory (`mem://`), and Lineage Artifacts (`artifacts://`)

1. **Ingest & search reference documentation (`kb://`)**:
   > Note: Direct `vfs_write` to `kb://` is blocked (`read-only` policy); indexed ingestion uses `kb_ingest`.
   - Call `kb_ingest`:
     ```json
     {
       "path": "arch/crossplane-xrds.md",
       "title": "PITS Crossplane XRDs",
       "content": "AIStorageWorkspace provisions Postgres, RustFS, Valkey, and ai-store MCP."
     }
     ```
   - Search with `kb_search` or regex `vfs_grep`:
     ```json
     { "query": "RustFS", "limit": 5 }
     ```
     ```json
     { "uri": "kb://", "pattern": "AIStorageWorkspace" }
     ```
2. **Persist scoped memory (`mem://`)**:
   - Call `mem_commit` (note `scope` requires a project prefix when `requireScope: true`):
     ```json
     {
       "scope": "pits/demo-user/session-01",
       "title": "Lab Cluster API Endpoint",
       "content": "AIStorageWorkspace lives in pits-cicd/pits-agent-space with Ready=True.",
       "tags": ["crossplane", "lab"],
       "ttlDays": 90
     }
     ```
   - Recall in a new session:
     ```json
     { "query": "pits-agent-space", "scope": "pits/demo-user", "limit": 5 }
     ```
3. **Publish an immutable deliverable & inspect lineage (`artifacts://` + `traces://`)**:
   - Call `artifact_publish` (overwrites or deletes to `artifacts://` are denied by Policy Guard):
     ```json
     {
       "path": "reports/readiness-audit.json",
       "content": "{\"workspace\":\"pits-agent-space\",\"ready\":true}\n",
       "runId": "run-tutorial-01",
       "agentId": "auditor-1",
       "sourceWs": "ws://demo"
     }
     ```
   - Inspect SHA256 digest & lineage (`artifact_lineage`) and recent audit trail (`traces_query`):
     ```json
     { "path": "reports/readiness-audit.json" }
     ```
     ```json
     { "limit": 20 }
     ```

---

### Tutorial 4: Cross-Sandbox Concurrency with `shared_lock` & `state_cas`

1. **Acquire a fenced lease on `shared://` before writing**:
   - Worker A calls `shared_lock`:
     ```json
     { "path": "handoff/test-plan.json", "owner": "win11-antigravity", "ttlSecs": 120 }
     ```
   - Worker B attempting `shared_lock` on the same path receives a lock contention response until Worker A finishes.
   - Worker A writes `vfs_write(uri="shared://handoff/test-plan.json", ...)` and releases via `shared_unlock(path="handoff/test-plan.json", owner="win11-antigravity")`.
2. **Coordinate pipeline phases with `state_cas`**:
   - Initialize checkpoint (`expectedVersion: 0` when unset, or inspect current version first):
     ```json
     { "key": "pipelines/ci-01/stage", "value": { "phase": "built", "sha": "abc123" }, "expectedVersion": 0 }
     ```
   - Advance deterministically with `expectedVersion: 1` -> `value: { "phase": "verified" }`. Stale writers with `expectedVersion: 0` fail atomically instead of clobbering state.

---

### Tutorial 5: Provision & Inspect Crossplane `AIStorageWorkspace` with `pitsctl` and `pits-agent`

1. **Inspect live Crossplane XRDs & Claims in `cluster-pits-lab`**:

```powershell
kubectl --kubeconfig="$env:TEMP\lab-kc.yaml" get xrd,compositions,providers.pkg.crossplane.io,functions.pkg.crossplane.io,aistorageworkspaces,authflowbundles,labdevstacks -A
```

Expected resources on `cluster-pits-lab`:
- **XRDs**: `xaistorageworkspaces.storage.ai.pits.io`, `xauthflowbundles.iam.corp.io`, `xlabdevstacks.lab.pits.io`
- **Claims**:
  - `pits-cicd/aistorageworkspace.storage.ai.pits.io/pits-agent-space` -> `mcpEndpoint: https://mcp.lab.martensite.dev/mcp`
  - `iam-system/authflowbundle.iam.corp.io/corp-fido2-browser` -> `activeFlowAlias: corp-fido2-browser-...`
  - `pits-cicd/labdevstack.lab.pits.io/pits-lab-acceleration`

2. **Query through `pitsctl` Unified Software Catalog**:

```powershell
# List all Crossplane-managed Resources & Components (claims, XRs, XRDs, compositions, providers, functions)
pitsctl get crossplane --live -o table

# Filter directly by resource type
pitsctl get ai-storage-workspace --live -o wide
pitsctl get auth-flow-bundle --live -o wide

# Inspect full landing page, Ready/Synced state, MCP endpoints & readyArchetypes
pitsctl describe resource:pits-cicd/pits-agent-space --live
pitsctl describe resource:iam-system/corp-fido2-browser --live
pitsctl describe pits-lab-acceleration --live
```

3. **Query through `pits-agent` Go API (`:18443`)**:
   - `GET /v1/crossplane` returns per-cluster `installed`, `xrds`, `packages` (`Provider`/`Function`), and `resources` (`Claim`, `Composite`, `Composition` + `highlights` such as `mcpEndpoint`, `readyArchetypes`, `activeFlowAlias`, `externalIp`).
   - `GET /v1/clusters` includes each cluster's nested `crossplane` inventory and aggregate `totalCrossplaneResources`.

---

## 4. Archetype Policy & Guardrails Reference

| Scheme | Access Mode | Deletion Allowed | Key Safety Invariant |
| :--- | :--- | :---: | :--- |
| `ws://` | `read-write` | Yes | Take `ws_snapshot` before destructive multi-file edits |
| `mem://` | `read-write` | Via `mem_forget` | `requireScope: true` rejects unscoped writes missing `<project>/...` |
| `kb://` | `read-only` | No | `vfs_write` / `vfs_delete` blocked at Policy Guard; index via `kb_ingest` |
| `data://`, `models://` | `read-only` | No | Stream via `vfs_read(uri, offset, limit)`; inspect cache with `data_cache_status` |
| `artifacts://` | `append-only` | No | Immutable & content-addressed (SHA256); requires `runId` and `agentId` |
| `traces://` | `append-only` | No | Rolling buffered JSONL audit logs; append via `traces_emit` |
| `state://` | `read-write` | Via state ops | Mutate shared workflow keys only with `state_cas` / `state_lease` |
| `shared://` | `read-write` | Yes | Acquire `shared_lock(path, owner)` before multi-sandbox writes |
