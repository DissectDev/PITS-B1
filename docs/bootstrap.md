# 分阶段实施步骤

每个阶段验收通过后再进入下一个。P0 和 P4 是关卡：P0 决定直通方案能否成立，P4 决定申领用哪种做法。

## P0 准备（关卡）

1. BIOS：打开 VT-d 和 AMT。
2. 装好四块盘，用 NixOS 安装盘启动核心机。
3. 盘点硬件：

   ```sh
   nix-shell -p pciutils ethtool util-linux --run 'sudo bash scripts/p0-inventory.sh'
   ```

4. 按输出填写 `site/core.nix`：系统盘、管理口和直通设备的 PCI 地址、`cpu.reservedSystemCPUs`。结果记进 [hardware.md](hardware.md)。
5. 用 `lspci -nn` 查数据盘的 vendor:device ID，填进 `gitops/infrastructure/config/kubevirt.yaml`。同时核对 I226-LM（`8086:125b`）和 X710（`8086:1572`）的 ID。
6. 本地评估一次：`nix eval .#nixosConfigurations.core.config.system.build.toplevel.drvPath`。

**验收（关卡）：**
- 每个要直通的设备所在的 IOMMU 分组里，没有宿主还要用的设备（PCIe 桥除外）。
- `nixosConfigurations.core` 能评估通过。

不满足时有两个办法：换一块盘当系统盘，或者把同组设备一起直通。

## P1 宿主

1. 准备机密文件（见 [secrets.md](secrets.md)），放进一个临时目录：

   ```sh
   mkdir -p staging/var/lib/pits/secrets && chmod 700 staging/var/lib/pits/secrets
   mkpasswd -m yescrypt > staging/var/lib/pits/secrets/admin.hash
   echo 'tskey-auth-...' > staging/var/lib/pits/secrets/tailscale-authkey
   chmod 600 staging/var/lib/pits/secrets/*
   ```

2. 在安装盘上设好 root 的 SSH 登录，然后从另一台机器执行：

   ```sh
   nix run github:nix-community/nixos-anywhere -- \
     --flake .#core --extra-files ./staging --target-host root@<安装盘的地址>
   ```

   disko 只会格式化 `site.systemDisk`。三块数据盘不在 disko 配置里，不会被碰。
   系统盘（Crucial P310）上现有的 NixOS 会被整盘清掉，需要保留的东西先拷走。
3. 重启后执行：`sudo bash scripts/check-passthrough.sh`。

**验收：**
- 直通设备都绑在 vfio-pci 上，宿主只看得到 mgmt0 和系统盘。
- 断电重启后能通过 Tailscale 登录。
- 在空盘上用 flake 重装，得到的系统完全相同。

`pits-vfio-bind` 服务在绑定前会检查设备有没有被宿主使用，例如挂载着的盘或者管理口。如果地址写错，它会拒绝绑定并报错，不会把系统盘交出去。

## P2 核心集群

1. 如果打开了 etcd 机外 S3，先写好 `/etc/rancher/rke2/config.yaml.d/90-secrets.yaml`，再重启 rke2-server。
2. 确认集群起来了：`kubectl get nodes`（宿主上已设好 `KUBECONFIG`）。
3. 让 Flux 接管：

   ```sh
   export GITHUB_TOKEN=...
   flux bootstrap github --owner=DissectDev --repository=PITS-B1 \
     --branch=main --path=gitops/clusters/core
   ```

4. `flux get kustomizations` 应当看到：`namespaces`、`controllers`、`local-path`、`config`、`policies` 等就绪，`core-nodes` 和 `zones` 处于暂停状态。

**验收：**
- `sudo rke2 etcd-snapshot save` 生成的快照出现在机外 S3 里。
- 在重装后的宿主上，用机外的快照（`rke2 server --cluster-reset --cluster-reset-restore-path=...`）恢复出原来的集群。

## P3 虚拟化与两台核心节点

