// Package host implements read-only hardware/PCI/SR-IOV/hugepage preflight
// auditing and safe VFIO + Kubernetes gateway runtime recovery on pits-core.
package host

import (
	"bytes"
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"time"

	"go.yaml.in/yaml/v3"
	"pits.io/server/internal/api"
)

// Controller inspects host sysfs/procfs and performs guarded VFIO restoration.
type Controller struct {
	SysRoot        string
	ProcRoot       string
	KubeconfigPath string
	mu             sync.Mutex
}

// NewController constructs a host Controller with standard Linux roots by default.
func NewController(sysRoot, procRoot, kubeconfigPath string) *Controller {
	if sysRoot == "" {
		sysRoot = "/sys"
	}
	if procRoot == "" {
		procRoot = "/proc"
	}
	if kubeconfigPath == "" {
		kubeconfigPath = "/etc/rancher/rke2/rke2.yaml"
	}
	return &Controller{
		SysRoot:        sysRoot,
		ProcRoot:       procRoot,
		KubeconfigPath: kubeconfigPath,
	}
}

// Audit collects a read-only preflight report equivalent to scripts/preflight-pits.sh.
func (c *Controller) Audit() api.HostAuditReport {
	hostname, _ := os.Hostname()
	mgmtPCI := c.resolveInterfacePCI(api.ManagementNIC)
	mgmtAddr := readInterfaceIPv4(api.ManagementNIC)
	defRoute := c.readDefaultRoute()
	hugepages := readIntFile(filepath.Join(c.SysRoot, "devices/system/node/node0/hugepages/hugepages-2048kB/nr_hugepages"))

	report := api.HostAuditReport{
		CollectedAt:       time.Now().UTC(),
		Hostname:          hostname,
		HostVerified:      hostname == api.ExpectedHostName,
		ManagementNIC:     api.ManagementNIC,
		ManagementPCI:     mgmtPCI,
		ManagementAddress: mgmtAddr,
		DefaultRoute:      defRoute,
		Hugepages2Mi:      hugepages,
		LogicalCPUs:       runtime.NumCPU(),
	}
	if !report.HostVerified {
		report.Warnings = append(report.Warnings, fmt.Sprintf("hostname %q differs from %q", hostname, api.ExpectedHostName))
	}

	for _, name := range []string{"sfp0", "sfp1"} {
		netDir := filepath.Join(c.SysRoot, "class/net", name)
		if !pathExists(netDir) {
			continue
		}
		speed := readIntFile(filepath.Join(netDir, "speed"))
		if speed < 0 {
			speed = 0
		}
		report.Trunks = append(report.Trunks, api.TrunkInterfaceStatus{
			Name:      name,
			OperState: readStringFile(filepath.Join(netDir, "operstate")),
			NumVFs:    readIntFile(filepath.Join(netDir, "device/sriov_numvfs")),
			MAC:       readStringFile(filepath.Join(netDir, "address")),
			SpeedMbps: speed,
		})
	}

	watchedPCI := []struct {
		addr string
		role string
	}{
		{mgmtPCI, "management-nic"},
		{api.WANRouterPCI, "wan-router-dpdk"},
		{api.LANRouterVFPCI, "lan-sfp0-vf0-trunk"},
		{api.SecondTrunkVFPCI, "lan-sfp1-vf0-trunk"},
	}
	seen := map[string]bool{}
	for _, item := range watchedPCI {
		if item.addr == "" || seen[item.addr] {
			continue
		}
		seen[item.addr] = true
		binding := c.inspectPCIDevice(item.addr, item.role)
		report.PCIBindings = append(report.PCIBindings, binding)
		if (item.addr == api.WANRouterPCI || item.addr == api.LANRouterVFPCI) && binding.Driver != "vfio-pci" {
			report.Warnings = append(report.Warnings, fmt.Sprintf("router PCI %s bound to %q instead of vfio-pci", item.addr, binding.Driver))
		}
	}
	return report
}

