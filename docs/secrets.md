# 机密清单

机密一律不进 Git，也不进 Nix store（store 对所有用户可读）。下表是全部需要手动准备的机密。

| 机密 | 放在哪 | 什么时候建 | 用途 |
| --- | --- | --- | --- |
| 管理员密码哈希 | 宿主 `/var/lib/pits/secrets/admin.hash` | P1：就地 rebuild 前手动放好；重装时随 `--extra-files` 放入 | 控制台登录、sudo。生成：`mkpasswd -m yescrypt` |
| Tailscale auth key | 宿主 `/var/lib/pits/secrets/tailscale-authkey` | 只在用 nixos-anywhere 重装时需要 | 一次性、带 `tag:pits-core`、短有效期。就地 rebuild 时沿用已有的登录状态 |
| 代理订阅配置 | 宿主 `/var/lib/pits/secrets/mihomo.yaml` | P2 | mihomo 的配置（Clash/mihomo 格式），含代理节点的凭据。服务商给的完整配置可以直接放；只有订阅链接时，用下方的最小配置 |
| NUT 密码 | 宿主 `/var/lib/pits/secrets/nut-upsmon` | 有 UPS 时 | upsmon 连 upsd |
| etcd 机外 S3 密钥 | 宿主 `/etc/rancher/rke2/config.yaml.d/90-secrets.yaml` | 配置机外 S3 时（目前未配置） | 见下方示例 |
| Steampipe 数据库密码 | Secret `pits-compliance/steampipe-db`（键 `password`） | 打开合规检查时（后置） | Steampipe 服务和 Powerpipe |
| ManageIQ API 账号 | Secret `pits-compliance/pits-miq-api`（键 `user`、`password`） | 打开写回时 | 合规结果写回 ManageIQ 标签，只给打标签的权限 |
| ManageIQ OIDC 客户端 | Secret `manageiq/manageiq-oidc`（键 `CLIENT_ID`、`CLIENT_SECRET`） | P3 切到 Keycloak 时 | ManageIQ 统一登录 |
| Velero 存储节点 S3 | Secret `velero/velero-s3-storage`（键 `cloud`） | P4 | 备份到存储节点 |
| Velero 机外 S3 | Secret `velero/velero-s3-offsite`（键 `cloud`） | 配置机外 S3 时（目前未配置） | 备份到机外 |
| 实验区控制面 cloud-init | Secret `zones/zone-<区名>-cp-cloudinit`（键 `userdata`） | P5 每建一个区 | 含该区的 rke2 token |
| 实验区只读 kubeconfig | Secret `pits-compliance/steampipe-zone-kubeconfigs`（每区一个键） | P5 | Steampipe 的区连接，由 `scripts/register-zone-provider.sh` 写入 |

`90-secrets.yaml` 示例（RKE2 会合并读取 `config.yaml.d/` 下的文件）：

```yaml
etcd-s3-access-key: "..."
etcd-s3-secret-key: "..."
```

Velero 的 `cloud` 文件是 AWS credentials 格式：

```ini
[default]
aws_access_key_id = ...
aws_secret_access_key = ...
```

`mihomo.yaml` 可以直接用服务商给的完整配置（Clash 客户端里用的那份）。不用自己改端口、监听地址、模式和规则：每次启动前 `pits-mihomo-config` 会改写这些项，见 `modules/proxy.nix`：
- 只留 7890 一个端口，只允许本机和核心集群的 Pod 使用；
- 控制接口只监听 127.0.0.1:9090；
- 关掉 TUN 和 DNS 监听；
- 所有流量都走一个自动选延迟最低节点的 `PITS` 组。

只有订阅链接时，写这几行就够了：

```yaml
proxy-providers:
  sub:
    type: http
    url: "https://<你的订阅链接>"
    interval: 86400
    path: ./providers/sub.yaml
```

改了这个文件后执行 `sudo systemctl restart mihomo`。
