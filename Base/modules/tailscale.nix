# 远程管理通道。只装在宿主上，不设 exit node，不广告路由（宿主不在核心网段和实验区里）。
{ lib, site, ... }:
{
  nixpkgs.overlays = [
    (
      final: prev:
      lib.optionalAttrs (prev.stdenv.hostPlatform.system == "x86_64-linux") {
        tailscale =
          let
            version = "1.104.7";
            src = final.fetchurl {
              url = "https://pkgs.tailscale.com/stable/tailscale_${version}_amd64.tgz";
              hash = "sha256-5WaT/3SmTyWRt+gZwoBjWhAKrh3fsO8Fj80FK3zY0Ww=";
            };
          in
          final.stdenv.mkDerivation {
            pname = "tailscale";
            inherit version src;

            nativeBuildInputs = [ final.makeWrapper ];
            dontConfigure = true;
            dontBuild = true;

            installPhase = ''
              runHook preInstall
              mkdir -p $out/bin $out/lib/systemd/system
              cp tailscale $out/bin/tailscale
              cp tailscaled $out/bin/tailscaled
              chmod 0755 $out/bin/tailscale $out/bin/tailscaled
              wrapProgram $out/bin/tailscaled \
                --prefix PATH : ${
                  lib.makeBinPath [
                    final.getent
                    final.iproute2
                    final.iptables
                    final.shadow
                  ]
                } \
                --suffix PATH : ${lib.makeBinPath [ final.procps ]}
              sed -e "s#/usr/sbin#$out/bin#g" -e "/^EnvironmentFile/d" systemd/tailscaled.service \
                > $out/lib/systemd/system/tailscaled.service
              sed -e "s#/usr/bin#$out/bin#g" systemd/tailscale-wait-online.service \
                > $out/lib/systemd/system/tailscale-wait-online.service
              cp systemd/tailscale-online.target $out/lib/systemd/system/tailscale-online.target
              chmod 0444 $out/lib/systemd/system/*
              runHook postInstall
            '';

            passthru = {
              inherit (prev.tailscale) derper;
            };

            meta = prev.tailscale.meta;
          };
      }
    )
  ];

  services.tailscale = {
    enable = true;
    openFirewall = true;
    useRoutingFeatures = "none";
    authKeyFile = site.tailscale.authKeyFile;
    # Apply on every start, including devices already enrolled in the tailnet.
    extraSetFlags = [ "--ssh=true" ];
    extraUpFlags = [
      "--hostname=${site.hostName}"
      "--accept-dns=false"
    ]
    ++ lib.optional (site.tailscale.tags != [ ]) "--advertise-tags=${lib.concatStringsSep "," site.tailscale.tags}";
  };
}
