# 系统盘分区。只描述系统盘：三块数据盘直通给存储节点，绝不能出现在这里，
# 否则重装时会被格式化。
#
# 系统盘上的内容：
#   /boot                ESP
#   /                    NixOS
#   /var/lib/rancher     RKE2 的 etcd 和容器镜像（在根分区里）
#   /var/lib/pits-local  local-path 本地卷：管理服务的数据、各台 VM 的系统盘（在根分区里，需要备份）
{ site, ... }:
{
  disko.devices.disk.system = {
    type = "disk";
    device = site.systemDisk;
    content = {
      type = "gpt";
      partitions = {
        ESP = {
          size = "1G";
          type = "EF00";
          content = {
            type = "filesystem";
            format = "vfat";
            mountpoint = "/boot";
            mountOptions = [ "umask=0077" ];
          };
        };
        root = {
          size = "100%";
          content = {
            type = "filesystem";
            format = "ext4";
            mountpoint = "/";
          };
        };
      };
    };
  };
}
