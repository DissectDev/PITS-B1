# OpenDAL AI Storage 五层架构与 Crossplane 配置聚合设计方案

> **适用范围**：`cluster-pits-lab` 计算集群（`netboot-001..005`，VLAN 20）及 PITS 多 Agent Harness（Antigravity / Claude Code / Codex / `test-pool`）  
> **关联文档**：  
> - [lab-cicd-crossplane-design.md](./lab-cicd-crossplane-design.md)（Lab 节点、Mayastor/tmpfs 存储分层与 Crossplane / Flux 边界）  
> - [tailscale-keycloak-iam-crossplane-design.md](./tailscale-keycloak-iam-crossplane-design.md)（Keycloak + Tailscale 零信任 IAM 与 Crossplane 台账规范）  
> - [cluster-lab-istio-ambient-keycloak-plan.md](./cluster-lab-istio-ambient-keycloak-plan.md)（Istio Ambient + Gateway `ext_authz` / JWT 鉴权入口）  
> **核心原则**：**Crossplane 做“基础设施与配置聚合控制面”、OpenDAL 做“统一数据面”、AI Storage Archetypes 做“语义 VFS 引擎”、Project Profile 做“声明式策略契约”、MCP Server 做“跨 Harness 通用 Producer”**。

---

## 1. 设计目标与解决的核心问题

1. **避免与单一 Agent Harness 生命周期耦合**：
   - Antigravity 原生 `sidecar.json`、`hooks.json` 与其他 Harness（Claude Code、Codex、Cursor、OpenCode）不互通；若由单个会话的 Stdio MCP 直接挂载 FUSE（`ofs`）或持有 SQLite 锁，多会话并发或多 Harness 协同时必然冲突。
   - **解法**：存储后端与语义引擎作为独立常驻服务（集群 Deployment 或本地 Singleton Daemon）运行，通过 **Streamable HTTP MCP（`/mcp`）** 与极薄 Stdio 入口对外暴露能力；**MCP Server 作为配置与能力的 Producer，各家 Plugin（`SKILL.md` + `AGENTS.md` + 极薄接入声明）仅作为纯 Consumer**。
2. **填平 OpenDAL 到 AI 存储语义之间的鸿沟**：
   - `mcp-server-opendal` 仅暴露了裸的 `read` / `write` / `list`，缺乏快照回滚、全文/向量索引、Append-only 血缘、分布式协同锁与权限隔离。
   - **解法**：在 OpenDAL 之上建立完整的 **AI Storage Archetypes（8 大语义存储原语）** 与 **Project Profile** 策略层。
3. **利用 Crossplane 实现“配置即交付（Configuration Offering）”**：
   - 在 `cluster-pits-lab` 复用现有 Crossplane 栈（`provider-helm`、`provider-kubernetes`、`provider-keycloak`、`function-go-templating`、`function-auto-ready`），将**物理存储供给 + 索引库初始化 + 凭证签发 + `ProjectProfile` 聚合 + MCP Server 发布**收敛为单一声明式 CR（`AIStorageWorkspace`）。

---

## 2. 五层总体架构

