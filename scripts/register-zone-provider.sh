#!/usr/bin/env bash
# 实验区控制面起来后：
#   1. 把这个区的集群注册为 ManageIQ provider（Kubernetes + KubeVirt 端点）；
#   2. 把只读 kubeconfig 写进 Secret steampipe-zone-kubeconfigs，Steampipe 重建后就有这个区的连接。
#
#   MIQ_URL=https://manageiq.<域名> MIQ_USER=admin MIQ_PASSWORD=... \
#     scripts/register-zone-provider.sh <区名> <控制面地址> <token 文件> [只读 kubeconfig 文件]
#
# 注意：第 1 步正是 P4 关卡要验证的内容。ManageIQ 能否在原生 K8s 上接管 KubeVirt 还没有确认，
# 下面的请求体按 ManageIQ REST API 的 provider 格式写，关卡验证时以实际返回为准再调整。
set -euo pipefail

[ $# -ge 3 ] || { sed -n '2,10p' "$0"; exit 2; }
zone=$1 host=$2 token_file=$3 ro_kubeconfig=${4:-}
: "${MIQ_URL:?需要 MIQ_URL}" "${MIQ_USER:?需要 MIQ_USER}" "${MIQ_PASSWORD:?需要 MIQ_PASSWORD}"

root=$(cd "$(dirname "$0")/.." && pwd)
jq -e --arg z "$zone" '.zones[] | select(.name == $z)' "$root/gitops/zones/registry/zones.json" >/dev/null ||
  { echo "区 $zone 不在区清单里，先跑 scripts/new-zone.sh" >&2; exit 1; }

token=$(cat "$token_file")
body=$(jq -n --arg name "zone-$zone" --arg host "$host" --arg token "$token" '{
  type: "ManageIQ::Providers::Kubernetes::ContainerManager",
  name: $name,
  connection_configurations: [
    { endpoint: { role: "default", hostname: $host, port: 6443, security_protocol: "ssl-without-validation" },
      authentication: { role: "bearer", auth_key: $token } },
    { endpoint: { role: "kubevirt", hostname: $host, port: 6443, security_protocol: "ssl-without-validation" },
      authentication: { role: "kubevirt", auth_key: $token } }
  ]
}')

echo "注册 ManageIQ provider zone-$zone ..."
curl -fsS -u "$MIQ_USER:$MIQ_PASSWORD" -H 'Content-Type: application/json' \
  -X POST "$MIQ_URL/api/providers" -d "$body" | jq .

if [ -n "$ro_kubeconfig" ]; then
  echo "写入 Steampipe 的区连接 kubeconfig ..."
  # 合并进已有的 Secret：每个区一个键，键名就是区名。
  kubectl -n pits-compliance get secret steampipe-zone-kubeconfigs -o json 2>/dev/null |
    jq --arg z "$zone" --arg v "$(base64 -w0 < "$ro_kubeconfig")" '.data[$z] = $v | del(.metadata.resourceVersion, .metadata.uid, .metadata.creationTimestamp, .metadata.managedFields)' |
    kubectl apply -f - 2>/dev/null ||
    kubectl -n pits-compliance create secret generic steampipe-zone-kubeconfigs --from-file="$zone=$ro_kubeconfig"
  kubectl -n pits-compliance rollout restart deployment/steampipe
else
  echo "没有给只读 kubeconfig：Steampipe 暂时不会有这个区的连接，合规基准会因此报警。"
fi
