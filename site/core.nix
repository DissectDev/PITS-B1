# 核心机的站点参数。
#
# 所有因机器、网络、账号而异的值都只在这里填，模块里不写死。
# 先在核心机上跑 scripts/p0-inventory.sh，按它的输出填写带 TODO 的项。
# 还有 TODO 时，`nixos-rebuild` 和 `nix eval .#nixosConfigurations.core...` 都会报错，这是有意的。
{
  hostName = "pits-core";
  timeZone = "Asia/Shanghai";

  # 宿主内核。7.3 是当前版本；NixOS 26.11 发布后换成它的 LTS 内核（例如 "linux_7_x"）。
  kernel = "linux_7_3";

  # 管理口：2.5G 口 A（I226-V，8086:125c），接光猫。按 PCI 地址匹配，并改名为 mgmt0。
  mgmt = {
    pciAddress = "0000:58:00.0";
    # 地址由上游光猫保证（P0 时 DHCP 拿到 192.168.71.35，建议在光猫上做 DHCP 保留）。
    # 改用静态地址时，把 dhcp 设为 false 并填下面三项。
    dhcp = true;
    address = null; # 例如 "192.168.1.10/24"
    gateway = null; # 例如 "192.168.1.1"
    dns = [ ];
  };

  # 系统盘：Crucial P310 1TB（c0a9:5426），0000:5a:00.0。
  #
  # systemDiskLayout 决定系统盘怎么来：
  #   "existing"  沿用盘上现有的分区（按 UUID 挂载），在机器上用 nixos-rebuild 切换，不清盘。
  #   "disko"     由 hosts/core/disko.nix 重新分区，用 nixos-anywhere 装机，会清空系统盘。
  #               系统盘坏了换新盘、或做“空盘重装”演练时用这个。
  systemDiskLayout = "existing";

  # "existing" 时用：取自图形安装器生成的 hardware-configuration.nix。
  existingFileSystems = {
    root = "/dev/disk/by-uuid/d4327997-3d50-4685-a95c-affe9573559d"; # ext4
    boot = "/dev/disk/by-uuid/1738-6257"; # vfat (ESP)
  };

  # "disko" 时用：/dev/disk/by-id 下的名字。disko 只会格式化这一块盘。
  systemDisk = "/dev/disk/by-id/nvme-CT1000P310SSD8_24514CD9A8E1";

  # 直通给 VM 的 PCI 设备，按 PCI 地址写（`lspci -D` 的第一列）。
  # 不要写系统盘和管理口；写错会让宿主失去它们。
  passthrough = {
    # 网络节点：I226-LM 和 X710 的两个口
    networkNode = [
      "0000:5b:00.0" # I226-LM（8086:125b），IOMMU 组 23
      "0000:03:00.0" # X710 端口 1（8086:1572），IOMMU 组 18
      "0000:03:00.1" # X710 端口 2（8086:1572），IOMMU 组 19
    ];
    # 存储节点：三块数据盘，各自独占一个 IOMMU 组
    storageNode = [
      "0000:01:00.0" # Kingston A2000 1TB（2646:2263），IOMMU 组 16
      "0000:02:00.0" # Samsung 980 1TB（144d:a809），IOMMU 组 17
      "0000:59:00.0" # WD_BLACK SN770 2TB（15b7:5017），IOMMU 组 21
    ];
  };

  # 留给宿主（kubelet、containerd、系统服务）的 CPU，其余进入 kubelet 的分配池。
  # P0 已核对：cpu0–7 是 4 个 P 核（4.5 GHz，每核两线程），cpu8–15 是 8 个 E 核（3.3 GHz）。
  cpu.reservedSystemCPUs = "14-15";

  admin = {
    user = "bill";
    sshKeys = [ "ssh-ed25519 TODO" ];
    # 控制台登录和 sudo 用的密码哈希（mkpasswd -m yescrypt 生成），装机时随 --extra-files 放进去。
    hashedPasswordFile = "/var/lib/pits/secrets/admin.hash";
  };

  tailscale = {
    # 机器已经登录过 Tailscale 时填 null：登录状态在 /var/lib/tailscale 里，rebuild 会保留。
    # 重装（systemDiskLayout = "disko"）时改成 "/var/lib/pits/secrets/tailscale-authkey"，
    # 放一个一次性、带标签、短有效期的 auth key。文件不进 Git。
    authKeyFile = null;
    tags = [ "tag:pits-core" ];
  };

  # 有 UPS 时打开。
  ups = {
    enable = false;
    name = "main";
    driver = "usbhid-ups";
    port = "auto";
    passwordFile = "/var/lib/pits/secrets/nut-upsmon";
  };

  rke2 = {
    # nixpkgs 里的 RKE2 包名。KubeVirt 1.9 支持 K8s 1.34–1.36，这里用 1.35。
    # 升级时一次只升一个小版本，并先打 etcd 快照。
    package = "rke2_1_35";
    # 额外的证书 SAN，例如 Tailscale 的 MagicDNS 名字
    tlsSans = [ ];
    # etcd 快照推到机外 S3。访问密钥不写在这里，见 docs/secrets.md。
    etcdS3 = {
      enable = false;
      endpoint = "";
      bucket = "";
      folder = "pits-core";
      region = "";
    };
  };
}
