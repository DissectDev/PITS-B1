# 核心机（MS-01）。只组合模块，具体取值全部来自 site/*.nix。
{ site, ... }:
{
  imports = [
    ../../modules/pits-agent.nix
    ../../modules/pits-portal.nix
    ./hardware.nix
    ./filesystems.nix
    ../../modules/site-check.nix
    ../../modules/base.nix
    ../../modules/network.nix
    ../../modules/vfio.nix
    ../../modules/sriov.nix
    ../../modules/tailscale.nix
    ../../modules/tailscale-relay.nix
    ../../modules/proxy.nix
    ../../modules/ups.nix
    ../../modules/rke2.nix
    ../../modules/btrfs.nix
    ../../modules/netboot.nix
    ../../modules/thunderbolt.nix
  ];

  networking.hostName = site.hostName;

  # 首次安装时的 NixOS 版本，之后不要随升级改动。
  system.stateVersion = "26.05";
}
