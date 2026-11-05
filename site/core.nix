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

  # 管理口：2.5G 口 A（I226-V），接光猫。按永久 MAC 地址匹配，并改名为 mgmt0。
  mgmt = {
    mac = "TODO:aa:bb:cc:dd:ee:ff";
    # 地址由上游光猫保证（建议在光猫上做 DHCP 保留）。改用静态地址时，把 dhcp 设为 false 并填下面三项。
    dhcp = true;
    address = null; # 例如 "192.168.1.10/24"
    gateway = null; # 例如 "192.168.1.1"
    dns = [ ];
  };

  # 系统盘（/dev/disk/by-id 下的名字）。disko 只会格式化这一块盘。
  systemDisk = "/dev/disk/by-id/nvme-TODO";

  # 直通给 VM 的 PCI 设备，按 PCI 地址写（`lspci -D` 的第一列）。
  # 不要写系统盘和管理口；写错会让宿主失去它们。
  passthrough = {
    # 网络节点：I226-LM 和 X710 的两个口
    networkNode = [
      "TODO-0000:00:00.0" # I226-LM
      "TODO-0000:00:00.0" # X710 端口 1
      "TODO-0000:00:00.1" # X710 端口 2
    ];
    # 存储节点：三块数据盘
    storageNode = [
      "TODO-0000:00:00.0"
      "TODO-0000:00:00.0"
      "TODO-0000:00:00.0"
    ];
  };

  # 留给宿主（kubelet、containerd、系统服务）的 CPU，其余进入 kubelet 的分配池。
  # 默认假设 P 核线程在前（cpu0–7）；用 `lscpu --extended` 核对后再定。
  cpu.reservedSystemCPUs = "14-15";

  admin = {
    user = "bill";
    sshKeys = [ "ssh-ed25519 TODO" ];
    # 控制台登录和 sudo 用的密码哈希（mkpasswd -m yescrypt 生成），装机时随 --extra-files 放进去。
    hashedPasswordFile = "/var/lib/pits/secrets/admin.hash";
  };

  tailscale = {
    # 一次性、带标签、短有效期的 auth key。文件不进 Git。
    authKeyFile = "/var/lib/pits/secrets/tailscale-authkey";
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
