# 声明式 Tailscale Peer Relay（UDP 40000）与本地自建 DERP（TCP 8443 + STUN UDP 3478），
# 在内网或受限 NAT 下减少对公网 DERP 服务器的依赖；不对 WAN 或 Lab(VLAN 20) 暴露开放中继。
{
  config,
  lib,
  pkgs,
  site,
  ...
}:
let
  cfg = config.pits.tailscale;
  relaySetup = pkgs.writeShellScript "pits-tailscale-peer-relay-setup" ''
    set -euo pipefail
    TS="${pkgs.tailscale}/bin/tailscale"
    for _ in $(seq 1 30); do
      if "$TS" status >/dev/null 2>&1; then
        break
      fi
      sleep 1
    done

    ARGS=( "--relay-server-port=${toString cfg.relay.port}" )
    ${lib.optionalString cfg.relay.advertiseStaticEndpoint ''
      EP_LIST=""
      MGMT_IP4=$(${pkgs.iproute2}/bin/ip -4 -o addr show dev mgmt0 2>/dev/null | ${pkgs.gawk}/bin/awk '{print $4}' | cut -d/ -f1 | head -n1 || true)
      if [ -n "$MGMT_IP4" ]; then
        EP_LIST="''${MGMT_IP4}:${toString cfg.relay.port}"
      fi
      if [ -n "$EP_LIST" ]; then
        ARGS+=( "--relay-server-static-endpoints=$EP_LIST" )
      fi
    ''}
    exec "$TS" set "''${ARGS[@]}"
  '';

  derpPreStart = pkgs.writeShellScript "pits-tailscale-derper-cert" ''
    set -euo pipefail
    DIR="/var/lib/pits/tailscale-derp"
    HOST=${lib.escapeShellArg cfg.derper.hostname}
    mkdir -p "$DIR"
    chmod 0700 "$DIR"
    CRT="$DIR/$HOST.crt"
    KEY="$DIR/$HOST.key"
    if [ ! -s "$CRT" ] || [ ! -s "$KEY" ]; then
      CNF=$(mktemp)
      trap 'rm -f "$CNF"' EXIT
      cat > "$CNF" << EOF
[req]
distinguished_name = dn
x509_extensions = v3_ext
prompt = no

[dn]
CN = $HOST

[v3_ext]
basicConstraints = CA:FALSE
keyUsage = digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = IP:$HOST,IP:100.95.95.88,DNS:pits-core.baiji-company.ts.net,DNS:pits-derp.home.arpa
EOF
      ${pkgs.openssl}/bin/openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
        -nodes -days 3650 -keyout "$KEY" -out "$CRT" -config "$CNF"
      chmod 0600 "$KEY"
      chmod 0644 "$CRT"
    fi
  '';
in
{
  options.pits.tailscale = {
    relay = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Enable Tailscale Peer Relay listener on pits-core.";
      };
      port = lib.mkOption {
        type = lib.types.port;
        default = 40000;
        description = "UDP port for Tailscale Peer Relay.";
      };
      advertiseStaticEndpoint = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Advertise mgmt0 IPv4 static endpoint (<mgmt0>:<port>) for local Tailscale peers.";
      };
    };

    derper = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Enable local Tailscale DERP + STUN relay restricted to mgmt LAN, Core VLAN 10, and tailnet.";
      };
      hostname = lib.mkOption {
        type = lib.types.str;
        default = "192.168.71.35";
        description = "Hostname / IP presented by local derper manual TLS cert.";
      };
      port = lib.mkOption {
        type = lib.types.port;
        default = 8443;
        description = "HTTPS port for local Tailscale DERP relay.";
      };
      stunPort = lib.mkOption {
        type = lib.types.port;
        default = 3478;
        description = "UDP STUN port for local Tailscale DERP relay.";
      };
      verifyClients = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Require local tailscaled client verification so only tailnet peers may relay.";
      };
    };
  };

  config = lib.mkMerge [
    (lib.mkIf cfg.relay.enable {
      networking.firewall.allowedUDPPorts = [ cfg.relay.port ];

      systemd.services.pits-tailscale-peer-relay = {
        description = "Configure Tailscale Peer Relay on pits-core";
        wantedBy = [ "multi-user.target" ];
        after = [ "tailscaled.service" "tailscaled-set.service" "network-online.target" ];
        wants = [ "tailscaled.service" "network-online.target" ];
        restartTriggers = [ relaySetup (builtins.toJSON cfg.relay) ];
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          ExecStart = relaySetup;
        };
      };
    })

    (lib.mkIf cfg.derper.enable {
      networking.firewall.extraCommands = ''
        iptables -A nixos-fw -p tcp -s ${site.mgmt.lanCidr} --dport ${toString cfg.derper.port} -j nixos-fw-accept
        iptables -A nixos-fw -p udp -s ${site.mgmt.lanCidr} --dport ${toString cfg.derper.stunPort} -j nixos-fw-accept
        iptables -A nixos-fw -p tcp -s 10.88.0.0/16 --dport ${toString cfg.derper.port} -j nixos-fw-accept
        iptables -A nixos-fw -p udp -s 10.88.0.0/16 --dport ${toString cfg.derper.stunPort} -j nixos-fw-accept
      '';

      systemd.services.pits-tailscale-derper = {
        description = "Local Tailscale DERP + STUN Relay Server";
        wantedBy = [ "multi-user.target" ];
        after = [ "network-online.target" "tailscaled.service" ];
        wants = [ "network-online.target" "tailscaled.service" ];
        restartTriggers = [ derpPreStart (builtins.toJSON cfg.derper) ];
        serviceConfig = {
          Type = "simple";
          Restart = "always";
          RestartSec = "5s";
          ExecStartPre = derpPreStart;
          ExecStart = lib.concatStringsSep " " (
            [
              "${pkgs.tailscale.derper}/bin/derper"
              "-hostname=${cfg.derper.hostname}"
              "-certmode=manual"
              "-certdir=/var/lib/pits/tailscale-derp"
              "-a=:${toString cfg.derper.port}"
              "-http-port=-1"
              "-stun"
              "-stun-port=${toString cfg.derper.stunPort}"
            ]
            ++ lib.optional cfg.derper.verifyClients "-verify-clients"
          );
        };
      };
    })
  ];
}