```mermaid
flowchart TB
    subgraph CONSUMERS["Agent Harness Consumers (极薄插件/适配壳, 零后端逻辑)"]
        AGY["Antigravity Plugin<br/>(mcp_config.json + SKILL.md)"]
        CC["Claude Code / Codex / Cursor<br/>(.mcp.json / config.toml + AGENTS.md)"]
        TP["pits-test-pool / E2B / Daytona / Modal<br/>(远端沙盒共享读写)"]
    end

    subgraph L5["Layer 5: MCP Server (能力与动态配置 Producer)"]
        MCP_HTTP["Streamable HTTP (/mcp) + 极薄 Stdio Proxy (ai-store mcp)<br/>Resources (profile://mounts) · Tools (vfs_*) · Prompts"]
    end

    subgraph L4["Layer 4: Project Profile (声明式配置与策略引擎)"]
        PROF["ConfigMap: project-profile.yaml + Secret: opendal-creds<br/>Backend Registry · Archetype 路由 · Policy Guard · 热重载"]
    end

    subgraph L3["Layer 3: AI Storage Archetypes (语义 VFS 引擎)"]
        ROUTER["VFS Router + Policy Guard + 元数据/索引协调面"]
        A1["ws:// Workspace<br/>(Snapshot/Diff/Rollback)"]
        A2["mem:// Memory<br/>(分层记忆 + TTL + Vector)"]
        A3["kb:// Knowledge<br/>(glob/grep/trigram/RAG)"]
        A4["data:// & models://<br/>(Byte-Range + NVMe Cache)"]
        A5["artifacts:// Artifacts<br/>(Content-addressed/血缘)"]
        A6["traces:// Traces<br/>(流式追加 + Parquet 归档)"]
        A7["state:// State<br/>(CAS 锁/Lease/队列)"]
        A8["shared:// Shared Drive<br/>(跨沙盒并发读写)"]
        ROUTER --> A1 & A2 & A3 & A4 & A5 & A6 & A7 & A8
    end

    subgraph L2["Layer 2: Apache OpenDAL (纯数据访问面)"]
        OP["Operator + Layer 中间件栈<br/>(Retry · Timeout · ConcurrencyLimit · Metrics · ofs FUSE / opendalfs)"]
    end

    subgraph L1["Layer 1: Cloud & Lab Storage 底座 (cluster-pits-lab + 多云)"]
        BLOB["对象/分布式存储: RustFS / S3 / R2 / Tigris / Hugging Face / PVC (mayastor-2, openebs-single-replica, tmpfs)"]
        IDX["索引与状态库: CNPG Postgres (pgvector + trigram) · SQLite · Redis"]
    end

    subgraph XP["Crossplane v1 Pipeline 控制面 (配置聚合与编排, 严格排除 netboot-004)"]
        XRD["XRD: XAIStorageWorkspace / AIStorageWorkspace"]
        ENV["ConfigMap: ai-storage-env-presets<br/>(环境基座预设)"]
        COMP["Composition (Pipeline):<br/>function-go-templating + function-auto-ready"]
        XRD --> COMP
        ENV --> COMP
        COMP -->|1. 调谐底座| BLOB & IDX
        COMP -->|2. 聚合配置| PROF
        COMP -->|3. 发布服务与网关| MCP_HTTP
    end

    CONSUMERS -->|HTTPS JWT / OIDC (Istio Gateway) 或本地 Socket| MCP_HTTP
    MCP_HTTP --> PROF --> ROUTER --> OP --> BLOB
    ROUTER --> IDX
```

---

## 3. 各层详细规划

### 3.1 Layer 1：物理存储与索引底座（对齐 `cluster-pits-lab` 硬件约束）

沿用 [lab-cicd-crossplane-design.md](./lab-cicd-crossplane-design.md) 的存储分层，严禁将任何工作负载调度至 `netboot-004`（`win11-001` 宿主机）：

| 存储类别 | 底层载体 / StorageClass | 适用数据 |
| :--- | :--- | :--- |
| **高可靠状态卷** | `mayastor-2`（2 副本 SPDK NVMe） | CNPG Postgres（`pgvector` / `pg_trgm` / 血缘元数据）、`workspace` SQLite 状态、RustFS 元数据 |
| **高性能对象/缓存卷** | `openebs-single-replica`（单副本 SPDK NVMe）+ 集群内 **RustFS**（Apache 2.0，S3 兼容） | `artifacts://`、`traces://`、`shared://` 对象块、`datasets://` 本地读穿透缓存 |
| **零磨损内存工作区** | DDR5 `tmpfs`（`emptyDir: Memory`） | 高频临时编译缓存、短命推理/评测暂存 |
| **外部多云与模型源** | AWS S3 / Cloudflare R2 / Tigris / Hugging Face Hub | 跨区归档、外部语料库、模型权重与公开数据集 |

---

### 3.2 Layer 2：Apache OpenDAL 数据访问面

