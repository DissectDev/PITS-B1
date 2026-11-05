# 机密清单

机密一律不进 Git，也不进 Nix store（store 对所有用户可读）。下表是全部需要手动准备的机密。

| 机密 | 放在哪 | 什么时候建 | 用途 |
| --- | --- | --- | --- |
| 管理员密码哈希 | 宿主 `/var/lib/pits/secrets/admin.hash` | P1 装机时随 `--extra-files` 放入 | 控制台登录、sudo。生成：`mkpasswd -m yescrypt` |
| Tailscale auth key | 宿主 `/var/lib/pits/secrets/tailscale-authkey` | P1 | 一次性、带 `tag:pits-core`、短有效期 |
| NUT 密码 | 宿主 `/var/lib/pits/secrets/nut-upsmon` | 有 UPS 时 | upsmon 连 upsd |
| etcd 机外 S3 密钥 | 宿主 `/etc/rancher/rke2/config.yaml.d/90-secrets.yaml` | P2 | 见下方示例 |
| Steampipe 数据库密码 | Secret `pits-compliance/steampipe-db`（键 `password`） | P4 | Steampipe 服务和 Powerpipe |
| ManageIQ API 账号 | Secret `pits-compliance/pits-miq-api`（键 `user`、`password`） | P4 打开写回时 | 合规结果写回 ManageIQ 标签，只给打标签的权限 |
| ManageIQ OIDC 客户端 | Secret `manageiq/manageiq-oidc`（键 `CLIENT_ID`、`CLIENT_SECRET`） | P4 切到 Keycloak 时 | ManageIQ 统一登录 |
| Velero 存储节点 S3 | Secret `velero/velero-s3-storage`（键 `cloud`） | P5 | 备份到存储节点 |
| Velero 机外 S3 | Secret `velero/velero-s3-offsite`（键 `cloud`） | P5 | 备份到机外 |
| 实验区控制面 cloud-init | Secret `zones/zone-<区名>-cp-cloudinit`（键 `userdata`） | P6 每建一个区 | 含该区的 rke2 token |
| 实验区只读 kubeconfig | Secret `pits-compliance/steampipe-zone-kubeconfigs`（每区一个键） | P6 | Steampipe 的区连接，由 `scripts/register-zone-provider.sh` 写入 |

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
