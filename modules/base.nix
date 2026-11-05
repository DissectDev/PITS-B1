# 宿主基础：引导、内核、账号、SSH、Nix、不休眠。
{
  lib,
  pkgs,
  site,
  ...
}:
{
  boot.loader.systemd-boot = {
    enable = true;
    configurationLimit = 20;
  };
  boot.loader.efi.canTouchEfiVariables = true;

  boot.kernelPackages = pkgs.linuxKernel.packages.${site.kernel};
  boot.blacklistedKernelModules = site.disabledKernelModules;

  time.timeZone = site.timeZone;
  i18n.defaultLocale = "en_US.UTF-8";

  # 账号全部声明式管理。admin 是本地账号，不依赖 Keycloak，也是宿主的应急入口。
  users.mutableUsers = false;
  users.users.${site.admin.user} = {
    isNormalUser = true;
    extraGroups = [ "wheel" ];
    openssh.authorizedKeys.keys = site.admin.sshKeys;
    hashedPasswordFile = site.admin.hashedPasswordFile;
  };

  services.openssh = {
    enable = true;
    settings = {
      PasswordAuthentication = false;
      KbdInteractiveAuthentication = false;
      PermitRootLogin = "no";
    };
  };

  nix.settings = {
    experimental-features = [
      "nix-command"
      "flakes"
    ];
    # 国内镜像优先，cache.nixos.org 兜底；签名公钥不变。
    substituters = [
      "https://mirrors.ustc.edu.cn/nix-channels/store"
      "https://mirrors.tuna.tsinghua.edu.cn/nix-channels/store"
      "https://cache.nixos.org"
    ];
    connect-timeout = 5;
    trusted-users = [ "@wheel" ];
    auto-optimise-store = true;
  };
  nix.gc = {
    automatic = true;
    dates = "weekly";
    options = "--delete-older-than 30d";
  };

  # 全天在线的宿主不休眠。
  systemd.sleep.settings.Sleep = {
    AllowSuspend = "no";
    AllowHibernation = "no";
    AllowHybridSleep = "no";
    AllowSuspendThenHibernate = "no";
  };

  environment.systemPackages = with pkgs; [
    git
    vim
    jq
    lsof
    pciutils
    usbutils
    nvme-cli
    ethtool
    tcpdump
    util-linux
  ];

  # 机密文件（密码哈希、Tailscale auth key、S3 密钥）放这里，不进 Git、不进 Nix store。
  systemd.tmpfiles.rules = [ "d /var/lib/pits/secrets 0700 root root -" ];
}
