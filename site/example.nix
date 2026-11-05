# 示例站点参数：值都是编的，只用于 CI 评估模块（nixosConfigurations.ci-core），不要部署。
import ./core.nix
// {
  mgmt = {
    mac = "58:47:ca:00:00:01";
    dhcp = true;
    address = null;
    gateway = null;
    dns = [ ];
  };
  systemDisk = "/dev/disk/by-id/nvme-EXAMPLE_SYSTEM_DISK";
  passthrough = {
    networkNode = [
      "0000:57:00.0"
      "0000:02:00.0"
      "0000:02:00.1"
    ];
    storageNode = [
      "0000:01:00.0"
      "0000:03:00.0"
      "0000:58:00.0"
    ];
  };
  admin = {
    user = "bill";
    sshKeys = [ "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAMPLEEX example" ];
    hashedPasswordFile = "/var/lib/pits/secrets/admin.hash";
  };
  ups = {
    enable = true;
    name = "main";
    driver = "usbhid-ups";
    port = "auto";
    passwordFile = "/var/lib/pits/secrets/nut-upsmon";
  };
}
