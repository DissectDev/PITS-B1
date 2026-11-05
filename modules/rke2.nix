# 核心 RKE2 单节点集群。
#
# - CNI 是 multus + canal：nixpkgs 的 services.rke2.cni 只能选一个，所以 cni 写在 config.yaml 里。
# - config.yaml 放在 /etc/rancher/rke2/ 下，RKE2 还会读同目录 config.yaml.d/*.yaml，
#   机密（etcd S3 密钥）放在 config.yaml.d/90-secrets.yaml，不进 Git 和 Nix store。
# - CPU Manager 必须在 kubelet 第一次启动前设好，之后改动要清 cpu_manager_state。
# - 拉镜像走宿主上的代理（modules/proxy.nix）。RKE2 自己拉运行时镜像、containerd 拉容器镜像都用它；
#   内网地址都在 NO_PROXY 里，RKE2 还会自动补上集群的 Pod 和 Service 网段。
#   镜像拉下来以后留在本地，之后重启不再需要代理。
{
  lib,
  pkgs,
  site,
  ...
}:
let
  yaml = pkgs.formats.yaml { };
  rke2 = pkgs.${site.rke2.package};
  proxyUrl = "http://127.0.0.1:${toString site.proxy.port}";

  rke2Config = {
    cni = [
      "multus"
      "canal"
    ];
    write-kubeconfig-mode = "0600";
    tls-san = [ site.hostName ] ++ site.rke2.tlsSans;
    # RKE2 的默认值，写出来是为了让快照节奏在代码里可见。
    etcd-snapshot-schedule-cron = "0 */12 * * *";
    etcd-snapshot-retention = 5;
  }
  // lib.optionalAttrs site.rke2.etcdS3.enable {
    etcd-s3 = true;
    etcd-s3-endpoint = site.rke2.etcdS3.endpoint;
    etcd-s3-bucket = site.rke2.etcdS3.bucket;
    etcd-s3-folder = site.rke2.etcdS3.folder;
  }
  // lib.optionalAttrs (site.rke2.etcdS3.enable && site.rke2.etcdS3.region != "") {
    etcd-s3-region = site.rke2.etcdS3.region;
  };
in
{
  services.rke2 = {
    enable = true;
    role = "server";
    package = rke2;
    nodeName = site.hostName;
    nodeLabel = [ "pits.io/fault-domain=core" ];

    extraKubeletConfig = {
      cpuManagerPolicy = "static";
      reservedSystemCPUs = site.cpu.reservedSystemCPUs;
    };

    # 离线镜像包：启动时导入 containerd，集群的系统组件不用再从 docker.io 拉。
    images = lib.optionals site.rke2.airgapImages [
      rke2.images-core-linux-amd64-tar-zst
      rke2.images-canal-linux-amd64-tar-zst
      rke2.images-multus-linux-amd64-tar-zst
    ];

    # 关机（包括 UPS 触发的关机）时先让 Pod 和 VM 优雅退出。
    gracefulNodeShutdown = {
      enable = true;
      shutdownGracePeriod = "300s";
      shutdownGracePeriodCriticalPods = "60s";
    };
  };

  systemd.services.rke2-server = lib.mkIf site.proxy.enable {
    # wants 而不是 requires：代理起不来时 RKE2 照样启动，用本地已有的镜像。
    wants = [ "mihomo.service" ];
    after = [ "mihomo.service" ];
    environment = {
      HTTP_PROXY = proxyUrl;
      HTTPS_PROXY = proxyUrl;
      NO_PROXY = lib.concatStringsSep "," [
        "localhost"
        "127.0.0.0/8"
        "10.0.0.0/8"
        "172.16.0.0/12"
        "192.168.0.0/16"
        "100.64.0.0/10"
        ".svc"
        ".cluster.local"
        ".home.arpa"
      ];
    };
  };

  environment.etc."rancher/rke2/config.yaml".source = yaml.generate "rke2-config.yaml" rke2Config;

  systemd.tmpfiles.rules = [
    "d /etc/rancher/rke2/config.yaml.d 0700 root root -"
    # local-path-provisioner 的数据目录（见 gitops/infrastructure/local-path）
    "d /var/lib/pits-local 0755 root root -"
  ];

  environment.systemPackages = with pkgs; [
    kubectl
    kubernetes-helm
    fluxcd
    kubevirt # virtctl
  ];
  environment.variables.KUBECONFIG = "/etc/rancher/rke2/rke2.yaml";
}
