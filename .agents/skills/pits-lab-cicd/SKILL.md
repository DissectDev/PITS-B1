---
name: pits-lab-cicd
description: >-
  Triggers and verifies PITS Lab CI/CD pipelines on `cluster-pits-lab` (Gitea Actions
  `act_runner`, Flux CD, Crossplane `LabDevStack`, Zot, Attic) and orchestrates core
  deployments to `pits-core` (`100.95.95.88`). Activate whenever pushing workspace changes
  to Lab Gitea, running `Invoke-CicdPipeline.ps1`, deploying via `Deploy-CoreArtifacts.ps1`,
  or troubleshooting Lab Flux/Crossplane/Gitea snapshot synchronization.
---

# PITS Lab CI/CD & Core Deployment (`CICD/`)

## Running `Invoke-CicdPipeline.ps1` Safely

1. **Uncommitted local edits or `HEAD` behind Lab Gitea snapshots (`-Snapshot`)**:
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\CICD\scripts\Invoke-CicdPipeline.ps1 -Snapshot
   ```
   - Uses a temporary `GIT_INDEX_FILE` (`-c core.autocrlf=false`) parented on Lab Gitea's current `main` tip so local `HEAD` and staging index remain untouched, while testing all uncommitted workspace changes on `act_runner`.
2. **Realigning Lab Gitea `main` after a local `git commit` (`-Force`)**:
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\CICD\scripts\Invoke-CicdPipeline.ps1 -Force
   ```
   - Use once after committing `CICD/` locally if Lab Gitea `main` holds prior `-Snapshot` commits.
3. **Standard push + wait + optional `pits-core` CD promotion**:
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\CICD\scripts\Invoke-CicdPipeline.ps1 -DeployCore -DeployTarget all
   ```

## Flux `prune: true` Guardrail

Lab Flux CD (`flux-system/Kustomization/lab-cicd-platform`) reconciles `./CICD/platform` from Lab Gitea `main` with `prune: true`.
- **Never** push (or `-Force` push) a commit lacking `CICD/platform/kustomization.yaml` to Lab Gitea `main`, as Flux would delete `LabDevStack` (`gitea`, `zot`, `attic`, `cnpg`, `act_runner`s).
- `Invoke-CicdPipeline.ps1` enforces this check automatically before pushing to `main`.

## Direct Core Deployment & Health Checks

- Deploy to `pits-core` / build targets (`all | portal | agent | test-runner | cicd-runner | windows | base`):
  ```powershell
  powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\CICD\scripts\Deploy-CoreArtifacts.ps1 -Target all
  ```
- Verify stack health and `netboot-004` (`win11-001` host) exclusion:
  ```powershell
  powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\CICD\scripts\Verify-CicdStack.ps1
  ```
