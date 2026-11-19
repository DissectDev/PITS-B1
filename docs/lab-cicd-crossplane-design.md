# PITS Infra 本地计算节点 CI/CD 与 Crossplane 配置面设计方案

> **适用范围**：`cluster-pits-lab` 计算集群（`netboot-001..005`，VLAN 20）  
> **设计原则**：硬件一致性基座（`pits-core` / `Base/`）与软件开发加速栈（`cluster-pits-lab`）**控制面与运行面彻底解耦**。

---

## 1. 设计目标与边界原则

### 1.1 为什么不复用 `pits-core` 的 Flux / CNPG / 控制器体系

1. **职责边界不同（Hardware Congruence vs. Software Velocity）**：
   - `pits-core`（RKE2，`192.168.71.35`）承载整个基础设施的硬件基座与控制面一致性（NixOS 26.05、EdgeRouter VPP、X710 SR-IOV VF、Btrfs SAN、Tinkerbell PXE）。其上的 `flux-system`、`kyverno`、`cnpg-system` 应专注于守护物理基座稳定，不应被高频变更的 CI/CD 服务、构建缓存或测试容器污染。
   - **故障域单向依赖**：若 `pits-core` 的硬件 CD 依赖运行在 `cluster-pits-lab` 上的 Gitea，一旦 Lab 节点维护或重刷 PXE，核心机硬件基座将失去 GitOps 协调能力。因此必须保持 `pits-core` 硬件 CD 独立运行，仅将软件 CI/CD 与缓存体系绑定到 `cluster-pits-lab` 计算节点。
2. **网络零跳与存储就近**：
   - 工作站 `win11-001` 运行在 `netboot-004` 上，与 `cluster-pits-lab` 计算节点同处 **VLAN 20（`10.89.20.0/24`）** 二层网络，推送代码、拉取预览/制品及调度测试完全不经过 Tailscale DERP 中继。
   - `cluster-pits-lab` 节点间已有 40Gbps Thunderbolt-net 互联及 OpenEBS Mayastor NVMe SPDK 存储池，天然适合高吞吐 CI/CD 与缓存 IO。
3. **Antigravity 协同闭环**：
   - Antigravity 在本地与 `pits-test-pool` 调度的测试（[devspace.yaml](../test-pool/devspace.yaml)、[Invoke-PitsTestPool.ps1](../test-pool/scripts/Invoke-PitsTestPool.ps1)）与 `act_runner` 在 CI 执行的门禁**共用同一套流水线与热池资源**，避免“本地通过、CI 失败”的漂移问题。

---

## 2. `cluster-pits-lab` 节点与存储底座盘点

| 节点 | VLAN 20 IP | 硬件/角色 | CI/CD 调度角色 |
| :--- | :--- | :--- | :--- |
| `netboot-001` | `10.89.20.21` | Talos `v1.14.5` · `7.3.1-talos` · Intel NPU · Mayastor | 有状态底座（Gitea/DB/Cache）+ Rust Runner + NPU DRA 门禁 |
| `netboot-002` | `10.89.20.22` | Talos `v1.14.5` · `7.3.1-talos` · Intel NPU · Mayastor | 有状态底座 + Linux Go `-race` Runner + NPU DRA 门禁 |
| `netboot-003` | `10.89.20.23` | Talos `v1.14.5` · `7.3.1-talos` · Mayastor | 有状态底座 + Portal/Headless Chromium E2E Runner |
| `netboot-004` | `10.89.20.24` | 宿主机运行 `win11-001` 工作站 VM | **硬反亲和严格排除（`NotIn [netboot-004]`）** |
| `netboot-005` | `10.89.20.25` | Talos `v1.14.5` · NVIDIA RTX 5090 (`nvidia-dra-driver-gpu`) | GPU / CUDA / DRA 专项 Runner 门禁 |

### 存储分层策略

