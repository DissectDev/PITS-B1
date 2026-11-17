{
  description = "Declarative NixOS package and module for PITS Go node daemon (pits-agent)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
    in
    {
      packages.${system} = rec {
        pits-agent = pkgs.callPackage ./package.nix { src = ../../server; };
        default = pits-agent;
      };

      overlays.default = final: _prev: {
        pits-agent = final.callPackage ./package.nix { src = ../../server; };
      };

      nixosModules = rec {
        pits-agent = import ./pits-agent-module.nix;
        default = pits-agent;
      };
    };
}
