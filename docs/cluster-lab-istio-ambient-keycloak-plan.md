# `cluster-pits-lab` Istio Ambient + Keycloak（`ext_authz`）零信任网关实施计划

> **适用范围**：`cluster-pits-lab`（Talos Linux `v1.14.5`，`netboot-001..005`，VLAN 20）  
> **关联文档**：  
> - [lab-cicd-crossplane-design.md](./lab-cicd-crossplane-design.md)（Lab 节点、存储与 Crossplane / Flux 边界）  
> - [tailscale-keycloak-iam-crossplane-design.md](./tailscale-keycloak-iam-crossplane-design.md)（Keycloak FIDO2 + Tailscale + 模式 A PEP 设计）  
> **核心决策**：在 `cluster-pits-lab` 部署 **Istio Ambient 模式**，由 **Istio Gateway（托管 Envoy）** 统一承载南北向零信任入口与 Keycloak `ext_authz` 鉴权，不再额外叠加独立 Envoy 网关；网格内部仅按需通过 `ztunnel` / Waypoint 提供无 Sidecar 的 mTLS 与服务级 RBAC。

---

## 1. 设计目标与核心边界

### 1.1 为什么在 `cluster-pits-lab` 选择 Ambient 模式而非 Sidecar 模式
1. **保护 CI/CD 短命与硬件池 Workload**：`cluster-pits-lab` 上运行着高并发短生命周期的 `act_runner`、DDR5 `tmpfs` 构建容器以及 NPU/GPU DRA 测试任务。Sidecar 模式会在每个 Pod 注入 `istio-proxy`，容易带来生命周期退出竞态、内存占用与启动延迟；Ambient 模式将 L4 mTLS 下沉至节点级 `ztunnel`，业务 Pod **零 Sidecar 侵入**。
2. **避免在 Istio 外再维护第二套 Envoy**：Istio Gateway 底层本身就是 Envoy，直接通过 Istio `MeshConfig.extensionProviders`、`AuthorizationPolicy` 与 `RequestAuthentication` 即可驱动原生 Envoy `ext_authz` 与 `jwt_authn` Filter，避免双层网关重复做 TLS 终结与路由维护。
3. **南北向与东西向严格解耦**：
   - **南北向边界（Istio Gateway）**：承接浏览器 OIDC 登录重定向（`ext_authz` -> `oauth2-proxy` -> Keycloak `corp` Realm）、Bearer JWT 校验、`acr=fido2` 强认证门槛，以及后续的 Tailscale Whois 设备合规联合裁决。
   - **东西向网格（`ztunnel` + 按需 Waypoint）**：负责服务间 SPIFFE mTLS 加密传输与细粒度访问控制；CI Runner 命名空间保持默认不启用 L7 Waypoint。

### 1.2 总体流量与鉴权架构

```mermaid
flowchart TB
    Client["用户浏览器 / CLI / Tailscale 终端"] -->|HTTPS / Bearer Token| GW["Istio Ingress Gateway (托管 Envoy, VLAN 20 / 10.89.20.21)"]

    subgraph AUTH["南北向零信任鉴权链 (仅作用于 Gateway)"]
        direction TB
        GW -->|1. 放行 /oauth2/* 回调| OAUTH_SVC["oauth2-proxy (/oauth2/callback)"]
        GW -->|2. CUSTOM AuthorizationPolicy (ext_authz)| OAUTH_EXT["oauth2-proxy (或联合 ext_authz 服务)"]
        OAUTH_EXT <-->|OIDC Code Flow / 会话校验| KC["Keycloak (corp Realm / pits-mode-a-gateway)"]
        OAUTH_EXT -.->|可选阶段二: 验设备与 Posture| TSD["tailscaled Whois API"]
        GW -->|3. RequestAuthentication (jwt_authn)| KC_JWKS["Keycloak JWKS 公钥"]
        GW -->|4. ALLOW AuthorizationPolicy| ACR_CHECK["校验 acr == fido2 & RBAC Groups"]
    end

    subgraph MESH["cluster-pits-lab Ambient 网格 (ztunnel mTLS)"]
        GW -->|mTLS + 透传 Identity Header / JWT| APP_GITEA["Gitea (git.lab.pits.home.arpa)"]
        GW -->|mTLS| APP_PORTAL["Portal / Lab Services"]
        GW -->|mTLS| APP_CACHE["Zot / Attic"]
        RUNNER["act_runner / test-pool (不进 L7 Waypoint)"] -.->|直连缓存| APP_CACHE
    end
```

