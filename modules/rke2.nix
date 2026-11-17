# 核心 RKE2 单节点集群。
#
# - CNI 是 multus + cilium：nixpkgs 的 services.rke2.cni 只能选一个，所以 cni 写在 config.yaml 里。
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
      "cilium"
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
  # Pin the tested Cilium patch images and preserve Multus/SR-IOV.
  ciliumHelmChartConfig = pkgs.writeText "rke2-cilium-config.yaml" ''
    apiVersion: helm.cattle.io/v1
    kind: HelmChartConfig
    metadata:
      name: rke2-cilium
      namespace: kube-system
    spec:
      valuesContent: |-
        # RKE2 v1.35.8+rke2r1 bundled chart rke2-cilium 1.19.601.
        # The bundled 1.19.6 agent fails its BPF helper probe on Linux 7.3.1.
        # Validate the current stable 1.19 patch images in the isolated VM first.
        # Primary mode: Multus delegates the default Pod network to Cilium.
        commonLabels:
          pits.io/fault-domain: core
          pits.io/tier: control
        global:
          clusterCIDR: 10.42.0.0/16
          clusterCIDRv4: 10.42.0.0/16
          clusterDNS: 10.43.0.10
          clusterDomain: cluster.local
          serviceCIDR: 10.43.0.0/16
          rke2DataDir: /var/lib/rancher/rke2
        kubeProxyReplacement: false
        image:
          override: quay.io/cilium/cilium:v1.19.10@sha256:e86fb55fa703ada191fbd4489513daa19674952c8c3c4985dc3c77ce51ac4f27
        k8sServiceHost: 192.168.71.35
        k8sServicePort: 6443
        devices: mgmt0
        routingMode: tunnel
        tunnelProtocol: vxlan
        tunnelPort: 8473
        ipam:
          mode: cluster-pool
          operator:
            # Disjoint from the existing node's Canal pool 10.42.0.0/24, while
            # retaining the declared cluster CIDR and existing network-policy ranges.
            clusterPoolIPv4PodCIDRList: [10.42.128.0/17]
            clusterPoolIPv4MaskSize: 24
        ipv4: {enabled: true}
        ipv6: {enabled: false}
        cni:
          exclusive: false
          customConf: false
          uninstall: false
          chainingMode: portmap
          confPath: /etc/cni/net.d
          binPath: /opt/cni/bin
        bpf:
          hostLegacyRouting: true
          masquerade: false
        enableIPv4Masquerade: true
        socketLB: {hostNamespaceOnly: true}
        hostFirewall: {enabled: false}
        operator:
          replicas: 1
          image:
            override: quay.io/cilium/operator-generic:v1.19.10@sha256:b3d1c26d4bc58e307282bad9639b4d598e73f91be1430028f36bc5fdce19a85c
          unmanagedPodWatcher: {restart: false}
        policyEnforcementMode: default
        # NixOS already mounts these filesystems. Avoid nsenter/mount helpers that
        # assume a traditional host filesystem layout.
        cgroup:
          autoMount: {enabled: false}
          hostRoot: /sys/fs/cgroup
        resources:
          requests: {cpu: 100m, memory: 256Mi}
          limits: {cpu: '2', memory: 1Gi}
  '';
in
{
  # Reserve 1 GiB for the container router's DPDK buffers. NixOS owns this
  # setting; do not run a second DaemonSet that changes global hugepage counts.
  boot.kernel.sysctl."vm.nr_hugepages" = 512;

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
  # RKE2 rewrites manifests during startup: use a writable copy, not a store link.
  system.activationScripts.pitsCiliumManifest.text = ''
    install -d -m 0755 /var/lib/rancher/rke2/server/manifests
    cp --remove-destination ${ciliumHelmChartConfig} /var/lib/rancher/rke2/server/manifests/rke2-cilium-config.yaml
    chmod 0644 /var/lib/rancher/rke2/server/manifests/rke2-cilium-config.yaml
  '';

  environment.etc."rancher/rke2/cilium-helmchart-config.yaml".source = ciliumHelmChartConfig;

  systemd.tmpfiles.rules = [
    "d /etc/rancher/rke2/config.yaml.d 0700 root root -"
    "d /var/lib/rancher/rke2/server/manifests 0755 root root -"
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
