# 出国代理：宿主上跑 mihomo，只给拉镜像、拉 chart 和 Git 的几个进程用。
#
# 走代理的：
#   - RKE2：拉自己的运行时镜像，以及 containerd 拉容器镜像（modules/rke2.nix）；
#   - Flux 的 source-controller：Helm chart 和 GitHub 上的 Git 仓库
#     （gitops/clusters/core/flux-system/kustomization.yaml）；
#   - nix-daemon：GitHub 上的下载。两个国内镜像站不走代理，代理停了也照样能装系统。
#     root 默认不经过 daemon，所以这里让 sudo 之后的 nix 命令也走 daemon（NIX_REMOTE=daemon）。
# 宿主的其他流量、VM 和分支都不走它。
#
# 订阅配置（Clash/mihomo 格式）放在 site.proxy.configFile，不进 Git。每次启动 mihomo 前，
# pits-mihomo-config 会读它并改写几项，保证代理只给本机和核心集群的 Pod 用：
#   - 只留一个混合端口 site.proxy.port，去掉其他入站端口、额外的 listeners 和账号密码；
#   - allow-lan 打开，但 lan-allowed-ips 只放行本机和 Pod 网段；防火墙也没有在管理口上开这个端口；
#   - 控制接口只监听 127.0.0.1:9090，去掉网页面板；
#   - 关掉 TUN 和 DNS 监听：宿主的路由和 DNS 不交给 mihomo；
#   - 订阅里没写 geox-url 时，GeoIP、GeoSite 数据改从 jsDelivr 下载（GitHub 在国内经常下不动）；
#   - 分流固定为全部走 PITS 组：这个组包含订阅里的所有节点，自动选延迟最低的。订阅自带的模式
#     （global、direct）、分组里当前选中的节点和分流规则都不起作用。走这个代理的只有拉镜像、
#     拉 chart 和 Git 的几个进程，目标全在国外，不需要分流；也不在本机解析域名，避开 DNS 污染。
# 改了订阅文件后执行 `sudo systemctl restart mihomo`，会重新生成。
{
  lib,
  pkgs,
  site,
  ...
}:
let
  cfg = site.proxy;
  url = "http://127.0.0.1:${toString cfg.port}";
  runtimeDir = "/run/pits-mihomo";

  # 本机和核心集群的 Pod 网段（RKE2 默认值，见 docs/addressing.md）
  allowed = [
    "127.0.0.0/8"
    "::1/128"
    "10.42.0.0/16"
  ];

  overrides = pkgs.writeText "mihomo-overrides.jq" ''
    del(.port, ."socks-port", ."redir-port", ."tproxy-port", .listeners, .authentication,
        ."external-controller-tls", ."external-controller-cors", ."external-ui", ."external-ui-url",
        ."external-ui-name", ."external-doh-server")
    | if (.dns | type) == "object" then .dns |= del(.listen) else . end
    | ."mixed-port" = $port
    | ."allow-lan" = true
    | ."bind-address" = "*"
    | ."lan-allowed-ips" = $allowed
    | ."lan-disallowed-ips" = []
    | ."external-controller" = "127.0.0.1:9090"
    | .tun = ((.tun // {}) | .enable = false)
    | .mode = "rule"
    | del(."rule-providers", ."sub-rules")
    | ."proxy-groups" = ([(."proxy-groups" // [])[] | select(.name != "PITS")] + [{
        "name": "PITS",
        "type": "url-test",
        "include-all": true,
        "url": "https://www.gstatic.com/generate_204",
        "interval": 300,
        "tolerance": 50
      }])
    | .rules = ["MATCH,PITS"]
    | ."geox-url" = (."geox-url" // {
        "geoip": "https://testingcf.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@release/geoip.dat",
        "geosite": "https://testingcf.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@release/geosite.dat",
        "mmdb": "https://testingcf.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@release/geoip.metadb",
        "asn": "https://testingcf.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@release/GeoLite2-ASN.mmdb"
      })
  '';
in
{
  config = lib.mkIf cfg.enable {
    services.mihomo = {
      enable = true;
      # 运行时生成的文件，不在 Nix store 里
      configFile = "${runtimeDir}/config.yaml";
    };

    systemd.services.pits-mihomo-config = {
      description = "Prepare the mihomo config from the subscription file";
      before = [ "mihomo.service" ];
      requiredBy = [ "mihomo.service" ];
      # mihomo 重启时一起重跑，订阅文件的改动随之生效
      partOf = [ "mihomo.service" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      path = with pkgs; [
        coreutils
        jq
        yq-go
      ];
      script = ''
        set -eu
        src=${lib.escapeShellArg cfg.configFile}
        if [ ! -s "$src" ]; then
          echo "pits-mihomo: 找不到订阅配置 $src，见 docs/secrets.md" >&2
          exit 1
        fi
        install -d -m 0700 ${runtimeDir}
        tmp=$(mktemp ${runtimeDir}/config.XXXXXX)
        # 先展开 YAML 的锚点和合并键，再用 jq 改写；mihomo 能直接读 JSON。
        yq -o=json 'explode(.)' "$src" \
          | jq --argjson port ${toString cfg.port} \
               --argjson allowed ${lib.escapeShellArg (builtins.toJSON allowed)} \
               -f ${overrides} > "$tmp"
        chmod 0600 "$tmp"
        mv "$tmp" ${runtimeDir}/config.yaml
      '';
    };

    systemd.services.mihomo = {
      requires = [ "pits-mihomo-config.service" ];
      after = [ "pits-mihomo-config.service" ];
      # 改写规则变了，nixos-rebuild switch 时重启 mihomo，新配置随之生效
      restartTriggers = [ overrides ];
    };

    systemd.services.nix-daemon.environment = {
      http_proxy = url;
      https_proxy = url;
      no_proxy = lib.concatStringsSep "," [
        "localhost"
        "127.0.0.1"
        "mirrors.ustc.edu.cn"
        "mirrors.tuna.tsinghua.edu.cn"
      ];
    };

    # root 能直接写 Nix store，所以 sudo nixos-rebuild 默认不经过 nix-daemon，自己下载，也就用不上
    # 上面给 nix-daemon 配的代理。让所有用户（包括 sudo 之后的 root）都经 daemon 构建和下载。
    environment.variables.NIX_REMOTE = "daemon";
    security.sudo.extraConfig = ''
      Defaults env_keep += "NIX_REMOTE"
    '';

    environment.systemPackages = [ pkgs.mihomo ];
  };
}
