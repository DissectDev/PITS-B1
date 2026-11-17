{ pkgs }:
pkgs.dockerTools.buildLayeredImage {
  name = "pits-btrfs-manager";
  tag = "6.19.1-20261108";
  contents = with pkgs; [
    bashInteractive
    btrfs-progs
    coreutils
    util-linux
    gnugrep
    findutils
    gawk
    tzdata
  ];
  extraCommands = ''
    mkdir -p tmp devices config state srv/pits-storage
    chmod 1777 tmp
  '';
  config = {
    Entrypoint = [ "/bin/bash" "/config/btrfs-manager.sh" ];
    Cmd = [ "manage" ];
    Env = [ "PATH=/bin:/sbin" "TZ=Asia/Shanghai" "TZDIR=/share/zoneinfo" ];
    WorkingDir = "/";
    User = "0:0";
  };
}
