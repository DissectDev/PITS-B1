# Imported by PITS-B1/hosts/core/default.nix. Host has no address on these VLANs.
{ ... }:
{
  boot.kernelModules = [ "nvmet_tcp" ];
  systemd.network.netdevs."30-netboot-core" = {
    netdevConfig = { Name = "sfp0.10"; Kind = "vlan"; };
    vlanConfig.Id = 10;
  };
  systemd.network.netdevs."30-netboot-lab" = {
    netdevConfig = { Name = "sfp0.20"; Kind = "vlan"; };
    vlanConfig.Id = 20;
  };
  systemd.network.networks."20-sfp0".vlan = [ "sfp0.10" "sfp0.20" ];
  systemd.network.networks."30-netboot-core" = {
    matchConfig.Name = "sfp0.10";
    networkConfig = {
      LinkLocalAddressing = "no";
      IPv6AcceptRA = false;
      ConfigureWithoutCarrier = true;
    };
    linkConfig.RequiredForOnline = "no";
  };
  systemd.network.networks."30-netboot-lab" = {
    matchConfig.Name = "sfp0.20";
    networkConfig = {
      LinkLocalAddressing = "no";
      IPv6AcceptRA = false;
      ConfigureWithoutCarrier = true;
    };
    linkConfig.RequiredForOnline = "no";
  };
}
