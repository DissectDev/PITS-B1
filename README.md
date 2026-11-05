# PITS-B1

PITS 核心机（MS-01）的基础设施代码，按[《PITS 核心机实施规划》](https://claude.ai/artifact/KVQQAv1pwzuTFeb2wTM5Bo)实现。一个仓库里有两部分：

- **NixOS flake**：宿主（管理域），只管管理网，把数据盘和其余网卡直通给两台核心节点 VM。
- **GitOps 树**：核心 RKE2 集群里的一切，由 Flux 从 `gitops/clusters/core` 开始部署。

## 架构一句话

核心机分四层，依赖只向下：宿主 NixOS → 核心 RKE2 集群（KubeVirt、local-path）→ 管理服务（ManageIQ、Keycloak、zot、监控、合规）→ 网络节点、存储节点和各实验区的控制面 VM。宿主层只依赖上游光猫，RKE2 起不来时仍能远程管理。

## 目录

| 路径 | 内容 |
| --- | --- |
| `flake.nix`、`hosts/core/` | 核心机的 NixOS 配置入口、硬件和系统盘分区 |
| `modules/` | 宿主模块：基础、网络（mgmt0 和 br-core）、VFIO 直通、Tailscale、UPS、RKE2 |
| `site/core.nix` | 宿主的站点参数，所有因机器而异的值只在这里填 |
| `gitops/clusters/core/` | Flux 入口：外部来源、部署顺序、GitOps 侧的站点参数 |
| `gitops/infrastructure/` | 命名空间、集群控制器（cert-manager、Kyverno、CNPG、KubeVirt、CDI）、KubeVirt 配置、br-core、内部 CA |
| `gitops/policies/` | 故障域标签策略 |
| `gitops/core-nodes/` | 网络节点和存储节点 VM |
| `gitops/management/` | 管理服务：`platform`（监控、zot、Velero）、`services`（Keycloak、ManageIQ、启动镜像、告警）、`compliance`（Steampipe、Powerpipe） |
| `gitops/zones/` | 区清单（唯一来源）、实验区控制面模板、各区目录 |
| `scripts/` | P0 硬件盘点、P1 直通验收、新建实验区、注册区 provider |
| `docs/` | 分阶段实施步骤、硬件盘点记录、机密清单、合规检查说明 |

## 站点参数

因环境而异的值集中在三处，填完才能部署：

1. `site/core.nix`：系统盘、管理口和直通设备的 PCI 地址、SSH 公钥、CPU 划分、RKE2 版本、etcd 机外 S3。还有占位值时，评估会直接失败。
2. `gitops/clusters/core/site-vars.yaml`：域名后缀、存储节点和机外的 S3 地址。
3. `gitops/infrastructure/config/kubevirt.yaml`：数据盘的 PCI vendor:device ID。

机密（密码、token、S3 密钥）一律不进 Git，清单见 [docs/secrets.md](docs/secrets.md)。

## 实施阶段

| 阶段 | 做什么 | 相关路径 |
| --- | --- | --- |
| P0 准备（关卡） | 盘点硬件、检查 IOMMU 分组、填站点参数 | `scripts/p0-inventory.sh`、`site/core.nix` |
| P1 宿主 | nixos-anywhere 安装、直通验收 | `hosts/core/`、`modules/`、`scripts/check-passthrough.sh` |
| P2 核心集群 | RKE2 起来后 `flux bootstrap`，etcd 快照推到机外 | `modules/rke2.nix`、`gitops/clusters/core/` |
| P3 虚拟化与核心节点 | 填镜像地址和数据盘 ID，打开 `core-nodes` | `gitops/infrastructure/config/`、`gitops/core-nodes/` |
| P4 管理与治理（关卡） | 验证 ManageIQ 能否管 RKE2 上的 KubeVirt；合规基准 | `gitops/management/` |
| P5 核心节点内部与备份 | 两台节点按 PITS Network / Storage 配置；Velero 两处备份；恢复演练 | `gitops/management/platform/backup.yaml` |
| P6 分支支撑 | 新建实验区、注册 provider、分支网络启动 | `gitops/zones/`、`scripts/new-zone.sh` |

每一步的命令和验收标准见 [docs/bootstrap.md](docs/bootstrap.md)。

## 本地检查

用示例参数评估宿主配置（不需要真机，不会部署）：

```sh
nix eval .#nixosConfigurations.ci-core.config.system.build.toplevel.drvPath
```

`site/core.nix` 填完之前，`nixosConfigurations.core` 会因断言失败而无法评估，`nix flake check` 也会因此失败。这是有意的：它提醒你 P0 还没做完。

## 还没确认的地方

下面这些是按文档写的，需要在对应阶段实测：

- **P4：** ManageIQ 在原生 K8s 上接管 KubeVirt 的方式。见 `scripts/register-zone-provider.sh`，它正是关卡要验证的那一步。
- **P4：** Steampipe 和 Powerpipe 的镜像地址，以及 Steampipe 自省表的名字（`steampipe_internal.steampipe_connection`）。
- **P4：** ManageIQ operator 的镜像 tag `spassky-1`。
- **P4：** zot 按需同步的 `destination` 写法。
- **P3：** 两台核心节点 VM 能否真的分到 P 核（CPU Manager 不区分 P 核和 E 核）。
- **P3：** AMT 在 I226-LM 直通后是否可用。
