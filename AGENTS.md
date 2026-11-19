# PITS Infra Workspace Always-On Rules

This `Infra` workspace (`c:\Project\PITS\Infra`, upstream `https://github.com/DissectDev/PITS-B1`) unifies:
- `Base/` — NixOS 26.05 (`pits-core` flake, `hosts/`, `modules/`, `site/`, `gitops/`, `scripts/`, `docs/`)
- `client/` — Rust `pitsctl` CLI, catalog engine, MCP server & embedded portal host
- `portal/` — Backstage v1.56 New Frontend System SPA
- `server/` — Go `pits-agent` privileged host/SAN/VPP daemon
- `deploy/` — Declarative NixOS deployment scripts & modules
- `catalog/` — Sourcing governance YAML seeds
- `test-pool/` — Parallel DevSpace + `agent-sandbox` CI verification pool
- `CICD/` — Lab CI/CD, caching & Crossplane control plane (`cluster-pits-lab`: Flux CD, Crossplane `LabDevStack`, CNPG, Gitea, Zot, Attic, `act_runner`) and unified build/test/deploy handover scripts
- `docs/` — Architecture & design documents (`lab-cicd-crossplane-design.md`)

All `.agents/` hooks, plugins, skills, and session handoffs apply at the unified `Infra` project root.

## 1. Mandatory 5-Minute Standing Progress Cron (`*/5 * * * *`)
In every conversation and every session within this project (`c:\Project\PITS\Infra`):
1. At the start of any execution turn (`Execution Mode`) involving multi-step commands, remote operations, cluster changes, builds, or background tasks:
   - Check running tasks with `manage_task(Action="list")`.
   - If no active `schedule(CronExpression="*/5 * * * *")` task is present, immediately call `schedule` with:
     - `CronExpression`: `"*/5 * * * *"`
     - `IsDaemon`: `true`
     - `MaxIterations`: `24`
     - `Prompt`: `"【5分钟常驻进度同步】请检查当前会话是否仍有进行中的任务或后台作业：若有，请用中文简要向用户同步最新进度、关键指标与下一步动作；若当前所有任务与后台作业均已完成且处于空闲等待用户输入状态，请调用 manage_task 取消本常驻定时任务以免空闲刷屏。"`
2. Whenever a `【5分钟常驻进度同步】` cron trigger fires while work is still in progress, send a concise Chinese progress update to the user with observed milestones, metrics, and the next step.
3. As soon as all tasks and background jobs for the current request are complete and the session is about to return to idle waiting for the user, cancel the standing `schedule` task via `manage_task(Action="kill")` before ending the turn so idle sessions do not spam messages.

## 2. CI & Verification Isolation
- **Independent Subproject CI + Shared Base**: Each subproject (`client/`, `server/`, `portal/`, `test-pool/`, `CICD/`) maintains its own independent workflow (`.github/workflows/{client,server,portal,test-pool,cicd}.yml`) and DevSpace test-pool shard (`shard-rust`, `shard-go`, `shard-portal`, `shard-npu-dra`), while `Base/` NixOS & GitOps evaluation (`base.yml`) and `test-pool/` / `CICD/` runner infrastructure are shared across the whole `Infra` workspace. Workflows target `cluster-pits-lab` self-hosted Gitea Actions `act_runner` labels (`pits-lab-rust`, `pits-lab-windows`, `pits-lab-go`, `pits-lab-e2e`, `pits-lab-npu`, `ubuntu-latest`) using `/opt/pits-cache` and `/ramdisk` DDR5 `tmpfs`.
- **CICD Build, Test & Deploy Handover**: Project-wide build, CI verification, and deployment tasks are handed over to the `CICD/` subproject:
  - Use `CICD/scripts/Invoke-CicdPipeline.ps1` to push to Lab Gitea, wait for `act_runner` CI workflows, and optionally trigger CD promotion (`-DeployCore`):
    - When the local working tree has uncommitted edits (or local `HEAD` is behind prior Lab CI snapshots), use **`-Snapshot`** to pack and push the current workspace via a temporary `GIT_INDEX_FILE` without altering local `HEAD` or index.
    - After committing `CICD/` locally, pass **`-Force`** once if Lab Gitea `main` holds prior test snapshot commits.
    - **Flux `prune: true` Guard**: Never push (or `-Force` push) a commit lacking `CICD/platform/kustomization.yaml` to Lab Gitea `main`, or Lab Flux CD (`lab-cicd-platform`) will prune `LabDevStack` (`Invoke-CicdPipeline.ps1` enforces this check automatically).
  - Use `CICD/scripts/Deploy-CoreArtifacts.ps1 -Target <all|portal|agent|test-runner|cicd-runner|windows|base>` for verified deployment to `pits-core` (`100.95.95.88`), Windows `client/bin/pitsctl.exe` cross-builds (`x86_64-pc-windows-gnu`), and Zot/Talos image caches; it runs the `test-pool/scripts/Invoke-PitsTestPool.ps1` pre-promotion gate and delegates NixOS `pits-portal` promotion to `deploy/nixos/promote-portal.ps1` (which remains backward-compatible and accepts `-ViaCicd`).
  - Verify Lab CI/CD stack health and `netboot-004` exclusion via `CICD/scripts/Verify-CicdStack.ps1`.
