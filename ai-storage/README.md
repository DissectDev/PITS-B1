# `ai-store` — 5-Layer OpenDAL AI Storage Runtime & MCP Producer

Implements the architecture specified in [ai-storage-opendal-mcp-crossplane-design.md](../docs/ai-storage-opendal-mcp-crossplane-design.md):

- **Layer 1 (Storage & Index Backing)**: RustFS / S3 / Hugging Face / Local FS + SQLite3 WAL + optional CloudNativePG Postgres (`pg_trgm` / `pgvector`) + Valkey/Redis.
- **Layer 2 (Apache OpenDAL Data Plane)**: `BackendRegistry` with uniform `RetryLayer`, `TimeoutLayer`, `ConcurrentLimitLayer`, `LoggingLayer`, and `MetricsLayer`.
- **Layer 3 (8 AI Storage Archetypes + VFS Router & Policy Guard)**:
  1. `ws://` — Mutable sandbox + `ws_snapshot`, `ws_diff`, `ws_rollback`, `ws_fork`
  2. `mem://` — Scoped hierarchical memory + TTL decay + vector/keyword `mem_commit` / `mem_recall` / `mem_forget`
  3. `kb://` — Read-only corpus + `vfs_glob`, `vfs_grep`, `kb_search`, `kb_ingest`
  4. `data://` & `models://` — Byte-Range streaming + NVMe/disk LRU read-through cache (`data_cache_status`)
  5. `artifacts://` — Append-only SHA256 content-addressed outputs + lineage (`artifact_publish`, `artifact_lineage`)
  6. `traces://` — Buffered high-throughput audit/trace stream + rolling JSONL flush (`traces_emit`, `traces_query`)
  7. `state://` — Atomic CAS + fenced leases (`state_cas`, `state_lease`)
  8. `shared://` — Cross-sandbox concurrent read/write drive with path-level lease locks (`shared_lock`, `shared_unlock`)
- **Layer 4 (`ProjectProfile`)**: Declarative `.ai-store.yaml` / Kubernetes `ConfigMap` + secret resolution + redaction + hot-reload.
- **Layer 5 (MCP Server Producer)**: Streamable HTTP (`/mcp`, `/healthz`, `/readyz`, `/metrics`) and `ai-store mcp` Singleton Daemon + thin Stdio Proxy.

## Quick Commands

```powershell
# Validate profile & mount tree
.\bin\ai-store.exe profile validate --profile .ai-store.example.yaml

# Run persistent Streamable HTTP MCP Producer
.\bin\ai-store.exe serve --profile .ai-store.example.yaml

# Run thin Stdio Proxy (auto-wakes & multiplexes onto local Singleton Daemon)
.\bin\ai-store.exe mcp
```

## Cross-Harness Thin Consumers & Tutorials

- **User Journeys & Step-by-Step Tutorials**: [ai-storage-user-journeys-and-tutorials.md](../docs/ai-storage-user-journeys-and-tutorials.md)
- **Cross-Harness Thin Consumers**: See `consumers/` and [`.agents/plugins/pits-ai-storage/`](../.agents/plugins/pits-ai-storage/plugin.json) — zero `sidecar.json` lifecycle coupling.
