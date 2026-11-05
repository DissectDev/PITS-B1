#!/usr/bin/env bash
# P1 验收：确认整口直通的设备都绑在 vfio-pci 上，X710 的 VF 都建好并绑在 vfio-pci 上，
# 宿主只在管理口上有地址、只看得到系统盘。
#
#   sudo bash scripts/check-passthrough.sh
set -u
fail=0

echo "== pits-vfio-bind 和 pits-sriov 服务"
systemctl --no-pager --lines=20 status pits-vfio-bind.service || fail=1
systemctl --no-pager --lines=20 status pits-sriov.service || fail=1

echo
echo "== 整口直通的设备（vfio-pci 上、不是 VF 的设备）"
for d in /sys/bus/pci/drivers/vfio-pci/0000:*; do
  [ -e "$d" ] || continue
  [ -e "$d/physfn" ] && continue
  addr=$(basename "$d")
  echo "  $addr  iommu=$(basename "$(readlink -f "$d/iommu_group")")  $(lspci -s "$addr" 2>/dev/null | cut -d' ' -f2-)"
done

echo
echo "== 宿主可见的物理网卡（有线的只应有 mgmt0 和 SR-IOV 的 PF sfp*；板载 Wi-Fi 留在宿主上）"
others=0
for n in /sys/class/net/*; do
  [ -e "$n/device" ] || continue
  name=$(basename "$n")
  if [ -d "$n/wireless" ] || [ -e "$n/phy80211" ]; then
    echo "  $name（无线，留在宿主上）"
    continue
  fi
  case $name in
    mgmt0)
      echo "  $name"
      ;;
    sfp[0-9])
      # PF 只给 VF 当母口，宿主自己不能在上面有地址。
      addrs=$(ip -o addr show dev "$name" | awk '{print $4}' | xargs)
      echo "  $name（SR-IOV PF，$(cat "$n/operstate")，MTU $(cat "$n/mtu")）"
      if [ -n "$addrs" ]; then
        echo "  !! $name 上有地址：$addrs"
        fail=1
      fi
      ;;
    *)
      echo "  $name"
      others=1
      ;;
  esac
done
[ "$others" = 0 ] || { echo "  !! 除 mgmt0 和 sfp* 外还有有线网卡留在宿主上"; fail=1; }

echo
echo "== X710 的 VF（每块都应在 vfio-pci 上，并且独占一个 IOMMU 组）"
pfs=0
for n in /sys/class/net/sfp[0-9]; do
  [ -e "$n" ] || continue
  pfs=$((pfs + 1))
  name=$(basename "$n")
  p=$(readlink -f "$n/device")
  echo "  $name：$(cat "$p/sriov_numvfs") / $(cat "$p/sriov_totalvfs") 块 VF，驱动 $(basename "$(readlink -f "$p/driver")")"
  for vf in "$p"/virtfn*; do
    [ -e "$vf" ] || continue
    idx=${vf##*virtfn}
    a=$(basename "$(readlink -f "$vf")")
    drv=$(basename "$(readlink -f "$vf/driver" 2>/dev/null || echo none)")
    g=$(readlink -f "$vf/iommu_group")
    members=$(ls "$g/devices" | wc -l)
    mark=""
    if [ "$drv" != vfio-pci ]; then mark="  !! 驱动不是 vfio-pci"; fail=1; fi
    if [ "$members" != 1 ]; then mark="$mark  !! IOMMU 组里有 $members 个设备"; fail=1; fi
    echo "    vf$idx  $a  驱动=$drv  iommu=$(basename "$g")$mark"
  done
  # VF 的 VLAN、MAC、trust、spoofchk（VM 起来后由 SR-IOV CNI 设置）
  ip link show "$name" | sed -n 's/^ *\(vf [0-9]*\)/      \1/p'
done
[ "$pfs" -gt 0 ] || { echo "  !! 没有找到 sfp* 网卡，pits-sriov 没有正常运行"; fail=1; }

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