- **核心定位**：纯粹的字节流与对象 I/O 层，**不感知任何 AI 业务语义**。
- **工程结构**：
  - **`BackendRegistry`**：依据 `ProjectProfile` 为每个已声明的物理后端实例化独立的 `opendal::Operator`（Rust 核心，或 Python `opendal.AsyncOperator`）。
  - **统一中间件栈（`Layer` 责任链）**：统一注入 `RetryLayer`（指数退避）、`TimeoutLayer`、`ConcurrentLimitLayer`、`LoggingLayer` 与 `PrometheusMetricsLayer`。
  - **多接入形态**：
    - 主通道走 OpenDAL 原生 `read` / `write` / `stat` / `list` / `delete` 与 `Byte-Range` 流式读取；
    - 针对需要 POSIX 目录语义的沙盒（如 `test-pool` / 本地容器），按需通过 `ofs`（FUSE）或 `opendalfs`（`fsspec`）挂载只读数据集或共享目录，**挂载生命周期由底层 Daemon/Pod 管理，绝不绑定到 Agent 会话**。

---

### 3.3 Layer 3：AI Storage Archetypes 全谱系设计

第 3 层通过 **VFS Router + Policy Guard** 对外呈现统一的多 URI 语义文件系统，将 **Blob 数据面（OpenDAL）** 与 **Index / Coordination 面（SQLite / CNPG Postgres / Redis）** 协同编排，完整覆盖 **8 大 AI Storage Archetypes**：

| # | Archetype | URI Scheme | 核心能力与语义契约 | 数据面（OpenDAL） | 索引 / 状态面 | 默认 Policy |
| :-: | :--- | :--- | :--- | :--- | :--- | :--- |
| 1 | **Workspace（工作区）** | `ws://` | Agent 可变代码/文件沙盒；支持 **`snapshot` / `diff` / `rollback` / `fork`**（借鉴 AgentFS） | 本地 PVC / `tmpfs` + OpenDAL 异步快照归档至对象存储 | SQLite（事务级目录树、文件哈希与快照指针） | `read-write` |
| 2 | **Memory（长短期记忆）** | `mem://` | 按 `tenant/project/user/session` 分级；支持 **TTL 衰减、层级树导航与语义召回**（借鉴 OpenViking） | OpenDAL（Markdown / JSON 记忆文档） | CNPG `pgvector` + SQLite FTS5 / KV | `read-write`（按 scope 隔离） |
| 3 | **Knowledge（知识库/语料）** | `kb://` | 文档/Wiki/代码库；支持 **`glob`、`grep`（trigram 正则）与混合语义检索**（借鉴 `mcp-virtual-fs`） | OpenDAL（S3 / RustFS / OSS） | CNPG（`pg_trgm` + `pgvector`）或 Tantivy 索引 | `read-only`（仅索引流水线可写） |
| 4 | **Datasets & Models（数据集与权重）** | `data://` `models://` | 大体积 Parquet/Lance/Iceberg 与 `safetensors`；**Byte-Range 切片、零拷贝读穿透缓存** | OpenDAL（`s3` / `huggingface`）+ `openebs-single-replica` NVMe 缓存层 | Manifest / Catalog 元数据（校验和、分片索引） | `read-only` |
| 5 | **Artifacts（产物与检查点）** | `artifacts://` | **内容寻址（SHA256）与不可变版本**；强制记录血缘（`run_id`、`agent_id`、`source_ws`） | OpenDAL（RustFS / S3 / R2） | Postgres 血缘表（Artifact URI ↔ Run 元数据） | `append-only` |
| 6 | **Traces & Audit（轨迹与审计）** | `traces://` | 高吞吐追加记录 LLM Prompt/Tool Call/耗时/合规审计；周期性压缩为列式归档（借鉴 ScopeDB） | OpenDAL（按小时/天滚动写入 Parquet / JSONL） | 内存/Redis 环形缓冲 + 异步批写刷盘 | `append-only` |
| 7 | **State & Coordination（协同状态与锁）** | `state://` | 多 Agent / Swarm 协调：**分布式租约锁（Lease）、状态机 Checkpoint、幂等键、消息板** | OpenDAL（大体积 Checkpoint 周期转储） | Redis / Postgres（强一致 CAS / `ETag` 原子更新） | `read-write`（强制 CAS/Lease） |
| 8 | **Shared Drive（跨沙盒共享盘）** | `shared://` | Antigravity 与外部沙盒（`test-pool` / E2B / Daytona / Modal）并发读写交换大文件（借鉴 Blaxel Agent Drive） | OpenDAL（RustFS / S3 中心桶）+ 可选 FUSE 挂载 | 路径级 Lease 锁 + `ETag` 乐观并发控制 | `read-write`（带租约与配额） |