1. 准备网络节点和存储节点的系统镜像（分别由 PITS Network 和 PITS Storage 的设计决定），放到任一 HTTP 地址，填进 `gitops/core-nodes/*.yaml`。
2. 把 `gitops/clusters/core/stack.yaml` 里 `core-nodes` 的 `suspend` 改成 `false`，然后提交。
3. 检查：
   - `kubectl -n core-nodes get vm,vmi`：两台 VM 都在运行。
   - 每台 VM 分到的 CPU：

     ```sh
     kubectl -n core-nodes exec <virt-launcher Pod> -- cat /sys/fs/cgroup/cpuset.cpus.effective
     ```

     对照 P0 记录的 P 核编号。
   - 吞吐：从一台分支经网络节点到存储节点跑 iperf3。

**验收：**
- 宿主重启后，两台 VM 自动起来，直通设备都在。
- `virtctl -n core-nodes stop network-node` 之后，宿主的管理和核心集群都不受影响。

## P4 管理与治理（关卡）

1. 创建 Steampipe 的数据库密码：

   ```sh
   kubectl -n pits-compliance create secret generic steampipe-db \
     --from-literal=password=$(openssl rand -hex 16)
   ```

2. 在局域网 DNS 里把 `*.<域名>` 指向宿主管理口。域名在 `gitops/clusters/core/site-vars.yaml` 里设置。
3. 等 `management-platform`、`management`、`compliance` 都就绪后，登录 ManageIQ（`https://manageiq.<域名>`），立即修改默认管理员密码。
4. **关卡验证：** 把核心集群加为 provider，选 Kubernetes 并带上 KubeVirt 端点。PoC 阶段可以用一个 cluster-admin 的 ServiceAccount token。然后按顺序走一遍：从模板创建 VM、审批、配额、退役。
5. 核对 README 里列出的、需要在 P4 确认的几处（镜像地址、Steampipe 自省表等）。

**验收（关卡）：**
- ManageIQ 能在 RKE2 上创建并回收 KubeVirt VM，并把两台核心节点记进台账。
- Powerpipe 基准能跑通。

不通过就启用退路：申请改成向本仓库提 PR，PR 本身就是台账和审批记录，由 Flux 执行。ManageIQ 可以只保留台账，或者撤掉。

通过后，按 [compliance.md](compliance.md) 打开合规结果写回，并把 ManageIQ 切到 Keycloak 登录（见 `gitops/management/services/manageiq/manageiq.yaml` 里的注释）。

## P5 核心节点内部与备份

1. 按 PITS Network 和 PITS Storage 的设计，配置两台节点的内部。
2. 在 `site-vars.yaml` 里填好两处 S3 的地址，并创建 Velero 的两个 Secret（见 [secrets.md](secrets.md)）。
3. `velero backup-location get`：两个位置都应当是 Available。

**验收（做两次恢复演练）：**
- 删掉 Keycloak 的数据库卷，用 `velero restore create --from-backup <备份名>` 恢复。
- 删掉 `storage-node-root`（存储节点的系统盘），让 CDI 从镜像重建，确认存储池完整导入。

## P6 分支支撑

1. 构建控制面 VM 镜像和分支网络启动镜像，放进启动镜像服务（`pits-boot` 命名空间的卷）。这两个镜像的 flake 输出还没写，是 P6 的第一项工作。
2. 新建实验区：

   ```sh
   scripts/new-zone.sh lab1 101 02:50:00:00:01:01
   git add -A && git commit -m "Add zone lab1"
   ```

3. 按脚本的提示，创建 cloud-init Secret，并在网络节点上为控制面分配地址、建好 VLAN。
4. 第一次建区时，把 `stack.yaml` 里 `zones` 的 `suspend` 改成 `false`。
5. 控制面起来后，在区集群里装 Piraeus 和 Kyverno，再应用 `gitops/zones/template/in-zone/` 下的清单。
6. 注册 provider，写入 Steampipe 的区连接：`scripts/register-zone-provider.sh lab1 <地址> <token 文件> <只读 kubeconfig>`。
7. 跑一遍规划里的四个冷启动测试。

**验收：**
- 一台分支上电后自动加入实验区。
- 在 ManageIQ 里给这个区申请一个 VM 成功。
- 合规基准里 `zone_connections_match_registry` 为 ok。
- 核心集群除了多出一台控制面 VM，没有别的变化。
