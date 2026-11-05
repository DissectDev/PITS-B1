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
      assertion = !isPlaceholder site.systemDisk;
      message = "site: systemDisk 还是占位值。先在核心机上跑 scripts/p0-inventory.sh，再填写 site/core.nix。";
    }
    {
      assertion = !isPlaceholder site.mgmt.mac;
      message = "site: mgmt.mac（管理口 I226-V 的 MAC）还是占位值。";
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
