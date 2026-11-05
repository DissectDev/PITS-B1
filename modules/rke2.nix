# 核心 RKE2 单节点集群。
#
# - CNI 是 multus + canal：nixpkgs 的 services.rke2.cni 只能选一个，所以 cni 写在 config.yaml 里。
# - config.yaml 放在 /etc/rancher/rke2/ 下，RKE2 还会读同目录 config.yaml.d/*.yaml，
#   机密（etcd S3 密钥）放在 config.yaml.d/90-secrets.yaml，不进 Git 和 Nix store。
# - CPU Manager 必须在 kubelet 第一次启动前设好，之后改动要清 cpu_manager_state。
{
  lib,
  pkgs,
  site,
  ...
}:
let
  yaml = pkgs.formats.yaml { };

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
    package = pkgs.${site.rke2.package};
    nodeName = site.hostName;
    nodeLabel = [ "pits.io/fault-domain=core" ];

    extraKubeletConfig = {
      cpuManagerPolicy = "static";
      reservedSystemCPUs = site.cpu.reservedSystemCPUs;
    };

    # 关机（包括 UPS 触发的关机）时先让 Pod 和 VM 优雅退出。
    gracefulNodeShutdown = {
      enable = true;
      shutdownGracePeriod = "300s";
      shutdownGracePeriodCriticalPods = "60s";
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
