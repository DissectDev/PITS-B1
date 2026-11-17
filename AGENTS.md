# PITS Infra Workspace Always-On Rules

This `Infra` workspace (`c:\Project\PITS\Infra`, upstream `https://github.com/DissectDev/PITS-B1`) unifies:
- `Base/` — NixOS 26.05 (`pits-core` flake, `hosts/`, `modules/`, `site/`, `gitops/`, `scripts/`, `docs/`)
- `client/` — Rust `pitsctl` CLI, catalog engine, MCP server & embedded portal host
- `portal/` — Backstage v1.56 New Frontend System SPA
- `server/` — Go `pits-agent` privileged host/SAN/VPP daemon
- `deploy/` — Declarative NixOS deployment scripts & modules
- `catalog/` — Sourcing governance YAML seeds
- `test-pool/` — Parallel DevSpace + `agent-sandbox` CI verification pool

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
- **Independent Subproject CI + Shared Base**: Each subproject (`client/`, `server/`, `portal/`, `test-pool/`) maintains its own independent GitHub Actions workflow (`.github/workflows/`) and DevSpace test-pool shard (`shard-rust`, `shard-go`, `shard-portal`, `shard-npu-dra`), while `Base/` NixOS & GitOps evaluation and `test-pool/` runner infrastructure are shared across the whole `Infra` workspace.
- **Pre-Promotion Gate**: Always verify modified subprojects via `test-pool/scripts/Invoke-PitsTestPool.ps1` before running `deploy/nixos/promote-portal.ps1`.

## 3. PITS Workspace Safety Invariants
- **Self-Session Lock**: This workstation (`pits-vms/win11-001`) runs inside the Lab Talos KubeVirt cluster. Never stop, reboot, or disrupt `win11-001` or its active Talos host without explicit user authorization or a detached `systemd-run` failover/cutover command.
- **PowerShell 5.1 SSH Piping**: On Windows PowerShell 5.1, never use bash `<<'EOF'` or `||`; pipe bash scripts with LF line endings via `ssh -T root@100.74.25.42 "bash -s"`.
- **Talos Netboot Guard**: Preserve `selinux=1` and `module.sig_enforce=1` on all Talos nodes; never introduce `machine.install`, `machine.disks`, `DiskWipeConfig`, or `UnattendedInstallConfig`.
- **Base Flake & Flux Alignment**: NixOS flake & GitOps manifests live under `Base/`. Any change to `Base/gitops/` paths must remain consistent with `Base/gitops/clusters/core/flux-system/gotk-sync.yaml` and `Base/gitops/clusters/core/stack.yaml`.

## 4. Block Storage & Disk Migration Guardrails
- **Explicit Sector Geometry (`4096B` vs `512B`)**: PITS NVMe/TCP SAN namespaces use `4096`-byte logical sectors (`4Kn`), whereas inner VM disk images may use `512`-byte sectors. Never invoke `losetup -P` / `--partscan` with default `512B` geometry; always inspect raw GPT/superblock headers (`"EFI PART"` at byte `4096` vs `512`) and pass explicit `--sector-size <512|4096>`.
- **Deep Non-Destructive `--dry-run` Preflight**: Any storage migration or cutover `--dry-run` must go beyond `is_file()` and non-destructively validate binary headers (GPT LBA signature offset, expected byte size, non-symlink check) and read-only partition enumeration before cutover.
- **Source-Geometry Fixture Gate for Standalone Scripts**: Target-only canary tests do not cover legacy extraction paths. Any standalone Python/Bash disk migration or format-conversion script outside `cargo test` must be verified end-to-end against a small sparse loopback fixture matching the exact source sector size (`4096B` or `512B`) before production execution.
- **`nvmet` Backing-File Integrity**: Before mounting a legacy SAN backing file on the host, assert `namespaces/<nsid>/enable == 0` on all SAN subsystems. When populating an active `nvmet`-backed file, write in-place (`dd conv=notrunc,fdatasync`), assert `(st_dev, st_ino, st_size)` is unchanged, and pass `cmp --silent` byte-for-byte verification before rebinding PVCs or starting the VM.
