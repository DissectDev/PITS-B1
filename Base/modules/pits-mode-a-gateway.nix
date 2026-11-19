# Declarative NixOS module for Mode-A Pre-Gateway (PEP) on `pits-core`.
#
# Enforces dual-factor Zero-Trust access in front of `pits-portal` (127.0.0.1:18090):
#   1. Machine identity & Device Posture via local `tailscaled` Whois socket
#      (`/var/run/tailscale/tailscaled.sock`) — rejects non-Tailscale, expired, or
#      improperly tagged peers.
#   2. Human identity & FIDO2 hardware-key strength (`acr=fido2`) via Keycloak OIDC
#      Authorization Code + PKCE / Bearer JWT verification (`https://keycloak.pits.home.arpa/realms/corp`).
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.pits-mode-a-gateway;
  gatewayExec =
    if cfg.package != null then
      "${cfg.package}/bin/pits-pep-gateway"
    else
      cfg.binaryPath;

  execArgs =
    [
      gatewayExec
      "-listen"
      cfg.listenAddress
      "-upstream"
      cfg.upstreamUrl
      "-tailscaled-sock"
      cfg.tailscaledSocket
      "-require-user-owned=${lib.boolToString cfg.requireUserOwned}"
      "-allowed-os"
      (lib.concatStringsSep "," cfg.allowedOS)
      "-oidc-issuer"
      cfg.oidcIssuer
      "-external-base-url"
      cfg.externalBaseUrl
      "-oidc-client-id"
      cfg.oidcClientId
      "-oidc-client-secret-file"
      cfg.oidcClientSecretFile
      "-oidc-ca-cert"
      cfg.caCertPath
      "-oidc-dial-override"
      cfg.oidcDialOverride
      "-required-acr"
      (lib.concatStringsSep "," cfg.requiredAcrValues)
      "-session-key-file"
      cfg.sessionKeyFile
      "-tls-cert"
      cfg.tlsCertPath
      "-tls-key"
      cfg.tlsKeyPath
    ]
    ++ lib.optionals (cfg.allowedTags != [ ]) [
      "-allowed-tags"
      (lib.concatStringsSep "," cfg.allowedTags)
    ]
    ++ lib.optionals (cfg.allowedUsers != [ ]) [
      "-allowed-users"
      (lib.concatStringsSep "," cfg.allowedUsers)
    ];
in
{
  options.services.pits-mode-a-gateway = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Enable the Mode-A PEP Gateway protecting pits-portal via Tailscale Whois + Keycloak FIDO2 OIDC.";
    };

    package = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = null;
      description = "Optional Nix derivation providing bin/pits-pep-gateway.";
    };

    binaryPath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/pits/pep-gateway/bin/pits-pep-gateway";
      description = "Staged static Linux ELF binary path used when package is null.";
    };

    stateDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/pits/pep-gateway";
      description = "Root-private state directory holding TLS cert/key, OIDC client secret, and session key.";
    };

    listenAddress = lib.mkOption {
      type = lib.types.str;
      default = "100.95.95.88:18444";
      description = "Tailscale interface HTTPS listen address for Mode-A PEP Gateway.";
    };

    upstreamUrl = lib.mkOption {
      type = lib.types.str;
      default = "http://127.0.0.1:18090";
      description = "Protected loopback upstream service URL (pits-portal).";
    };

    tailscaledSocket = lib.mkOption {
      type = lib.types.str;
      default = "/var/run/tailscale/tailscaled.sock";
      description = "Path to local tailscaled LocalAPI Unix socket.";
    };

    requireUserOwned = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Reject tagged devices unless explicitly listed in allowedTags (preserves employee identity).";
    };

    allowedTags = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "Optional list of allowed Tailscale infrastructure tags.";
    };

    allowedUsers = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "Optional allowlist of Tailscale user login names (empty allows any user-owned tailnet peer).";
    };

    allowedOS = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [
        "windows"
        "linux"
        "macos"
      ];
      description = "Allowed Tailscale peer operating systems.";
    };

    oidcIssuer = lib.mkOption {
      type = lib.types.str;
      default = "https://sso.martensite.dev/realms/corp";
      description = "Keycloak OIDC realm issuer URL.";
    };

    externalBaseUrl = lib.mkOption {
      type = lib.types.str;
      default = "https://100.95.95.88:18444";
      description = "External base URL used for OIDC callback redirect_uri.";
    };

    oidcClientId = lib.mkOption {
      type = lib.types.str;
      default = "pits-mode-a-gateway";
      description = "Keycloak OIDC clientId for Mode-A PEP Gateway.";
    };

    oidcClientSecretFile = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/pits/pep-gateway/oidc-client-secret";
      description = "Root-only file containing OIDC client secret.";
    };

    oidcDialOverride = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1:443";
      description = "Local ingress dial override when reaching keycloak.pits.home.arpa.";
    };

    caCertPath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/pits/pep-gateway/ca.crt";
      description = "Path to internal CA PEM (pits-ca) trusted for Keycloak TLS.";
    };

    requiredAcrValues = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ "fido2" ];
      description = "Accepted OIDC ACR values proving hardware FIDO2 authentication.";
    };

    sessionKeyFile = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/pits/pep-gateway/session.key";
      description = "Root-only 32-byte hex AES-GCM session encryption key file.";
    };

    tlsCertPath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/pits/pep-gateway/tls.crt";
      description = "Gateway HTTPS server certificate PEM.";
    };

    tlsKeyPath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/pits/pep-gateway/tls.key";
      description = "Gateway HTTPS server private key PEM.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = config.networking.hostName == "pits-core";
        message = "services.pits-mode-a-gateway must only be enabled on pits-core.";
      }
    ];

    systemd.tmpfiles.rules = [
      "d ${cfg.stateDir} 0700 root root -"
      "d ${cfg.stateDir}/bin 0755 root root -"
    ];

    systemd.services.pits-mode-a-gateway = {
      description = "PITS Mode-A Zero-Trust PEP Gateway (Tailscale Whois + Keycloak FIDO2)";
      after = [
        "network-online.target"
        "tailscaled.service"
        "pits-portal.service"
        "rke2-server.service"
      ];
      wants = [
        "network-online.target"
        "tailscaled.service"
        "pits-portal.service"
      ];
      wantedBy = [ "multi-user.target" ];

      unitConfig = {
        ConditionPathExists = [
          cfg.oidcClientSecretFile
          cfg.caCertPath
          cfg.tlsCertPath
          cfg.tlsKeyPath
        ] ++ lib.optional (cfg.package == null) cfg.binaryPath;
      };

      path = with pkgs; [
        coreutils
        gnugrep
        iproute2
      ];

      preStart = ''
        if printf '%s\n' ${lib.escapeShellArg cfg.listenAddress} | grep -q '^100\.'; then
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

      script = ''
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
