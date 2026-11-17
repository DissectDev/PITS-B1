# Candidate host support for container-managed Btrfs. Do not activate alone.
# The caller must remove only the three data NVMe controllers from VFIO first.
{ pkgs, site, ... }:
{
  assertions = [
    {
      assertion = site.passthrough.storageNode == [ ];
      message = "Release the three data NVMe controllers from VFIO before container ownership.";
    }
  ];

  # Btrfs uses the existing host kernel and the kernel page cache.
  boot.supportedFilesystems = [ "btrfs" ];
  boot.kernelModules = [ "btrfs" ];
  environment.systemPackages = [ pkgs.btrfs-progs ];

  # The container owns the filesystem mount and its maintenance schedule.
  # No data-device formatting or mounting is performed by this module.
  systemd.tmpfiles.rules = [
    "d /srv/pits-storage 0750 root root -"
    "d /var/lib/pits/btrfs-manager 0700 root root -"
  ];
}
