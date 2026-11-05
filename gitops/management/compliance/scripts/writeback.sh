#!/bin/sh
# 把合规结果写回 ManageIQ：不合规的 VM 打上 pits_compliance/noncompliant 标签，恢复合规的去掉。
# 前提（P3，见 docs/compliance.md）：
#   - ManageIQ 里已有标签分类 pits_compliance，含标签 noncompliant；
#   - Secret pits-miq-api 里有 user 和 password（只给打标签的权限）；
#   - 环境变量 MIQ_WRITEBACK=true。
set -eu

apk add --no-cache curl jq >/dev/null

result=/out/result.json
alarms=$(jq -r '[.. | objects | select(has("status") and has("resource")) | select(.status == "alarm") | .resource] | unique | .[]' "$result")
count=$(printf '%s\n' "$alarms" | grep -c . || true)
echo "writeback: 不合规项 $count 个"
printf '%s\n' "$alarms"

if [ "${MIQ_WRITEBACK:-false}" != "true" ]; then
  echo "writeback: MIQ_WRITEBACK 未打开，只输出结果"
  exit 0
fi

api() { curl -fsS -u "$MIQ_USER:$MIQ_PASSWORD" -H 'Content-Type: application/json' "$@"; }
tag='{"category":"pits_compliance","name":"noncompliant"}'

# 资源名形如 <连接>/<命名空间>/<名字>；ManageIQ 里的 VM 名就是最后一段。
want=$(printf '%s\n' "$alarms" | awk -F/ 'NF>=3 {print $NF}' | sort -u)

# 1. 给不合规的 VM 打标签
for name in $want; do
  id=$(api "$MIQ_URL/api/vms?filter%5B%5D=name=$name&expand=resources&attributes=id" | jq -r '.resources[0].id // empty')
  [ -n "$id" ] || { echo "writeback: ManageIQ 里找不到 VM $name"; continue; }
  api -X POST "$MIQ_URL/api/vms/$id/tags" -d "{\"action\":\"assign\",\"resources\":[$tag]}" >/dev/null
  echo "writeback: $name 标为不合规"
done

# 2. 已恢复合规的去掉标签
api "$MIQ_URL/api/vms?by_tag=/pits_compliance/noncompliant&expand=resources&attributes=id,name" |
  jq -r '.resources[] | "\(.id) \(.name)"' |
  while read -r id name; do
    if ! printf '%s\n' "$want" | grep -qx "$name"; then
      api -X POST "$MIQ_URL/api/vms/$id/tags" -d "{\"action\":\"unassign\",\"resources\":[$tag]}" >/dev/null
      echo "writeback: $name 恢复合规，去掉标签"
    fi
  done
