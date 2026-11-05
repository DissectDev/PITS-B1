# 宿主网络：只有管理口在用；br-core 是核心机内部 VM 互联用的网桥，不接物理口，宿主不配地址。
# 其余网卡（I226-LM、X710）直通给网络节点，宿主看不到，见 modules/vfio.nix。
{ lib, site, ... }:
{
  networking.useNetworkd = true;
  networking.useDHCP = false;
  services.resolved.enable = true;

  systemd.network = {
    enable = true;

    # 按 PCI 地址匹配管理口并改名，避免网卡枚举顺序变化。
    links."10-mgmt" = {
      matchConfig.Path = "pci-${site.mgmt.pciAddress}";
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
      # 核心机内部网络和实验区经网络节点的上行口访问（见 site.coreNet）。
      routes = map (prefix: {
        Destination = prefix;
        Gateway = site.coreNet.via;
      }) site.coreNet.routedPrefixes;
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
    # 管理端口不对所有来源开放：只接受光猫局域网（IPv4）和核心机内部网络、实验区的连接，
    # tailnet 整体可信。光猫会给宿主分公网 IPv6，所以这里不放行任何 IPv6 来源。
    # 30080、30500 是 NodePort，kube-proxy 在进入这条链之前就转发了，这里列出只是为了说明用途。
    allowedTCPPorts = [ ];
    extraCommands =
      let
        ports = lib.concatStringsSep "," (
          map toString [
            22 # SSH
            443 # ingress
            6443 # RKE2 API
            30080 # 镜像服务（gitops/infrastructure/images）
            30500 # zot 镜像缓存（gitops/management/platform/registry.yaml）
          ]
        );
        sources = [ site.mgmt.lanCidr ] ++ site.coreNet.routedPrefixes;
      in
      lib.concatMapStrings (src: ''
        iptables -A nixos-fw -p tcp -s ${src} -m multiport --dports ${ports} -j nixos-fw-accept
      '') sources;
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
