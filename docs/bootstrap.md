# 分阶段实施步骤

每个阶段验收通过后再进入下一个。P0 和 P4 是关卡：P0 决定直通方案能否成立，P4 决定申领用哪种做法。P1 的 SR-IOV 自检也是关卡：它决定 X710 能不能按宽松模式切给各台 VM。

## P0 准备（关卡）

1. BIOS：打开 VT-d 和 AMT。
2. 装好四块盘，用 NixOS 安装盘启动核心机。
3. 盘点硬件：

   ```sh
   nix-shell -p pciutils ethtool util-linux --run 'sudo bash scripts/p0-inventory.sh'
   ```

4. 按输出填写 `site/core.nix`：系统盘、管理口、直通设备和 X710 两个口（`sriov.pfs`）的 PCI 地址、`cpu.reservedSystemCPUs`。结果记进 [hardware.md](hardware.md)。
5. 用 `lspci -nn` 查数据盘的 vendor:device ID，填进 `gitops/infrastructure/config/kubevirt.yaml`。同时核对 I226-LM（`8086:125b`）的 ID。X710（`8086:1572`）不在那里，它的 VF（`8086:154c`）由 SR-IOV device plugin 分配。
6. 本地评估一次：`nix eval .#nixosConfigurations.core.config.system.build.toplevel.drvPath`。

**验收（关卡）：**
- 每个要直通的设备所在的 IOMMU 分组里，没有宿主还要用的设备（PCIe 桥除外）。
- `nixosConfigurations.core` 能评估通过。

不满足时有两个办法：换一块盘当系统盘，或者把同组设备一起直通。

## P1 宿主

系统盘有两种来法，由 `site/core.nix` 里的 `systemDiskLayout` 决定：

| 方式 | `systemDiskLayout` | 什么时候用 | 系统盘 |
| --- | --- | --- | --- |
| A：就地切换 | `"existing"`（当前设置） | 盘上已经装好 NixOS，现在就是这种情况 | 不清盘，按 UUID 沿用现有分区 |
| B：重装 | `"disko"` | 系统盘坏了换新盘，或做“空盘重装”演练 | 整盘清空后按 `hosts/core/disko.nix` 重新分区 |

### 方式 A：在机器上 nixos-rebuild

1. 确认 `site/core.nix` 里的 `admin.user`（`libraskywalker`）和 `admin.sshKeys` 就是你现在登录用的账号和公钥。
   配置里设了 `users.mutableUsers = false`，没在配置里声明的用户和密码都会被清掉，root 密码也一样。
   在光猫上给网络节点上行口（I226-LM）预留 192.168.71.2（见 [addressing.md](addressing.md)）。P1 时这个地址上还没有设备，宿主只是先装好指向它的路由。
2. 在核心机上放好密码哈希：

   ```sh
   sudo install -d -m 700 /var/lib/pits/secrets
   nix-shell -p mkpasswd --run 'mkpasswd -m yescrypt' | sudo tee /var/lib/pits/secrets/admin.hash >/dev/null
   sudo chmod 600 /var/lib/pits/secrets/admin.hash
   ```

3. 把仓库拉到核心机上，构建并设为下次启动：

   ```sh
   nix-shell -p git --run 'git clone --depth 1 https://github.com/DissectDev/PITS-B1.git' && cd PITS-B1
   sudo nixos-rebuild boot --flake .#core \
     --option extra-experimental-features 'nix-command flakes' \
     --option substituters "https://mirrors.ustc.edu.cn/nix-channels/store https://cache.nixos.org"
   ```

   - **用 `boot` 而不是 `switch`。** 网络会从 NetworkManager 换成 networkd，网卡改名为 `mgmt0`，同时要做直通绑定、建 SR-IOV 的 VF、换内核。当场切换会断掉 SSH。以后凡是改了直通设备或 `sriov.pfs`，也都用 `boot` 再重启。
   - **要带镜像源。** rebuild 时用的是当前系统的 nix.conf，新配置里的镜像要到切换之后才生效，所以要在命令里带上。
   - **源码来自国内镜像。** `flake.lock` 已经提交在仓库里，nixpkgs 从清华 TUNA 的 git 镜像拉取。disko 只在重装时才用到，就地 rebuild 不访问 GitHub。

   连 GitHub 慢、`git clone` 卡住时，有两个办法：

   - **借电脑上的 Clash 走代理。** 在 Clash 里打开“允许局域网连接”，然后：

     ```sh
     nix-shell -p git --run 'git -c http.proxy=http://<电脑的局域网地址>:7897 clone --depth 1 https://github.com/DissectDev/PITS-B1.git'
     ```

     端口以 Clash 里显示的为准（常见的是 7897 或 7890）。
   - **在电脑上下载再拷过去。** 在 GitHub 页面下载 ZIP，或者在电脑上 clone，然后用 `scp -r` 拷到核心机。flake 用的是 `flake.lock` 里锁定的版本，拷过去的仓库不需要带 `.git` 也能构建。