| 存储类（StorageClass） | 特性 | 承接的 CI/CD 数据 |
| :--- | :--- | :--- |
| `mayastor-2` | 2 副本 SPDK 块卷（`Immediate`、支持在线扩容） | **不可丢失状态**：Gitea Git 仓库/LFS、CloudNativePG Postgres（Gitea + Attic 库） |
| `openebs-single-replica` | 单副本 SPDK 高速卷（可重建、零冗余写放大） | **可重建缓存**：Zot OCI Registry/Pull-Through blobs、Attic Nix NAR 缓存、Actions Cache |
| DDR5 `tmpfs` (`emptyDir: Memory`) | 零 NVMe 磁盘磨损、内存级构建速度 | `act_runner` 挂载/复用的 `pits-test-pool` 编译与单测工作区 |

---

## 3. 总体架构

```mermaid
flowchart TB
    WS["win11-001 (netboot-004, VLAN 20 直连)"] -->|git push / tea CLI| GIT_SVC["Gitea (http://git.lab.pits.home.arpa:3000)"]
    AGY["Antigravity Agent"] -->|快速局部回归 1-3s| TP["pits-test-pool (DevSpace / SandboxClaim)"]
    AGY -->|tea / API 查 CI 状态与日志| GIT_SVC

    subgraph LAB["cluster-pits-lab (独立控制面与加速栈, 强制排除 netboot-004)"]
        FLUX["Lab Flux CD (flux-system)"] -->|拉取 lab-platform/ & test-pool/| GIT_SVC
        GIT_SVC -->|Webhook seconds-level| FLUX
        FLUX -->|下发 XRD & LabDevStack CR| XP["Lab Crossplane (crossplane-system)"]

        XP -->|编排| CNPG["Lab CNPG Postgres (mayastor-2)"]
        XP -->|编排| GIT_SVC
        XP -->|编排| ZOT["Zot OCI Registry & Pull-Through Cache (openebs-single-replica)"]
        XP -->|编排| ATTIC["Attic Nix Binary Cache (openebs-single-replica)"]
        XP -->|编排| RUNNERS["act_runner 池"]

        RUNNERS -->|netboot-001..003| SHARD_CPU["CPU/tmpfs 分片: Rust / Go / Portal E2E"]
        RUNNERS -->|netboot-001..002| SHARD_NPU["硬件分片: Intel NPU DRA"]
        RUNNERS -->|netboot-005| SHARD_GPU["硬件分片: RTX 5090 GPU DRA"]
        RUNNERS -->|读写缓存| ZOT
        RUNNERS -->|push NAR| ATTIC
        SHARD_CPU -.->|复用相同脚本与热池| TP
    end

    GIT_SVC -->|后台异步单向 Push Mirror (main)| GH["GitHub: DissectDev/PITS-B1"]
    CORE_FLUX["pits-core 硬件 Flux CD (独立运行, 不依赖 Lab)"] -.->|继续守卫 Base/ 硬件一致性| CORE_RKE2["pits-core RKE2"]
```

---

## 4. Crossplane 配置面设计

### 4.1 控制器选型（全离线优先）

在 `cluster-pits-lab` 的 `crossplane-system` 中部署最小化组合，避免引入难以离线维护的冷门第三方 Provider：

- `crossplane` 核心控制器
- `provider-helm`（管理 CNPG Operator、Gitea、Zot 等 Helm Release）
- `provider-kubernetes`（管理 Namespace、Secret、CNPG `Cluster`、Attic、`act_runner`、幂等初始化 `Job`、Flux `GitRepository` / `Receiver`）
- `function-go-templating`（编写声明式 Composition 模板，并统一注入 `netboot-004` 硬反亲和规则）

> **为什么不单独部署 `provider-gitea`？**  
> 在离线环境中维护额外 Upjet Provider 镜像与认证依赖成本高。采用 **静态预置 Runner Token Secret + Crossplane 托管幂等 Reconcile `Job`**（调用 Gitea REST API 确保 Repo / Push Mirror / Webhook 存在）更轻量、透明且易调试。

### 4.2 抽象接口定义（XRD：`XLabDevStack` / `LabDevStack`）

