#!/usr/bin/env bash
# 新建一个实验区：把区写进区清单，并按模板生成它的控制面 VM。
# ManageIQ 的“实验区”目录项会调用同样的步骤；手动也可以直接跑。
#
#   scripts/new-zone.sh <区名> <控制面 MAC>
#   例：scripts/new-zone.sh lab1 02:50:00:00:01:01
#
# 宽松模式下各区不单独分 VLAN：控制面 VM 和分支都在核心网段（VLAN 10）里，见 docs/addressing.md。
#
# 只改仓库里的文件，不提交、不推送；检查无误后自己提交。
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
registry="$root/gitops/zones/registry/zones.json"
clusters="$root/gitops/zones/clusters"
template="$root/gitops/zones/template/cp-vm.yaml"

if [ $# -ne 2 ]; then
  sed -n '2,11p' "$0"
  exit 2
fi
zone=$1 mac=$2

[[ $zone =~ ^[a-z][a-z0-9-]{0,20}$ ]] || { echo "区名只能用小写字母、数字和连字符，以字母开头，不超过 21 个字符" >&2; exit 1; }
[[ $mac =~ ^([0-9a-f]{2}:){5}[0-9a-f]{2}$ ]] || { echo "MAC 格式应为小写的 aa:bb:cc:dd:ee:ff" >&2; exit 1; }

if jq -e --arg z "$zone" '.zones[] | select(.name == $z)' "$registry" >/dev/null; then
  echo "区 $zone 已经在区清单里" >&2; exit 1
fi
if jq -e --arg m "$mac" '.zones[] | select(.controlPlaneMac == $m)' "$registry" >/dev/null; then
  echo "MAC $mac 已被其他区的控制面使用" >&2; exit 1
fi

# 1. 写进区清单（唯一来源）
tmp=$(mktemp)
jq --arg z "$zone" --arg m "$mac" \
  '.zones += [{"name": $z, "controlPlaneMac": $m}] | .zones |= sort_by(.name)' \
  "$registry" > "$tmp"
mv "$tmp" "$registry"

# 2. 生成控制面 VM
mkdir -p "$clusters/$zone"
{
  echo "# 由 scripts/new-zone.sh 根据 gitops/zones/template/cp-vm.yaml 生成。"
  grep -v '^#' "$template" | sed -e "s/ZONE_NAME/$zone/g" -e "s/ZONE_MAC/$mac/g"
} > "$clusters/$zone/cp-vm.yaml"
cat > "$clusters/$zone/kustomization.yaml" <<EOF
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
resources:
  - cp-vm.yaml
EOF

# 3. 按现有子目录重写 zones/clusters 的资源列表
{
  echo "# 每个实验区一个子目录，由 scripts/new-zone.sh 生成并加到这里。"
  echo "apiVersion: kustomize.config.k8s.io/v1beta1"
  echo "kind: Kustomization"
  echo "resources:"
  for d in "$clusters"/*/; do
    if [ -f "$d/kustomization.yaml" ]; then echo "  - $(basename "$d")"; fi
  done
} > "$clusters/kustomization.yaml"

cat <<EOF
已生成实验区 $zone（控制面 MAC $mac）。接下来：
  1. 检查改动并提交：git diff && git add -A && git commit
  2. 创建控制面的 cloud-init Secret（含 rke2 token，不进 Git）：
       kubectl -n zones create secret generic zone-$zone-cp-cloudinit --from-file=userdata=<文件>
  3. 在网络节点上为 MAC $mac 在核心网段里分一个固定地址（10.88.0.101 起，见 docs/addressing.md），
     并为这个区的分支划好 DHCP 地址段。
     控制面从 sfp0 的共享 VF 池（vf2–7，共 6 块）里拿 VF；已经用了几块，看
     kubectl describe node 输出里 pits.io/sfp0_shared 那一行。
  4. 控制面起来后：
       scripts/register-zone-provider.sh $zone <控制面地址> <token 文件>
     它会注册 ManageIQ provider，并把只读 kubeconfig 写进 Steampipe 的 Secret。
EOF
