# Declarative NixOS module for `pits-core` (`/home/libraskywalker/PITS-B1/Base/modules/pits-portal.nix`).
#
# Design principles aligned with PITS-B1 & pure Nix Flake evaluation:
# 1. 100% pure Flake compatible (`nixos-rebuild switch --flake path:/home/libraskywalker/PITS-B1/Base#core`
#    succeeds without `--impure`, without network access, and without committing binaries to Git).
# 2. Runs the PITS Backstage v1.56 Resource Portal & Catalog API directly on `pits-core`, hitting
#    local `https://127.0.0.1:18443` (`pits-agent`) and `https://127.0.0.1:6443` (RKE2) over loopback
#    (<1ms), eliminating Tailscale DERP amplification for control telemetry.
# 3. Defaults to binding strictly `127.0.0.1:18090` and `100.74.25.42:18090` (`tailscale0`),
#    leaving `mgmt0` and Pod CNI interfaces unexposed unless explicitly configured.
# 4. Private operator profile & kubeconfig/mTLS material live in `/var/lib/pits/portal/profile.yaml`
#    (`0600 root:root`), never in `/nix/store` or Git.
{
  config,
  lib,
  pkgs,
  site ? { },
  ...
}:
let
  cfg = config.services.pits-portal;
  portalExec =
    if cfg.package != null then
      "${cfg.package}/bin/pitsctl"
    else
      cfg.binaryPath;
  execArgs =
    [
      portalExec
      "--profile"
      cfg.profilePath
    ]
    ++ lib.optionals (cfg.workspaceSource != null) [
      "--workspace-source"
      cfg.workspaceSource
    ]
    ++ [ "serve" ]
    ++ lib.concatMap (addr: [
      "--listen"
      addr
    ]) cfg.listenAddresses;
in
{
  options.services.pits-portal = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Enable the declarative PITS Backstage v1.56 control-plane portal & API server.";
    };

    package = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = null;
      description = "Optional Nix derivation providing `bin/pitsctl` (e.g. `pkgs.callPackage ./pits-portal-package.nix {}`).";
    };

    binaryPath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/pits/portal/bin/pitsctl";
      description = "Path to built Linux ELF `pitsctl` used when `services.pits-portal.package` is null.";
    };

    listenAddresses = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [
        "127.0.0.1:18090"
        "100.74.25.42:18090"
      ];
      description = "Exact socket addresses to bind (defaulting to loopback + tailscale0 IP so LAN/CNI cannot reach the portal).";
    };

    stateDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/pits/portal";
      description = "Root-private runtime state directory (`PITS_STATE_DIR`) outside `/nix/store`.";
    };

    profilePath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/pits/portal/profile.yaml";
      description = "Root-private PITS operator profile YAML (`0600`) pointing at loopback pits-agent/RKE2/Lab configs.";
    };

    workspaceSource = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Optional path on pits-core holding non-secret `edge-router` hardware contracts.";
    };

    openMgmtFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Also allow TCP 18090 from `site.mgmt.lanCidr` in addition to `tailscale0`.";
    };

    sourcingDbUrlFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Optional root-readable secret file containing a postgres:// URL exported as PITS_SOURCING_DB_URL for multi-host sourcing sync.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = config.networking.hostName == "pits-core";
        message = "services.pits-portal must only be enabled on pits-core.";
      }
      {
        assertion = cfg.listenAddresses != [ ];
        message = "services.pits-portal.listenAddresses must contain at least one address.";
      }
    ];

    systemd.tmpfiles.rules = [
      "d ${cfg.stateDir} 0700 root root -"
      "d ${cfg.stateDir}/bin 0755 root root -"
      "d ${cfg.stateDir}/rdp 0700 root root -"
    ];

    networking.firewall.extraCommands = lib.mkIf (cfg.openMgmtFirewall && (site ? mgmt) && (site.mgmt ? lanCidr)) ''
      iptables -A nixos-fw -p tcp -s ${site.mgmt.lanCidr} --dport 18090 -j nixos-fw-accept
    '';

    systemd.services.pits-portal = {
      description = "PITS Backstage v1.56 Control-Plane Portal & Resource Catalog Server";
      after = [
        "network-online.target"
        "rke2-server.service"
        "tailscaled.service"
        "pits-agent.service"
      ];
      wants = [
        "network-online.target"
        "tailscaled.service"
        "pits-agent.service"
      ];
      wantedBy = [ "multi-user.target" ];

      unitConfig = {
        ConditionPathExists = [
          cfg.profilePath
        ] ++ lib.optional (cfg.package == null) cfg.binaryPath;
      };

      path = with pkgs; [
        coreutils
        gnugrep
        iproute2
      ];

      environment = {
        PITS_STATE_DIR = cfg.stateDir;
        LD_LIBRARY_PATH = lib.makeLibraryPath [ pkgs.sqlite ];
      };

      preStart = ''
        if printf '%s\n' ${lib.escapeShellArgs cfg.listenAddresses} | grep -q '^100\.'; then
          i=0
          while [ "$i" -lt 40 ]; do
            if ip -4 -o addr show dev tailscale0 2>/dev/null | grep -q 'inet 100\.'; then
              break
            fi
            i=$((i + 1))
            sleep 0.5
          done
        fi
      '';

      script =
        if cfg.sourcingDbUrlFile != null then
          ''
            if [ -r ${lib.escapeShellArg cfg.sourcingDbUrlFile} ]; then
              export PITS_SOURCING_DB_URL="$(tr -d '\r\n' < ${lib.escapeShellArg cfg.sourcingDbUrlFile})"
            fi
            exec ${lib.escapeShellArgs execArgs}
          ''
        else
          ''
            exec ${lib.escapeShellArgs execArgs}
          '';

      serviceConfig = {
        Type = "simple";
        User = "root";
        Group = "root";
        UMask = "0077";
        Restart = "on-failure";
        RestartSec = "3s";
        LimitNOFILE = 65536;
      };
    };
  };
}