```yaml
apiVersion: apiextensions.crossplane.io/v1
kind: CompositeResourceDefinition
metadata:
  name: xlabdevstacks.lab.pits.io
spec:
  group: lab.pits.io
  names:
    kind: XLabDevStack
    plural: xlabdevstacks
  claimNames:
    kind: LabDevStack
    plural: labdevstacks
  versions:
    - name: v1alpha1
      served: true
      referenceable: true
      schema:
        openAPIV3Schema:
          type: object
          properties:
            spec:
              type: object
              required: [domain, gitea, caches, runners]
              properties:
                domain:
                  type: string
                  default: lab.pits.home.arpa
                excludeNodes:
                  type: array
                  items: { type: string }
                  default: ["netboot-004"]
                storage:
                  type: object
                  properties:
                    stateStorageClass:
                      type: string
                      default: mayastor-2
                    cacheStorageClass:
                      type: string
                      default: openebs-single-replica
                gitea:
                  type: object
                  properties:
                    namespace: { type: string, default: pits-cicd }
                    repoSize: { type: string, default: 50Gi }
                    dbSize: { type: string, default: 20Gi }
                    mirrorRepo: { type: string, default: DissectDev/PITS-B1 }
                    githubRemote: { type: string, default: https://github.com/DissectDev/PITS-B1.git }
                    mirrorBranches:
                      type: array
                      items: { type: string }
                      default: ["main"]
                caches:
                  type: object
                  properties:
                    zotSize: { type: string, default: 120Gi }
                    atticSize: { type: string, default: 120Gi }
                    upstreamMirrors:
                      type: array
                      items: { type: string }
                      default: ["docker.io", "ghcr.io", "quay.io"]
                runners:
                  type: object
                  properties:
                    cpuReplicas: { type: integer, default: 3 }
                    npuReplicas: { type: integer, default: 1 }
                    gpuReplicas: { type: integer, default: 1 }
```

### 4.3 实例化清单示例（`LabDevStack` Claim）

```yaml
apiVersion: lab.pits.io/v1alpha1
kind: LabDevStack
metadata:
  name: pits-lab-acceleration
  namespace: pits-cicd
spec:
  domain: lab.pits.home.arpa
  excludeNodes:
    - netboot-004
  storage:
    stateStorageClass: mayastor-2
    cacheStorageClass: openebs-single-replica
  gitea:
    namespace: pits-cicd
    repoSize: 50Gi
    dbSize: 20Gi
    mirrorRepo: DissectDev/PITS-B1
    githubRemote: https://github.com/DissectDev/PITS-B1.git
    mirrorBranches: ["main"]
  caches:
    zotSize: 120Gi
    atticSize: 120Gi
    upstreamMirrors: ["docker.io", "ghcr.io", "quay.io"]
  runners:
    cpuReplicas: 3
    npuReplicas: 1
    gpuReplicas: 1
```

---

## 5. 四大核心组件配置细节

### 5.1 Gitea（主 Git Host + Push Mirror + Actions Cache）

1. **数据库**：由 Composition 在 `pits-cicd` 下生成 CNPG `Cluster`（`instances: 2`，`storageClass: mayastor-2`，`size: 20Gi`），自动产出 `gitea-pg-app` Secret 供 Gitea 挂载。
2. **核心环境变量与存储**：
   - `GITEA__database__DB_TYPE=postgres`（直连同命名空间 `gitea-pg-rw`）
   - `GITEA__actions__ENABLED=true`
   - `GITEA__actions__ARTIFACT_RETENTION_DAYS=7`
   - `GITEA__mirror__ENABLED=true`、`GITEA__mirror__MIN_INTERVAL=5m`
   - 禁用内置 SSH 或暴露高位 NodePort（如 `30222`），HTTP 服务走 `3000` / VLAN 20 VIP，便于免密钥或 Token 极速内网操作。
3. **免交互初始化 Job（由 Crossplane 管理）**：
   - 读取预先创建的 Secret（含 `GITEA_ADMIN_USER/PASS`、`RUNNER_REG_TOKEN`、`GITHUB_MIRROR_PAT`、`FLUX_WEBHOOK_SECRET`）；
   - 通过 Gitea API 幂等执行：
     - 创建组织 `DissectDev` 与仓库 `PITS-B1`；
     - 创建 `Push Mirror`（`remote_address: https://github.com/DissectDev/PITS-B1.git`，`sync_on_commit: true`）；
     - 为仓库注册 Gitea Webhook 指向 Lab `notification-controller`；
     - 将固定 `RUNNER_REG_TOKEN` 与 `act_runner` 共享。

