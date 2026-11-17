---
name: pits-test-pool
description: >-
  Runs parallel, zero-disk-wear verification of the PITS Infra workspace (Rust `pitsctl`,
  Linux Go `pits-agent -race`, Backstage v1.56 Portal build + Headless Chromium E2E, and
  optional Intel NPU DRA v1 hardware checks) on the `cluster-pits-lab` (`netboot-001..003`)
  DDR5 `tmpfs` Warm Pool via CNCF DevSpace and Kubernetes `agent-sandbox`. Activate this
  skill whenever modifying `c:\Project\PITS\Infra` (`client/`, `server/`, `portal/`, `deploy/`)
  code, running pre-promotion gates before `promote-portal.ps1`, or spawning background
  Verifier Subagents / `/teamwork` workers.
---

# PITS Parallel Test Pool (`cluster-pits-lab`)

## Overview

`win11-001` does not require a local Rust toolchain or Linux kernel to verify `PITS\Infra`.
All compilation and end-to-end verification run remotely inside `cluster-pits-lab` (`Namespace: pits-test-pool`) across `netboot-001..003` (strictly excluding workstation host `netboot-004`), backed by DDR5 `emptyDir: { medium: Memory }` (`tmpfs`).

## Quick Verification Commands

1. **Full 3-Shard Parallel Gate (Default — Rust + Linux Go `-race` + Portal & Headless Chromium E2E)**:
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\test-pool\scripts\Invoke-PitsTestPool.ps1
   ```
   Parses the returned JSON (`overallStatus == "passed"`, per-shard `durationMs`, `summary`, and `logTail` on failure).

2. **Targeted Single-Shard Fast Check (`~1–3s` warm)**:
   - Rust only (`netboot-001`): `... Invoke-PitsTestPool.ps1 -Shard rust`
   - Linux Go `-race` only (`netboot-002`): `... Invoke-PitsTestPool.ps1 -Shard go-linux`
   - Portal `tsc` + `vite` + `pitsctl serve` + Headless Chromium (`netboot-003`): `... Invoke-PitsTestPool.ps1 -Shard portal-e2e`

3. **Include Intel AI Boost NPU DRA v1 Hardware Gate (4-Way Parallel)**:
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\test-pool\scripts\Invoke-PitsTestPool.ps1 -IncludeHardware
   ```

4. **Concurrent Multi-Agent / `/teamwork` Mode (`SandboxClaim`)**:
   When multiple subagents edit and test different working trees concurrently, pass `-ClaimSandbox` (and `-ClientDir <worktree>`) to allocate an isolated pre-warmed `tmpfs` pod from `pits-agent-warmpool` (`kubernetes-sigs/agent-sandbox`):
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\test-pool\scripts\Invoke-PitsTestPool.ps1 -ClaimSandbox -ClientDir <path-to-infra-worktree>
   ```

## Background Verifier Subagent Pattern

After completing non-trivial edits in `c:\Project\PITS\Infra` (`client/`, `server/`, `portal/`, `deploy/`), invoke a `self` subagent asynchronously (`Role: "Test Pool Verifier"`) instructing it to run [Invoke-PitsTestPool.ps1](../../test-pool/scripts/Invoke-PitsTestPool.ps1) (and `-IncludeHardware` if `k8s.rs` / `talos.rs` / `vm.rs` changed) and report any failing `shard` and `logTail`. Only run `promote-portal.ps1` once all shards report `"passed"`.

## Maintenance

- **Ensure / Reconcile Warm Pool Manifests**:
  `powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\test-pool\scripts\Invoke-PitsTestPool.ps1 -DeployPool`
- **Refresh Offline Runner Image** (only needed when `Cargo.lock` or `portal/package-lock.json` adds new dependencies):
  Run [build-and-publish-image.ps1](../../test-pool/image/build-and-publish-image.ps1) to rebuild `docker.io/pits/test-runner:v1.2` on `pits-core` and update `talos-image-cache` (`10.89.20.14:3172`).
