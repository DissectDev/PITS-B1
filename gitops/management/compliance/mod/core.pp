# 资源名统一写成 <Steampipe 连接>/<命名空间>/<名字>，写回 ManageIQ 时取最后一段。
# 查询都显式写 schema：k8s_core 是核心集群，all_k8s 是全部集群的聚合。

benchmark "core" {
  title = "PITS 核心机"
  children = [
    control.namespace_fault_domain,
    control.vm_fault_domain,
    control.pvc_fault_domain,
    control.no_core_in_zones,
    control.zone_connections_match_registry,
    control.etcd_snapshot_recent,
  ]
}

control "namespace_fault_domain" {
  title = "命名空间标注了故障域和层级"
  sql   = <<-EOQ
    select
      sp_connection_name || '/-/' || name as resource,
      case
        when coalesce(labels, '{}'::jsonb) ?& array['pits.io/fault-domain', 'pits.io/tier'] then 'ok'
        else 'alarm'
      end as status,
      case
        when coalesce(labels, '{}'::jsonb) ?& array['pits.io/fault-domain', 'pits.io/tier']
          then name || '：' || (labels ->> 'pits.io/fault-domain') || ' / ' || (labels ->> 'pits.io/tier')
        else name || ' 缺少 pits.io/fault-domain 或 pits.io/tier 标签'
      end as reason
    from
      all_k8s.kubernetes_namespace
    where
      name not in ('default','kube-system','kube-public','kube-node-lease','flux-system','cert-manager','kyverno','cnpg-system','kubevirt','cdi','local-path-storage')
  EOQ
}

control "vm_fault_domain" {
  title = "VM 标注了故障域"
  sql   = <<-EOQ
    select
      sp_connection_name || '/' || namespace || '/' || name as resource,
      case when coalesce(labels, '{}'::jsonb) ? 'pits.io/fault-domain' then 'ok' else 'alarm' end as status,
      case
        when coalesce(labels, '{}'::jsonb) ? 'pits.io/fault-domain'
          then name || '：' || (labels ->> 'pits.io/fault-domain')
        else name || ' 缺少 pits.io/fault-domain 标签'
      end as reason
    from
      all_k8s.kubernetes_virtualmachine
  EOQ
}

control "pvc_fault_domain" {
  title = "PVC 标注了故障域"
  sql   = <<-EOQ
    select
      sp_connection_name || '/' || namespace || '/' || name as resource,
      case when coalesce(labels, '{}'::jsonb) ? 'pits.io/fault-domain' then 'ok' else 'alarm' end as status,
      case
        when coalesce(labels, '{}'::jsonb) ? 'pits.io/fault-domain'
          then name || '：' || (labels ->> 'pits.io/fault-domain')
        else name || ' 缺少 pits.io/fault-domain 标签'
      end as reason
    from
      all_k8s.kubernetes_persistent_volume_claim
    where
      namespace not in ('default','kube-system','kube-public','kube-node-lease','flux-system','cert-manager','kyverno','cnpg-system','kubevirt','cdi','local-path-storage')
  EOQ
}

control "no_core_in_zones" {
  title = "标为 core 的工作负载没有跑在分支集群里"
  sql   = <<-EOQ
    with offenders as (
      select sp_connection_name || '/' || namespace || '/' || name as resource
      from all_k8s.kubernetes_virtualmachine
      where sp_connection_name <> 'k8s_core' and labels ->> 'pits.io/fault-domain' = 'core'
      union
      select sp_connection_name || '/' || namespace || '/' || name
      from all_k8s.kubernetes_pod
      where sp_connection_name <> 'k8s_core' and labels ->> 'pits.io/fault-domain' = 'core'
    )
    select resource, 'alarm' as status, resource || ' 标为 core，却运行在分支集群里' as reason
    from offenders
    union all
    select 'all_k8s/-/zones', 'ok', '分支集群里没有标为 core 的工作负载'
    where not exists (select 1 from offenders)
  EOQ
}

control "zone_connections_match_registry" {
  title = "Steampipe 的区连接数等于区清单里的区数"
  # steampipe_internal.steampipe_connection 是 Steampipe 的自省表；P3 时核对该表在所用版本里的名字。
  sql   = <<-EOQ
    with registry as (
      select coalesce(jsonb_array_length(((data ->> 'zones.json')::jsonb) -> 'zones'), 0) as n
      from k8s_core.kubernetes_config_map
      where namespace = 'pits-compliance' and labels ->> 'pits.io/registry' = 'zones'
    ),
    conns as (
      select count(*) as n
      from steampipe_internal.steampipe_connection
      where name like 'k8s_zone_%'
    )
    select
      'k8s_core/pits-compliance/pits-zones' as resource,
      case when r.n = c.n then 'ok' else 'alarm' end as status,
      format('区清单里有 %s 个区，Steampipe 有 %s 个区连接', coalesce(r.n::text, '（读不到区清单）'), c.n) as reason
    from conns c
    left join registry r on true
  EOQ
}

control "etcd_snapshot_recent" {
  title = "核心集群 24 小时内有 etcd 快照"
  sql   = <<-EOQ
    select
      'k8s_core/-/etcd-snapshots' as resource,
      case when max(creation_timestamp) > now() - interval '24 hours' then 'ok' else 'alarm' end as status,
      '最近一次 etcd 快照：' || coalesce(max(creation_timestamp)::text, '没有') as reason
    from
      k8s_core.kubernetes_etcdsnapshotfile
  EOQ
}