---

## 2. 与现有 `Infra/CICD` 底座的约束对齐

在 `cluster-pits-lab` 上线 Istio Ambient 必须严格满足现有仓库的四项约束：

1. **`netboot-004` 硬排除护栏（CI 强门禁）**
   - [.github/workflows/cicd.yml](../.github/workflows/cicd.yml) 会校验 `CICD/` 下所有 `Deployment`、`Job`、`DeploymentRuntimeConfig` 是否显式配置 `kubernetes.io/hostname NotIn [netboot-004]` 的 `nodeAffinity`。
   - **实施要求**：`istiod`、Gateway Deployment、`oauth2-proxy`、联合 `ext_authz` 服务及任何初始化 `Job` 均必须显式写入该 `nodeAffinity`（Helm Release 通过 `values` 传入）。同时为 `istio-cni` / `ztunnel` DaemonSet 明确 `netboot-004` 的调度/容忍策略（如仅在非工作站节点运行或确保不影响 `win11-001` 资源）。
2. **Talos Linux PodSecurity 与 CNI 路径**
   - `cluster-pits-lab` 运行 Talos `v1.14.5`（Kubeconfig：`pits-lab-talos-k8s.yaml`）。
   - `istio-system` 与 `ztunnel`（或承载 `istio-cni` 的系统命名空间）必须设置标签 `pod-security.kubernetes.io/enforce: privileged`，否则节点级 CNI 与 `ztunnel` 会被 Talos 默认的受限 PodSecurity 拒绝启动。
   - `istio-cni` 需按 Talos 标准配置 `cniBinDir: /opt/cni/bin`、`cniConfDir: /etc/cni/net.d`，并匹配集群当前底层 CNI（见 Phase 0 核查）。
3. **离线 / 缓存镜像体系**
   - 依据 [Import-CicdImages.ps1](../CICD/scripts/Import-CicdImages.ps1)，集群依赖 `talos-image-cache`（`10.89.20.14:3172`）与 Zot Pull-Through 缓存。
   - 需将 `istio/pilot`、`istio/install-cni`、`istio/ztunnel`、`istio/proxyv2` 及 `quay.io/oauth2-proxy/oauth2-proxy` 纳入离线镜像导入与缓存预热。
4. **Keycloak Client 台账复用与域名对齐**
   - 复用 [CICD/iam/clients.yaml](../CICD/iam/clients.yaml) 中已声明的 `pits-mode-a-gateway`（Credential Secret：`iam-system/mode-a-gateway-oidc-secret`）及 [CICD/iam/realm-corp.yaml](../CICD/iam/realm-corp.yaml) 的 `corp` Realm（`acr.loa.map: '{"fido2":2}'`）。
   - 需补充 `cluster-pits-lab` 对外域名（如 `https://*.lab.pits.home.arpa/oauth2/callback`）到 `validRedirectUris` 与 `webOrigins`。

---

## 3. 目录结构规划

计划在 `Infra/CICD/` 下新增声明式目录，并统一由 [CICD/platform/kustomization.yaml](../CICD/platform/kustomization.yaml) 纳管：

```text
Infra/CICD/
├── iam/                         # [已有] corp Realm, FIDO2 AuthFlow, pits-mode-a-gateway Client
├── mesh/                        # [新增] Istio Ambient 控制面与数据面
│   ├── kustomization.yaml
│   ├── namespaces.yaml          # istio-system / ztunnel (privileged PodSecurity)
│   ├── gateway-api-crds.yaml    # Kubernetes Gateway API 标准 CRD（若集群尚未预装）
│   └── istio-ambient-helm.yaml  # Crossplane Release / Flux HelmRelease (base, istiod, cni, ztunnel)
├── gateway/                     # [新增] 南北向入口网关与 ext_authz 鉴权策略
│   ├── kustomization.yaml
│   ├── oauth2-proxy.yaml        # ext_authz Deployment + Service (排除 netboot-004)
│   ├── gateway.yaml             # Gateway + HTTPRoute (/oauth2/* 放行 + 业务路由)
│   └── security-policies.yaml   # AuthorizationPolicy (CUSTOM / ALLOW) + RequestAuthentication
└── platform/
    └── kustomization.yaml       # [更新] 引入 ../mesh 与 ../gateway
```

