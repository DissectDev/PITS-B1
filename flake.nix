{
  description = "PITS-B1 核心机：NixOS 宿主（管理域）与核心 RKE2 集群";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    disko = {
      url = "github:nix-community/disko/v1.13.0";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      disko,
      ...
    }:
    let
      system = "x86_64-linux";

      # 同一套模块，换一份站点参数就是另一台机器（产品线里的“变体”）。
      mkCore =
        site:
        nixpkgs.lib.nixosSystem {
          specialArgs = { inherit site; };
          modules = [
            disko.nixosModules.disko
            ./hosts/core
          ];
        };
    in
    {
      nixosConfigurations = {
        # 真机。site/core.nix 里还有占位值时，评估会被 modules/site-check.nix 拦下。
        core = mkCore (import ./site/core.nix);

        # 用示例参数评估，只用于检查模块本身是否正确，不要部署。
        ci-core = mkCore (import ./site/example.nix);
      };

      checks.${system}.ci-core = self.nixosConfigurations.ci-core.config.system.build.toplevel;
    };
}
