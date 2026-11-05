# CI 用的站点参数：沿用 core.nix，只补上编造的 SSH 公钥并打开 UPS，
# 让 nixosConfigurations.ci-core 能评估到全部模块。不要部署。
let
  core = import ./core.nix;
in
core
// {
  admin = core.admin // {
    sshKeys = [ "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAMPLEEX example" ];
  };
  ups = core.ups // {
    enable = true;
  };
}