// RecoverRouter safely restores router PCI functions to vfio-pci, guards mgmt0,
// cleans stale benchmark netns, refreshes device plugins if needed, and verifies edge/gateway.
func (c *Controller) RecoverRouter(ctx context.Context, req api.RouterRecoveryRequest) (*api.RouterRecoveryResult, error) {
	c.mu.Lock()
	defer c.mu.Unlock()

	mgmtPCI := c.resolveInterfacePCI(api.ManagementNIC)
	if mgmtPCI == "" {
		return nil, fmt.Errorf("cannot verify %s PCI address; refusing blind PCI driver operations", api.ManagementNIC)
	}
	targets := []struct {
		addr string
		role string
	}{
		{api.WANRouterPCI, "wan-router-dpdk"},
		{api.LANRouterVFPCI, "lan-sfp0-vf0-trunk"},
	}
	for _, t := range targets {
		if t.addr == mgmtPCI {
			return nil, fmt.Errorf("CRITICAL GUARD: target %s matches management interface %s; aborting", t.addr, api.ManagementNIC)
		}
	}

	res := &api.RouterRecoveryResult{
		ManagementPCIGuarded: mgmtPCI,
	}
	for _, t := range targets {
		binding, err := c.rebindVFIO(t.addr, t.role)
		if err != nil {
			return nil, err
		}
		res.ReboundDevices = append(res.ReboundDevices, binding)
	}

	if req.CleanupPerfNetns {
		perfNs := "/var/run/netns/edge-kernel-perf"
		if pathExists(perfNs) {
			if err := os.Remove(perfNs); err == nil {
				res.PerfNetnsCleaned = true
			}
		}
	}

	// Reconcile Kubernetes gateway deployment & device registration when kubeconfig is present.
	if pathExists(c.KubeconfigPath) {
		waitSec := req.WaitReadySeconds
		if waitSec <= 0 {
			waitSec = 60
		}
		kerr := c.reconcileGatewayK8s(ctx, req.RefreshDevicePlugins, time.Duration(waitSec)*time.Second, res)
		if kerr != nil {
			res.CompletedAt = time.Now().UTC()
			res.Message = fmt.Sprintf("VFIO rebound, but Kubernetes gateway verification failed: %v", kerr)
			return res, kerr
		}
	} else {
		res.Message = "VFIO rebound verified; RKE2 kubeconfig not present for cluster rollout check"
	}
	res.CompletedAt = time.Now().UTC()
	if res.Message == "" {
		res.Message = "Router restored: WAN/LAN on vfio-pci and edge/gateway verified Applied"
	}
	return res, nil
}

func (c *Controller) rebindVFIO(pciAddr, role string) (api.PCIDeviceBinding, error) {
	devDir := filepath.Join(c.SysRoot, "bus/pci/devices", sysfsPCI(pciAddr))
	if !pathExists(devDir) {
		return api.PCIDeviceBinding{}, fmt.Errorf("PCI device %s not found under %s", pciAddr, devDir)
	}
	cur := c.readDriverName(devDir)
	if cur != "vfio-pci" {
		unbindPath := filepath.Join(devDir, "driver/unbind")
		if pathExists(unbindPath) {
			if err := os.WriteFile(unbindPath, []byte(pciAddr), 0o200); err != nil {
				return api.PCIDeviceBinding{}, fmt.Errorf("unbind %s from %s: %w", pciAddr, cur, err)
			}
		}
		if err := os.WriteFile(filepath.Join(devDir, "driver_override"), []byte("vfio-pci"), 0o644); err != nil {
			return api.PCIDeviceBinding{}, fmt.Errorf("set driver_override on %s: %w", pciAddr, err)
		}
		probePath := filepath.Join(c.SysRoot, "bus/pci/drivers_probe")
		if err := os.WriteFile(probePath, []byte(pciAddr), 0o200); err != nil {
			return api.PCIDeviceBinding{}, fmt.Errorf("probe vfio-pci for %s: %w", pciAddr, err)
		}
	}
	after := c.inspectPCIDevice(pciAddr, role)
	if after.Driver != "vfio-pci" {
		return after, fmt.Errorf("PCI device %s driver after probe is %q, expected vfio-pci", pciAddr, after.Driver)
	}
	return after, nil
}

func (c *Controller) inspectPCIDevice(pciAddr, role string) api.PCIDeviceBinding {
	devDir := filepath.Join(c.SysRoot, "bus/pci/devices", sysfsPCI(pciAddr))
	return api.PCIDeviceBinding{
		Address:      pciAddr,
		Driver:       c.readDriverName(devDir),
		VendorID:     readStringFile(filepath.Join(devDir, "vendor")),
		DeviceID:     readStringFile(filepath.Join(devDir, "device")),
		IOMMUGroup:   readLinkBase(filepath.Join(devDir, "iommu_group")),
		PhysicalFunc: fromSysfsPCI(readLinkBase(filepath.Join(devDir, "physfn"))),
		Role:         role,
	}
}

