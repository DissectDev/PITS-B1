# X710 的 SR-IOV：PF 留在宿主上，VF 交给 vfio-pci，再由核心集群里的 SR-IOV device plugin 分给 VM。
#
# VF 的 VLAN、MAC、trust、spoofchk 只能从 PF 上设置，而 PF 在宿主手里。所以网络节点只拿到自己的
# VF，改不了存储节点那块 VF 接在哪个 VLAN 上：两台节点在管理上是隔离的，数据面上存储节点照样
# 在网络节点下游。具体每块 VF 怎么设，由核心集群的 NetworkAttachmentDefinition 决定。
#
# 启动时：
#   1. networkd 的 .link 按 PCI 地址把 PF 改名为 sfp0/sfp1 并设 MTU；.network 只把口拉起来，不配地址。
#   2. pits-sriov 在 RKE2 之前建好 VF，并把每块 VF 绑到 vfio-pci。建 VF 前关掉自动探测，
#      宿主不会给 VF 加载 iavf 驱动。
{
  lib,
  pkgs,
  site,
  ...
}:
let
  pfs = site.sriov.pfs;

  pfArgs = lib.concatMapStringsSep " " (
    pf: lib.escapeShellArgs [
      pf.name
      pf.pciAddress
      (toString pf.numVfs)
    ]
  ) pfs;
in
{
  systemd.network.links = lib.listToAttrs (
    map (
      pf:
      lib.nameValuePair "20-${pf.name}" {
        matchConfig.Path = "pci-${pf.pciAddress}";
        linkConfig = {
          Name = pf.name;
          MTUBytes = toString site.sriov.mtu;
        };
      }
    ) pfs
  );

  # PF 只负责把 VF 的流量送上光纤，宿主自己不在这些网里：不配 IPv4，也不要 IPv6 链路本地地址。
  systemd.network.networks = lib.listToAttrs (
    map (
      pf:
      lib.nameValuePair "20-${pf.name}" {
        matchConfig.Name = pf.name;
        networkConfig = {
          LinkLocalAddressing = "no";
          IPv6AcceptRA = false;
          ConfigureWithoutCarrier = true;
        };
        linkConfig.RequiredForOnline = "no";
      }
    ) pfs
  );

  systemd.services.pits-sriov = {
    description = "Create X710 virtual functions and bind them to vfio-pci";
    wantedBy = [ "multi-user.target" ];
    before = [ "rke2-server.service" ];
    after = [ "systemd-udevd.service" ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
    };
    path = with pkgs; [
      kmod
      coreutils
      psmisc
    ];
    script = ''
      set -eu
      modprobe vfio-pci

      driver_of() {
        if [ -e "/sys/bus/pci/devices/$1/driver" ]; then
          basename "$(readlink -f "/sys/bus/pci/devices/$1/driver")"
        fi
      }
      group_of() { basename "$(readlink -f "/sys/bus/pci/devices/$1/iommu_group")"; }
      # 设备的 vfio 节点被进程打开，说明有 VM 正在用它
      in_use() {
        g=$(group_of "$1")
        [ -e "/dev/vfio/$g" ] && fuser -s "/dev/vfio/$g"
      }

      setup_pf() {
        name=$1 addr=$2 want=$3
        p=/sys/bus/pci/devices/$addr
        if [ ! -e "$p" ]; then
          echo "pits-sriov: $addr 不存在，检查 site/core.nix 里的 sriov.pfs" >&2
          exit 1
        fi
        if [ ! -e "$p/sriov_totalvfs" ]; then
          echo "pits-sriov: $addr 不支持 SR-IOV" >&2
          exit 1
        fi

        # PF 要在宿主的 i40e 驱动上。以前整口直通时留下的 vfio-pci 绑定在这里解除
        # （在线切换配置、还没重启时会遇到）。
        cur=$(driver_of "$addr")
        if [ "$cur" != i40e ]; then
          if [ "$cur" = vfio-pci ] && in_use "$addr"; then
            echo "pits-sriov: $addr 正被 VM 整口占用，先停掉那台 VM" >&2
            exit 1
          fi
          if [ -n "$cur" ]; then echo "$addr" > "$p/driver/unbind"; fi
          echo > "$p/driver_override"
          modprobe i40e
          echo "$addr" > /sys/bus/pci/drivers_probe
        fi

        # 等网卡出现，并等 udev 按 .link 把它改名
        dev=""
        for _ in $(seq 60); do
          if [ -e "$p/net/$name" ]; then dev=$name; break; fi
          sleep 1
        done
        if [ -z "$dev" ]; then
          for n in "$p"/net/*; do [ -e "$n" ] && dev=$(basename "$n"); done
          if [ -z "$dev" ]; then
            echo "pits-sriov: $addr 绑定 i40e 后没有出现网卡" >&2
            exit 1
          fi
          echo "pits-sriov: $addr 的网卡叫 $dev，不是 $name；重启一次让 .link 生效" >&2
        fi

        total=$(cat "$p/sriov_totalvfs")
        if [ "$want" -gt "$total" ]; then
          echo "pits-sriov: $name 最多支持 $total 块 VF，site 里写的是 $want" >&2
          exit 1
        fi

        have=$(cat "$p/sriov_numvfs")
        if [ "$have" != "$want" ]; then
          # 改 VF 数量要先把现有的 VF 全部删掉，有 VM 在用时不能这么做。
          for vf in "$p"/virtfn*; do
            [ -e "$vf" ] || continue
            a=$(basename "$(readlink -f "$vf")")
            if [ "$(driver_of "$a")" = vfio-pci ] && in_use "$a"; then
              echo "pits-sriov: $name 的 VF $a 正被 VM 使用，不能把 VF 数量从 $have 改成 $want" >&2
              exit 1
            fi
          done
          echo 0 > "$p/sriov_drivers_autoprobe"
          if [ "$have" != 0 ]; then echo 0 > "$p/sriov_numvfs"; fi
          echo "$want" > "$p/sriov_numvfs"
        fi

        i=0
        while [ "$i" -lt "$want" ]; do
          a=$(basename "$(readlink -f "$p/virtfn$i")")
          q=/sys/bus/pci/devices/$a
          echo vfio-pci > "$q/driver_override"
          d=$(driver_of "$a")
          if [ "$d" != vfio-pci ]; then
            if [ -n "$d" ]; then echo "$a" > "$q/driver/unbind"; fi
            echo "$a" > /sys/bus/pci/drivers_probe
          fi
          d=$(driver_of "$a")
          if [ "$d" != vfio-pci ]; then
            echo "pits-sriov: $name vf$i（$a）绑定到了 ''${d:-（无驱动）}，不是 vfio-pci" >&2
            exit 1
          fi
          echo "pits-sriov: $name vf$i $a -> vfio-pci (iommu group $(group_of "$a"))"
          i=$((i + 1))
        done
      }

      set -- ${pfArgs}
      while [ $# -gt 0 ]; do
        setup_pf "$1" "$2" "$3"
        shift 3
      done
    '';
  };
}
