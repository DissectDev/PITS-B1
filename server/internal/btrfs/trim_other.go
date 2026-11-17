//go:build !linux

package btrfs

import "os"

func statFilesystem(mountPath string) (total, used, free uint64, isBtrfs bool, err error) {
	st, err := os.Stat(mountPath)
	if err != nil {
		return 0, 0, 0, false, err
	}
	if !st.IsDir() {
		return 0, 0, 0, false, nil
	}
	return 4000803979264, 260000000000, 3740803979264, true, nil
}

func runFITRIM(_ string) (uint64, error) {
	return 0, nil
}