### 5.2 CI Cache 三层加速体系

| 层级 | 组件 | 挂载存储 | 解决的痛点 |
| :--- | :--- | :--- | :--- |
| **1. OCI 镜像代理与私有库** | **Zot** (`oci.lab.pits.home.arpa:5000`) | `openebs-single-replica` (`120Gi`) | 统一代理 `docker.io`/`ghcr.io`/`quay.io` 并存储自研镜像，配合 `10.89.20.14:3172` 实现零外网镜像拉取 |
| **2. Nix 二进制构件缓存** | **Attic** (`nix-cache.lab.pits.home.arpa`) | `openebs-single-replica` (`120Gi`) + CNPG 库 | 缓存 `Base/`、`pitsctl`、`pits-agent`、自定义内核/工具链 NAR，避免每次重跑 `nix-build` 与全量 `vendor` |
| **3. 编译与依赖缓存** | **Gitea Actions Cache + `tmpfs`/PV** | DDR5 `tmpfs` + `openebs-single-replica` | 复用预热 Cargo/Go/npm 离线包与 `sccache`/Actions Cache，消除重复下载依赖 |

### 5.3 `act_runner` 执行池（与 `test-pool` 合一）

Crossplane Composition 在 `pits-test-pool` 命名空间创建三类 `act_runner`：

1. **通用计算 Runner（`runner-cpu`，3 副本分布于 `netboot-001..003`）**：
   - 标签：`pits-lab-rust`、`pits-lab-go`、`pits-lab-e2e`、`ubuntu-latest`
   - 使用固定权限的 `ServiceAccount`（仅能读写 `pits-test-pool` 命名空间中的 `SandboxClaim` / `Pod`），Workflow 内直接调用 [devspace.yaml](../test-pool/devspace.yaml) 与 [runner-verify-shard.sh](../test-pool/scripts/runner-verify-shard.sh)，享受现有 1–3s 热池增量验证能力。
2. **Intel NPU 硬件 Runner（`runner-npu`，亲和 `netboot-001/002`）**：
   - 标签：`pits-lab-npu`
   - 用于调度包含 `Shard 4 (Intel NPU DRA v1)` 的硬件门禁任务。
3. **NVIDIA GPU 硬件 Runner（`runner-gpu`，亲和 `netboot-005`）**：
   - 标签：`pits-lab-gpu`
   - 用于调度依赖 RTX 5090 (`nvidia-dra-driver-gpu`) 的 AI/渲染/驱动基准门禁。

> **强制护栏**：所有 Runner Pod 及创建的临时沙箱 Pod 均继承 `kubernetes.io/hostname NotIn ["netboot-004"]`，绝对不争抢 `win11-001` 宿主机资源。

### 5.4 Lab 侧独立 Flux CD（仅管理软件加速与测试环境）

在 `cluster-pits-lab` 安装独立的 `flux-system`（不碰 `pits-core`）：

- **同步范围**：只同步仓库内的 `lab-platform/` 与 `test-pool/manifests/`（**明确排除 `Base/`**，或通过 `.fluxignore` 仅放行 `!lab-platform/` 和 `!test-pool/`）。
- **内网极速源 + 秒级 Webhook**：
  ```yaml
  apiVersion: source.toolkit.fluxcd.io/v1
  kind: GitRepository
  metadata:
    name: lab-platform-repo
    namespace: flux-system
  spec:
    interval: 5m
    url: http://gitea-http.pits-cicd.svc.cluster.local:3000/DissectDev/PITS-B1.git
    ref:
      branch: main
    ignore: |
      /*
      !/lab-platform/
      !/test-pool/manifests/
  ---
  apiVersion: notification.toolkit.fluxcd.io/v1
  kind: Receiver
  metadata:
    name: gitea-lab-webhook
    namespace: flux-system
  spec:
    type: gitea
    events: ["push"]
    secretRef:
      name: gitea-webhook-token
    resources:
      - kind: GitRepository
        name: lab-platform-repo
  ```