4. 手边准备好显示器和键盘，然后重启。起不来就在 systemd-boot 菜单里选上一代。
5. Tailscale 的登录状态会保留，所以 `site.tailscale.authKeyFile` 设为 `null`。主机名和标签不会自动更新，要手动执行一次：

   ```sh
   sudo tailscale up --hostname=pits-core --accept-dns=false --advertise-tags=tag:pits-core
   ```

   使用这个标签前，tailnet 的 ACL 里要先给它配 `tagOwners`。

### 方式 B：nixos-anywhere 重装

1. 把 `systemDiskLayout` 改成 `"disko"`，`tailscale.authKeyFile` 改成 `"/var/lib/pits/secrets/tailscale-authkey"`。
2. 准备机密文件，放进一个临时目录（见 [secrets.md](secrets.md)）：

   ```sh
   mkdir -p staging/var/lib/pits/secrets && chmod 700 staging/var/lib/pits/secrets
   mkpasswd -m yescrypt > staging/var/lib/pits/secrets/admin.hash
   echo 'tskey-auth-...' > staging/var/lib/pits/secrets/tailscale-authkey
   chmod 600 staging/var/lib/pits/secrets/*
   ```

3. 用安装盘启动核心机，设好 root 的 SSH 登录，然后从另一台机器执行：

   ```sh
   nix run github:nix-community/nixos-anywhere -- \
     --flake .#core --extra-files ./staging --target-host root@<安装盘的地址>
   ```

   disko 只会格式化 `site.systemDisk`。三块数据盘不在 disko 配置里，不会被碰。系统盘上原有的内容会全部清掉。

### 验收

重启后执行 `sudo bash scripts/check-passthrough.sh`，然后确认：

- 整口直通的设备（I226-LM 和三块数据盘）都绑在 vfio-pci 上，宿主只看得到 mgmt0、sfp0、sfp1 和系统盘，sfp0、sfp1 上没有地址。
- sfp0 有 8 块 VF、sfp1 有 4 块，每块都绑在 vfio-pci 上，并且**各自独占一个 IOMMU 组**。不满足的话 VF 分不给 VM，宽松模式就不成立，先停下来。
- 断电重启后能通过 Tailscale 登录。
- “空盘重装得到完全相同的系统”要用方式 B 才能验证。走方式 A 的话，把这一项挪到 P5 的恢复演练里做。

**SR-IOV 自检（关卡）：**

```sh
sudo bash scripts/sriov-selftest.sh sfp0     # 光纤插着跑一次
sudo bash scripts/sriov-selftest.sh sfp0     # 拔掉 sfp0 的光纤再跑一次
```

脚本临时借用 sfp0 上最后两块 VF，在宿主上验证：同一 VLAN 的 VF 之间能直接互通，不同 VLAN 之间不通，拔掉光纤后仍能互通。三项都通过，核心网段才能按 [addressing.md](addressing.md) 的宽松模式搭。跑完 VF 会还给 vfio-pci。

`pits-vfio-bind` 服务在绑定前会检查设备有没有被宿主使用，例如挂载着的盘或者管理口。如果地址写错，它会拒绝绑定并报错，不会把系统盘交出去。`pits-sriov` 服务在改 VF 数量前会检查有没有 VM 正在用这些 VF，有就拒绝。

## P2 核心集群

