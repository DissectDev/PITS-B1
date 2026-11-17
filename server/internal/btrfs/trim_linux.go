//go:build linux

package btrfs

import (
	"fmt"
	"os"
	"unsafe"

	"golang.org/x/sys/unix"
)

const (
	btrfsSuperMagic = 0x9123683e
	fitrimIoctl     = 0xc0185879
)

type fstrimRange struct {
	Start  uint64
	Len    uint64
	MinLen uint64
}

func statFilesystem(mountPath string) (total, used, free uint64, isBtrfs bool, err error) {
	var st unix.Statfs_t
	if err := unix.Statfs(mountPath, &st); err != nil {
		return 0, 0, 0, false, err
	}
	bsize := uint64(st.Bsize)
	total = st.Blocks * bsize
	free = st.Bavail * bsize
	if total >= st.Bfree*bsize {
		used = total - st.Bfree*bsize
	}
	return total, used, free, int64(st.Type) == btrfsSuperMagic, nil
}

func runFITRIM(mountPath string) (uint64, error) {
	f, err := os.Open(mountPath)
	if err != nil {
		return 0, err
	}
	defer f.Close()
	rng := fstrimRange{Start: 0, Len: ^uint64(0), MinLen: 0}
	_, _, errno := unix.Syscall(unix.SYS_IOCTL, f.Fd(), uintptr(fitrimIoctl), uintptr(unsafe.Pointer(&rng)))
	if errno != 0 {
		return 0, fmt.Errorf("FITRIM ioctl on %s: %w", mountPath, errno)
	}
	return rng.Len, nil
}
