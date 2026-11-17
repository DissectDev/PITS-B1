# Declarative NixOS module for `pits-core` (`/home/libraskywalker/PITS-B1/Base/modules/pits-agent.nix`).
#
# Design principles aligned with PITS-B1 & pure Nix Flake evaluation:
# 1. 100% pure Flake compatible (`nixos-rebuild switch --flake path:/home/libraskywalker/PITS-B1/Base#core`
#    succeeds without `--impure`, without network access, and without committing binaries to Git).
# 2. Optional `services.pits-agent.package` builds from Go source + vendored dependencies via
#    `./package.nix` (`vendorHash = null`), while `binaryPath` defaults to the SHA-256 verified
#    static Linux ELF staged at `/var/lib/pits/agent/bin/pits-agent`.
# 3. Private mTLS keys reside in root-only `/var/lib/pits/agent/pki` (`0700`), never in `/nix/store` or Git.
# 4. `tailscale0` (`100.74.25.42`) is already in `networking.firewall.trustedInterfaces` for `tag:admin`;
#    management LAN (`192.168.71.0/24`) access is opt-in via `services.pits-agent.openMgmtFirewall`.
{
  config,
  lib,
  pkgs,
  site ? { },
  ...
}:
let
  cfg = config.services.pits-agent;
  agentExec =
    if cfg.package != null then
      "${cfg.package}/bin/pits-agent"
    else
      cfg.binaryPath;
in
{
  options.services.pits-agent = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Enable the declarative PITS privileged Go control daemon (pits-agent).";
    };

    package = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = null;
      description = "Optional Nix derivation providing `bin/pits-agent` (e.g. `pkgs.callPackage ./package.nix {}`).";
    };

    binaryPath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/pits/agent/bin/pits-agent";
      description = "Staged static ELF path used when `services.pits-agent.package` is null.";
    };

    listenAddress = lib.mkOption {
      type = lib.types.str;
      default = "0.0.0.0:18443";
      description = "mTLS 1.3 listen address (`100.74.25.42:18443` on Tailscale).";
    };

    pkiDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/pits/agent/pki";
      description = "Root-private directory holding `ca.crt`, `server.crt`, and `server.key` (outside Nix store).";
    };

    kubeconfigPath = lib.mkOption {
      type = lib.types.str;
      default = "/etc/rancher/rke2/rke2.yaml";
      description = "Path to host RKE2 admin kubeconfig.";
    };

    btrfsMount = lib.mkOption {
      type = lib.types.str;
      default = "/srv/pits-storage/tank";
      description = "Mount point of `pits-data` Btrfs pool (`777b7660-ef8e-421a-ac60-d6c4cb2fea39`).";
    };

    btrfsStateDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/pits/btrfs-manager";
      description = "Persistent maintenance state directory shared with `pits-storage/btrfs-manager`.";
    };

    sanRoot = lib.mkOption {
      type = lib.types.str;
      default = "/srv/pits-storage/tank/@data/san";
      description = "NVMe/TCP SAN NOCOW raw backing extents root.";
    };

    pxeReportsDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/pits/tinkerbell-20261108/reports";
      description = "Hook non-destructive hardware discovery JSON directory.";
    };

    pxeAssetsDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/pits/tinkerbell-20261108/public/talos";
      description = "Staged Talos diskless `vmlinuz` and `initramfs.xz` directory.";
    };

    labNetns = lib.mkOption {
      type = lib.types.str;
      default = "auto";
      description = "Lab VLAN 20 network namespace (`auto` discovers live Multus macvlan Pod via `/proc`).";
    };

    openMgmtFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Also allow TCP 18443 from `site.mgmt.lanCidr` in addition to `tailscale0`.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = config.networking.hostName == "pits-core";
        message = "services.pits-agent must only be enabled on pits-core.";
      }
    ];

    systemd.tmpfiles.rules = [
      "d /var/lib/pits/agent 0700 root root -"
      "d /var/lib/pits/agent/bin 0755 root root -"
      "d ${cfg.pkiDir} 0700 root root -"
    ];

    networking.firewall.extraCommands = lib.mkIf (cfg.openMgmtFirewall && (site ? mgmt) && (site.mgmt ? lanCidr)) ''
      iptables -A nixos-fw -p tcp -s ${site.mgmt.lanCidr} --dport 18443 -j nixos-fw-accept
    '';

    systemd.services.pits-agent = {
      description = "PITS Privileged Host, GoVPP, NVMe/TCP SAN & Lab VLAN 20 Bridge Daemon";
      after = [
        "network-online.target"
        "rke2-server.service"
        "tailscaled.service"
      ];
      wants = [ "network-online.target" ];
      wantedBy = [ "multi-user.target" ];

      unitConfig = {
        ConditionPathExists = [
          "${cfg.pkiDir}/server.key"
        ] ++ lib.optional (cfg.package == null) cfg.binaryPath;
      };

      path = with pkgs; [
        iproute2
        kmod
        util-linux
        btrfs-progs
      ];

      serviceConfig = {
        Type = "simple";
        User = "root";
        Group = "root";
        UMask = "0077";
        ExecStart = lib.escapeShellArgs [
          agentExec
          "-listen"
          cfg.listenAddress
          "-tls-ca"
          "${cfg.pkiDir}/ca.crt"
          "-tls-cert"
          "${cfg.pkiDir}/server.crt"
          "-tls-key"
          "${cfg.pkiDir}/server.key"
          "-kubeconfig"
          cfg.kubeconfigPath
          "-btrfs-mount"
          cfg.btrfsMount
          "-btrfs-state"
          cfg.btrfsStateDir
          "-san-root"
          cfg.sanRoot
          "-pxe-reports"
          cfg.pxeReportsDir
          "-pxe-assets"
          cfg.pxeAssetsDir
          "-lab-netns"
          cfg.labNetns
        ];
        Restart = "on-failure";
        RestartSec = "3s";
        LimitNOFILE = 65536;
      };
    };
  };
}