核心机要从 docker.io、ghcr.io、quay.io 拉镜像，从 github.io 和 GitHub 拉 Helm chart 和 Git 仓库，这些来源在国内慢或者拉不到。宿主上跑一个 mihomo 代理（`modules/proxy.nix`），只给 RKE2、Flux 的 source-controller 和 nix-daemon 用；宿主的其他流量、VM 和分支都不走它。

1. 放好代理的订阅配置，格式和要求见 [secrets.md](secrets.md)：

   ```sh
   sudo install -m 600 <你的 Clash/mihomo 配置文件> /var/lib/pits/secrets/mihomo.yaml
   ```

2. 更新仓库并切换。这次不改网卡和直通，可以直接 `switch`：

   ```sh
   cd PITS-B1 && git pull
   sudo nixos-rebuild switch --flake .#core
   ```

3. 确认代理可用：

   ```sh
   systemctl status mihomo --no-pager | head -5
   curl -x http://127.0.0.1:7890 -s -o /dev/null -w '%{http_code}\n' https://registry-1.docker.io/v2/   # 应为 401
   ```

   代理固定走 `PITS` 组：订阅里的所有节点，自动选延迟最低的那个。订阅自带的模式、分组和规则都不起作用。查看当前用的是哪个节点（控制接口的密码沿用订阅配置里的 `secret`，没有就是空的）：

   ```sh
   S=$(sudo jq -r '.secret // ""' /run/pits-mihomo/config.yaml)
   curl -s -H "Authorization: Bearer $S" 127.0.0.1:9090/proxies/PITS | jq -r .now
   ```

4. 等 RKE2 起来。切换时 rke2-server 会带着代理设置重启，之前卡在拉镜像的话会接着拉：

   ```sh
   sudo kubectl get nodes          # pits-core 为 Ready
   sudo kubectl get pods -A        # 全部 Running 或 Completed
   ```

5. 让 Flux 接管。Flux 的安装清单就在仓库里（`gitops/clusters/core/flux-system`），手动应用一次，之后 Flux 按这个目录管理自己。仓库是公开的，不需要 GitHub token：

   ```sh
   sudo kubectl apply --server-side -k gitops/clusters/core/flux-system
   sudo kubectl wait --for=condition=Established --timeout=2m \
     crd/gitrepositories.source.toolkit.fluxcd.io crd/kustomizations.kustomize.toolkit.fluxcd.io
   sudo kubectl apply --server-side -k gitops/clusters/core/flux-system
   ```

   第一次应用时 CRD 还没就绪，最后两个资源（GitRepository 和 Kustomization）会报错；等 CRD 就绪后再应用一次就好。

6. 检查：

   ```sh
   sudo flux get sources all
   sudo flux get kustomizations
   ```

   `namespaces`、`controllers`、`local-path`、`config`、`policies`、`keycloak-operator`、`manageiq-operator` 应当都是 Ready；`core-nodes`、`management-platform`、`management`、`compliance`、`zones` 处于暂停状态。`controllers` 要拉的镜像最多，第一次可能要十几分钟。

7. 代理跑通后，打开 RKE2 的离线镜像包：把 `site/core.nix` 里的 `rke2.airgapImages` 改成 `true` 再构建一次。镜像包由 nix-daemon 经代理从 GitHub 下载，有几百 MB。打开之后，即使重装，核心集群启动也不依赖 docker.io。

**验收：**
- `sudo rke2 etcd-snapshot save --name p2` 能生成快照，`sudo rke2 etcd-snapshot list` 能看到它。
- 用这份快照恢复出原来的集群。恢复会把集群状态退回到快照那一刻，所以紧接着拍完快照做：

  ```sh
  sudo systemctl stop rke2-server
  sudo rke2 server --cluster-reset --cluster-reset-restore-path=/var/lib/rancher/rke2/server/db/snapshots/<快照文件名>
  sudo systemctl start rke2-server
  ```

- 停掉代理（`sudo systemctl stop mihomo`）后重启 rke2-server，集群照样起来：镜像已经在本地，不再需要外网。验完再 `sudo systemctl start mihomo`。

机外 S3 目前还没有，快照只留在本机。机外这一份在 P5 之前补上，补法见 `gitops/management/platform/backup.yaml` 末尾。补上之后，再用机外的快照做一次恢复。

## P3 虚拟化与两台核心节点