#### 第 3 层统一接口契约（Router & Guard）
所有请求进入 VFS Router 后严格按 4 步流转：
1. **URI Normalize & Sandbox**：校验 scheme 合法性，规范化路径并阻断 `../` 越权逃逸；
2. **Policy Guard**：核对 `ProjectProfile` 中该 prefix 的模式（`read-only` / `append-only` / `read-write`）、单文件体积上限、MIME/扩展名白名单与 Scope 隔离；
3. **Archetype Handler Dispatch**：调用对应原语引擎（例如 `artifacts://` 写入时自动计算 SHA256 并写血缘表；`ws://` 写入前按需触发轻量增量记录）；
4. **Telemetry Emit**：异步向 `traces://` 投递结构化审计事件。

---

### 3.4 Layer 4：Project Profile（配置与策略契约）

`ProjectProfile` 是 Crossplane 聚合产出的标准配置载体（`ConfigMap` + `Secret`），同时也可作为本地开发时的独立 `.ai-store.yaml` 运行，实现本地与集群行为一致。

```yaml
apiVersion: storage.ai.pits.io/v1alpha1
kind: ProjectProfile
metadata:
  name: pits-agent-space
  namespace: ai-storage-pits
spec:
  backends:
    lab_rustfs:
      scheme: s3
      endpoint: http://rustfs.pits-storage.svc.cluster.local:9000
      bucket: pits-ai-storage
      region: us-east-1
      credentialRef: secret://pits-opendal-creds/rustfs
    hf_public:
      scheme: huggingface
      repo: org/pits-datasets
      credentialRef: secret://pits-opendal-creds/hf
    local_ws:
      scheme: fs
      root: /var/lib/ai-store/ws

  archetypes:
    workspace:
      uri: ws://
      backend: local_ws
      index: sqlite:///var/lib/ai-store/ws-meta.db
      archiveBackend: lab_rustfs
      archivePrefix: /snapshots/pits
    memory:
      uri: mem://
      backend: lab_rustfs
      prefix: /memory/pits
      index: postgres://secret://pits-opendal-creds/pg-dsn?schema=ai_mem
      ttlDays: 90
    knowledge:
      uri: kb://
      backend: lab_rustfs
      prefix: /knowledge/pits
      index: postgres://secret://pits-opendal-creds/pg-dsn?schema=ai_kb
      enableTrigram: true
      enableVector: true
    datasets:
      uri: data://
      backend: hf_public
      cacheDir: /var/cache/ai-store/nvme
      cacheMaxBytes: 53687091200 # 50Gi
    artifacts:
      uri: artifacts://
      backend: lab_rustfs
      prefix: /artifacts/pits
      index: postgres://secret://pits-opendal-creds/pg-dsn?schema=ai_artifacts
      contentAddressed: true
    traces:
      uri: traces://
      backend: lab_rustfs
      prefix: /traces/pits
      flushIntervalSec: 30
      format: parquet
    state:
      uri: state://
      backend: lab_rustfs
      prefix: /state/pits
      coordinator: redis://secret://pits-opendal-creds/redis-url
    sharedDrive:
      uri: shared://
      backend: lab_rustfs
      prefix: /shared/pits
      lockBackend: redis://secret://pits-opendal-creds/redis-url

  policies:
    kb://:
      mode: read-only
    data://:
      mode: read-only
    artifacts://:
      mode: append-only
      maxFileSizeMB: 512
    traces://:
      mode: append-only
    ws://:
      mode: read-write
      maxFileSizeMB: 64
    mem://:
      mode: read-write
    state://:
      mode: read-write
    shared://:
      mode: read-write
      maxFileSizeMB: 1024

  exposure:
    transport: streamable-http
    port: 8787
    path: /mcp
    toolsPreset: full # full | readonly | sandbox-worker
```

---

### 3.5 Layer 5：MCP Server（纯 Producer 与跨 Harness 消费）

