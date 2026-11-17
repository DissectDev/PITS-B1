---
name: pits-storage-migration
description: >-
  Standard preflight, sector-geometry detection (4096B 4Kn SAN vs 512B guest disk),
  sparse loopback fixture testing, and in-place nvmet copy verification for PITS
  block storage, SAN backing files, and KubeVirt VM disk migrations. Activate this
  skill whenever writing, reviewing, or running disk cutover, losetup, nvmet, or
  storage migration scripts in PITS.
---

# PITS Storage & VM Disk Migration Runbook

Follow all four gates whenever inspecting, extracting, converting, or migrating PITS SAN backing files (`*.raw`) or KubeVirt VM disk images.

## 1. Detect Logical Sector Geometry (`4096B` vs `512B`) — Never Assume `512B`

PITS NVMe/TCP SAN (`nvmet`) exposes **`4096`-byte (`4Kn`)** logical sectors (`SECTOR_4K = 4096` in `client/src/san.rs`), while inner guest disks (SATA/virtio `disk.img`) typically use **`512`-byte** sectors. Calling `losetup --partscan` without `--sector-size` defaults to `512` and silently misses `4Kn` GPT partitions.

Always probe the GPT Primary Header (`b"EFI PART"` at LBA 1) before attaching a loop device:

```python
def detect_gpt_sector_size(path) -> int:
    with open(path, "rb") as f:
        hdr = f.read(8192)
    sig_512 = hdr[512:520] == b"EFI PART"
    sig_4096 = hdr[4096:4104] == b"EFI PART"
    if sig_4096 and not sig_512:
        return 4096
    if sig_512 and not sig_4096:
        return 512
    raise RuntimeError(f"{path}: ambiguous or missing GPT header (512={sig_512}, 4096={sig_4096})")
```

Attach read-only with explicit geometry and wait for kernel partition nodes:

```bash
losetup --read-only --sector-size "${SECTOR_SIZE}" --partscan --find --show "${IMG}"
# Verify ${LOOPDEV}p1 is a block device before mount -o ro,norecovery
```

## 2. Deep Non-Destructive `--dry-run` Preflight

Even while a live VM holds a SAN namespace open (when mounting a dirty XFS/ext4 filesystem is forbidden), `--dry-run` **must** still perform zero-side-effect binary checks:
1. Read the first `8192` bytes of the source image and verify `detect_gpt_sector_size()` succeeds.
2. Verify source vs. target byte sizes, non-symlink status, and free space.
3. Verify target K8s PV/PVC/NAD readiness and node `virt-handler` health.

## 3. Mandatory Sparse Fixture Test for Extraction / Conversion Scripts

Target-state canaries (e.g., `mig-canary` on raw `Block` RWX) do **not** exercise legacy source extraction code. Before running any standalone Python/Bash migration script on production data, test the extraction path on `/tmp` using a tiny sparse fixture with the **exact same sector geometry** (`4096B`):

```bash
FIX_SRC=$(mktemp /tmp/pits-4kn-src.XXXXXX.raw)
FIX_DST=$(mktemp /tmp/pits-raw-dst.XXXXXX.raw)
truncate -s 64M "${FIX_SRC}"
truncate -s 16M "${FIX_DST}"
printf 'label: gpt\nsector-size: 4096\n,\n' | sfdisk "${FIX_SRC}"
LOOP=$(losetup --sector-size 4096 --partscan --find --show "${FIX_SRC}")
mkfs.xfs -q "${LOOP}p1"
# Mount ${LOOP}p1, create miniature payload matching source layout, detach,
# then run the migration script's extraction function against ${FIX_SRC} -> ${FIX_DST}
# and verify cmp --silent passes before cleaning up.
```

## 4. Safe `nvmet` Cutover & In-Place Copy Verification

1. **Exclusive Quiesce Check**: Before loop-mounting any legacy SAN backing file on `pits-core`, assert `/sys/kernel/config/nvmet/subsystems/<nqn>/namespaces/<nsid>/enable` reads `0` across **all** candidate SAN subsystems.
2. **Preserve Active `nvmet` Backing Inode**: When copying into a file already bound to an active `nvmet` namespace, never `rm`, `mv`, or replace the file (which orphans the open inode held by `nvmet`). Write **in-place**:
   ```bash
   dd if="${SRC_IMG}" of="${SHARED_RAW}" bs=16M iflag=fullblock conv=notrunc,fdatasync status=none
   ```
3. **Pre-Boot Verification**: Assert `(st_dev, st_ino, st_size)` of `${SHARED_RAW}` did not change, and run `cmp --silent "${SRC_IMG}" "${SHARED_RAW}"` **before** rebinding PVCs or starting the VM.