---

## 4. 分阶段实施计划

### Phase 0：环境核查与前置准备
**目标**：确认集群底层网络与 Keycloak 连通性，消除盲区后再生成最终 YAML。

- [ ] **核查底层 CNI 与 Gateway API CRD**：
  - 在 `cluster-pits-lab` 查询当前运行的 CNI（Flannel / Cilium / Calico）及是否已存在 `gateway.networking.k8s.io` CRD，决定 `istio-cni` 的 `ambient.enabled` 与 chained 模式参数。
- [ ] **核查 Keycloak 实例位置与网络可达性**：
  - 确认 `cluster-pits-lab` 内 Pod 与工作站各自访问 Keycloak `corp` Realm 的准确 URL（如 `https://sso.martensite.dev/realms/corp` 或内网直连地址）以及是否使用私有 CA（若是，需准备 CA ConfigMap 挂载给 `oauth2-proxy` 与 `istiod`/Envoy）。
- [ ] **准备镜像离线缓存**：
  - 选定 Istio 目标版本（建议支持稳定 Ambient GA 的 `1.24.x`+）与 `oauth2-proxy` 镜像 tag，更新 [Import-CicdImages.ps1](../CICD/scripts/Import-CicdImages.ps1) 完成向 `talos-image-cache` / Zot 的同步。
- [ ] **规划网关 TLS 证书与端口**：
  - 确定 `*.lab.pits.home.arpa` 的 TLS Secret（`cert-manager` 签发或内网 CA 签发）以及是否复用 `10.89.20.21`（MetalLB / kube-vip / NodePort）。

---

### Phase 1：部署 Istio Ambient 底座（`CICD/mesh/`）
**目标**：在 `cluster-pits-lab` 拉起 `istiod`、`istio-cni`、`ztunnel`，并注册 `ext_authz` Provider。

- [ ] 创建 `istio-system` 与 `ztunnel` Namespace，配置 `pod-security.kubernetes.io/enforce: privileged`。
- [ ] 安装 Kubernetes Gateway API CRD（Standard channel）。
- [ ] 通过 Crossplane `provider-helm`（复用 [CICD/crossplane/core/providers.yaml](../CICD/crossplane/core/providers.yaml)）或 Flux `HelmRelease` 部署：
  1. `istio/base`
  2. `istio/istiod`（开启 `profile=ambient`，配置 `meshConfig.extensionProviders`）
  3. `istio/cni`（`profile=ambient`，适配 Talos 路径）
  4. `istio/ztunnel`（`profile=ambient`）
- [ ] 为 `istiod` 及相关 Deployment 显式注入 `kubernetes.io/hostname NotIn [netboot-004]`，并通过 `kubectl get pods -n istio-system` 验证健康状态。

---

### Phase 2：部署 `oauth2-proxy` 与更新 Keycloak Client 台账
**目标**：打通 Keycloak `pits-mode-a-gateway` 与 `ext_authz` 服务端。

- [ ] **扩展 Keycloak Client 回调地址**：
  - 更新 [CICD/iam/clients.yaml](../CICD/iam/clients.yaml)，在 `pits-mode-a-gateway` 的 `validRedirectUris` 与 `webOrigins` 中增加 Lab 网关域名（如 `https://portal.lab.pits.home.arpa/oauth2/callback`、`https://git.lab.pits.home.arpa/oauth2/callback`）。
  - 确认 Keycloak `pits-mode-a-gateway` Client Scope/Mapper 会将 `acr` 声明同时写入 **ID Token 与 Access Token**。
- [ ] **部署 `oauth2-proxy`（`iam-system` 命名空间）**：
  - 挂载 `iam-system/mode-a-gateway-oidc-secret` 中的 `client-id` 与 `client-secret`（并生成 Cookie 加密密钥 `OAUTH2_PROXY_COOKIE_SECRET`）。
  - 配置关键参数：
    - `--provider=keycloak-oidc`
    - `--oidc-issuer-url=<KEYCLOAK_CORP_ISSUER>`
    - `--acr-values=fido2`（确保发起认证请求时要求 FIDO2 强度）
    - `--set-xauthrequest=true`、`--pass-access-token=true`、`--pass-authorization-header=true`
    - `--reverse-proxy=true`、`--skip-provider-button=true`
  - 配置 `kubernetes.io/hostname NotIn [netboot-004]` 亲和性，满足 CI 守卫。

