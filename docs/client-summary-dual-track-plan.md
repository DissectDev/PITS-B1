# `[client]` 16 Resource Type 双轨 Summary（静态策略 × 动态变化）实施规划与交接文档

> **来源会话**：`PITS Assistant` 规划会话（`a86ae553-f908-4496-9922-36b12ac08364`）  
> **目标执行项目**：`PITS Infra`（`c:\Project\PITS\Infra`）  
> **建议新会话标题**：`[client] 16 Resource Type Dual-Track Summary`

---

## 1. 已拍板决策（User-Approved Decisions）

1. **四档观测模式（Observation Mode）与 16 项 Resource Type 归类已确认**：
   - **`policy-first`（静态策略主控 + 漂移守卫，Slow 300s / On-Change）**：
     - `pxe-boot-profile`、`auth-flow-bundle`
   - **`hybrid`（策略边界 + 运行态双轨，Standard 60s）**：
     - `baremetal-host`、`npu-accelerator`、`san-volume`、`ai-storage-workspace`
   - **`dynamic-first`（动态遥测与趋势主控，Fast 15s）**：
     - `gpu-accelerator`、`virtual-machine`、`edge-router`、`storage-pool`、`kubernetes-cluster`、`cicd-pipeline`
   - **`governance-event`（治理评分 + 周期/事件动态，Event-Driven）**：
     - `vendor-saas`、`commercial-sdk`、`managed-infra`、`outsourced-dev`
2. **Phase 1 + Phase 2 一并落地**：
   - 引入结构化 `EntitySummary`（向后兼容保留 `summary_line`）与 `ResourceTypeSummary` 聚合 `highlights` / `observationMode` / `driftedCount`。
   - 引入轻量内存 RingBuffer（`TelemetryHistory`）计算相邻采样周期变化量（`Δ` / 速率 Mbps / 丢包 Δ / 错误计数 Δ / 容量变化 Δ / 宿主机漂移 / `last_changed_at`），并实现分层刷新节奏（Fast 15s 刷新 Router/VPP + K8s/DRA/VM/CICD；Standard 60s 刷新含 Talos COSI / Hook 的全量快照）。
3. **Phase 3 优先补齐两类高价值深度动态探针（已确定重点）**：
   - **GoVPP 速率与丢包计数**（接口级与聚合 `rx/tx packets/bytes`、相邻采样速率 `Mbps`/`pps`、`drops`/`errors` 及 `Δ`）。
   - **K8s DRA `ResourceClaim` 占用数 + GPU/NPU 资源占用率**（`/apis/resource.k8s.io/v1/resourceclaims` 分配明细 + DevicePlugin Pod GPU 分配兜底，输出单卡状态、占用率 `%`、消费 Pod 列表与集群/类型级总占用率）。

---

## 2. 当前已完成进度（`Infra/server` 已就绪并通过测试）

以下 `Infra/server`（Go `pits-agent`）改动已完成并通过 `go test ./...` 与 `GOOS=linux GOARCH=amd64 go build ./...`：

