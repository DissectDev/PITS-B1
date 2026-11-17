#!/bin/bash
set -euo pipefail

mount_root=/srv/pits-storage
mount_dir=$mount_root/tank
devices=(
  /dev/disk/by-id/nvme-KINGSTON_SA2000M81000G_50026B7683F82E20-part1
  /dev/disk/by-id/nvme-Samsung_SSD_980_1TB_S64ANS0RB40147Z-part1
  /dev/disk/by-id/nvme-WD_BLACK_SN770_2TB_23312N400193-part1
)
mount_options=noatime,compress=zstd:3,commit=60,nodiscard

check_devices() {
  local dev
  for dev in "${devices[@]}"; do
    [ -b "$dev" ] || { echo "Missing data partition: $dev" >&2; return 1; }
  done
}

check_identity() {
  local dev found
  : "${FS_UUID:?Filesystem UUID must be supplied}"
  check_devices
  for dev in "${devices[@]}"; do
    found=$(blkid -p -s UUID -o value "$dev")
    [ "$found" = "$FS_UUID" ] || { echo "Unexpected filesystem on $dev" >&2; return 1; }
    [ "$(blkid -p -s TYPE -o value "$dev")" = btrfs ]
  done
}

check_mount() {
  mountpoint -q "$mount_dir"
  [ "$(findmnt -n -M "$mount_dir" -o FSTYPE)" = btrfs ]
  [ "$(findmnt -n -M "$mount_dir" -o UUID)" = "$FS_UUID" ]
}

cleanup() {
  trap - TERM INT EXIT
  if check_mount; then
    btrfs filesystem sync "$mount_dir" || true
    umount "$mount_dir" || { echo 'Unable to unmount active filesystem' >&2; exit 1; }
  fi
}

maintenance() {
  local week month
  week=$(date +%G-W%V)
  month=$(date +%Y-%m)
  # Maintenance is serialized in this single manager. State lives outside the volume.
  if [ "$(date +%u-%H)" = 7-04 ] && [ "$(cat /state/last-trim 2>/dev/null || true)" != "$week" ]; then
    if fstrim -v "$mount_dir"; then printf '%s\n' "$week" > /state/last-trim; fi
  fi
  if [ "$(date +%d-%H)" = 01-03 ] && [ "$(cat /state/last-scrub 2>/dev/null || true)" != "$month" ]; then
    if btrfs scrub start -B "$mount_dir"; then printf '%s\n' "$month" > /state/last-scrub; fi
  fi
}

case "${1:-manage}" in
  initialize)
    [ "${CONFIRM_FORMAT:-}" = pits-data-20261108 ] || { echo 'Missing one-time format authorization' >&2; exit 1; }
    check_devices
    for dev in "${devices[@]}"; do
      existing_type=$(blkid -p -s TYPE -o value "$dev" || true)
      existing_table=$(blkid -p -s PTTYPE -o value "$dev" || true)
      if [ -n "$existing_type$existing_table" ]; then echo "Refusing existing signature: $dev" >&2; exit 1; fi
      if findmnt -rn -S "$dev" | grep -q .; then echo "Refusing mounted partition: $dev" >&2; exit 1; fi
    done
    mkfs.btrfs --data raid0 --metadata raid1 --sectorsize 4096 --label pits-data "${devices[@]}"
    blkid "${devices[@]}"
    ;;
  status)
    check_mount
    ;;
  trim)
    check_mount
    fstrim -v "$mount_dir"
    ;;
  scrub)
    check_mount
    btrfs scrub start -B "$mount_dir"
    ;;
  manage)
    check_identity
    mkdir -p "$mount_dir"
    if mountpoint -q "$mount_dir"; then
      check_mount
    else
      # Drop unmounted registrations that may retain paths from an old namespace.
      btrfs device scan --forget "${devices[@]}"
      btrfs device scan "${devices[@]}"
      mount -t btrfs -o "$mount_options,device=${devices[1]},device=${devices[2]}" "${devices[0]}" "$mount_dir"
    fi
    check_mount
    trap cleanup TERM INT EXIT
    for subvolume in @data @archive @snapshots; do
      if [ ! -e "$mount_dir/$subvolume" ]; then btrfs subvolume create "$mount_dir/$subvolume"; fi
      btrfs subvolume show "$mount_dir/$subvolume" >/dev/null
    done
    printf 'Mounted %s at %s\n' "$FS_UUID" "$mount_dir"
    btrfs filesystem usage "$mount_dir"
    while :; do
      maintenance
      sleep 60 &
      wait $!
    done
    ;;
  *) echo 'Supported modes: initialize, manage, status, trim, scrub' >&2; exit 2 ;;
esac
