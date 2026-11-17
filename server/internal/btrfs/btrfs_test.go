package btrfs

import (
	"os"
	"path/filepath"
	"testing"

	"pits.io/server/internal/api"
)

func TestBtrfsStatusAndMaintain(t *testing.T) {
	base := t.TempDir()
	mnt := filepath.Join(base, "tank")
	sysRoot := filepath.Join(base, "sys")
	stateDir := filepath.Join(base, "state")

	for _, sub := range []string{"@data", "@archive", "@snapshots"} {
		_ = os.MkdirAll(filepath.Join(mnt, sub), 0o755)
	}
	dev1 := filepath.Join(sysRoot, "fs/btrfs", api.ExpectedBtrfsUUID, "devinfo/1")
	_ = os.MkdirAll(dev1, 0o755)
	_ = os.WriteFile(filepath.Join(dev1, "path"), []byte("/dev/nvme0n1-samsung\n"), 0o644)
	_ = os.WriteFile(filepath.Join(dev1, "error_stats"), []byte("write_errs 0\nread_errs 0\nflush_errs 0\ncorruption_errs 0\ngeneration_errs 0\n"), 0o644)

	// Exercise native OS statFilesystem probe on base directory first.
	if _, _, _, _, err := statFilesystem(mnt); err != nil {
		t.Fatalf("statFilesystem(%s) returned error: %v", mnt, err)
	}

	mgr := NewManager(mnt, sysRoot, stateDir)
	mgr.StatFS = func(string) (uint64, uint64, uint64, bool, error) {
		return 4000803979264, 260000000000, 3740803979264, true, nil
	}
	mgr.FitrimFn = func(string) (uint64, error) {
		return 1048576, nil
	}
	st := mgr.Status()
	if !st.Mounted || !st.UUIDVerified || !st.Healthy || len(st.Subvolumes) != 3 || len(st.Devices) != 1 {
		t.Fatalf("unexpected Btrfs status: %+v", st)
	}
	if st.Devices[0].HistoricalMediaNote == "" {
		t.Fatalf("expected Samsung historical SMART baseline note")
	}

	trimRes, err := mgr.Maintain("trim")
	if err != nil || trimRes.Operation != "trim" {
		t.Fatalf("trim failed: %+v, err=%v", trimRes, err)
	}
	scrubRes, err := mgr.Maintain("scrub")
	if err != nil || scrubRes.Operation != "scrub" {
		t.Fatalf("scrub failed: %+v, err=%v", scrubRes, err)
	}
}
