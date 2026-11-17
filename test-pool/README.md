# PITS Parallel Test Pool (`cluster-pits-lab`)

基于 **CNCF DevSpace (`v6.3.21`)** + **Kubernetes SIG Apps `agent-sandbox` (`v1.0.11`)** 构建的零磁盘磨损、纯离线、多节点并发测试热池。

## 架构全景

```text
win11-001 (Antigravity / Verifier Subagents, 零本地 Docker / 零本地 Rust 依赖)
   │  直连 Lab VLAN 20 K8s API (https://10.89.20.21:6443)
   │  DevSpace 二进制增量流式同步 (<200ms 仅推脏文件 diff，预置 /tmp/devspacehelper)
   ▼
cluster-pits-lab (Namespace: pits-test-pool, 硬反亲和排除桌面所在 netboot-004)
   ├── [netboot-001 · runner-rust]       10Gi DDR5 tmpfs · cargo nextest --offline + cargo check
   ├── [netboot-002 · runner-go-linux]    8Gi DDR5 tmpfs · Linux 6.18 真机 CGO go test -race ./...
   ├── [netboot-003 · runner-portal-e2e]  8Gi DDR5 tmpfs · Zen4 tsc + vite + pitsctl serve + Headless Chromium CDP 点检
   ├── [001/002 · shard4-npu-dra-verify] 按需 DRA v1 ResourceClaim 挂载 Intel AI Boost (8086:7d1d) 硬件验证
   └── [001..003 · SandboxWarmPool]      K8s agent-sandbox 预热沙箱池，供 /teamwork 多 Agent <100ms 独占认领
```

- **离线镜像分发链路**：由 [build-and-publish-image.ps1](image/build-and-publish-image.ps1) 调用 `pits-core` NixOS (`PITS-B1/Base#core`) 原生构建分层 OCI 镜像 `docker.io/pits/test-runner:v1.2`（内置 Rust `1.95`、`cargo-nextest`、Go `1.26`、GCC ThreadSanitizer、Node.js `24`、Headless Chromium、以及离线 `Cargo vendor` / `Go vendor` / Linux `node_modules`），原子导入 `talos-image-cache` (`http://10.89.20.14:3172`)，VLAN 20 全程零外网依赖。

---

## 目录结构

- [devspace.yaml](devspace.yaml) — CNCF DevSpace 并行验证流水线定义（`verify` / `verify-full` / 单分片 `shard-*` / `deploy`）
- [scripts/Invoke-PitsTestPool.ps1](scripts/Invoke-PitsTestPool.ps1) — 工作站与 Antigravity 免弹窗单入口 CLI（输出聚合结构化 JSON）
- [scripts/runner-verify-shard.sh](scripts/runner-verify-shard.sh) — 容器内 `tmpfs` 增量编译与测试执行器
- [scripts/portal-headless-e2e.mjs](scripts/portal-headless-e2e.mjs) — 基于 Node 24 原生 CDP 的无头 Chromium 真机点检脚本
- [manifests/00-namespace-rbac.yaml](manifests/00-namespace-rbac.yaml) — `pits-test-pool` 命名空间与最小权限 ServiceAccount
- [manifests/05-agent-sandbox-controller.yaml](manifests/05-agent-sandbox-controller.yaml) — K8s SIG Apps `agent-sandbox` `v1.0.11` CRD 与控制器（已钉扎避开 `netboot-004`）
- [manifests/10-warm-pool-deployments.yaml](manifests/10-warm-pool-deployments.yaml) — 绑定 `001..003` 的 3 个常驻 `tmpfs` 热池 Deployment
- [manifests/20-agent-sandbox-warmpool.yaml](manifests/20-agent-sandbox-warmpool.yaml) — 动态 `SandboxTemplate` 与 `SandboxWarmPool`（多 Agent 并发隔离）
- [manifests/30-shard4-npu-dra-job.yaml](manifests/30-shard4-npu-dra-job.yaml) — 按需 Intel NPU DRA v1 硬件门禁 Job
- [.agents/skills/pits-test-pool/SKILL.md](.agents/skills/pits-test-pool/SKILL.md) — Antigravity 自动验收 Skill

---

## 常用命令

```powershell
# 1. 日常 3 路全量并行门禁 (Rust + Linux Go -race + Portal 构建与 Chromium E2E)
powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\test-pool\scripts\Invoke-PitsTestPool.ps1

# 2. 4 路全开（追加 Intel NPU DRA v1 真机硬件验证）
powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\test-pool\scripts\Invoke-PitsTestPool.ps1 -IncludeHardware

# 3. 单分片极速回归 (~1-2s)
powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\test-pool\scripts\Invoke-PitsTestPool.ps1 -Shard go-linux

# 4. 多 Agent 并发隔离模式（通过 SandboxClaim 独占认领预热 Pod）
powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\test-pool\scripts\Invoke-PitsTestPool.ps1 -ClaimSandbox

# 5. 直接使用原生开源 DevSpace CLI
$env:KUBECONFIG = "$env:LOCALAPPDATA\PITS\Client\clusters\pits-lab-talos-k8s.yaml"
devspace run-pipeline verify -n pits-test-pool
```

### Antigravity Command Allowlist 配置建议

将以下前缀加入 Antigravity 的自动允许列表（Allowlist），即可让主 Agent 或后台 Verifier Subagent 全程免弹窗自动并发验收：
- `powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\test-pool\scripts\Invoke-PitsTestPool.ps1`
- `devspace run-pipeline`
