# PITS Infra Workspace (`DissectDev/PITS-B1`)

`c:\Project\PITS\Infra` 统一了 PITS 基础设施的已实施工程（上游仓库：`https://github.com/DissectDev/PITS-B1`）：

| 目录 | 职责 | 技术栈 / 说明 |
| :--- | :--- | :--- |
| `Base/` | 核心机 NixOS 26.05 宿主与核心 RKE2 GitOps | Nix Flake (`Base/flake.nix`)、宿主模块 (`Base/modules/`)、站点参数 (`Base/site/core.nix`)、Flux GitOps (`Base/gitops/`) |
| `client/` | `pitsctl` CLI、四层统一实体目录、MCP Server 与嵌入式门户服务端 | Rust (`cargo test` / `pitsctl serve` / `pitsctl mcp`) |
| `portal/` | Backstage `v1.56.x` New Frontend System 管理门户 | React / Vite SPA，构建至 `portal/dist/` 后由 `client/build.rs` 编译期内嵌 |
| `server/` | `pits-agent` 特权节点守护进程（`:18443` mTLS 1.3） | Go (`sysfs` / `nvmet` / `btrfs` / `govpp` / Lab VLAN 20 `HTTP CONNECT` 隧道) |
| `deploy/` | `pits-core` 声明式 NixOS 发布脚本与模块模板 | `deploy/nixos/promote-portal.ps1`、`pits-agent-module.nix`、`pits-portal-module.nix` |
| `catalog/` | 外部寻源治理（Sourcing Governance）目录种子 | `catalog/sourcing/*.yaml` |
| `test-pool/` | `cluster-pits-lab` 并行零磨损测试热池 | CNCF DevSpace + K8s `agent-sandbox` (`test-pool/scripts/Invoke-PitsTestPool.ps1`) |
| `.agents/` | 项目级统一 Agent 治理（Hooks、Skills、MCP 插件、Session Handoff） | 规则见 `AGENTS.md` |
| `.github/workflows/` | 独立子项目 CI + 共享基座 CI | `base.yml`、`client.yml`、`server.yml`、`portal.yml`、`test-pool.yml` |

---

## 常用工作流

### 1. 并行验收测试池 (`test-pool/`)
```powershell
# 3 路全量并行验证 (Rust + Linux Go -race + Portal E2E)
powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\test-pool\scripts\Invoke-PitsTestPool.ps1

# 单分片快速回归 (`rust` | `go-linux` | `portal-e2e` | `npu-dra`)
powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\test-pool\scripts\Invoke-PitsTestPool.ps1 -Shard rust
```

### 2. 本地 Portal 开发与发布到 `pits-core` (`deploy/`)
```powershell
# 前端 HMR Preview（直连 pits-core 后端）
Set-Location c:\Project\PITS\Infra\portal
npm run dev

# 发布到 pits-core NixOS (`PITS-B1/Base#core`)
powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\deploy\nixos\promote-portal.ps1
```

### 3. 基座 NixOS 评估 (`Base/`)
```sh
nix eval ./Base#nixosConfigurations.ci-core.config.system.build.toplevel.drvPath
```
