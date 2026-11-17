package pxe

import (
	"crypto/sha256"
	"encoding/hex"
	"os"
	"path/filepath"
	"testing"

	"pits.io/server/internal/api"
)

func TestStoreSaveListAndVerifyAssets(t *testing.T) {
	base := t.TempDir()
	store := NewStore(filepath.Join(base, "reports"), filepath.Join(base, "assets"))

	// Reject disk mutation approval in inventory report.
	badPayload := []byte(`{"schemaVersion":1,"approvedForDiskMutation":true,"network":[{"mac":"58:47:ca:75:f6:9a"}]}`)
	if err := store.SaveReport("netboot-004", "58:47:ca:75:f6:9a", badPayload); err == nil {
		t.Fatalf("expected approvedForDiskMutation=true to be rejected")
	}

	// Valid Hook hardware inventory report.
	goodPayload := []byte(`{"schemaVersion":1,"approvedForDiskMutation":false,"network":[{"mac":"58:47:ca:75:f6:9a"}]}`)
	if err := store.SaveReport("netboot-004", "58:47:ca:75:f6:9a", goodPayload); err != nil {
		t.Fatalf("SaveReport failed: %v", err)
	}
	reports, err := store.ListReports()
	if err != nil || len(reports) != 1 || string(reports["netboot-004"]) == "" {
		t.Fatalf("unexpected ListReports output: %v, err=%v", reports, err)
	}

	// Stage kernel + initramfs and verify SHA-256 digests.
	devAssets := filepath.Join(base, "assets", "netboot-004")
	_ = os.MkdirAll(devAssets, 0o755)
	kernelBytes := []byte("talos-v1.14.5-vmlinuz")
	initrdBytes := []byte("talos-v1.14.5-initramfs-pits-nvme-early")
	_ = os.WriteFile(filepath.Join(devAssets, "vmlinuz"), kernelBytes, 0o644)
	_ = os.WriteFile(filepath.Join(devAssets, "initramfs.xz"), initrdBytes, 0o644)

	kh := sha256.Sum256(kernelBytes)
	ih := sha256.Sum256(initrdBytes)

	res, err := store.VerifyAssets(api.PXEVerifyAssetsRequest{
		DeviceID: "netboot-004",
		Checksums: map[string]string{
			"vmlinuz":      hex.EncodeToString(kh[:]),
			"initramfs.xz": hex.EncodeToString(ih[:]),
		},
	})
	if err != nil || !res.Verified || len(res.Assets) != 2 {
		t.Fatalf("VerifyAssets failed: res=%+v, err=%v", res, err)
	}
}