- **协议选型**：优先暴露 **Streamable HTTP（`/mcp`）**（直接兼容 Antigravity `mcp_config.json`、`Claude Code`、`Codex`、`Cursor`）；本地单机场景由同一二进制提供 `ai-store mcp --profile ...`（内置 Singleton Daemon 唤醒逻辑）。
- **MCP 三大原语职责划分**：
  1. **Resources（动态配置与导航树）**：
     - `profile://active`：返回当前脱敏后的 `ProjectProfile`（各 URI 前缀、读写模式、文件大小限制）；
     - `mounts://tree`：返回顶层语义挂载结构摘要；
     - `ws://snapshots`：当前工作区可用快照列表。
  2. **Tools（按 Archetype 暴露的高信噪比原语）**：
     - 通用检索与读写：`vfs_stat`、`vfs_list`、`vfs_read(uri, offset, limit)`、`vfs_write`、`vfs_glob`、`vfs_grep`；
     - Workspace：`ws_snapshot`、`ws_diff`、`ws_rollback`、`ws_fork`；
     - Memory / Knowledge：`mem_recall`、`mem_commit`、`kb_search`；
     - Artifacts / Shared / State：`artifact_publish`、`state_cas`、`shared_lock` / `shared_unlock`。
  3. **Prompts（动态工作区契约）**：
     - `storage-workflow-guide`：由服务端依据当前 Profile 实时生成，引导 Agent “检索优先用 `vfs_grep`/`kb_search`，长产物归档至 `artifacts://`，跨沙盒大文件放 `shared://`”。
- **Harness 消费侧（极薄 Consumer）**：
  - **Antigravity Plugin** 仅包含：指向网关 URL 的 `mcp_config.json`（含 Bearer Token `variables`）+ 引导如何使用 8 类 URI 的 `SKILL.md` + `rules/AGENTS.md`；**不再依赖 `sidecar.json` 启动存储后端**。
  - **外部沙盒协同**：当主 Agent 将重计算任务 Delegate 给 `pits-test-pool` / E2B / Daytona / Modal 时，只需把 `shared://` 或 `artifacts://` 的对应只读/受限凭证及 MCP Endpoint 注入远端沙盒即可共享同一数据面。

---

## 4. Crossplane 配置聚合设计（对齐 `Infra/CICD/crossplane/` 体系）

### 4.1 控制器与 Provider 选型

直接复用 `cluster-pits-lab` 已部署的 Crossplane 组件（[providers.yaml](../CICD/crossplane/core/providers.yaml)），保持零新增重型外部依赖：
- `crossplane`（`apiextensions.crossplane.io/v1` XRD + `claimNames` + `mode: Pipeline`）
- `provider-kubernetes`（管理 Namespace、PVC、CNPG `Cluster` / `Database`、RustFS Bucket 初始化 `Job`、聚合 `ConfigMap`/`Secret`、MCP Server `Deployment`/`Service`、Istio `HTTPRoute`/`AuthorizationPolicy`）
- `provider-helm`（按需管理共享 RustFS / Redis 实例）
- `provider-keycloak`（为每个 `AIStorageWorkspace` 签发独立的 OIDC Client，接入 Istio Gateway JWT 鉴权）
- `function-go-templating` + `function-auto-ready`（执行环境预设合并、条件渲染、`ProjectProfile` YAML 聚合与 `netboot-004` 硬反亲和注入）

> **为什么先用 `provider-kubernetes` + 幂等初始化 Job 管理 RustFS/S3 Bucket，而不是引入 `provider-aws`？**  
> 与 [lab-cicd-crossplane-design.md](./lab-cicd-crossplane-design.md) 不单独部署 `provider-gitea` 的决策一致：`cluster-pits-lab` 为离线优先环境，用 Crossplane 托管的幂等 `Job`（调用 S3/RustFS Admin API 创建 Bucket 与受限 IAM Key 并写入 `Secret`）比引入整套 Upjet Cloud Provider 更轻量；若后续接公有云 S3/R2，只需增加一个可选 `provider-aws` 映射分支，上层 `AIStorageWorkspace` XRD 无需变动。

### 4.2 抽象接口（XRD：`XAIStorageWorkspace` / `AIStorageWorkspace`）

规划存放路径：`CICD/crossplane/xrds/xaistorageworkspace-xrd.yaml`

