# PITS Base (`pits-core` NixOS & GitOps)

`Base/` 为原 `PITS-B1` 的核心机基座目录，仅保留已实施路径：

- **NixOS Flake (`flake.nix`, `hosts/core/`, `modules/`, `site/`)**：`pits-core`（MS-01）管理域宿主配置，含 `network` (`mgmt0`)、`sriov` (X710 VF)、`vfio` (`I226-LM`)、`tailscale` + `tailscale-relay`、`proxy`、`rke2` (Multus + Cilium)、`btrfs` (三盘容器管理)、`netboot` (`nvmet_tcp` + VLAN 10/20)、`thunderbolt`、`pits-agent` 与 `pits-portal`。
- **GitOps 树 (`gitops/`)**：由 Flux 从 `Base/gitops/clusters/core` 同步，包含 `namespaces`、`controllers` (cert-manager / Kyverno / CNPG / KubeVirt / CDI / SR-IOV)、`local-path`、`config` (KubeVirt / PKI / Networks / Boot Images)、`policies`、`core-nodes/storage` (`btrfs-manager`) 与 operator 清单。

## 本地检查

```sh
nix eval ./Base#nixosConfigurations.ci-core.config.system.build.toplevel.drvPath
```

在 `pits-core` 上声明式切换：

```sh
sudo nixos-rebuild switch --flake path:/home/libraskywalker/PITS-B1/Base#core
```
