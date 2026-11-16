#!/usr/bin/env bash
# SR-IOV 自检：临时借用一个 PF 上编号最大的两块 VF，在宿主上验证宽松模式依赖的三件事，
# 验证完把 VF 还给 vfio-pci。
#
#   1. 同一个 PF 上的 VF 之间能直接互通（走网卡内部的交换，不出光纤）。
#      核心网段里网络节点、存储节点和控制面 VM 之间就是这样通信的。
#   2. 端口 VLAN 能隔离：两块 VF 在不同 VLAN 时不通。VM 自己改不了端口 VLAN，这是管理隔离的基础。
#   3. 光纤拔掉时，设了 link-state enable 的 VF 之间照样互通（核心网段不依赖外面的线缆和交换机）。
#      这一项要拔掉光纤后再跑一次本脚本。
#
#   sudo bash scripts/sriov-selftest.sh [PF 名，默认 sfp0]
#
# 测试流量用 VLAN 4000/4001 和 10.255.255.0/30，不碰真正在用的 VLAN 和地址。
# 借用前会确认这两块 VF 没有被 VM 使用；最好在 P4 部署 VM 之前跑。
set -euo pipefail

pf=${1:-sfp0}
dev=/sys/class/net/$pf/device
[ -e "$dev/sriov_numvfs" ] || { echo "$pf 不是 SR-IOV 的 PF（pits-sriov 跑过了吗？）" >&2; exit 1; }
total=$(cat "$dev/sriov_numvfs")
[ "$total" -ge 2 ] || { echo "$pf 上只有 $total 块 VF，至少要 2 块" >&2; exit 1; }

vfs=($((total - 2)) $((total - 1)))
macs=(02:50:ff:ff:00:01 02:50:ff:ff:00:02)
nss=(pits-sriov-a pits-sriov-b)
ips=(10.255.255.1/30 10.255.255.2/30)
addrs=()
for i in 0 1; do
  addrs[i]=$(basename "$(readlink -f "$dev/virtfn${vfs[i]}")")
done

driver_of() {
  if [ -e "/sys/bus/pci/devices/$1/driver" ]; then
    basename "$(readlink -f "/sys/bus/pci/devices/$1/driver")"
  fi
}
rebind() { # $1 = PCI 地址，$2 = 驱动
  local q=/sys/bus/pci/devices/$1 d
  echo "$2" > "$q/driver_override"
  d=$(driver_of "$1")
  if [ -n "$d" ] && [ "$d" != "$2" ]; then echo "$1" > "$q/driver/unbind"; fi
  if [ "$(driver_of "$1")" != "$2" ]; then echo "$1" > /sys/bus/pci/drivers_probe; fi
}

for i in 0 1; do
  g=$(basename "$(readlink -f "/sys/bus/pci/devices/${addrs[i]}/iommu_group")")
  if [ -e "/dev/vfio/$g" ] && fuser -s "/dev/vfio/$g"; then
    echo "$pf vf${vfs[i]}（${addrs[i]}）正被 VM 使用，换一个 PF 或先停掉那台 VM" >&2
    exit 1
  fi
done

cleanup() {
  set +e
  for i in 0 1; do
    ip netns del "${nss[i]}" 2>/dev/null
  done
  sleep 1
  for i in 0 1; do
    ip link set "$pf" vf "${vfs[i]}" vlan 0 state auto 2>/dev/null
    ip link set "$pf" vf "${vfs[i]}" mac 00:00:00:00:00:00 2>/dev/null
    rebind "${addrs[i]}" vfio-pci
    echo "  vf${vfs[i]}（${addrs[i]}）已还给 $(driver_of "${addrs[i]}")"
  done
}
trap cleanup EXIT

echo "== $pf 的物理链路：$(cat "/sys/class/net/$pf/carrier" 2>/dev/null | sed 's/1/已连接/;s/0/未连接/')"
echo "   借用 vf${vfs[0]}（${addrs[0]}）和 vf${vfs[1]}（${addrs[1]}）"

modprobe iavf
for i in 0 1; do
  ip link set "$pf" vf "${vfs[i]}" mac "${macs[i]}" vlan 4000 spoofchk on trust off state enable
  rebind "${addrs[i]}" iavf
done

# 等 iavf 建好网卡，放进各自的网络命名空间
for i in 0 1; do
  q=/sys/bus/pci/devices/${addrs[i]}
  n=""
  for _ in $(seq 30); do
    for x in "$q"/net/*; do [ -e "$x" ] && n=$(basename "$x"); done
    [ -n "$n" ] && break
    sleep 1
  done
  [ -n "$n" ] || { echo "vf${vfs[i]} 绑上 iavf 后没有出现网卡" >&2; exit 1; }
  ip netns add "${nss[i]}"
  ip link set "$n" netns "${nss[i]}"
  ip -n "${nss[i]}" link set "$n" name vf0
  ip -n "${nss[i]}" addr add "${ips[i]}" dev vf0
  ip -n "${nss[i]}" link set vf0 up
done

# 改 VF 的 VLAN 会让网卡重置这块 VF，iavf 要几秒钟才恢复，所以“通”要多试几次。
reachable() {
  for _ in $(seq 15); do
    if ip netns exec "${nss[0]}" ping -c 1 -W 1 10.255.255.2 >/dev/null 2>&1; then return 0; fi
    sleep 1
  done
  return 1
}

result=0
echo
echo "== 1. 同一 VLAN（4000）里两块 VF 互通"
if reachable; then
  echo "   通过"
else
  echo "   !! 不通"
  result=1
fi

echo "== 2. 不同 VLAN（4000 和 4001）之间隔离"
ip link set "$pf" vf "${vfs[1]}" vlan 4001
sleep 8
if ip netns exec "${nss[0]}" ping -c 3 -W 1 10.255.255.2 >/dev/null 2>&1; then
  echo "   !! 居然通了，端口 VLAN 没有起作用"
  result=1
else
  # 再改回同一个 VLAN，确认刚才不通是 VLAN 造成的，而不是 VF 没恢复
  ip link set "$pf" vf "${vfs[1]}" vlan 4000
  if reachable; then
    echo "   通过（不同 VLAN 时不通，改回同一 VLAN 后又通了）"
  else
    echo "   !! 改回同一 VLAN 后也不通，这一项没法判断"
    result=1
  fi
fi

echo
if [ "$(cat "/sys/class/net/$pf/carrier" 2>/dev/null)" = 1 ]; then
  echo "第 3 项还没测：拔掉 $pf 的光纤后再跑一次本脚本，第 1 项仍应通过。"
else
  echo "== 3. 光纤未连接，第 1 项的结果就是第 3 项的结果"
fi

echo
echo "== 收尾"
exit "$result"