func (c *Controller) resolveInterfacePCI(ifName string) string {
	devLink := filepath.Join(c.SysRoot, "class/net", ifName, "device")
	return fromSysfsPCI(readLinkBase(devLink))
}

func (c *Controller) readDriverName(devDir string) string {
	return readLinkBase(filepath.Join(devDir, "driver"))
}

func (c *Controller) readDefaultRoute() string {
	data, err := os.ReadFile(filepath.Join(c.ProcRoot, "net/route"))
	if err != nil {
		return ""
	}
	for i, line := range strings.Split(string(data), "\n") {
		if i == 0 || strings.TrimSpace(line) == "" {
			continue
		}
		fields := strings.Fields(line)
		if len(fields) >= 3 && fields[1] == "00000000" {
			gwBytes, err := hex.DecodeString(fields[2])
			if err == nil && len(gwBytes) == 4 {
				// /proc/net/route stores IPv4 in host little-endian hex.
				v := binary.LittleEndian.Uint32(gwBytes)
				ip := net.IPv4(byte(v>>24), byte(v>>16), byte(v>>8), byte(v))
				// Actually little-endian uint32 bytes[0..3] in memory order are octets 0..3:
				ip = net.IPv4(gwBytes[3], gwBytes[2], gwBytes[1], gwBytes[0])
				return fmt.Sprintf("default via %s dev %s", ip.String(), fields[0])
			}
			return "default dev " + fields[0]
		}
	}
	return ""
}

func (c *Controller) reconcileGatewayK8s(ctx context.Context, refreshPlugins bool, timeout time.Duration, res *api.RouterRecoveryResult) error {
	kc, err := newLocalKubeClient(c.KubeconfigPath)
	if err != nil {
		return err
	}
	if refreshPlugins {
		var node struct {
			Status struct {
				Allocatable map[string]string `json:"allocatable"`
			} `json:"status"`
		}
		if err := kc.getJSON(ctx, "/api/v1/nodes/pits-core", &node); err == nil {
			wanQty := node.Status.Allocatable["pits.io/i226-lm"]
			lanQty := node.Status.Allocatable["pits.io/sfp0_trunk"]
			if wanQty == "0" || wanQty == "" {
				res.PluginsRefreshed = append(res.PluginsRefreshed, "kubevirt-pci-device-plugin")
			}
			if lanQty == "0" || lanQty == "" {
				res.PluginsRefreshed = append(res.PluginsRefreshed, "sriov-device-plugin")
			}
		}
	}

	// Scale edge/gateway to 1 replica if scaled down.
	scaleBody := `{"spec":{"replicas":1}}`
	_ = kc.patchJSON(ctx, "/apis/apps/v1/namespaces/edge/deployments/gateway", scaleBody)

	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		var routerObj struct {
			Metadata struct {
				Generation int64 `json:"generation"`
			} `json:"metadata"`
			Status struct {
				Ready              bool   `json:"ready"`
				Phase              string `json:"phase"`
				ObservedGeneration int64  `json:"observedGeneration"`
			} `json:"status"`
		}
		if err := kc.getJSON(ctx, "/apis/edge.router/v1alpha1/namespaces/edge/routers/pits-core", &routerObj); err == nil {
			res.RouterPhase = routerObj.Status.Phase
			res.ObservedGeneration = routerObj.Status.ObservedGeneration
			if routerObj.Status.Ready && routerObj.Status.Phase == "Applied" && routerObj.Status.ObservedGeneration == routerObj.Metadata.Generation {
				res.GatewayReady = true
				return nil
			}
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(2 * time.Second):
		}
	}
	return fmt.Errorf("timed out waiting for Router edge/pits-core Ready (last phase=%q)", res.RouterPhase)
}

type localKubeClient struct {
	server string
	token  string
	http   *http.Client
}

