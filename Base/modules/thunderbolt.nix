# 高速 Thunderbolt / USB4 IPv4 数据平面（10.90.0.0/16，MTU 9000）与 Linux 7.3+ thunderbolt_stream / busy_poll 声明式模块。
# 宿主机与实验区严格三层隔离，不桥接 Core(VLAN 10) 或 Lab(VLAN 20)。
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.pits.thunderbolt;
  validHopId = h: h == -1 || h == 0 || (h >= 8 && h <= 65535);
  reconcileScript = pkgs.writeShellScript "pits-thunderbolt-reconcile" ''
    set -euo pipefail
    MODPROBE="${pkgs.kmod}/bin/modprobe"
    "$MODPROBE" configfs >/dev/null 2>&1 || true
    "$MODPROBE" thunderbolt >/dev/null 2>&1 || true
    "$MODPROBE" thunderbolt_net >/dev/null 2>&1 || true
    ${lib.optionalString cfg.stream.enable ''
      "$MODPROBE" thunderbolt_stream >/dev/null 2>&1 || true
    ''}

    if [ ! -d /sys/kernel/config/thunderbolt ] && ! mountpoint -q /sys/kernel/config 2>/dev/null; then
      mount -t configfs none /sys/kernel/config 2>/dev/null || true
    fi

    # Auto-authorize pending Thunderbolt devices and keep power control active.
    for dev in /sys/bus/thunderbolt/devices/*; do
      [ -e "$dev/authorized" ] || continue
      if [ "$(cat "$dev/authorized" 2>/dev/null || true)" = "0" ]; then
        echo 1 > "$dev/authorized" 2>/dev/null || true
      fi
      if [ -w "$dev/power/control" ] && [ "$(cat "$dev/power/control" 2>/dev/null || true)" = "auto" ]; then
        echo on > "$dev/power/control" 2>/dev/null || true
      fi
    done

    ${lib.optionalString cfg.stream.enable ''
      BASE="/sys/kernel/config/thunderbolt/stream"
      if [ -d "$BASE" ]; then
        write_if_diff() {
          local path="$1" val="$2"
          local cur=""
          [ -r "$path" ] && cur="$(tr -d ' \n' < "$path" 2>/dev/null || true)"
          if [ "$cur" != "$val" ]; then
            printf '%s\n' "$val" > "$path"
          fi
        }
        reconcile_hopid() {
          local path="$1" target="$2"
          local cur=""
          [ -r "$path" ] && cur="$(tr -d ' \n' < "$path" 2>/dev/null || true)"
          if [ "$target" = "-1" ]; then
            if [ -z "$cur" ] || [ "$cur" = "0" ]; then
              printf '%s\n' "-1" > "$path"
            fi
          elif [ "$cur" != "$target" ]; then
            printf '%s\n' "$target" > "$path"
          fi
        }

        SERVICES="${lib.concatStringsSep " " cfg.stream.services}"
        for dom in /sys/bus/thunderbolt/devices/domain*; do
          [ -e "$dom" ] || continue
          idx="''${dom##*domain}"
          cand="''${idx}-1.0"
          case " $SERVICES " in
            *" $cand "*) ;;
            *) SERVICES="$SERVICES $cand" ;;
          esac
        done

        STREAM_NAME=${lib.escapeShellArg cfg.stream.name}
        BUSY_POLL_VAL="${if cfg.stream.busyPoll then "1" else "0"}"
        RING_SIZE="${toString cfg.stream.ringSize}"
        THROTTLING="${toString cfg.stream.throttlingNs}"
        IN_HOP="${toString cfg.stream.inHopId}"
        OUT_HOP="${toString cfg.stream.outHopId}"

        for svc in $SERVICES; do
          [ -n "$svc" ] || continue
          dom_idx="''${svc%%-*}"
          if [ ! -e "/sys/bus/thunderbolt/devices/domain''${dom_idx}" ] && [ ! -d "$BASE/$svc" ]; then
            continue
          fi
          mkdir -p "$BASE/$svc/$STREAM_NAME" 2>/dev/null || continue
          grp="$BASE/$svc/$STREAM_NAME"
          apply_attrs() {
            write_if_diff "$grp/ring_size" "$RING_SIZE"
            write_if_diff "$grp/throttling" "$THROTTLING"
            if [ -e "$grp/busy_poll" ]; then
              write_if_diff "$grp/busy_poll" "$BUSY_POLL_VAL"
            fi
          }
          if ! apply_attrs 2>/dev/null; then
            echo 0 > "$grp/in_hopid" 2>/dev/null || true
            echo 0 > "$grp/out_hopid" 2>/dev/null || true
            apply_attrs
          fi
          reconcile_hopid "$grp/in_hopid" "$IN_HOP"
          reconcile_hopid "$grp/out_hopid" "$OUT_HOP"
        done
      fi
    ''}

    ${lib.optionalString cfg.busyPoll.enable ''
      for ifc in ${lib.concatStringsSep " " cfg.busyPoll.interfaces}; do
        [ -d "/sys/class/net/$ifc" ] || continue
        [ -w "/sys/class/net/$ifc/napi_defer_hard_irqs" ] && echo "${toString cfg.busyPoll.napiDeferHardIrqs}" > "/sys/class/net/$ifc/napi_defer_hard_irqs" 2>/dev/null || true
        [ -w "/sys/class/net/$ifc/gro_flush_timeout" ] && echo "${toString cfg.busyPoll.groFlushTimeoutNs}" > "/sys/class/net/$ifc/gro_flush_timeout" 2>/dev/null || true
      done
    ''}
  '';
in
{
  options.pits.thunderbolt = {
    busyPoll = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Enable declarative network stack socket busy_poll/busy_read and tb* NAPI deferral.";
      };
      socketBusyPollUs = lib.mkOption {
        type = lib.types.ints.between 0 100000;
        default = 50;
        description = "Global net.core.busy_poll sysctl in microseconds.";
      };
      socketBusyReadUs = lib.mkOption {
        type = lib.types.ints.between 0 100000;
        default = 50;
        description = "Global net.core.busy_read sysctl in microseconds.";
      };
      interfaces = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ "tb0" "tb1" ];
        description = "Thunderbolt-net interfaces tuned with NAPI busy polling.";
      };
      napiDeferHardIrqs = lib.mkOption {
        type = lib.types.ints.between 0 1024;
        default = 2;
        description = "NAPI hard IRQ deferral cycles on thunderbolt-net interfaces.";
      };
      groFlushTimeoutNs = lib.mkOption {
        type = lib.types.ints.between 0 100000000;
        default = 200000;
        description = "GRO flush timeout in nanoseconds for NAPI busy polling.";
      };
    };

    stream = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Enable Linux 7.3+ thunderbolt_stream kernel module and ConfigFS channel reconciliation.";
      };
      name = lib.mkOption {
        type = lib.types.str;
        default = "pits0";
        description = "Stream child group name under /sys/kernel/config/thunderbolt/stream/<service>/.";
      };
      services = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ "0-1.0" "1-1.0" ];
        description = "Thunderbolt stream service paths (host domain instances) to reconcile.";
      };
      busyPoll = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Enable Linux 7.3+ ConfigFS busy_poll attribute on provisioned thunderbolt_stream channels.";
      };
      ringSize = lib.mkOption {
        type = lib.types.ints.between 32 4096;
        default = 256;
        description = "ConfigFS ring_size (32..=4096) for thunderbolt_stream channels.";
      };
      throttlingNs = lib.mkOption {
        type = lib.types.ints.unsigned;
        default = 0;
        description = "ConfigFS throttling delay in nanoseconds (0 for lowest latency).";
      };
      inHopId = lib.mkOption {
        type = lib.types.int;
        default = -1;
        description = "Input HopID (-1 = kernel auto-allocate >= 8, 0 = deallocate, or >= 8).";
      };
      outHopId = lib.mkOption {
        type = lib.types.int;
        default = -1;
        description = "Output HopID (-1 = kernel auto-allocate >= 8, 0 = deallocate, or >= 8).";
      };
    };
  };

  config = {
    assertions = [
      {
        assertion = cfg.stream.name != "" && builtins.match "^[A-Za-z0-9._-]+$" cfg.stream.name != null;
        message = "pits.thunderbolt.stream.name must be non-empty ASCII matching [A-Za-z0-9._-]+";
      }
      {
        assertion = validHopId cfg.stream.inHopId && validHopId cfg.stream.outHopId;
        message = "pits.thunderbolt.stream HopIDs must be -1 (auto), 0 (unused), or 8..=65535";
      }
    ];

    boot.kernelModules = [
      "thunderbolt"
      "thunderbolt_net"
    ] ++ lib.optional cfg.stream.enable "thunderbolt_stream";

    boot.kernel.sysctl = lib.mkIf cfg.busyPoll.enable {
      "net.core.busy_poll" = cfg.busyPoll.socketBusyPollUs;
      "net.core.busy_read" = cfg.busyPoll.socketBusyReadUs;
    };

    # 自动授权 XDomain 点对点网络设备并为 thunderbolt-net 启用 NAPI busy_poll 延迟中断
    services.udev.extraRules = ''
      ACTION=="add", SUBSYSTEM=="thunderbolt", ATTR{authorized}=="0", ATTR{authorized}="1"
    '' + lib.optionalString cfg.busyPoll.enable ''
      SUBSYSTEM=="net", ACTION=="add|move", DRIVERS=="thunderbolt-net", ATTR{napi_defer_hard_irqs}="${toString cfg.busyPoll.napiDeferHardIrqs}", ATTR{gro_flush_timeout}="${toString cfg.busyPoll.groFlushTimeoutNs}"
    '';

    systemd.services.pits-thunderbolt-stream = {
      description = "Reconcile PITS thunderbolt_stream ConfigFS busy_poll and NAPI fabric parameters";
      wantedBy = [ "multi-user.target" ];
      after = [ "systemd-modules-load.service" "systemd-udev-settle.service" ];
      restartTriggers = [
        reconcileScript
        (builtins.toJSON cfg)
      ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = reconcileScript;
      };
    };

    systemd.network = {
      netdevs."30-tb-lo0" = {
        netdevConfig = {
          Name = "tb-lo0";
          Kind = "dummy";
        };
      };

      networks."30-tb-lo0" = {
        matchConfig.Name = "tb-lo0";
        address = [ "10.90.0.1/32" ];
        networkConfig = {
          ConfigureWithoutCarrier = true;
          LinkLocalAddressing = "no";
          IPv6AcceptRA = false;
        };
        linkConfig.RequiredForOnline = "no";
      };

      links."30-tb0" = {
        matchConfig = {
          Driver = "thunderbolt-net";
          Path = "pci-0000:00:0d.2*";
        };
        linkConfig = {
          Name = "tb0";
          MTUBytes = "9000";
        };
      };

      links."30-tb1" = {
        matchConfig = {
          Driver = "thunderbolt-net";
          Path = "pci-0000:00:0d.3*";
        };
        linkConfig = {
          Name = "tb1";
          MTUBytes = "9000";
        };
      };

      networks."30-tb0" = {
        matchConfig.Name = "tb0";
        address = [ "10.90.1.1/30" ];
        networkConfig = {
          ConfigureWithoutCarrier = true;
          LinkLocalAddressing = "ipv6";
          IPv6AcceptRA = false;
        };
        linkConfig = {
          MTUBytes = "9000";
          RequiredForOnline = "no";
        };
      };

      networks."30-tb1" = {
        matchConfig.Name = "tb1";
        address = [ "10.90.2.1/30" ];
        networkConfig = {
          ConfigureWithoutCarrier = true;
          LinkLocalAddressing = "ipv6";
          IPv6AcceptRA = false;
        };
        linkConfig = {
          MTUBytes = "9000";
          RequiredForOnline = "no";
        };
      };
    };

    # 零信任隔离：不把 tb+ 加入 trustedInterfaces；只放行 10.90.0.0/16 的 ICMP 连通性探测，
    # 继续阻断从实验区经 Thunderbolt 直连宿主 SSH/RKE2/Agent/Portal 等管理端口。
    networking.firewall.extraCommands = ''
      iptables -A nixos-fw -i tb+ -s 10.90.0.0/16 -p icmp -j nixos-fw-accept
    '';
  };
}
