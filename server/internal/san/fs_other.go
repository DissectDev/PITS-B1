//go:build !linux

package san

import (
	"fmt"
	"os"
)

// VerifyBtrfsBacking is a portable check used when running unit tests on non-Linux hosts.
func VerifyBtrfsBacking(rootDir string) error {
	st, err := os.Stat(rootDir)
	if err != nil {
		return err
	}
	if !st.IsDir() {
		return fmt.Errorf("%s is not a directory", rootDir)
	}
	return nil
}

func EnsureNOCOW(dirPath string) error {
	_, err := os.Stat(dirPath)
	return err
}

func CheckNOCOW(dirPath string) bool {
	st, err := os.Stat(dirPath)
	return err == nil && st.IsDir()
}

func PreallocateExtents(f *os.File, offset, length int64) error {
	if length <= 0 {
		return nil
	}
	if err := f.Truncate(offset + length); err != nil {
		return err
	}
	return f.Sync()
}

func OpenNoFollow(path string, create bool) (*os.File, error) {
	if lstat, err := os.Lstat(path); err == nil && lstat.Mode()&os.ModeSymlink != 0 {
		return nil, fmt.Errorf("backing file cannot be a symlink: %s", path)
	}
	flags := os.O_RDWR
	if create {
		flags |= os.O_CREATE | os.O_EXCL
	}
	return os.OpenFile(path, flags, 0o600)
}
