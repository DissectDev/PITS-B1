# AI Storage Consumer Rules

- Treat the `ai-store` MCP Server as the single source of truth for storage mounts (`mounts://tree`) and security policies (`profile://active`).
- Do not rely on Harness-specific `sidecar.json` lifecycle hooks to manage storage daemons; use either the remote Streamable HTTP endpoint (`https://mcp.lab.martensite.dev/mcp`) or the cross-harness singleton daemon proxy (`ai-store mcp`).
- Respect immutable/read-only contracts: `kb://`, `data://`, and `models://` are read-only; `artifacts://` and `traces://` are append-only.
