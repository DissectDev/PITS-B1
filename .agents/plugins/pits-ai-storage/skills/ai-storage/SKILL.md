---
name: ai-storage
description: >-
  Guides Agents on consuming the 5-layer OpenDAL AI Storage MCP Producer across all
  8 semantic archetypes (`ws://`, `mem://`, `kb://`, `data://`, `models://`,
  `artifacts://`, `traces://`, `state://`, `shared://`) without binding to a specific
  Agent Harness lifecycle.
---

# PITS AI Storage Consumer Workflow

The `ai-store` MCP Server is the **Producer** of active storage capabilities, backend mounts, and policy contracts; this plugin is a **pure Consumer**.

## First Step in Any Session

1. Read MCP resource `mounts://tree` and `profile://active` (or invoke prompt `storage-workflow-guide`) to discover the active URI schemes, read/write/append-only policies, and file size limits.
2. Never hardcode S3/RustFS/Postgres/Hugging Face credentials in prompts or harness config.

## Choosing the Right Archetype URI

| Scheme | Semantics | Key Tools |
| :--- | :--- | :--- |
| `ws://` | Mutable sandbox with snapshots | `ws_snapshot`, `ws_diff`, `ws_rollback`, `ws_fork`, `vfs_read`, `vfs_write` |
| `mem://` | Scoped memory (`<project>/<user>/<session>/...`) + TTL | `mem_commit`, `mem_recall`, `mem_forget` |
| `kb://` | Read-only knowledge base / corpus | `vfs_glob`, `vfs_grep`, `kb_search`, `vfs_read` (never `vfs_write`) |
| `data://` / `models://` | Read-only large datasets & weights with Byte-Range + NVMe cache | `vfs_read(uri, offset, limit)`, `data_cache_status` |
| `artifacts://` | Append-only content-addressed SHA256 outputs + lineage | `artifact_publish`, `artifact_lineage` |
| `traces://` | Append-only audit & trajectory logs | `traces_emit`, `traces_query` |
| `state://` | Multi-agent coordination CAS & fenced lease locks | `state_cas`, `state_lease` |
| `shared://` | Cross-sandbox concurrent read/write (`Antigravity` <-> `test-pool`) | `shared_lock`, `vfs_write`, `shared_unlock` |

## Safety Rules

- Always search `kb://` with `vfs_grep`, `vfs_glob`, or `kb_search` before reading large files.
- Always take a `ws_snapshot` before destructive workspace refactors.
- Always acquire `shared_lock` with a unique `owner` before writing shared files under `shared://` across sandboxes.
