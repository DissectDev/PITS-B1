#!/bin/sh
# 从区清单生成 Steampipe 的 kubernetes 连接配置。
# 输入：/registry/zones.json（ConfigMap pits-zones）、/zone-kubeconfigs/<区名>（Secret）
# 输出：/home/steampipe/.steampipe/config/kubernetes.spc
set -eu

cfg_dir=/home/steampipe/.steampipe/config
out="$cfg_dir/kubernetes.spc"
mkdir -p "$cfg_dir"

tables='["virtualmachines.kubevirt.io", "etcdsnapshotfiles.k3s.cattle.io"]'

{
  echo '# 由 render-connections.sh 生成，不要手改。'
  echo 'connection "k8s_core" {'
  echo '  plugin                 = "kubernetes"'
  echo '  source_types           = ["deployed"]'
  echo "  custom_resource_tables = $tables"
  echo '}'
} > "$out"

missing=0
for zone in $(yq -p json -r '.zones[].name' /registry/zones.json); do
  conn="k8s_zone_$(echo "$zone" | tr '-' '_')"
  if [ -f "/zone-kubeconfigs/$zone" ]; then
    {
      echo "connection \"$conn\" {"
      echo '  plugin                 = "kubernetes"'
      echo "  config_path            = \"/zone-kubeconfigs/$zone\""
      echo '  source_types           = ["deployed"]'
      echo "  custom_resource_tables = $tables"
      echo '}'
    } >> "$out"
  else
    # 不生成连接：合规基准里的“连接数等于区数”会因此报警，而不是悄悄少查一个区。
    echo "render-connections: 区 $zone 还没有 kubeconfig（Secret steampipe-zone-kubeconfigs 里缺键 $zone）" >&2
    missing=$((missing + 1))
  fi
done

{
  echo 'connection "all_k8s" {'
  echo '  plugin      = "kubernetes"'
  echo '  type        = "aggregator"'
  echo '  connections = ["k8s_*"]'
  echo '}'
} >> "$out"

echo "render-connections: 已写入 $out，缺 kubeconfig 的区：$missing"
cat "$out"
