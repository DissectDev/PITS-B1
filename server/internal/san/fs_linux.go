//go:build linux

package san

import (
	"fmt"
	"os"

	"golang.org/x/sys/unix"
)

const (
	btrfsSuperMagic = 0x9123683e
	fsIOCGetFlags   = 0x80086601
	fsIOCSetFlags   = 0x40086602
	fsNOCOWFlag     = 0x00800000
)

// VerifyBtrfsBacking confirms rootDir resides on a mounted Btrfs filesystem.
func VerifyBtrfsBacking(rootDir string) error {
	var st unix.Statfs_t
	if err := unix.Statfs(rootDir, &st); err != nil {
		return fmt.Errorf("statfs %s: %w", rootDir, err)
	}
	if int64(st.Type) != btrfsSuperMagic {
		return fmt.Errorf("path %s is not on Btrfs (magic=0x%x)", rootDir, st.Type)
	}
	return nil
}

// EnsureNOCOW sets FS_NOCOW_FL (+C) directly via Linux ioctl without shelling out to chattr.
func EnsureNOCOW(dirPath string) error {
	f, err := os.Open(dirPath)
	if err != nil {
		return err
	}
	defer f.Close()
	flags, err := unix.IoctlGetInt(int(f.Fd()), fsIOCGetFlags)
	if err != nil {
		return fmt.Errorf("FS_IOC_GETFLAGS on %s: %w", dirPath, err)
	}
	if flags&fsNOCOWFlag == 0 {
		if err := unix.IoctlSetPointerInt(int(f.Fd()), fsIOCSetFlags, flags|fsNOCOWFlag); err != nil {
			return fmt.Errorf("FS_IOC_SETFLAGS FS_NOCOW_FL on %s: %w", dirPath, err)
		}
	}
	after, err := unix.IoctlGetInt(int(f.Fd()), fsIOCGetFlags)
	if err != nil || after&fsNOCOWFlag == 0 {
		return fmt.Errorf("FS_NOCOW_FL verification failed on %s", dirPath)
	}
	return nil
}

// CheckNOCOW inspects whether FS_NOCOW_FL is active on dirPath.
func CheckNOCOW(dirPath string) bool {
	f, err := os.Open(dirPath)
	if err != nil {
		return false
	}
	defer f.Close()
	flags, err := unix.IoctlGetInt(int(f.Fd()), fsIOCGetFlags)
	return err == nil && (flags&fsNOCOWFlag != 0)
}

// PreallocateExtents reserves real physical extents via Linux fallocate(2) + fsync(2).
func PreallocateExtents(f *os.File, offset, length int64) error {
	if length <= 0 {
		return nil
	}
	if err := unix.Fallocate(int(f.Fd()), 0, offset, length); err != nil {
		return fmt.Errorf("fallocate(%d, %d): %w", offset, length, err)
	}
	return f.Sync()
}

// OpenNoFollow opens or creates a backing file while rejecting symlinks via O_NOFOLLOW.
func OpenNoFollow(path string, create bool) (*os.File, error) {
	flags := unix.O_RDWR | unix.O_NOFOLLOW | unix.O_CLOEXEC
	if create {
		flags |= unix.O_CREAT | unix.O_EXCL
	}
	fd, err := unix.Open(path, flags, 0o600)
	if err != nil {
		return nil, err
	}
	return os.NewFile(uintptr(fd), path), nil
}
