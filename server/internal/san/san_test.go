package san

import (
	"os"
	"path/filepath"
	"testing"

	"pits.io/server/internal/api"
)

func TestDeterministicWWIDMatchesEnrolledPITSContracts(t *testing.T) {
	cases := []struct {
		nqn      string
		nsName   string
		wantWWID string
	}{
		{"nqn.2026-11.arpa.home.pits:netboot-001", "system", "uuid.6f58a394-206a-5991-8e37-929f2aed7ecd"},
		{"nqn.2026-11.arpa.home.pits:netboot-001", "userdata", "uuid.d5d12205-1537-5ddd-96d5-8e829540b651"},
		{"nqn.2026-11.arpa.home.pits:netboot-002", "system", "uuid.0db2c830-a3a9-50e5-8ea8-3ea90aafa2bd"},
		{"nqn.2026-11.arpa.home.pits:netboot-002", "userdata", "uuid.fef249f2-87c7-51e1-8b6a-2a3bac123fa2"},
		{"nqn.2026-11.arpa.home.pits:netboot-003", "system", "uuid.4d307d50-3a36-50ac-be22-c39af6337c5f"},
		{"nqn.2026-11.arpa.home.pits:netboot-003", "userdata", "uuid.29bba904-a2e8-54ae-9918-b8c351851c36"},
		{"nqn.2026-11.arpa.home.pits:netboot-004", "system", "uuid.2bca88f0-1844-572a-8f32-ef2b1e227333"},
		{"nqn.2026-11.arpa.home.pits:netboot-004", "userdata", "uuid.18268b6c-597c-5c8e-abb5-2ceccee6e445"},
		{"nqn.2026-11.arpa.home.pits:netboot-004", "vmstore", "uuid.c55a459d-a8cb-5177-b2ad-f47d603ee6ed"},
	}
	for _, tc := range cases {
		got := ExpectedWWID(tc.nqn, tc.nsName)
		if got != tc.wantWWID {
			t.Fatalf("%s/%s: got %s, want %s", tc.nqn, tc.nsName, got, tc.wantWWID)
		}
	}
}

func TestSystemGPT4KWriteAndReadback(t *testing.T) {
	tmp := filepath.Join(t.TempDir(), "system.raw")
	totalSize := int64((StateStartLBA + StateSizeSectors + 512) * SectorSize4K)
	f, err := os.OpenFile(tmp, os.O_RDWR|os.O_CREATE|os.O_TRUNC, 0o600)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	if err := f.Truncate(totalSize); err != nil {
		t.Fatal(err)
	}
	written, err := WriteSystemGPT4K(f, totalSize)
	if err != nil {
		t.Fatalf("WriteSystemGPT4K failed: %v", err)
	}
	if len(written) != 2 {
		t.Fatalf("expected 2 GPT partitions, got %d", len(written))
	}
	readBack, err := ReadGPT4K(f, totalSize)
	if err != nil {
		t.Fatalf("ReadGPT4K failed: %v", err)
	}
	if len(readBack) != 2 {
		t.Fatalf("expected 2 partitions on readback, got %d", len(readBack))
	}
	if readBack[0].Name != "META" || readBack[0].StartLBA != 256 || readBack[0].Sectors != 256 {
		t.Fatalf("unexpected META partition: %+v", readBack[0])
	}
	if readBack[1].Name != "STATE" || readBack[1].StartLBA != 512 || readBack[1].Sectors != 262144 {
		t.Fatalf("unexpected STATE partition: %+v", readBack[1])
	}
}

func TestSANManagerProvisionHotAddAndExpand(t *testing.T) {
	base := t.TempDir()
	sanRoot := filepath.Join(base, "san")
	cfgRoot := filepath.Join(base, "nvmet")
	_ = os.MkdirAll(sanRoot, 0o700)
	_ = os.MkdirAll(cfgRoot, 0o755)

	mgr := NewManager(sanRoot, cfgRoot, false)
	systemBytes := int64((StateStartLBA + StateSizeSectors + 512) * SectorSize4K)
	userBytes := int64(32 * 1024 * 1024)

	spec := api.SANDeviceSpec{
		DeviceID:     "netboot-004",
		PXEMAC:       "58:47:ca:75:f6:9a",
		ProductUUID:  "c39be600-2d37-11ef-9418-3174c88d7400",
		SubsystemNQN: "nqn.2026-11.arpa.home.pits:netboot-004",
		HostNQN:      "nqn.2026-11.arpa.home.pits:host-netboot-004",
		Address:      "10.89.20.17",
		Port:         4420,
		ConfigfsPort: 4423,
		Namespaces: []api.SANNamespaceSpec{
			{ID: 1, Name: "system", Bytes: systemBytes, InitSystemGPT: true},
			{ID: 2, Name: "userdata", Bytes: userBytes},
		},
	}

	st, err := mgr.Provision(api.SANProvisionRequest{Spec: spec, ConfigureListener: true})
	if err != nil {
		t.Fatalf("initial Provision failed: %v", err)
	}
	if !st.Healthy || st.AllowAnyHost || !st.ListenerActive || len(st.Namespaces) != 2 {
		t.Fatalf("unexpected target status: %+v", st)
	}
	if !st.Namespaces[0].GPTVerified {
		t.Fatalf("expected system.raw 4KiB GPT verified")
	}

	// Forbid shrinking!
	shrinkSpec := spec
	shrinkSpec.Namespaces = []api.SANNamespaceSpec{
		{ID: 1, Name: "system", Bytes: systemBytes},
		{ID: 2, Name: "userdata", Bytes: userBytes - 4096},
	}
	if _, err := mgr.Provision(api.SANProvisionRequest{Spec: shrinkSpec}); err == nil {
		t.Fatalf("expected shrink attempt to be rejected")
	}

	// Hot-add NSID 3 (vmstore) without touching listener!
	spec.VMStore = true
	spec.Namespaces = append(spec.Namespaces, api.SANNamespaceSpec{
		ID: 3, Name: "vmstore", Bytes: 64 * 1024 * 1024,
	})
	st2, err := mgr.Provision(api.SANProvisionRequest{Spec: spec, HotAddOnly: true})
	if err != nil {
		t.Fatalf("hot-add vmstore failed: %v", err)
	}
	if len(st2.Namespaces) != 3 || st2.Namespaces[2].ExpectedWWID != "uuid.c55a459d-a8cb-5177-b2ad-f47d603ee6ed" {
		t.Fatalf("unexpected vmstore status: %+v", st2.Namespaces)
	}

	// Additive expansion of userdata (NSID 2).
	expanded, err := mgr.Expand(api.SANExpandRequest{
		DeviceID:    "netboot-004",
		NamespaceID: 2,
		NewBytes:    userBytes + 8*1024*1024,
	})
	if err != nil {
		t.Fatalf("Expand failed: %v", err)
	}
	if expanded.ActualBytes != userBytes+8*1024*1024 {
		t.Fatalf("unexpected expanded bytes: %d", expanded.ActualBytes)
	}
}
