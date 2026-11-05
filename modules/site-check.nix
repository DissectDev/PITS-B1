# 部署前检查站点参数：占位值没填完、PCI 地址格式不对、直通设备重复时，评估直接失败。
# 直通写错的代价是宿主丢掉系统盘或管理口，所以这里宁可严格。
{ lib, site, ... }:
let
  isPlaceholder = s: lib.hasInfix "TODO" s;
  pciAddr = a: builtins.match "[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\\.[0-7]" a != null;
  allPassthrough = site.passthrough.networkNode ++ site.passthrough.storageNode;
in
{
  assertions = [
    {
      assertion = lib.elem site.systemDiskLayout [
        "existing"
        "disko"
      ];
      message = "site: systemDiskLayout 只能是 \"existing\" 或 \"disko\"。";
    }
    {
      assertion =
        site.systemDiskLayout != "existing"
        || (lib.hasPrefix "/dev/disk/by-uuid/" site.existingFileSystems.root
          && lib.hasPrefix "/dev/disk/by-uuid/" site.existingFileSystems.boot);
      message = "site: systemDiskLayout = \"existing\" 时，existingFileSystems.root 和 boot 要写成 /dev/disk/by-uuid/... 的形式。";
    }
    {
      assertion = !isPlaceholder site.systemDisk;
      message = "site: systemDisk 还是占位值。先在核心机上跑 scripts/p0-inventory.sh，再填写 site/core.nix。";
    }
    {
      assertion = pciAddr site.mgmt.pciAddress;
      message = "site: mgmt.pciAddress（管理口 I226-V 的 PCI 地址）格式不对，应为 0000:58:00.0 这样的小写形式。";
    }
    {
      assertion = !(lib.elem site.mgmt.pciAddress allPassthrough);
      message = "site: 管理口 ${site.mgmt.pciAddress} 出现在 passthrough 里，宿主会失去管理口。";
    }
    {
      assertion = builtins.match "[0-9]+\\.[0-9]+\\.[0-9]+\\.[0-9]+" site.coreNet.via != null;
      message = "site: coreNet.via 应当是网络节点上行口的 IPv4 地址。";
    }
    {
      assertion = site.mgmt.dhcp || (site.mgmt.address != null && site.mgmt.gateway != null);
      message = "site: mgmt.dhcp = false 时必须填写 mgmt.address 和 mgmt.gateway。";
    }
    {
      assertion = lib.all pciAddr allPassthrough;
      message = "site: passthrough 里有不是 PCI 地址的值（格式应为 0000:02:00.0，小写）。当前值：${toString allPassthrough}";
    }
    {
      assertion = lib.length (lib.unique allPassthrough) == lib.length allPassthrough;
      message = "site: passthrough 里有重复的 PCI 地址。";
    }
    {
      assertion = site.admin.sshKeys != [ ] && lib.all (k: !isPlaceholder k) site.admin.sshKeys;
      message = "site: admin.sshKeys 还没填。没有公钥就进不了宿主。";
    }
  ];
}