```yaml
apiVersion: apiextensions.crossplane.io/v1
kind: CompositeResourceDefinition
metadata:
  name: xaistorageworkspaces.storage.ai.pits.io
spec:
  group: storage.ai.pits.io
  names:
    kind: XAIStorageWorkspace
    plural: xaistorageworkspaces
  claimNames:
    kind: AIStorageWorkspace
    plural: aistorageworkspaces
  versions:
    - name: v1alpha1
      served: true
      referenceable: true
      schema:
        openAPIV3Schema:
          type: object
          required: [spec]
          properties:
            spec:
              type: object
              required: [projectName, environment]
              properties:
                projectName:
                  type: string
                environment:
                  type: string
                  enum: [lab-local, lab-rustfs, cloud-s3-hybrid]
                  default: lab-rustfs
                excludeNodes:
                  type: array
                  items: { type: string }
                  default: ["netboot-004"]
                domain:
                  type: string
                  default: mcp.lab.pits.home.arpa
                archetypes:
                  type: object
                  properties:
                    workspace:
                      type: object
                      properties:
                        enabled: { type: boolean, default: true }
                        pvcSize: { type: string, default: 20Gi }
                    memory:
                      type: object
                      properties:
                        enabled: { type: boolean, default: true }
                        ttlDays: { type: integer, default: 90 }
                    knowledge:
                      type: object
                      properties:
                        enabled: { type: boolean, default: true }
                        enableVector: { type: boolean, default: true }
                        enableTrigram: { type: boolean, default: true }
                    datasets:
                      type: object
                      properties:
                        enabled: { type: boolean, default: false }
                        hfRepo: { type: string }
                        cacheSize: { type: string, default: 50Gi }
                    artifacts:
                      type: object
                      properties:
                        enabled: { type: boolean, default: true }
                        maxFileSizeMB: { type: integer, default: 512 }
                    traces:
                      type: object
                      properties:
                        enabled: { type: boolean, default: true }
                        format: { type: string, default: parquet }
                    state:
                      type: object
                      properties:
                        enabled: { type: boolean, default: true }
                    sharedDrive:
                      type: object
                      properties:
                        enabled: { type: boolean, default: true }
                mcp:
                  type: object
                  properties:
                    replicas: { type: integer, default: 1 }
                    authMode:
                      type: string
                      enum: [keycloak-jwt, bearer-secret]
                      default: keycloak-jwt
            status:
              type: object
              properties:
                mcpEndpoint: { type: string }
                profileConfigMap: { type: string }
                credentialSecret: { type: string }
                readyArchetypes:
                  type: array
                  items: { type: string }
```

### 4.3 Crossplane Composition Pipeline 聚合流程

规划存放路径：`CICD/crossplane/compositions/aistorage-workspace-composition.yaml`

在 `mode: Pipeline` 下，由 `function-go-templating` 分阶段渲染并聚合以下资源：

1. **Step 1：加载环境基座预设（`EnvironmentPreset`）**
   - 根据 `spec.environment`（`lab-rustfs` / `lab-local` / `cloud-s3-hybrid`）注入默认存储端点（如集群内 RustFS Service URL）、StorageClass（`mayastor-2` / `openebs-single-replica`）、共享 CNPG Postgres / Redis 地址、Zot 镜像地址与 `netboot-004` 排除规则。
2. **Step 2：按需调谐第 1 层基础设施资源**
   - **对象存储 Bucket & IAM 凭据**：通过幂等 `Job` 确保 Bucket（`<project>-ai-storage`）与项目级受限 Key 存在，输出至 `Secret/<project>-opendal-creds`；
   - **索引与状态面**：若启用 `memory` 或 `knowledge` / `artifacts`，创建独立的 CNPG `Database`（自动 `CREATE EXTENSION IF NOT EXISTS vector, pg_trgm;`）或隔离 Schema；若启用 `state` / `sharedDrive`，分配 Redis Prefix 与 ACL；若启用 `workspace` / `datasets`，申请 `mayastor-2` 或 `openebs-single-replica` PVC。
3. **Step 3：核心配置聚合（生成第 4 层 `ProjectProfile`）**
   - 在模板引擎中把 Step 1 的环境端点 + Step 2 的 Bucket/DB/Redis/PVC 信息 + `spec.archetypes` 开关 + 默认安全策略（`kb://` 与 `data://` 强制 `read-only`、`artifacts://` 与 `traces://` 强制 `append-only`），**实时聚合成一份完整的 `project-profile.yaml` 并写入 `ConfigMap/<project>-profile`**。
   - 同时计算配置摘要 `sha256` 注解写入 MCP Server Pod Template，配置变更时触发滚动平滑更新。