1. 准备网络节点和存储节点的系统镜像（qcow2，分别由 PITS Network 和 PITS Storage 的设计决定），上传到镜像服务：

   ```sh
   pod=$(kubectl -n pits-boot get pod -l app=boot-images -o name)
   kubectl -n pits-boot cp network-node.qcow2 ${pod#pod/}:/usr/share/nginx/html/network-node.qcow2
   kubectl -n pits-boot cp storage-node.qcow2 ${pod#pod/}:/usr/share/nginx/html/storage-node.qcow2
   ```

   镜像服务的卷是只读挂载的，上传前把 `gitops/infrastructure/images/boot-images.yaml` 里的 `readOnly: true` 临时去掉，或者直接把文件复制到宿主上该卷对应的目录（`/var/lib/pits-local/` 下以 `pvc-` 开头、名字里带 `boot-images` 的目录）。
   两台节点在核心网段上的地址、MAC 和各自拿到的 VF 见 [addressing.md](addressing.md)，节点内部的网络按这张表配置。网络节点要在 sfp0 干道上建 VLAN 10 子接口做网关；存储节点那块 VF 的 VLAN 由宿主设好，节点里直接用不带标签的网卡。
2. 交换机上把接 sfp0 的端口设成带 VLAN 10 的 trunk，分支的端口设成 VLAN 10 的 access 口。
3. 把 `gitops/clusters/core/stack.yaml` 里 `core-nodes` 的 `suspend` 改成 `false`，然后提交。
4. 检查：
   - `kubectl get node -o jsonpath='{.items[0].status.allocatable}'`：能看到 `pits.io/sfp0_trunk`、`pits.io/sfp0_storage`、`pits.io/sfp0_shared`、`pits.io/sfp1_trunk` 四个 VF 资源池，数量分别是 1、1、6、1。
   - `kubectl -n core-nodes get vm,vmi`：两台 VM 都在运行。
   - `ip link show sfp0`（在宿主上）：vf0 是 trust on、spoof checking off，vf1 是 vlan 10、MAC 02:50:00:00:00:10。
   - 每台 VM 分到的 CPU：

     ```sh
     kubectl -n core-nodes exec <virt-launcher Pod> -- cat /sys/fs/cgroup/cpuset.cpus.effective
     ```

     对照 P0 记录的 P 核编号。
   - 存储节点能 ping 通网关 10.88.0.1；宿主能访问 `http://10.88.0.10:3900`（经网络节点路由）。
   - 吞吐：从一台分支直接到存储节点跑 iperf3（同一网段，不经过网络节点）。

**验收：**
- 宿主重启后，两台 VM 自动起来，直通设备都在。
- `virtctl -n core-nodes stop network-node` 之后，宿主的管理和核心集群都不受影响；分支和存储节点之间照样互通（同一网段，不经过网络节点），只是出不了这个网段。
- 在网络节点里改干道 VF 的 VLAN 或 MAC 设置，存储节点那块 VF 不受影响（`ip link show sfp0` 里 vf1 不变）。

## P4 管理与治理（关卡）

先把 `gitops/clusters/core/stack.yaml` 里 `management-platform`、`management`、`compliance` 的 `suspend` 改成 `false`，提交后由 Flux 部署。

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
   scripts/new-zone.sh lab1 02:50:00:00:01:01
   git add -A && git commit -m "Add zone lab1"
   ```

3. 按脚本的提示，创建 cloud-init Secret，并在网络节点上为控制面分配地址、为这个区的分支划好 DHCP 地址段。
4. 第一次建区时，把 `stack.yaml` 里 `zones` 的 `suspend` 改成 `false`。
5. 控制面起来后，在区集群里装 Piraeus 和 Kyverno，再应用 `gitops/zones/template/in-zone/` 下的清单。
6. 注册 provider，写入 Steampipe 的区连接：`scripts/register-zone-provider.sh lab1 <地址> <token 文件> <只读 kubeconfig>`。
7. 跑一遍规划里的四个冷启动测试。

**验收：**
- 一台分支上电后自动加入实验区。
- 在 ManageIQ 里给这个区申请一个 VM 成功。
- 合规基准里 `zone_connections_match_registry` 为 ok。
- 核心集群除了多出一台控制面 VM，没有别的变化。
