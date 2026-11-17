# Infra Project Sessions & Handoff

## Unified Workspace Context (`c:\Project\PITS\Infra`)

On `2026-11-17`, the `Client`, `TestPool`, `Base`, `Network`, `Router`, and `PITS-B1` projects were consolidated into `c:\Project\PITS\Infra` (upstream: `https://github.com/DissectDev/PITS-B1`).

- **Agent Governance**: Project-wide rules (`AGENTS.md`), hooks (`.agents/hooks/`), MCP plugins (`.agents/plugins/pits-sourcing-mcp/`), and skills (`.agents/skills/`) live at the `Infra/` root and apply across all subprojects (`Base/`, `client/`, `portal/`, `server/`, `deploy/`, `catalog/`, `test-pool/`).
- **Archived Unimplemented Paths**: Legacy/unimplemented artifacts and pre-migration session backups are preserved under `c:\Project\PITS\archive\` (`antigravity-sessions-backup/`).

## Migrated Antigravity Sessions (`Infra` project: `27b657fe-89f2-4834-bbef-44e579efee5b`)

All existing Antigravity sessions from `Client` (`29d5959e-1e82-4744-8fc8-23fd1c5931af`) and `TestPool` (`ccba2b71-8a88-458a-a122-90ccdb54241f`) — 7 top-level sessions plus 10 child subagent sessions (`Base`, `Network`, `Router` had 0 sessions) — have been rebound to `project_id = 27b657fe-89f2-4834-bbef-44e579efee5b` (`workspace_uris = ["file:///c%3A/Project/PITS/Infra"]`):

- `2d9e34a4-1d9a-40c5-9686-0b3d69ba72bc` — General Greeting (existing `Infra`)
- `218ca77d-250f-4de2-bb13-9d22f875f9ff` — Thunderbolt Net Network Configuration (from `Client`)
- `92d7eccf-325d-4bc1-b1c6-d58715a33370` — Backstage Sourcing Audit Integration (from `Client`)
- `8748cc4b-3217-42c8-bcf3-bac7f4d5b195` — Sync Edge-Router And Fix Netboot5 (from `Client`)
- `9132aca9-f8ef-463c-960f-4f26152eb3da` — Tailscale Alternatives and Service Levels (from `Client`)
- `685c7487-466c-47ba-839b-78378aa28531` — Configure EdgeRouter For VLAN 30 (from `Client`)
- `1e144590-02be-40e1-873e-ea2b30b1c0da` — Codex Project RKE2 Client Refactoring (+ 10 subagent sessions, from `Client`)
- `c0205c8d-0d81-4858-9a03-c8e848687e83` — Cluster Pits Lab Test Pool (from `TestPool`)