---

### Phase 3：配置 Istio Gateway 与三段式零信任策略（`CICD/gateway/`）
**目标**：让所有经网关进入的浏览器与 API 流量强制经过 Keycloak 鉴权与 FIDO2 强度检验。

- [ ] **创建 Gateway 与 HTTPRoute**：
  - 定义 Gateway API `Gateway`（监听 `443` HTTPS，挂载 `*.lab.pits.home.arpa` TLS 证书；若需保留客户端源 IP 用于后续 Tailscale 校验，Gateway Service 配置 `externalTrafficPolicy: Local`）。
  - 添加最高优先级 `HTTPRoute`：将 `/oauth2/` 前缀直接路由到 `oauth2-proxy` Service（确保登录跳转与 `/oauth2/callback` 可达）。
  - 添加业务 `HTTPRoute`（例如首批接入 Portal / 测试端点，验证稳定后再接入 Gitea Web 等）。
- [ ] **挂载 `CUSTOM` `AuthorizationPolicy`（`ext_authz`）**：
  - 通过 `targetRefs`（Gateway API）或 `selector` 仅绑定到 Gateway Workload。
  - 设置 `action: CUSTOM`，`provider.name: "oauth2-proxy-ext-authz"`。
  - 在 `rules.to.operation.notPaths` 中排除 `/oauth2/*`（以及必要的健康检查路径）。
- [ ] **配置 `RequestAuthentication` + `ALLOW` `AuthorizationPolicy`**：
  - `RequestAuthentication` 配置 Keycloak `corp` Realm `issuer` 与 `jwksUri`，允许网关透传出来的 Token 或 CLI 直接携带的 `Authorization: Bearer ...` 被 Envoy 原生 `jwt_authn` 校验。
  - `AuthorizationPolicy (ALLOW)` 校验 `request.auth.claims["acr"]` 包含 `"fido2"`，并按 `groups` / `realm_access.roles` 做路径与角色映射。

---

### Phase 4：Ambient 东西向纳管与 Tailscale Whois 联合裁决演进
**目标**：完成服务间 mTLS 加固，并平滑对接设计文档中的设备身份校验。

- [ ] **渐进式启用 Ambient L4 mTLS**：
  - 为业务命名空间（如 `iam-system`、业务服务 Namespace）添加标签 `istio.io/dataplane-mode=ambient`，观察 `ztunnel` HBONE 加密隧道连通性。
  - 确认 `act_runner` / `test-pool` 任务仍保持零额外开销，未误引入 L7 Waypoint 代理。
- [ ] **按需引入 Waypoint Envoy（L7 东西向策略）**：
  - 仅在需要跨服务细粒度 HTTP 方法/路径级鉴权的 Namespace 部署 `Gateway`（`gatewayClassName: istio-waypoint`）。
- [ ] **阶段二扩展：Tailscale Whois 联合裁决（可选演进）**：
  - 若需在 `cluster-pits-lab` 网关同时执行设计文档中的“人 + 设备”双因子拦截，可将 `oauth2-proxy` 后的 `ext_authz` 升级为轻量 Go/OPA `envoyExtAuthzGrpc` 适配器：在放行前同时检查 Keycloak Token `acr=fido2` 与本地/节点 `tailscaled` Whois 设备合规定标（Posture）。

---

### Phase 5：CI 门禁、验收测试与应急机制（Break-glass）

#### 5.1 自动化门禁与脚本检查
- [ ] 运行 Lab Gitea CI（[cicd.yml](../.github/workflows/cicd.yml)）验证所有新增 YAML 语法及 `netboot-004` 硬排除规则通过。
- [ ] 扩展 [Verify-CicdStack.ps1](../CICD/scripts/Verify-CicdStack.ps1)，增加 `istio-system` / `ztunnel` / `oauth2-proxy` Pod 就绪检查。

