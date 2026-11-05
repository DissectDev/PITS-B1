# 合规检查

ManageIQ 管申请时的规则（审批、配额），Kyverno 管创建时的拦截，Powerpipe 管运行中的检查，三者不重叠。

## 组成

| 部分 | 位置 | 作用 |
| --- | --- | --- |
| 区清单 | `gitops/zones/registry/zones.json` | 唯一来源。Steampipe 连接、ManageIQ provider、各区控制面都从它来 |
| Steampipe | `gitops/management/compliance/steampipe.yaml` | 启动时按区清单生成连接：`k8s_core`、每区一个 `k8s_zone_<区名>`、聚合连接 `all_k8s` |
| Powerpipe 基准 | `gitops/management/compliance/mod/` | 六项检查，见下表 |
| 定时任务 | CronJob `pits-benchmark`，每 2 小时 | 跑基准；打开写回后把结果写回 ManageIQ |
| 告警 | `gitops/management/services/alerts/alerts.yaml` | 基准超过 4 小时没成功、任务失败、备份过期 |

## 基准里的检查

| 检查 | 不通过时的含义 |
| --- | --- |
| `namespace_fault_domain` | 有命名空间没标故障域或层级 |
| `vm_fault_domain` | 有 VM 没标故障域 |
| `pvc_fault_domain` | 有卷没标故障域 |
| `no_core_in_zones` | 标为 core 的工作负载跑到了分支集群里 |
| `zone_connections_match_registry` | Steampipe 的区连接数和区清单对不上：某个区少了 kubeconfig，查询会悄悄少一块 |
| `etcd_snapshot_recent` | 核心集群 24 小时内没有 etcd 快照 |

## 打开写回（合规检查部署、P4 关卡通过后）

1. 在 ManageIQ 里建标签分类和标签（也可以在界面里建）：

   ```sh
   curl -k -u admin:<密码> -H 'Content-Type: application/json' -X POST \
     https://manageiq.<域名>/api/categories \
     -d '{"name":"pits_compliance","description":"PITS 合规","single_value":true,"show":true}'
   # 记下返回的 id，再加标签：
   curl -k -u admin:<密码> -H 'Content-Type: application/json' -X POST \
     https://manageiq.<域名>/api/categories/<id>/tags \
     -d '{"name":"noncompliant","description":"不合规"}'
   ```

2. 建一个只有打标签权限的 ManageIQ 账号，把账号密码放进 Secret `pits-compliance/pits-miq-api`。
3. 把 `gitops/management/compliance/powerpipe.yaml` 里的 `MIQ_WRITEBACK` 改成 `"true"`，提交。

之后在 ManageIQ 里就能按“不合规”筛选 VM。
