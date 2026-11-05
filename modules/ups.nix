# UPS（NUT）。断电时 upsmon 触发关机；关机时 kubelet 的优雅关机（见 modules/rke2.nix）
# 先让 VM 走 ACPI 关机，再关宿主。
{ lib, site, ... }:
lib.mkIf site.ups.enable {
  power.ups = {
    enable = true;
    mode = "standalone";
    ups.${site.ups.name} = {
      driver = site.ups.driver;
      port = site.ups.port;
      description = "PITS core UPS";
    };
    users.upsmon = {
      passwordFile = site.ups.passwordFile;
      upsmon = "primary";
    };
    upsmon.monitor.${site.ups.name}.user = "upsmon";
  };
}
