package host

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"pits.io/server/internal/api"
)

func TestHostControllerMgmt0SafetyGuard(t *testing.T) {
	base := t.TempDir()
	sysRoot := filepath.Join(base, "sys")
	procRoot := filepath.Join(base, "proc")
	_ = os.MkdirAll(filepath.Join(sysRoot, "class/net/mgmt0"), 0o755)
	_ = os.MkdirAll(procRoot, 0o755)

	// Simulate catastrophic miswiring where mgmt0 resolves to WANRouterPCI (0000:5b:00.0):
	// RecoverRouter MUST refuse immediately with CRITICAL GUARD!
	_ = os.WriteFile(filepath.Join(sysRoot, "class/net/mgmt0/device"), []byte("/sys/bus/pci/devices/"+sysfsPCI(api.WANRouterPCI)), 0o644)

	ctrl := NewController(sysRoot, procRoot, filepath.Join(base, "missing-kubeconfig"))
	_, err := ctrl.RecoverRouter(context.Background(), api.RouterRecoveryRequest{})
	if err == nil || !strings.Contains(err.Error(), "CRITICAL GUARD") {
		t.Fatalf("expected CRITICAL GUARD when mgmt0 matches router PCI, got: %v", err)
	}

	// Now wire mgmt0 properly to 0000:58:00.0 and verify Audit + RecoverRouter when WAN/LAN are on vfio-pci.
	_ = os.WriteFile(filepath.Join(sysRoot, "class/net/mgmt0/device"), []byte("/sys/bus/pci/devices/"+sysfsPCI("0000:58:00.0")), 0o644)
	for _, item := range []struct {
		addr   string
		driver string
	}{
		{"0000:58:00.0", "igc"},
		{api.WANRouterPCI, "vfio-pci"},
		{api.LANRouterVFPCI, "vfio-pci"},
	} {
		devDir := filepath.Join(sysRoot, "bus/pci/devices", sysfsPCI(item.addr))
		_ = os.MkdirAll(devDir, 0o755)
		_ = os.WriteFile(filepath.Join(devDir, "driver"), []byte("/sys/bus/pci/drivers/"+item.driver), 0o644)
		_ = os.WriteFile(filepath.Join(devDir, "vendor"), []byte("0x8086"), 0o644)
		_ = os.WriteFile(filepath.Join(devDir, "device"), []byte("0x125b"), 0o644)
		_ = os.WriteFile(filepath.Join(devDir, "iommu_group"), []byte("/sys/kernel/iommu_groups/24"), 0o644)
	}

	audit := ctrl.Audit()
	if audit.ManagementPCI != "0000:58:00.0" || len(audit.PCIBindings) < 3 {
		t.Fatalf("unexpected Audit output: %+v", audit)
	}
	res, err := ctrl.RecoverRouter(context.Background(), api.RouterRecoveryRequest{})
	if err != nil || res.ManagementPCIGuarded != "0000:58:00.0" || len(res.ReboundDevices) != 2 {
		t.Fatalf("RecoverRouter failed: res=%+v, err=%v", res, err)
	}
}
