# 沿用系统盘上现有的分区（site.systemDiskLayout = "existing"），在机器上直接 nixos-rebuild。
# 分区取自图形安装器生成的 hardware-configuration.nix，按 UUID 挂载。
{ lib, site, ... }:
lib.mkIf (site.systemDiskLayout == "existing") {
  fileSystems."/" = {
    device = site.existingFileSystems.root;
    fsType = "ext4";
  };

  fileSystems."/boot" = {
    device = site.existingFileSystems.boot;
    fsType = "vfat";
    options = [
      "fmask=0077"
      "dmask=0077"
    ];
  };

  swapDevices = [ ];
}
