# pits-host01 三盘 Btrfs 实施记录

实施日期：2026-11-08，Asia/Shanghai。用户明确授权在线解除三盘直通并清空创建 Btrfs，实施已完成。

## 实际结果

三块数据盘已从 vfio-pci 移交给宿主 NVMe 驱动。Btrfs 由 RKE2 中 `pits-storage/btrfs-manager` 单实例管理容器管理，宿主继续使用 Linux 7.3.1；没有更换内核或重启主机。

| 数据盘 | PCI | 序列号 | 数据分区字节数 |
| --- | --- | --- | ---: |
| Kingston A2000 1TB | 0000:01:00.0 | 50026B7683F82E20 | 1,000,203,091,968 |
| Samsung 980 1TB | 0000:02:00.0 | S64ANS0RB40147Z | 1,000,203,091,968 |
| WD_BLACK SN770 2TB | 0000:59:00.0 | 23312N400193 | 2,000,397,795,328 |

总设备空间为 **4,000,803,979,264 字节，约 3.64TiB**。文件系统卷标 `pits-data`，UUID `777b7660-ef8e-421a-ac60-d6c4cb2fea39`。

- 数据：RAID0，初始块组为三盘条带。
- 元数据：RAID1；系统块组：RAID1。
- 扇区：4096 字节；元数据节点：16384 字节。
- 每盘使用一个从第 2048 个 512 字节扇区开始的 GPT 分区，即 1MiB 对齐。
- 实际挂载选项包括 `noatime,compress=zstd:3,ssd,space_cache=v2,commit=60`，并已明确关闭 discard，保持写屏障和同步写语义。
- 创建了 `@data`、`@archive`、`@snapshots` 三个子卷。

普通文件目录：`/srv/pits-storage/tank/@data`。归档目录：`/srv/pits-storage/tank/@archive`。快照目录：`/srv/pits-storage/tank/@snapshots`。

3.64TiB 是总设备空间；用户文件还要承担元数据、快照和其他开销，异容量条带的宽度会变化。数据 RAID0 没有单盘容错，元数据镜像不能恢复丢失的条带用户数据。

## 验证

1. 所有设备均按 PCI、序列号、实际容量及稳定 by-id 路径核对后清理；Crucial P310 系统盘排除在操作之外，根分区 UUID 仍为 `d4327997-3d50-4685-a95c-affe9573559d`。
2. 格式化在一次性容器任务中完成；正常管理容器启动只扫描、验证 UUID 及挂载，不自动格式化。一次性任务已删除，日志保留。
3. 从容器写入并同步 8MiB 随机数据，正常删除并重建管理容器，重新挂载后 SHA-256 校验一致；测试文件已移除。
4. 三盘 `btrfs device stats` 的读、写、刷新、损坏及代数错误计数均为零。
5. 两次小规模 scrub 均未发现错误；第二次在最终持久状态目录配置下完成，状态记录正常。
6. 容器内外使用相同 by-id 路径。启动时只注销本卷三个未挂载设备的旧扫描缓存再注册，解决容器重建后残留旧命名空间设备路径的问题；未重新格式化卷。
7. 路由容器保持原 Pod UID `5cdf3a3a-7c65-4dc4-a4f8-c5b75fdc0c17`，接口和 Ready 状态正常；RKE2 与 Tailscale 服务 active。管理网关 `192.168.71.1` ping 正常，VPP 到 AC `10.88.1.1` 两次探测全部返回。宿主到 AC 的 ping 未返回，本次没有修改网络策略或路由配置。

这些验证没有包含全盘写满、完整介质扫描或整机断电/重启试验。容器重建验证已完成，不能将其等同于断电测试。

## 硬盘健康记录

Kingston 和 WD 的 NVMe 介质错误计数均为 0。Samsung 980 存在 **1 次历史介质错误**，严重告警为 0；此前自检记录通过，本次新启动的短自检也通过，操作后介质错误计数仍为 1，没有增加。该记录未被清零，仍应持续观察；短自检和当前文件系统检查不保证所有介质区域完好。

旧 Kingston 上的 EFI、ext4 和 LUKS 分区，以及旧 Samsung 上的 EFI、ext4 分区，已按授权清理并重建。常规格式化不是承诺覆盖每一个旧数据扇区的安全擦除。

## 运维入口

```sh
# 宿主查看挂载、空间与错误
findmnt /srv/pits-storage/tank
btrfs filesystem usage /srv/pits-storage/tank
btrfs device usage /srv/pits-storage/tank
btrfs device stats -c /srv/pits-storage/tank

# 集群中查看管理容器
kubectl -n pits-storage get ds,pods
kubectl -n pits-storage logs -l app=btrfs-manager

# 从管理容器运行维护；不需要在宿主重新挂载
kubectl -n pits-storage exec daemonset/btrfs-manager -- /bin/bash /config/btrfs-manager.sh trim
kubectl -n pits-storage exec daemonset/btrfs-manager -- /bin/bash /config/btrfs-manager.sh scrub
```

管理容器按 Asia/Shanghai 执行维护：每周日 04:00–04:59 检查并执行一次批量 TRIM；每月 1 日 03:00–03:59 检查并执行一次 scrub。以状态文件防止同一周/月重复执行；若该时间段容器没有运行，下一次按下一个正常窗口执行，不自动补跑。没有安排全盘 balance、自动去重或自动碎片整理。

`commit=60` 增加异步写合并机会，不保证每 60 秒才写盘。fsync、脏页回写、内存压力等仍可提前触发。此次未修改全机 `vm.dirty_*` 参数。

在同步后的一次 35 秒空闲抽样中，Kingston 与 WD 各增加 4 次写入、各 64KiB，Samsung 没有新增写入。这说明当前设置并不保证完全静默；这次短抽样也不能代表长期写入频率。原始计数保存在实施目录的 `idle.before` 与 `idle.after`。

## 配置与恢复资料

- 本工作区的部署目录：`deploy/pits-host01/storage/`。`kustomization.yaml` 引用最终 `daemonset.yaml` 与脚本 ConfigMap；模板和镜像构建文件也在该目录。
- 宿主事实来源 `/home/libraskywalker/PITS-B1`：新增 `modules/btrfs.nix`，从 `hosts/core/default.nix` 引入；数据直通列表清空；KubeVirt 的三盘分配项已移除。
- `/home/libraskywalker/PITS-B1/gitops/core-nodes/storage/` 保存同一份管理容器声明，原 storage-node VM 不再被 Kustomization 引用。core-nodes 的 Flux 同步仍沿用此前暂停状态；本次已手动应用最终声明，未提交或推送 Git 变更。
- 容器镜像 `docker.io/library/pits-btrfs-manager:6.19.1-20261108` 由锁定的 Nix 软件包构建，已导入本节点的 containerd；使用 `imagePullPolicy: Never` 并固定节点。镜像归档通过 `/var/lib/pits/btrfs-migration-20261108/manager-image` 保留；如节点镜像缓存被清理，需先重新导入归档再启动。
- `/var/lib/pits/btrfs-migration-20261108/` 保存原文件备份、原分区表、SMART 基线、初始化日志、自检与校验结果及镜像。此目录仅 root 可访问。
- 管理状态与 scrub 记录保存在 `/var/lib/pits/btrfs-manager/`，数据卷本身不依赖管理容器可写层。

NixOS 配置回滚不能撤销已经进行的格式化；旧直通配置也不能在新卷挂载期间直接恢复。