- [`server/internal/api/types.go`](file:///c:/Project/PITS/Infra/server/internal/api/types.go)
  - `VPPInterface` 新增 `rxPackets` / `rxBytes` / `txPackets` / `txBytes` / `drops` / `rxErrors` / `txErrors`。
  - 新增 `VPPAggregateCounters` 并挂到 `VPPInspectReport.Counters`。
  - 新增 `DRAClaimInfo` 并在 `DiscoveredClusterReport` 中新增 `draClaims`、`devicePluginGpuUsed`、`allocatedGpus`、`gpuOccupancyPct`（`ClusterDiscoveryReport` 同步新增 `allocatedGpus`）。
- [`server/internal/vpp/counters.go`](file:///c:/Project/PITS/Infra/server/internal/vpp/counters.go)、[`counters_test.go`](file:///c:/Project/PITS/Infra/server/internal/vpp/counters_test.go)、[`inspector_linux.go`](file:///c:/Project/PITS/Infra/server/internal/vpp/inspector_linux.go)
  - 通过 VPP `cliInband("show interface")` 解析各接口 `rx/tx packets/bytes/drops/rx-error/tx-error` 并合并到 `VPPInspectReport`（含 Windows/Linux 跨平台单元测试）。
- [`server/internal/clusters/manager.go`](file:///c:/Project/PITS/Infra/server/internal/clusters/manager.go)
  - 新增 `fetchDRAClaims`（读取 `/apis/resource.k8s.io/v1/resourceclaims`）与 `fetchPodCountsAndGPUUsage`（统计非终止 Pod 的 DevicePlugin GPU 请求/限额），计算每个集群的 `AllocatedGPUs` 与 `GPUOccupancyPct`。

---

## 3. 待在 `[client]` 新会话中执行的完整任务清单

### Step 1：Rust `Infra/client` — DRA `ResourceClaim` 与 GPU/NPU 占用率采集
- **文件**：`client/src/k8s.rs`、`client/src/catalog/synthesizer.rs`
- 新增 `DraClaimAllocation` 结构并扩展 `ClusterControlPlaneSummary`：
  - `dra_claims: Vec<DraClaimAllocation>`（解析 `/apis/resource.k8s.io/v1/resourceclaims` 中的 `status.allocation.devices.results`、`nodeSelector`、`status.reservedFor`）。
  - `device_plugin_gpu_used: BTreeMap<String, usize>`（从非终止 Pod 统计 `nvidia.com/gpu`、`amd.com/gpu`、`gpu.intel.com/*` 请求数，兼容无 DRA 的集群）。
  - 在 `inspect_cluster_summary`（`k8s.rs`）、`inspect_cluster_fast`（`synthesizer.rs`）与 `parse_agent_cluster_summary`（`synthesizer.rs`）中同步解析上述字段。
  - 为每个 `gpu-accelerator`、`npu-accelerator` 与 `kubernetes-cluster` 计算精确的 **Claim 占用数、占用率 `%`、消费 Pod 列表**（单卡被 Claim 占用即 `100% (1/1)`，空闲即 `0% (0/1)`；多卡节点/集群汇总 `allocated/total (XX%)`）。

### Step 2：数据契约升级（向后兼容 `summary_line`）
- **文件**：`client/src/catalog/model.rs`、`portal/src/types.ts`
- 新增：
  - `ObservationMode` (`policy-first` | `hybrid` | `dynamic-first` | `governance-event`)
  - `SummaryPill { key, label, value, status }`
  - `DynamicSignal { key, label, value, delta, trend, observed_age_secs, status }`
  - `EntitySummary { observation_mode, cadence_tier, headline, static_policy, dynamic_signals, drift_detected, drift_summary, last_changed_at }`
- `PitsEntityDetail` 新增 `summary: EntitySummary`（保留 `summary_line: String` 兼容所有既有断言与 API 消费者）。
- `ResourceTypeSummary` 新增：
  - `observation_mode: String`
  - `cadence_tier: String`
  - `policy_compliant_count: usize`
  - `drifted_count: usize`
  - `highlights: Vec<SummaryPill>`

### Step 3：双轨 Summary 构建器 + 内存 RingBuffer Delta & 分层刷新引擎
- **文件**：新增 `client/src/catalog/summary.rs`，接入 `client/src/catalog/mod.rs`、`client/src/catalog/synthesizer.rs`、`client/src/catalog/discovery.rs`
- **Phase 2 `TelemetryHistory` RingBuffer**（在 `CatalogRegistry` 中保留最近 20 个采样点）：
  - VPP 累计计数 (`rxBytes`, `txBytes`, `rxPackets`, `txPackets`, `drops`, `errors`) → 计算实时 **`Rx/Tx Mbps`、`pps`、`Δdrops`、`Δerrors`**。
  - Btrfs `usedBytes` 与 5 类 sysfs 错误计数 → 计算 **`Δused` 与 `Δerrors`**。
  - 集群节点/Pod/GPU 占用数 → 计算 **`Δnodes`、`Δpods`、`Δgpu_used`**。
  - VM 宿主机/Phase、CI Flux revision、Sourcing `prodDependents` → 记录 `last_changed_at` 与漂移标记。
- **分层刷新策略**：
  - `FAST_CACHE_TTL = 15s`：仅快速刷新 Router/VPP、Core/Lab/Extra K8s（含 DRA Claims/Pods/VMs/CICD）与 Btrfs，复用已缓存的 Wave-2 Talos COSI / Hook，避免压垮 `pits-agent`。
  - `STANDARD_CACHE_TTL = 60s`：执行全量刷新（含 Wave-2 Talos COSI）。
- **16 项 Resource Type 逐项双轨摘要实现**（确保每项都同时产出 `static_policy`、`dynamic_signals`、`drift_detected` 与类型目录 `highlights`）：
  1. `baremetal-host`（`hybrid`）：Policy(`mgmt0`/`netboot-guard`/VFIO/契约WWID) + Dynamic(K8s Ready、Talos Services `ok/tot`、COSI WWID 对齐、BootMode 漂移)
  2. `npu-accelerator`（`hybrid`）：Policy(PCI/CPU匹配、期望驱动、DeviceClass) + Dynamic(驱动绑定、DRA Slice、DRA Claim 占用率 %)
  3. `gpu-accelerator`（`dynamic-first`）：Policy(型号/VRAM/PCI、DRA Driver) + Dynamic(**DRA Claim / DevicePlugin 占用率 %**、消费 Pod、DRA vs DevicePlugin 漂移)
  4. `virtual-machine`（`dynamic-first`）：修复 `win11-001` 硬编码宿主机问题；Policy(vCPU/RAM/UEFI+vTPM/Self-Session Lock) + Dynamic(**实际宿主机与迁移漂移**、VMI Phase、ISO 挂载漂移、RDP TLS 验证态)
  5. `edge-router`（`dynamic-first`）：Policy(VLAN 10↔20、Lab→Mgmt、mgmt0 Guard、SHA256) + Dynamic(**VPP Freshness Age ≤30s**、Crossplane Paused、Link Up/Total、**GoVPP Rx/Tx 速率 + Drops/Errors 及 Δ**)
  6. `pxe-boot-profile`（`policy-first`）：Policy(selinux/sig_enforce/禁写盘/OUI) + Dynamic(SHA-256 核验比、当前 Hook 模式节点数)
  7. `storage-pool`（`dynamic-first`）：Policy(Btrfs UUID/Subvols 或 Mayastor SPDK/Node) + Dynamic(水位 %、**容量 Δ**、I/O 错误与 **Δ**、Mayastor alert、TRIM/Scrub 新鲜度)
  8. `san-volume`（`hybrid`）：Policy(NSID/UUIDv5 WWID/Host ACL/扩容保护) + Dynamic(configfs Enabled、NOCOW +C、COSI 挂载确认、容量漂移、4KiB GPT)
  9. `ai-storage-workspace`（`hybrid`）：Policy(Env/Domain/Zero-Trust AuthMode/8-Archetype 契约) + Dynamic(Crossplane Synced/Ready、就绪 Archetype 比 `N/8`、MCP 端点态)
  10. `kubernetes-cluster`（`dynamic-first`）：Policy(角色/隧道/Etcd/DeviceClasses) + Dynamic(API 可达性、Ready/Total Nodes、Running/Total Pods、Bound PVs、**DRA GPU/NPU 占用率 %** 及 Δ)
  11. `cicd-pipeline`（`dynamic-first`）：Policy(6-Workflow/Base隔离/`netboot-004` 排除红线) + Dynamic(`netboot-004` 零违规检测、Gitea/CNPG/Zot/Attic 四联灯、Flux revision 变化、CPU/NPU/GPU Runner 就绪比)
  12. `auth-flow-bundle`（`policy-first`）：Policy(Realm/FlowAlias/BrowserFlow Binding/LoA-2 FIDO2) + Dynamic(**`activeFlowAlias` vs `spec.flowAlias` 漂移检测**、Crossplane Ready)
  13. `vendor-saas`（`governance-event`）：Policy(Quadrant/Knockout/FinOps/DomainTier) + Dynamic(Windmill Mismatch、`daysToRenewal` 倒计时、`prodDeps/baseline`、Re-Audit)
  14. `commercial-sdk`（`governance-event`）：Policy(突出 **Adapter 隔离红线 X1** + X4 合规) + Dynamic(依赖组件膨胀 Δ、盲评离散度、续约倒计时、Re-Audit)
  15. `managed-infra`（`governance-event`）：Policy(4 层域血缘等级 + 钱包份额) + Dynamic(上游组件依赖变更、Windmill 错配、临界区 `[2.70–3.30]`、续约窗口)
  16. `outsourced-dev`（`governance-event`）：Policy(替代弹性 Y4/X2 + 定制深度 X3) + Dynamic(盲评人数与分歧度 `maxBallotSpread`、依赖变更、Re-Audit)

### Step 4：CLI (`pitsctl`) 呈现升级
- **文件**：`client/src/main.rs`
- `pitsctl types`：输出 `RESOURCE TYPE | MODE | ENTITIES | HEALTH | DRIFT | HIGHLIGHTS`（`-o wide` 保留 `CATEGORY` 与 `ACTIONS`）。
- `pitsctl get <type>`：增加 `--view <auto|policy|live|drift>`。
- `pitsctl inspect <entity>`：输出 `Observation Mode`、`Drift`、`Static Policy`、`Dynamic Signals (Value / Δ / Age)`。

### Step 5：Backstage Portal UI 升级
- **文件**：`portal/src/types.ts`、`portal/src/pages/ResourceTypeDirectoryPage.tsx`、`portal/src/components/EntityOverviewCards.tsx`
- `ResourceTypeDirectoryPage`：16 张类型卡片展示 `ObservationMode` 标签、`Drifted` 徽章与专属 `highlights` 指标胶囊条。
- `EntityStatusSummaryCard`：升级为 **Static Policy & Contract Band** + **Live Dynamic Signals & Occupancy Band** + Drift Alert 横幅（如有漂移）。

### Step 6：全量回归验证与测试
- `Infra/server`：`go test ./...` + `GOOS=linux GOARCH=amd64 go build ./...`（补充 `clusters` DRA Claim & GPU 占用率单测）。
- `Infra/client`：`cargo test`（补充 16 项 `EntitySummary`、VPP 速率/丢包 Δ、DRA Claim GPU 占用率 %、RingBuffer `last_changed_at` 与 `pitsctl get --view` 单测）。
- `Infra/portal`：`npm run typecheck` / `npm run build`（或对应 vite build 命令）验证前端类型与打包零错误。
