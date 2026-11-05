# 把要直通的设备交给 vfio-pci。
#
# 按 PCI 地址绑定，不按 vendor:device ID：系统盘和数据盘如果是同一型号，按 ID 绑定会把系统盘一起交出去。
#
# 分两步：
#   1. initrd 和正式系统里都用 udev 规则给这些地址设 driver_override=vfio-pci，
#      让 nvme、igc、i40e 驱动加载时跳过它们。
#   2. pits-vfio-bind 服务在 RKE2 启动前确认每个设备都绑在 vfio-pci 上，没绑上就重新探测；
#      绑定前会检查设备是不是正被宿主使用（挂载着的盘、管理口），是就拒绝并报错。
{
  lib,
  pkgs,
  site,
  ...
}:
let
  devices = site.passthrough.networkNode ++ site.passthrough.storageNode;

  overrideRules = lib.concatMapStrings (addr: ''
    ACTION=="add", SUBSYSTEM=="pci", KERNEL=="${addr}", ATTR{driver_override}="vfio-pci"
  '') devices;
in
{
  boot.kernelParams = [
    "intel_iommu=on"
    "iommu=pt"
  ];

  boot.initrd.kernelModules = [
    "vfio_pci"
    "vfio"
    "vfio_iommu_type1"
  ];
  boot.kernelModules = [
    "vfio_pci"
    "vfio"
    "vfio_iommu_type1"
  ];

  boot.initrd.services.udev.rules = overrideRules;
  services.udev.extraRules = overrideRules;

  systemd.services.pits-vfio-bind = {
    description = "Bind PITS passthrough devices to vfio-pci";
    wantedBy = [ "multi-user.target" ];
    before = [ "rke2-server.service" ];
    after = [ "systemd-udev-settle.service" ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
    };
    path = with pkgs; [
      kmod
      coreutils
      util-linux
      gnugrep
    ];
    script = ''
      set -eu
      modprobe vfio-pci
      mgmt_addr=${lib.escapeShellArg site.mgmt.pciAddress}

      for dev in ${lib.escapeShellArgs devices}; do
        p=/sys/bus/pci/devices/$dev
        if [ "$dev" = "$mgmt_addr" ]; then
          echo "pits-vfio: $dev 是管理口，拒绝直通" >&2
          exit 1
        fi
        if [ ! -e "$p" ]; then
          echo "pits-vfio: $dev 不存在，检查 site/core.nix 里的地址" >&2
          exit 1
        fi

        cur=""
        if [ -e "$p/driver" ]; then cur=$(basename "$(readlink -f "$p/driver")"); fi

        if [ "$cur" != "vfio-pci" ] && [ -n "$cur" ]; then
          # 安全检查：正在被宿主使用的设备不交出去。
          for blk in "$p"/nvme/nvme*/nvme*n*; do
            [ -e "$blk" ] || continue
            name=$(basename "$blk")
            if lsblk -nro MOUNTPOINTS "/dev/$name" | grep -q .; then
              echo "pits-vfio: $dev 上的 /dev/$name 已挂载，可能是系统盘，拒绝直通" >&2
              exit 1
            fi
          done
          for net in "$p"/net/*; do
            [ -e "$net" ] || continue
            if [ "$(basename "$net")" = "mgmt0" ]; then
              echo "pits-vfio: $dev 是管理口，拒绝直通" >&2
              exit 1
            fi
          done
          echo "$dev" > "$p/driver/unbind"
        fi

        echo vfio-pci > "$p/driver_override"
        if [ "$cur" != "vfio-pci" ]; then
          echo "$dev" > /sys/bus/pci/drivers_probe
        fi

        cur=$(basename "$(readlink -f "$p/driver" 2>/dev/null || echo none)")
        if [ "$cur" != "vfio-pci" ]; then
          echo "pits-vfio: $dev 绑定到了 $cur 而不是 vfio-pci" >&2
          exit 1
        fi
        echo "pits-vfio: $dev -> vfio-pci (iommu group $(basename "$(readlink -f "$p/iommu_group")"))"
      done
    '';
  };
}
