{
  description = "PITS-B1 核心机：NixOS 宿主（管理域）与核心 RKE2 集群";

  inputs = {
    # nixpkgs 从清华 TUNA 的 git 镜像取，国内拉取快；flake.lock 锁定的 rev 和 GitHub 上一致。
    nixpkgs.url = "git+https://mirrors.tuna.tsinghua.edu.cn/git/nixpkgs.git?ref=nixos-26.05&shallow=1";
    # disko 只在 systemDiskLayout = "disko"（重装）时用到，就地 rebuild 时不会下载它。
    disko = {
      url = "git+https://github.com/nix-community/disko?ref=refs/tags/v1.13.0&shallow=1";
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
            ./hosts/core
          ]
          # 只有重装时才引入 disko，这样就地 rebuild 不需要访问 GitHub。
          ++ nixpkgs.lib.optionals (site.systemDiskLayout == "disko") [
            disko.nixosModules.disko
            ./hosts/core/disko.nix
          ];
        };
    in
    {
      nixosConfigurations = {
        # 真机。site/core.nix 里还有占位值时，评估会被 modules/site-check.nix 拦下。
        core = mkCore (import ./site/core.nix);

        # 用示例参数评估，只用于检查模块本身是否正确，不要部署。
        # 两种系统盘方式各评估一份。
        ci-core = mkCore (import ./site/example.nix);
        ci-core-disko =
          let
            example = import ./site/example.nix;
          in
          mkCore (
            example
            // {
              systemDiskLayout = "disko";
              tailscale = example.tailscale // {
                authKeyFile = "/var/lib/pits/secrets/tailscale-authkey";
              };
            }
          );
      };

      checks.${system} = {
        ci-core = self.nixosConfigurations.ci-core.config.system.build.toplevel;
        ci-core-disko = self.nixosConfigurations.ci-core-disko.config.system.build.toplevel;
      };
    };
}
