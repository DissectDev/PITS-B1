# 远程管理通道。只装在宿主上，不设 exit node，不广告路由（宿主不在核心网段和实验区里）。
{ lib, site, ... }:
{
  services.tailscale = {
    enable = true;
    openFirewall = true;
    useRoutingFeatures = "none";
    authKeyFile = site.tailscale.authKeyFile;
    extraUpFlags = [
      "--hostname=${site.hostName}"
      "--accept-dns=false"
    ]
    ++ lib.optional (site.tailscale.tags != [ ]) "--advertise-tags=${lib.concatStringsSep "," site.tailscale.tags}";
  };
}
