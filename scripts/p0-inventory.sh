#!/usr/bin/env bash
# P0：在核心机上（NixOS 安装盘或已装好的宿主）收集填写 site/core.nix 需要的信息。
#
#   sudo bash scripts/p0-inventory.sh
#
# 需要 lspci、ethtool、lsblk、lscpu。缺的话：nix-shell -p pciutils ethtool util-linux
set -u

hr() { printf '\n==== %s ====\n' "$1"; }

hr "CPU 拓扑（P 核有两个线程且 MAXMHZ 更高；E 核每核一个线程）"
lscpu --extended=CPU,CORE,MAXMHZ,ONLINE 2>/dev/null || lscpu --extended

hr "IOMMU 分组"
if [ ! -d /sys/kernel/iommu_groups ] || [ -z "$(ls -A /sys/kernel/iommu_groups 2>/dev/null)" ]; then
  echo "没有 IOMMU 分组：检查 BIOS 里的 VT-d，以及内核参数 intel_iommu=on。"
else
  for g in $(ls /sys/kernel/iommu_groups | sort -n); do
    echo "group $g:"
    for d in /sys/kernel/iommu_groups/$g/devices/*; do
      addr=$(basename "$d")
      drv=$(basename "$(readlink -f "$d/driver" 2>/dev/null)" 2>/dev/null)
      printf '    %s  [%s]  %s\n' "$addr" "${drv:-无驱动}" "$(lspci -nns "$addr" 2>/dev/null | cut -d' ' -f2-)"
    done
  done
fi

hr "NVMe 盘：PCI 地址 ↔ by-id ↔ 是否挂载"
for c in /sys/class/nvme/nvme*; do
  [ -e "$c" ] || continue
  addr=$(basename "$(readlink -f "$c/device")")
  model=$(cat "$c/model" 2>/dev/null | xargs)
  serial=$(cat "$c/serial" 2>/dev/null | xargs)
  group=$(basename "$(readlink -f "$c/device/iommu_group")")
  echo "$addr  iommu=$group  $model  SN=$serial"
  for ns in "$c"/nvme*n*; do
    [ -e "$ns" ] || continue
    n=$(basename "$ns")
    for l in /dev/disk/by-id/*; do
      [ "$(readlink -f "$l")" = "/dev/$n" ] && echo "    by-id: $l"
    done
    mp=$(lsblk -nro MOUNTPOINTS "/dev/$n" 2>/dev/null | grep . | tr '\n' ' ')
    echo "    /dev/$n  挂载: ${mp:-无}"
  done
done

hr "网卡：PCI 地址 ↔ 驱动 ↔ 永久 MAC"
for n in /sys/class/net/*; do
  [ -e "$n/device" ] || continue
  name=$(basename "$n")
  addr=$(basename "$(readlink -f "$n/device")")
  drv=$(basename "$(readlink -f "$n/device/driver")")
  perm=$(ethtool -P "$name" 2>/dev/null | awk '{print $NF}')
  group=$(basename "$(readlink -f "$n/device/iommu_group")")
  link=$(cat "$n/operstate" 2>/dev/null)
  ip4=$(ip -4 -o addr show "$name" 2>/dev/null | awk '{print $4}' | tr '\n' ' ')
  echo "$name  $addr  iommu=$group  driver=$drv  mac=${perm:-?}  link=$link  ${ip4}"
done

cat <<'EOF'

==== 怎么填 site/core.nix ====
- mgmt.mac：driver=igc、接着光猫、有地址的那个口（I226-V）的永久 MAC。
- passthrough.networkNode：另一个 igc 口（I226-LM）和两个 i40e 口（X710）的 PCI 地址。
- passthrough.storageNode：三块数据盘的 PCI 地址。
- systemDisk：剩下那块盘的 /dev/disk/by-id 路径。建议用最慢的 x2 位那块。
- 关卡：每个要直通的设备所在的 IOMMU 分组里，不能有宿主还要用的设备
  （PCIe 桥除外）。不满足就换一块盘当系统盘，或者把同组设备一起直通。
- cpu.reservedSystemCPUs：按上面的 CPU 拓扑，填两个 E 核的线程号。
EOF
