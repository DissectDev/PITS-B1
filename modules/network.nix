# 宿主网络：只有管理口在用；br-core 是核心机内部 VM 互联用的网桥，不接物理口，宿主不配地址。
# 其余网卡（I226-LM、X710）直通给网络节点，宿主看不到，见 modules/vfio.nix。
{ lib, site, ... }:
{
  networking.useNetworkd = true;
  networking.useDHCP = false;
  services.resolved.enable = true;

  systemd.network = {
    enable = true;

    # 按永久 MAC 匹配管理口并改名，避免网卡枚举顺序变化。
    links."10-mgmt" = {
      matchConfig.PermanentMACAddress = site.mgmt.mac;
      linkConfig.Name = "mgmt0";
    };

    networks."10-mgmt" = {
      matchConfig.Name = "mgmt0";
      networkConfig =
        if site.mgmt.dhcp then
          {
            DHCP = "ipv4";
            IPv6AcceptRA = true;
          }
        else
          { IPv6AcceptRA = true; };
      address = lib.optional (!site.mgmt.dhcp) site.mgmt.address;
      gateway = lib.optional (!site.mgmt.dhcp) site.mgmt.gateway;
      dns = site.mgmt.dns;
      linkConfig.RequiredForOnline = "routable";
    };

    netdevs."20-br-core".netdevConfig = {
      Kind = "bridge";
      Name = "br-core";
    };

    networks."20-br-core" = {
      matchConfig.Name = "br-core";
      networkConfig = {
        LinkLocalAddressing = "no";
        IPv6AcceptRA = false;
        ConfigureWithoutCarrier = true;
      };
      linkConfig.RequiredForOnline = "no";
    };
  };

  networking.firewall = {
    enable = true;
    # 管理口只开 SSH、ingress、RKE2 API，以及给分支用的两个 NodePort；
    # Tailscale 自己的 UDP 端口由 tailscale 模块打开。
    allowedTCPPorts = [
      22
      443
      6443
      30080 # 启动镜像（gitops/management/services/boot-images）
      30500 # zot 镜像缓存（gitops/management/platform/registry.yaml）
    ];
    # Pod 网络（Canal：cali* 和 flannel.1）和 tailnet 视为可信。
    trustedInterfaces = [
      "cali+"
      "flannel.1"
      "tailscale0"
    ];
    # Tailscale 和 Kubernetes 都需要宽松的反向路径检查。
    checkReversePath = "loose";
  };
}
