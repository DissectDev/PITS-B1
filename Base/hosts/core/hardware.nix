# MS-01 的硬件设置。装机后可以用 `nixos-generate-config --show-hardware-config` 的输出核对这里。
# 注意：这里不放 vfio 相关内容，直通统一在 modules/vfio.nix 里处理。
{ modulesPath, ... }:
{
  imports = [ (modulesPath + "/installer/scan/not-detected.nix") ];

  boot.initrd.availableKernelModules = [
    "xhci_pci"
    "thunderbolt"
    "nvme"
    "usbhid"
    "usb_storage"
    "sd_mod"
  ];
  boot.kernelModules = [ "kvm-intel" ];

  hardware.cpu.intel.updateMicrocode = true;
  hardware.enableRedistributableFirmware = true;

  nixpkgs.hostPlatform = "x86_64-linux";
}