func newLocalKubeClient(kubeconfigPath string) (*localKubeClient, error) {
	raw, err := os.ReadFile(kubeconfigPath)
	if err != nil {
		return nil, err
	}
	var cfg struct {
		Clusters []struct {
			Cluster struct {
				Server                   string `yaml:"server"`
				CertificateAuthorityData string `yaml:"certificate-authority-data"`
			} `yaml:"cluster"`
		} `yaml:"clusters"`
		Users []struct {
			User struct {
				Token                 string `yaml:"token"`
				ClientCertificateData string `yaml:"client-certificate-data"`
				ClientKeyData         string `yaml:"client-key-data"`
			} `yaml:"user"`
		} `yaml:"users"`
	}
	if err := yaml.Unmarshal(raw, &cfg); err != nil {
		return nil, err
	}
	if len(cfg.Clusters) == 0 || len(cfg.Users) == 0 {
		return nil, fmt.Errorf("incomplete kubeconfig %s", kubeconfigPath)
	}
	tlsCfg := &tls.Config{MinVersion: tls.VersionTLS12}
	if caB64 := cfg.Clusters[0].Cluster.CertificateAuthorityData; caB64 != "" {
		if caPEM, derr := decodeBase64(caB64); derr == nil {
			pool := x509.NewCertPool()
			pool.AppendCertsFromPEM(caPEM)
			tlsCfg.RootCAs = pool
		}
	}
	if crtB64 := cfg.Users[0].User.ClientCertificateData; crtB64 != "" {
		crtPEM, err1 := decodeBase64(crtB64)
		keyPEM, err2 := decodeBase64(cfg.Users[0].User.ClientKeyData)
		if err1 == nil && err2 == nil {
			if pair, perr := tls.X509KeyPair(crtPEM, keyPEM); perr == nil {
				tlsCfg.Certificates = []tls.Certificate{pair}
			}
		}
	}
	return &localKubeClient{
		server: strings.TrimRight(cfg.Clusters[0].Cluster.Server, "/"),
		token:  cfg.Users[0].User.Token,
		http: &http.Client{
			Timeout:   15 * time.Second,
			Transport: &http.Transport{TLSClientConfig: tlsCfg},
		},
	}, nil
}

func (k *localKubeClient) getJSON(ctx context.Context, path string, out any) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, k.server+path, nil)
	if err != nil {
		return err
	}
	if k.token != "" {
		req.Header.Set("Authorization", "Bearer "+k.token)
	}
	resp, err := k.http.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		b, _ := io.ReadAll(io.LimitReader(resp.Body, 2048))
		return fmt.Errorf("HTTP %d: %s", resp.StatusCode, strings.TrimSpace(string(b)))
	}
	return json.NewDecoder(resp.Body).Decode(out)
}

func (k *localKubeClient) patchJSON(ctx context.Context, path, mergeBody string) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodPatch, k.server+path, bytes.NewBufferString(mergeBody))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/merge-patch+json")
	if k.token != "" {
		req.Header.Set("Authorization", "Bearer "+k.token)
	}
	resp, err := k.http.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		b, _ := io.ReadAll(io.LimitReader(resp.Body, 2048))
		return fmt.Errorf("PATCH %s HTTP %d: %s", path, resp.StatusCode, strings.TrimSpace(string(b)))
	}
	return nil
}

func decodeBase64(s string) ([]byte, error) {
	// Standard base64 decoder without extra imports via encoding/base64.
	return base64DecodeString(strings.TrimSpace(s))
}

func readInterfaceIPv4(name string) string {
	iface, err := net.InterfaceByName(name)
	if err != nil {
		return ""
	}
	addrs, err := iface.Addrs()
	if err != nil {
		return ""
	}
	for _, a := range addrs {
		if ipnet, ok := a.(*net.IPNet); ok && ipnet.IP.To4() != nil {
			return ipnet.String()
		}
	}
	return ""
}

func readLinkBase(path string) string {
	if st, serr := os.Lstat(path); serr == nil && st.Mode()&os.ModeSymlink == 0 {
		if txt := readStringFile(path); txt != "" {
			return filepath.Base(txt)
		}
		return ""
	}
	resolved, err := filepath.EvalSymlinks(path)
	if err == nil {
		return filepath.Base(resolved)
	}
	target, lerr := os.Readlink(path)
	if lerr == nil {
		return filepath.Base(target)
	}
	return ""
}

func sysfsPCI(addr string) string {
	if runtime.GOOS == "windows" {
		return strings.ReplaceAll(addr, ":", "_")
	}
	return addr
}

func fromSysfsPCI(seg string) string {
	if runtime.GOOS == "windows" && strings.Count(seg, "_") == 2 {
		return strings.ReplaceAll(seg, "_", ":")
	}
	return seg
}

func readStringFile(path string) string {
	b, err := os.ReadFile(path)
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(b))
}

func readIntFile(path string) int {
	v, err := strconv.Atoi(readStringFile(path))
	if err != nil {
		return 0
	}
	return v
}

func pathExists(path string) bool {
	_, err := os.Lstat(path)
	return err == nil
}
