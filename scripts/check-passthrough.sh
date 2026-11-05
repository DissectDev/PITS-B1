#!/usr/bin/env bash
# P1 验收：确认直通设备都绑在 vfio-pci 上，宿主只看得到管理口和系统盘。
#
#   sudo bash scripts/check-passthrough.sh
set -u
fail=0

echo "== pits-vfio-bind 服务"
systemctl --no-pager --lines=20 status pits-vfio-bind.service || fail=1

echo
echo "== vfio-pci 上的设备"
for d in /sys/bus/pci/drivers/vfio-pci/0000:*; do
  [ -e "$d" ] || continue
  addr=$(basename "$d")
  echo "  $addr  iommu=$(basename "$(readlink -f "$d/iommu_group")")  $(lspci -s "$addr" 2>/dev/null | cut -d' ' -f2-)"
done

echo
echo "== 宿主可见的物理网卡（应当只有 mgmt0）"
others=0
for n in /sys/class/net/*; do
  [ -e "$n/device" ] || continue
  name=$(basename "$n")
  echo "  $name"
  [ "$name" = "mgmt0" ] || others=1
done
[ "$others" = 0 ] || { echo "  !! 除 mgmt0 外还有物理网卡没被直通"; fail=1; }

echo
echo "== 宿主可见的 NVMe（应当只有系统盘）"
count=0
for c in /sys/class/nvme/nvme*; do
  [ -e "$c" ] || continue
  count=$((count + 1))
  echo "  $(basename "$c")  $(basename "$(readlink -f "$c/device")")  $(xargs < "$c/model")"
done
[ "$count" = 1 ] || { echo "  !! 宿主看到 $count 块 NVMe，应当只有 1 块"; fail=1; }

echo
if [ "$fail" = 0 ]; then echo "通过"; else echo "未通过"; fi
exit "$fail"