---

## 6. 仓库目录规划建议

保持 `Base/` 纯净，在 `c:\Project\PITS\Infra` 根目录下新增 `lab-platform/` 与 `docs/`：

```text
c:\Project\PITS\Infra\
├── Base/                        # [不动] 仅 pits-core 硬件基座与 Core Flux GitOps
├── test-pool/                   # 现有 DevSpace + agent-sandbox 热池与脚本
├── docs/
│   └── lab-cicd-crossplane-design.md  # 本文档
└── lab-platform/                # [新增] cluster-pits-lab 独立软加速平台
    ├── bootstrap/               # Lab Flux CD 组件 + gotk-sync (仅看 lab-platform/ & test-pool/)
    ├── crossplane/
    │   ├── core/                # Crossplane + provider-helm + provider-kubernetes + function-go-templating
    │   ├── xrds/                # XLabDevStack XRD
    │   └── compositions/        # Go-templating Composition (含 netboot-004 强制反亲和)
    └── instances/
        └── pits-lab-devstack.yaml  # 唯一声明式 LabDevStack 实例 CR
```

---

## 7. 离线 Bootstrap 实施顺序

由于 `cluster-pits-lab` 为无盘 PXE Talos 集群且已运行 `talos-image-cache`（`10.89.20.14:3172`），建议按以下 5 步一次性拉起：

1. **Step 1：导入底座镜像到 `10.89.20.14:3172`**
   - 利用现有的 [import-to-talos-cache.py](../test-pool/image/import-to-talos-cache.py) 机制，预先将 Flux、Crossplane（+ 3 个 Package）、CloudNativePG、Gitea、Zot、Attic、`act_runner` 所需镜像导入内网 cache。
2. **Step 2：初始化 Lab 侧 Flux + Crossplane**
   - 使用 `pits-lab-talos-k8s.yaml` kubeconfig 在 `cluster-pits-lab` 安装 `flux-system` 和 `crossplane-system`（所有 Deployment 带 `NotIn [netboot-004]`）。
3. **Step 3：注入运行期凭证 Secret（不入库）**
   - 在 `pits-cicd` 创建 `lab-devstack-secrets`（包含 `GITEA_ADMIN_PASS`、`RUNNER_REG_TOKEN`、`GITHUB_MIRROR_PAT`、`ATTIC_TOKEN`、`FLUX_WEBHOOK_SECRET`）。
4. **Step 4：Apply `LabDevStack` CR，由 Crossplane 拉起全栈**
   - 依次等待 CNPG (`mayastor-2`)、Gitea、Zot、Attic、Core/NPU/GPU `act_runner` 就绪；校验 `netboot-004` 上零新 Pod 落入。
5. **Step 5：切换工作站主源与 CI Workflow**
   - 将本地 `c:\Project\PITS\Infra` 推送至 `http://git.lab.pits.home.arpa:3000/DissectDev/PITS-B1.git`；
   - 确认 Gitea 自动后台 Push Mirror 到 GitHub 成功；
   - 在 [`.github/workflows/`](../.github/workflows) 中将 `runs-on` 切到 `pits-lab-rust` / `pits-lab-go` / `pits-lab-e2e` / `pits-lab-npu` / `pits-lab-gpu`，并将 Lab Flux `GitRepository` 切至同集群 Gitea 内网地址。

---

## 8. 安全性与自锁红线核对

- **不触碰 `netboot-004` / `win11-001`**：Crossplane Composition 对所有工作负载强制注入 `kubernetes.io/hostname NotIn [netboot-004]`。
- **不污染 `pits-core`**：`pits-core` 零新增 Pod、零共享 Postgres、零反向强依赖。
- **公开仓库防泄密**：由于 GitHub 上 `DissectDev/PITS-B1` 为公开仓库，Gitea Push Mirror 仅同步脱敏主分支（如 `main`），所有 Token/PAT/Kubeconfig 仅存于集群 `Secret` 与本地 `%LOCALAPPDATA%\PITS\Client`。
