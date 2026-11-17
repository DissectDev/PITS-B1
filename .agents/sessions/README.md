# Infra Project Sessions & Handoff

## Unified Workspace Context (`c:\Project\PITS\Infra`)

On `2026-11-17`, the previously separate `Client`, `TestPool`, and `PITS-B1` (NixOS core) projects were unified into `c:\Project\PITS\Infra` (upstream: `https://github.com/DissectDev/PITS-B1`).

- **Agent Governance**: Project-wide rules (`AGENTS.md`), hooks (`.agents/hooks/`), MCP plugins (`.agents/plugins/pits-sourcing-mcp/`), and skills (`.agents/skills/`) now live at the `Infra/` root and apply across all subprojects (`Base/`, `client/`, `portal/`, `server/`, `deploy/`, `catalog/`, `test-pool/`).
- **Archived Unimplemented Paths**: Legacy/unimplemented artifacts from `Base/`, `Network/`, `Router/`, and suspended `PITS-B1` GitOps plans (`management/{platform,services,compliance}`, `zones/`, `core-nodes/{network,storage}-node.yaml`) were moved out of the active workspace to `c:\Project\PITS\archive\`.

## Note on Antigravity Historical Sessions

Antigravity stores global conversation state in `C:\Users\pitsadmin\.gemini\antigravity\conversations\` and per-session trajectories under `C:\Users\pitsadmin\.gemini\antigravity\brain\`, which are protected outside the workspace sandbox (`Access is denied`). Start new Antigravity sessions directly in `c:\Project\PITS\Infra`; project rules, skills, hooks, and MCP servers will load automatically from `Infra/.agents/` and `Infra/AGENTS.md`.
