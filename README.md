# PITS Infra Workspace (`DissectDev/PITS-B1`)

`c:\Project\PITS\Infra` 统一了 PITS 基础设施的已实施工程（上游仓库：`https://github.com/DissectDev/PITS-B1`）：

| 目录 | 职责 | 技术栈 / 说明 |
| :--- | :--- | :--- |
| `Base/` | 核心机 NixOS 26.05 宿主与核心 RKE2 GitOps | Nix Flake (`Base/flake.nix`)、宿主模块 (`Base/modules/`)、站点参数 (`Base/site/core.nix`)、Flux GitOps (`Base/gitops/`) |
| `ai-storage/` | OpenDAL 五层 AI Storage 运行时与 MCP Producer | Rust (`ai-store`：OpenDAL + 8 大语义 Archetypes + `ProjectProfile` + Streamable HTTP / Singleton Daemon MCP) |
| `client/` | `pitsctl` CLI、四层统一实体目录、MCP Server 与嵌入式门户服务端 | Rust (`cargo test` / `pitsctl serve` / `pitsctl mcp`) |
| `portal/` | Backstage `v1.56.x` New Frontend System 管理门户 | React / Vite SPA，构建至 `portal/dist/` 后由 `client/build.rs` 编译期内嵌 |
| `server/` | `pits-agent` 特权节点守护进程（`:18443` mTLS 1.3） | Go (`sysfs` / `nvmet` / `btrfs` / `govpp` / Lab VLAN 20 `HTTP CONNECT` 隧道) |
| `deploy/` | `pits-core` 声明式 NixOS 发布脚本与模块模板 | `deploy/nixos/promote-portal.ps1`（由 `CICD/scripts/Deploy-CoreArtifacts.ps1` 统一编排）、`pits-agent-module.nix`、`pits-portal-module.nix` |
| `catalog/` | 外部寻源治理（Sourcing Governance）目录种子 | `catalog/sourcing/*.yaml` |
| `test-pool/` | `cluster-pits-lab` 并行零磨损测试热池 | CNCF DevSpace + K8s `agent-sandbox` (`test-pool/scripts/Invoke-PitsTestPool.ps1`) |
| `CICD/` | `cluster-pits-lab` 独立 CI/CD、缓存与 Crossplane 平台（构建/测试/发布统一接管入口） | Gitea Actions + Zot + Attic + CNPG + Crossplane (`CICD/scripts/Invoke-CicdPipeline.ps1`, `CICD/scripts/Deploy-CoreArtifacts.ps1`) |
| `docs/` | 架构与工程设计文档 | `docs/lab-cicd-crossplane-design.md`, `docs/tailscale-keycloak-iam-crossplane-design.md`, `docs/cluster-lab-istio-ambient-keycloak-plan.md`, `docs/ai-storage-opendal-mcp-crossplane-design.md`, `docs/ai-storage-user-journeys-and-tutorials.md` |
| `.agents/` | 项目级统一 Agent 治理（Hooks、Skills、MCP 插件、Session Handoff） | 规则见 `AGENTS.md` |
| `.github/workflows/` | 独立子项目 CI + 共享基座 CI（运行于 `cluster-pits-lab` 自托管 `act_runner`） | `base.yml`、`cicd.yml`、`client.yml`、`server.yml`、`portal.yml`、`test-pool.yml` |

---

## 常用工作流

### 1. 统一 CI/CD 构建、验证与发布接管 (`CICD/`)
项目构建、门禁测试与 `pits-core` / Lab 制品发布任务已统一交接至 `CICD/` 子项目：
```powershell
# 推送至 Lab Gitea 并等待 cluster-pits-lab act_runner 全量门禁通过（可选 -DeployCore 联动发布）
powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\CICD\scripts\Invoke-CicdPipeline.ps1

# 验证后统一发布制品到 pits-core / Zot / Talos 缓存（portal | agent | test-runner | cicd-runner | base | all）
powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\CICD\scripts\Deploy-CoreArtifacts.ps1 -Target all

# 检查 Lab CI/CD 栈健康状态与 netboot-004 严格排除护栏
powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\CICD\scripts\Verify-CicdStack.ps1
```

### 2. 并行验收测试池 (`test-pool/`)
```powershell
# 3 路全量并行验证 (Rust + Linux Go -race + Portal E2E)
powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\test-pool\scripts\Invoke-PitsTestPool.ps1

# 单分片快速回归 (`rust` | `go-linux` | `portal-e2e` | `npu-dra`)
powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\test-pool\scripts\Invoke-PitsTestPool.ps1 -Shard rust
```

### 3. 本地 Portal 开发与向后兼容发布 (`deploy/`)
```powershell
# 前端 HMR Preview（直连 pits-core 后端）
Set-Location c:\Project\PITS\Infra\portal
npm run dev

# 推荐通过 CICD 统一入口发布 Portal（或带 -ViaCicd 向后兼容调用）
powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\CICD\scripts\Deploy-CoreArtifacts.ps1 -Target portal
```

### 3. 基座 NixOS 评估 (`Base/`)
```sh
nix eval ./Base#nixosConfigurations.ci-core.config.system.build.toplevel.drvPath
```
