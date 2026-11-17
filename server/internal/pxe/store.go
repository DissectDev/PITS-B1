// Package pxe manages Hook hardware inventory reports and SHA-256 verified
// netboot boot assets (vmlinuz / initramfs.xz) on pits-core.
package pxe

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"

	"pits.io/server/internal/api"
)

var devicePattern = regexp.MustCompile(`^netboot-\d{3}$`)

// Store manages inventory JSON evidence and boot assets on disk.
type Store struct {
	ReportsDir string
	AssetsDir  string
}

func NewStore(reportsDir, assetsDir string) *Store {
	if reportsDir == "" {
		reportsDir = "/var/lib/pits/tinkerbell-20261108/reports"
	}
	if assetsDir == "" {
		assetsDir = "/var/lib/pits/tinkerbell-20261108/public/talos"
	}
	return &Store{ReportsDir: reportsDir, AssetsDir: assetsDir}
}

// ListReports returns all persisted Hook hardware inventory JSON blobs by deviceID.
func (s *Store) ListReports() (map[string]json.RawMessage, error) {
	out := map[string]json.RawMessage{}
	entries, err := os.ReadDir(s.ReportsDir)
	if err != nil {
		if os.IsNotExist(err) {
			return out, nil
		}
		return nil, err
	}
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".json") {
			continue
		}
		dev := strings.TrimSuffix(entry.Name(), ".json")
		if !devicePattern.MatchString(dev) {
			continue
		}
		if raw, rerr := os.ReadFile(filepath.Join(s.ReportsDir, entry.Name())); rerr == nil {
			out[dev] = raw
		}
	}
	return out, nil
}

// SaveReport validates schemaVersion=1 and approvedForDiskMutation=false before atomic persistence.
func (s *Store) SaveReport(deviceID string, expectedMAC string, payload []byte) error {
	if !devicePattern.MatchString(deviceID) {
		return fmt.Errorf("invalid deviceID %q", deviceID)
	}
	if len(payload) == 0 || len(payload) > 1024*1024 {
		return fmt.Errorf("inventory payload size %d outside allowed range", len(payload))
	}
	var parsed struct {
		SchemaVersion           int `json:"schemaVersion"`
		ApprovedForDiskMutation *bool `json:"approvedForDiskMutation"`
		Network                 []struct {
			MAC string `json:"mac"`
		} `json:"network"`
	}
	if err := json.Unmarshal(payload, &parsed); err != nil {
		return fmt.Errorf("invalid inventory JSON: %w", err)
	}
	if parsed.SchemaVersion != 1 {
		return fmt.Errorf("unsupported inventory schemaVersion %d", parsed.SchemaVersion)
	}
	if parsed.ApprovedForDiskMutation == nil || *parsed.ApprovedForDiskMutation {
		return fmt.Errorf("inventory report cannot assert disk mutation approval")
	}
	if expectedMAC != "" {
		found := false
		want := strings.ToLower(expectedMAC)
		for _, nic := range parsed.Network {
			if strings.ToLower(nic.MAC) == want {
				found = true
				break
			}
		}
		if !found {
			return fmt.Errorf("enrolled MAC %s missing from inventory network interfaces", expectedMAC)
		}
	}
	if err := os.MkdirAll(s.ReportsDir, 0o700); err != nil {
		return err
	}
	target := filepath.Join(s.ReportsDir, deviceID+".json")
	tmp := target + ".new"
	f, err := os.OpenFile(tmp, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0o600)
	if err != nil {
		return err
	}
	if _, err := f.Write(payload); err != nil {
		f.Close()
		return err
	}
	if err := f.Sync(); err != nil {
		f.Close()
		return err
	}
	if err := f.Close(); err != nil {
		return err
	}
	return os.Rename(tmp, target)
}

// VerifyAssets hashes staged kernel/initramfs files and checks expected SHA-256 digests.
func (s *Store) VerifyAssets(req api.PXEVerifyAssetsRequest) (*api.PXEVerifyAssetsResult, error) {
	if !devicePattern.MatchString(req.DeviceID) {
		return nil, fmt.Errorf("invalid deviceID %q", req.DeviceID)
	}
	devDir := filepath.Join(s.AssetsDir, req.DeviceID)
	res := &api.PXEVerifyAssetsResult{
		DeviceID: req.DeviceID,
		Verified: true,
		Mismatch: map[string]string{},
	}
	names := []string{"vmlinuz", "initramfs.xz"}
	for k := range req.Checksums {
		if k != "vmlinuz" && k != "initramfs.xz" {
			names = append(names, k)
		}
	}
	sort.Strings(names)
	for _, name := range names {
		cleanName := filepath.Base(name)
		p := filepath.Join(devDir, cleanName)
		digest, size, modTime, err := hashFileSHA256(p)
		if err != nil {
			res.Verified = false
			res.Mismatch[cleanName] = "missing"
			continue
		}
		res.Assets = append(res.Assets, api.PXEAssetInfo{
			DeviceID:  req.DeviceID,
			Name:      cleanName,
			Path:      p,
			SizeBytes: size,
			SHA256:    digest,
			UpdatedAt: modTime.ModTime().UTC(),
		})
		if want, ok := req.Checksums[cleanName]; ok && want != "" && !strings.EqualFold(want, digest) {
			res.Verified = false
			res.Mismatch[cleanName] = digest
		}
	}
	if len(res.Mismatch) == 0 {
		res.Mismatch = nil
	}
	return res, nil
}

func hashFileSHA256(path string) (string, int64, os.FileInfo, error) {
	st, err := os.Stat(path)
	if err != nil {
		return "", 0, nil, err
	}
	f, err := os.Open(path)
	if err != nil {
		return "", 0, nil, err
	}
	defer f.Close()
	h := sha256.New()
	n, err := io.Copy(h, f)
	if err != nil {
		return "", 0, nil, err
	}
	return hex.EncodeToString(h.Sum(nil)), n, st, nil
}
