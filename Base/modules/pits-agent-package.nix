# Pure offline Nix derivation for Go `pits-agent`.
# Uses committed `server/vendor` (`vendorHash = null`) so `nixos-rebuild`
# builds reproducibly inside the Nix sandbox without network/proxy access.
{
  lib,
  buildGoModule,
  src ? ../../server,
}:
buildGoModule {
  pname = "pits-agent";
  version = "1.0.0";
  inherit src;

  vendorHash = null;
  subPackages = [ "cmd/pits-agent" ];

  env.CGO_ENABLED = "0";
  ldflags = [
    "-s"
    "-w"
  ];

  doCheck = true;

  meta = with lib; {
    description = "PITS Privileged Host, GoVPP, NVMe/TCP SAN & Lab Bridge Daemon";
    platforms = [ "x86_64-linux" ];
    mainProgram = "pits-agent";
  };
}