- **Lab CI/CD vs. Core Hardware Isolation**: `cluster-pits-lab` CI/CD (`CICD/`) is strictly decoupled from `pits-core` RKE2 hardware GitOps (`Base/`). Lab Flux CD (`CICD/bootstrap/gotk-sync.yaml`) must exclude `Base/`, and all `CICD/` Deployments, Clusters, Jobs, and runners must enforce `kubernetes.io/hostname NotIn [netboot-004]`.
- **Antigravity App Launch**: Whenever starting or relaunching Antigravity on this workstation (`win11-001`), ALWAYS use `C:\Users\pitsadmin\Desktop\Start-Antigravity-App.cmd` (which configures `http://127.0.0.1:7897` proxy flags) instead of invoking `Antigravity.exe` directly.

## 3. PITS Workspace Safety Invariants
- **Self-Session Lock**: This workstation (`pits-vms/win11-001`) runs inside the Lab Talos KubeVirt cluster. Never stop, reboot, or disrupt `win11-001` or its active Talos host without explicit user authorization or a detached `systemd-run` failover/cutover command.
- **PowerShell 5.1 SSH Piping**: On Windows PowerShell 5.1, never use bash `<<'EOF'` or `||`; pipe bash scripts with LF line endings via `ssh -T root@100.95.95.88 "bash -s"`.
- **Talos Netboot Guard**: Preserve `selinux=1` and `module.sig_enforce=1` on all Talos nodes; never introduce `machine.install`, `machine.disks`, `DiskWipeConfig`, or `UnattendedInstallConfig`.
- **Base Flake & Flux Alignment**: NixOS flake & GitOps manifests live under `Base/`. Any change to `Base/gitops/` paths must remain consistent with `Base/gitops/clusters/core/flux-system/gotk-sync.yaml` and `Base/gitops/clusters/core/stack.yaml`.

## 4. Block Storage & Disk Migration Guardrails
- **Explicit Sector Geometry (`4096B` vs `512B`)**: PITS NVMe/TCP SAN namespaces use `4096`-byte logical sectors (`4Kn`), whereas inner VM disk images may use `512`-byte sectors. Never invoke `losetup -P` / `--partscan` with default `512B` geometry; always inspect raw GPT/superblock headers (`"EFI PART"` at byte `4096` vs `512`) and pass explicit `--sector-size <512|4096>`.
- **Deep Non-Destructive `--dry-run` Preflight**: Any storage migration or cutover `--dry-run` must go beyond `is_file()` and non-destructively validate binary headers (GPT LBA signature offset, expected byte size, non-symlink check) and read-only partition enumeration before cutover.
- **Source-Geometry Fixture Gate for Standalone Scripts**: Target-only canary tests do not cover legacy extraction paths. Any standalone Python/Bash disk migration or format-conversion script outside `cargo test` must be verified end-to-end against a small sparse loopback fixture matching the exact source sector size (`4096B` or `512B`) before production execution.
- **`nvmet` Backing-File Integrity**: Before mounting a legacy SAN backing file on the host, assert `namespaces/<nsid>/enable == 0` on all SAN subsystems. When populating an active `nvmet`-backed file, write in-place (`dd conv=notrunc,fdatasync`), assert `(st_dev, st_ino, st_size)` is unchanged, and pass `cmp --silent` byte-for-byte verification before rebinding PVCs or starting the VM.