4. **Step 4：部署第 5 层 MCP Server 与安全网关暴露**
   - 渲染 `Deployment` + `Service`（挂载 `ConfigMap/<project>-profile`、`Secret/<project>-opendal-creds` 及 PVC，强制 `kubernetes.io/hostname NotIn [netboot-004]`）；
   - 若 `authMode == keycloak-jwt`，通过 `provider-keycloak` 创建对应 Client Scope/Audience，并渲染 Istio `HTTPRoute` + `RequestAuthentication` + `AuthorizationPolicy`（复用 Istio Ambient Gateway）；
   - 由 `function-auto-ready` 确认全链路健康后，将 `status.mcpEndpoint` 回写至 `AIStorageWorkspace`。

---

## 5. 仓库目录规划与演进路线图

### 5.1 建议在 `Infra` 下的落地目录结构

- **服务运行时（Rust 或 Python）**：可独立置于 `Infra/ai-storage/`（或在 `Infra/client/` 下扩充新子 crate）：
  - `core/`：OpenDAL `BackendRegistry` + 中间件 `Layer` + VFS Router + Policy Guard
  - `archetypes/`：8 个语义原语引擎实现（`workspace.rs`, `memory.rs`, `knowledge.rs`, `datasets.rs`, `artifacts.rs`, `traces.rs`, `state.rs`, `shared.rs`）
  - `profile/`：`ProjectProfile` Schema 解析、校验与热重载
  - `mcp/`：Streamable HTTP Server + `ai-store mcp` 薄 Stdio/Daemon CLI
- **Crossplane 配置聚合清单（并入 `Infra/CICD/`）**：
  - `CICD/crossplane/xrds/xaistorageworkspace-xrd.yaml`
  - `CICD/crossplane/compositions/aistorage-workspace-composition.yaml`
  - `CICD/platform/instances/pits-ai-storage.yaml`（通过 Lab Flux CD 自动同步）
- **极薄 Harness Consumer（并入 `Infra/.agents/`）**：
  - `.agents/plugins/pits-ai-storage/plugin.json`
  - `.agents/plugins/pits-ai-storage/mcp_config.json`（仅填 Streamable HTTP URL + Token 变量）
  - `.agents/plugins/pits-ai-storage/skills/ai-storage/SKILL.md`

### 5.2 分阶段实施里程碑

| 阶段 | 目标 | 关键交付物与验收标准 |
| :--- | :--- | :--- |
| **Phase 1：数据面与核心 4 Archetypes 闭环** | 跑通第 2/3/4/5 层核心链路 | 完成 OpenDAL 封装、`ProjectProfile` Loader、Streamable HTTP MCP Server，先落 `workspace`、`knowledge`（FTS5/grep）、`artifacts`、`shared` 4 个原语；本地 `ai-store mcp` 单实例模式可用 |
| **Phase 2：补齐全 8 大 Archetypes** | 完整覆盖 AI 存储全谱系 | 接入 CNPG `pgvector` + `pg_trgm`（补齐 `memory` 与混合 `knowledge`）、`opendal-service-hf` + NVMe 缓存（`datasets`/`models`）、Redis CAS/Lease（`state` 与 `sharedDrive` 锁）、Parquet 滚动归档（`traces`） |
| **Phase 3：Crossplane 配置聚合上线** | 实现 GitOps 一键交付 | 在 `cluster-pits-lab` 上线 `XAIStorageWorkspace` XRD + Pipeline Composition，通过 `kubectl apply` 自动拉起 RustFS 桶/CNPG 库/Profile `ConfigMap`/MCP Server，满足 `netboot-004` CI 门禁 |
| **Phase 4：零信任网关与多 Harness 分发** | 跨团队/跨工具统一消费 | 接入 Istio Ambient Gateway + Keycloak JWT；发布极薄 Antigravity Plugin 与通用 `SKILL.md` / `AGENTS.md`，验证 Antigravity、Claude Code、Codex 与 `pits-test-pool` 并发读写同一工作区 |