#### 5.2 功能与安全验收矩阵
| 用例 | 预期结果 | 验证重点 |
| :--- | :--- | :--- |
| 未登录浏览器访问受保护域名 | 302 重定向至 Keycloak `corp` 登录页 | `CUSTOM` `ext_authz` 拦截与 `/oauth2/*` 放行无死循环 |
| 仅账密登录（未完成 FIDO2） | Keycloak 流程拦截或网关返回 `403 Forbidden` | `authflow-fido2` 与 `acr == "fido2"` 策略生效 |
| 完成 FIDO2 硬件密钥登录 | 成功回跳 `/oauth2/callback` 并打开后端服务，上游收到 `X-Auth-Request-*` | 全链路 OIDC + Cookie 会话正常 |
| 客户端伪造 `X-Auth-Request-User` 请求头 | 未登录时被拦截；登录后被网关覆盖为真实身份 | Envoy/proxy Header 防伪造覆盖生效 |
| CLI 带无效/过期 `Bearer JWT` 访问 API | `401 Unauthorized` | `RequestAuthentication` JWKS 验签生效 |
| 检查 `netboot-004` 节点调度 | 网关、控制面、`oauth2-proxy` 零 Pod 落入 `netboot-004` | 不抢占 `win11-001` 宿主机资源 |

#### 5.3 应急逃生通道（Break-glass）
- **严禁将 Flux CD 拉取 Gitea（`http://gitea-http.pits-cicd.svc.cluster.local:3000`）的集群内流量或管理员 `kubectl` 控制面依赖挂到这套网关鉴权上**，确保即使 Keycloak 或 Gateway 出现配置错误，仍可通过集群内 Service + `kubectl` / GitOps 推送修复配置，避免自锁。

---

## 5. 核心配置基线参考（供下一阶段直接落地）

> 以下为 `MeshConfig` 与网关策略的核心结构示意，正式实施时将按 Phase 0 核查的真实域名与 `issuer` URL 填充。

```yaml
# 1. istiod MeshConfig 中注册 ext_authz 提供者
meshConfig:
  extensionProviders:
    - name: oauth2-proxy-ext-authz
      envoyExtAuthzHttp:
        service: oauth2-proxy.iam-system.svc.cluster.local
        port: 4180
        failOpen: false
        includeRequestHeadersInCheck:
          - authorization
          - cookie
          - x-forwarded-for
        headersToUpstreamOnAllow:
          - authorization
          - x-auth-request-user
          - x-auth-request-email
          - x-auth-request-preferred-username
        headersToDownstreamOnAllow:
          - set-cookie
        headersToDownstreamOnDeny:
          - content-type
          - set-cookie
```

```yaml
# 2. Gateway 级拦截：CUSTOM ext_authz + JWT 校验 + FIDO2 acr 强断言
apiVersion: security.istio.io/v1
kind: AuthorizationPolicy
metadata:
  name: lab-gateway-ext-authz
  namespace: istio-system
spec:
  targetRefs:
    - kind: Gateway
      group: gateway.networking.k8s.io
      name: lab-zero-trust-gateway
  action: CUSTOM
  provider:
    name: oauth2-proxy-ext-authz
  rules:
    - to:
        - operation:
            notPaths:
              - "/oauth2/*"
---
apiVersion: security.istio.io/v1
kind: RequestAuthentication
metadata:
  name: lab-gateway-keycloak-jwt
  namespace: istio-system
spec:
  targetRefs:
    - kind: Gateway
      group: gateway.networking.k8s.io
      name: lab-zero-trust-gateway
  jwtRules:
    - issuer: "https://<keycloak-host>/realms/corp"
      jwksUri: "https://<keycloak-host>/realms/corp/protocol/openid-connect/certs"
      forwardOriginalToken: true
---
apiVersion: security.istio.io/v1
kind: AuthorizationPolicy
metadata:
  name: lab-gateway-require-fido2
  namespace: istio-system
spec:
  targetRefs:
    - kind: Gateway
      group: gateway.networking.k8s.io
      name: lab-zero-trust-gateway
  action: ALLOW
  rules:
    - to:
        - operation:
            paths:
              - "/oauth2/*"
    - from:
        - source:
            requestPrincipals: ["*"]
      when:
        - key: request.auth.claims[acr]
          values: ["fido2"]
```
