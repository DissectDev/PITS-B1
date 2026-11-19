// Package clusters manages Kubernetes cluster registrations and live
// Kubernetes + DRA v1 GPU/NPU discovery for pits-agent.
package clusters

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/url"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"pits.io/server/internal/api"
	"pits.io/server/internal/kube"
)

// ClientFactory creates a Kubernetes REST client from a kubeconfig path.
type ClientFactory func(kubeconfigPath string) (*kube.Client, error)

// Manager maintains registered Kubernetes clusters and probes their nodes and DRA v1 slices.
type Manager struct {
	defaultKubeconfig string
	registryPath      string
	clustersDir       string
	factory           ClientFactory

	mu            sync.RWMutex
	registrations []api.ClusterRegistration
}

// NewManager initializes a cluster Manager backed by an optional JSON registry file.
func NewManager(defaultKubeconfig, registryPath string, factory ClientFactory) *Manager {
	if factory == nil {
		factory = kube.NewFromKubeconfig
	}
	clustersDir := ""
	if registryPath != "" {
		clustersDir = filepath.Join(filepath.Dir(registryPath), "clusters")
	}
	m := &Manager{
		defaultKubeconfig: defaultKubeconfig,
		registryPath:      registryPath,
		clustersDir:       clustersDir,
		factory:           factory,
	}
	m.load()
	return m
}

func (m *Manager) load() {
	if m.registryPath == "" {
		return
	}
	raw, err := os.ReadFile(m.registryPath)
	if err != nil {
		return
	}
	var regs []api.ClusterRegistration
	if err := json.Unmarshal(raw, &regs); err != nil {
		return
	}
	m.mu.Lock()
	m.registrations = regs
	m.mu.Unlock()
}

func (m *Manager) saveLocked() error {
	if m.registryPath == "" {
		return nil
	}
	if err := os.MkdirAll(filepath.Dir(m.registryPath), 0o700); err != nil {
		return err
	}
	raw, err := json.MarshalIndent(m.registrations, "", "  ")
	if err != nil {
		return err
	}
	tmp := m.registryPath + ".tmp"
	if err := os.WriteFile(tmp, raw, 0o600); err != nil {
		return err
	}
	return os.Rename(tmp, m.registryPath)
}

func sanitizeName(raw string) string {
	var b strings.Builder
	prevDash := false
	for _, r := range strings.ToLower(strings.TrimSpace(raw)) {
		if (r >= 'a' && r <= 'z') || (r >= '0' && r <= '9') {
			b.WriteRune(r)
			prevDash = false
		} else if !prevDash && b.Len() > 0 {
			b.WriteByte('-')
			prevDash = true
		}
	}
	return strings.Trim(b.String(), "-")
}

// Register validates and persists a Kubernetes cluster into pits-agent's discovery scope.
func (m *Manager) Register(req api.ClusterRegisterRequest) (*api.ClusterRegistration, error) {
	name := sanitizeName(req.Name)
	if name == "" {
		return nil, fmt.Errorf("cluster name is required")
	}
	kcPath := strings.TrimSpace(req.KubeconfigPath)
	kcYAML := strings.TrimSpace(req.KubeconfigYAML)
	if kcPath == "" && kcYAML == "" {
		return nil, fmt.Errorf("either kubeconfigPath or kubeconfigYaml is required")
	}

	var client *kube.Client
	var err error
	if kcYAML != "" {
		client, err = kube.NewFromKubeconfigBytes([]byte(kcYAML))
		if err != nil {
			return nil, fmt.Errorf("invalid kubeconfigYaml: %w", err)
		}
		if m.clustersDir != "" {
			if err := os.MkdirAll(m.clustersDir, 0o700); err != nil {
				return nil, fmt.Errorf("create clusters dir: %w", err)
			}
			kcPath = filepath.Join(m.clustersDir, name+".yaml")
			if err := os.WriteFile(kcPath, []byte(kcYAML), 0o600); err != nil {
				return nil, fmt.Errorf("write kubeconfig %s: %w", kcPath, err)
			}
		}
	} else {
		client, err = m.factory(kcPath)
		if err != nil {
			return nil, fmt.Errorf("load kubeconfig %s: %w", kcPath, err)
		}
	}

	reg := api.ClusterRegistration{
		Name:           name,
		DisplayName:    strings.TrimSpace(req.DisplayName),
		Role:           strings.TrimSpace(req.Role),
		KubeconfigPath: kcPath,
		ServerURL:      client.ServerURL(),
	}
	if reg.DisplayName == "" {
		reg.DisplayName = name
	}
	if reg.Role == "" {
		reg.Role = "external-k8s"
	}

	m.mu.Lock()
	defer m.mu.Unlock()
	replaced := false
	for i := range m.registrations {
		if m.registrations[i].Name == name {
			m.registrations[i] = reg
			replaced = true
			break
		}
	}
	if !replaced {
		m.registrations = append(m.registrations, reg)
	}
	if err := m.saveLocked(); err != nil {
		return nil, err
	}
	return &reg, nil
}

// Unregister removes a registered cluster by name.
func (m *Manager) Unregister(name string) error {
	slug := sanitizeName(name)
	m.mu.Lock()
	defer m.mu.Unlock()
	next := make([]api.ClusterRegistration, 0, len(m.registrations))
	found := false
	for _, r := range m.registrations {
		if r.Name == slug || r.Name == name {
			found = true
			continue
		}
		next = append(next, r)
	}
	if !found {
		return fmt.Errorf("cluster %q not registered", name)
	}
	m.registrations = next
	return m.saveLocked()
}

// EffectiveRegistrations returns the local RKE2 cluster (if present) plus all registered clusters.
func (m *Manager) EffectiveRegistrations() []api.ClusterRegistration {
	m.mu.RLock()
	extra := make([]api.ClusterRegistration, len(m.registrations))
	copy(extra, m.registrations)
	m.mu.RUnlock()

	var out []api.ClusterRegistration
	seen := map[string]bool{}
	if m.defaultKubeconfig != "" {
		if _, err := os.Stat(m.defaultKubeconfig); err == nil {
			out = append(out, api.ClusterRegistration{
				Name:           "cluster-pits-core",
				DisplayName:    "cluster-pits-core (Core NixOS RKE2 Cluster)",
				Role:           "core-infrastructure",
				KubeconfigPath: m.defaultKubeconfig,
			})
			seen["cluster-pits-core"] = true
		}
	}
	for _, r := range extra {
		if !seen[r.Name] {
			seen[r.Name] = true
			out = append(out, r)
		}
	}
	return out
}

// AllowedTunnelTarget checks whether hostPort matches any registered cluster API server endpoint.
func (m *Manager) AllowedTunnelTarget(hostPort string) bool {
	reqHost, reqPort, err := net.SplitHostPort(strings.TrimSpace(hostPort))
	if err != nil {
		return false
	}
	for _, r := range m.EffectiveRegistrations() {
		rawURL := r.ServerURL
		if rawURL == "" && r.KubeconfigPath != "" {
			if kc, kerr := m.factory(r.KubeconfigPath); kerr == nil {
				rawURL = kc.ServerURL()
			}
		}
		if rawURL == "" {
			continue
		}
		u, uerr := url.Parse(rawURL)
		if uerr != nil {
			continue
		}
		h := u.Hostname()
		p := u.Port()
		if p == "" {
			if strings.EqualFold(u.Scheme, "https") {
				p = "443"
			} else {
				p = "80"
			}
		}
		if strings.EqualFold(h, reqHost) && p == reqPort {
			return true
		}
	}
	return false
}

// DiscoverAll probes every Kubernetes cluster in pits-agent's discovery scope concurrently.
func (m *Manager) DiscoverAll(ctx context.Context) api.ClusterDiscoveryReport {
	regs := m.EffectiveRegistrations()
	reports := make([]api.DiscoveredClusterReport, len(regs))

	var wg sync.WaitGroup
	for i, reg := range regs {
		wg.Add(1)
		go func(idx int, r api.ClusterRegistration) {
			defer wg.Done()
			probeCtx, cancel := context.WithTimeout(ctx, 8*time.Second)
			defer cancel()
			reports[idx] = m.probeOne(probeCtx, r)
		}(i, reg)
	}
	wg.Wait()

	out := api.ClusterDiscoveryReport{
		CollectedAt:   time.Now().UTC(),
		TotalClusters: len(reports),
		Clusters:      reports,
	}
	for _, c := range reports {
		if c.Reachable {
			out.ReachableClusters++
		}
		out.TotalGPUs += c.GPUCount
		out.AllocatedGPUs += c.AllocatedGPUs
		if c.Crossplane != nil {
			out.TotalCrossplaneResources += len(c.Crossplane.Resources) + len(c.Crossplane.XRDs)
		}
	}
	return out
}

func (m *Manager) probeOne(ctx context.Context, reg api.ClusterRegistration) api.DiscoveredClusterReport {
	rep := api.DiscoveredClusterReport{
		Name:        reg.Name,
		DisplayName: reg.DisplayName,
		Kind:        "kubernetes-cluster",
		Role:        reg.Role,
		ServerURL:   reg.ServerURL,
	}
	kc, err := m.factory(reg.KubeconfigPath)
	if err != nil {
		rep.Error = err.Error()
		return rep
	}
	if kc.ServerURL() != "" {
		rep.ServerURL = kc.ServerURL()
	}

	var ver struct {
		GitVersion string `json:"gitVersion"`
	}
	if err := kc.GetJSON(ctx, "/version", &ver); err != nil {
		rep.Error = err.Error()
		return rep
	}
	rep.Reachable = true
	rep.GitVersion = ver.GitVersion

	var (
		nodes        []api.ClusterNodeInfo
		runP         int
		totP         int
		dpGPUUsed    map[string]int
		pvs          int
		dcs          []string
		slices       []api.DRASliceInfo
		claims       []api.DRAClaimInfo
		xp           *api.CrossplaneInventory
		wg           sync.WaitGroup
	)
	wg.Add(7)
	go func() {
		defer wg.Done()
		nodes = fetchNodes(ctx, kc)
	}()
	go func() {
		defer wg.Done()
		runP, totP, dpGPUUsed = fetchPodCountsAndGPUUsage(ctx, kc)
	}()
	go func() {
		defer wg.Done()
		pvs = fetchBoundPVs(ctx, kc)
	}()
	go func() {
		defer wg.Done()
		dcs = fetchDeviceClasses(ctx, kc)
	}()
	go func() {
		defer wg.Done()
		slices = fetchDRASlices(ctx, kc)
	}()
	go func() {
		defer wg.Done()
		claims = fetchDRAClaims(ctx, kc)
	}()
	go func() {
		defer wg.Done()
		xp = fetchCrossplaneInventory(ctx, kc)
	}()
	wg.Wait()

	rep.Nodes = nodes
	rep.NodeCount = len(nodes)
	for _, n := range nodes {
		if n.Ready {
			rep.ReadyNodes++
		}
	}
	rep.RunningPods = runP
	rep.TotalPods = totP
	rep.BoundPVs = pvs
	rep.DeviceClasses = dcs
	rep.DRASlices = slices
	rep.DRAClaims = claims
	if len(dpGPUUsed) > 0 {
		rep.DevicePluginGPUUsed = dpGPUUsed
	}
	rep.Crossplane = xp
	rep.GPUCount = countGPUs(nodes, slices)
	rep.AllocatedGPUs = countAllocatedGPUs(claims, dpGPUUsed)
	if rep.GPUCount > 0 {
		pct := float64(rep.AllocatedGPUs) * 100.0 / float64(rep.GPUCount)
		if pct > 100.0 {
			pct = 100.0
		}
		rep.GPUOccupancyPct = pct
	}
	return rep
}

func countAllocatedGPUs(claims []api.DRAClaimInfo, dpGPUUsed map[string]int) int {
	draSeen := map[string]bool{}
	for _, c := range claims {
		if !c.Allocated {
			continue
		}
		drv := strings.ToLower(c.Driver)
		if strings.Contains(drv, "gpu") || strings.Contains(drv, "nvidia") {
			key := strings.ToLower(c.Driver + "/" + c.Pool + "/" + c.Device)
			draSeen[key] = true
		}
	}
	if len(draSeen) > 0 {
		return len(draSeen)
	}
	total := 0
	for _, used := range dpGPUUsed {
		total += used
	}
	return total
}

func countGPUs(nodes []api.ClusterNodeInfo, slices []api.DRASliceInfo) int {
	draGPUs := 0
	for _, sl := range slices {
		drv := strings.ToLower(sl.Driver)
		if strings.Contains(drv, "gpu") || strings.Contains(drv, "nvidia") {
			draGPUs += len(sl.Devices)
		}
	}
	if draGPUs > 0 {
		return draGPUs
	}
	allocGPUs := 0
	for _, n := range nodes {
		for k, v := range n.GPUAllocatable {
			kl := strings.ToLower(k)
			if kl == "nvidia.com/gpu" || kl == "amd.com/gpu" || strings.HasPrefix(kl, "gpu.intel.com/") {
				if cnt, err := strconv.Atoi(strings.TrimSpace(v)); err == nil && cnt > 0 {
					allocGPUs += cnt
				}
			}
		}
	}
	return allocGPUs
}

func fetchNodes(ctx context.Context, kc *kube.Client) []api.ClusterNodeInfo {
	var raw struct {
		Items []struct {
			Metadata struct {
				Name   string            `json:"name"`
				Labels map[string]string `json:"labels"`
			} `json:"metadata"`
			Status struct {
				NodeInfo struct {
					KubeletVersion string `json:"kubeletVersion"`
				} `json:"nodeInfo"`
				Addresses []struct {
					Type    string `json:"type"`
					Address string `json:"address"`
				} `json:"addresses"`
				Conditions []struct {
					Type   string `json:"type"`
					Status string `json:"status"`
				} `json:"conditions"`
				Allocatable map[string]string `json:"allocatable"`
			} `json:"status"`
		} `json:"items"`
	}
	if err := kc.GetJSON(ctx, "/api/v1/nodes", &raw); err != nil {
		return nil
	}
	out := make([]api.ClusterNodeInfo, 0, len(raw.Items))
	for _, item := range raw.Items {
		info := api.ClusterNodeInfo{
			Name:           item.Metadata.Name,
			KubeletVersion: item.Status.NodeInfo.KubeletVersion,
		}
		for _, a := range item.Status.Addresses {
			if a.Type == "InternalIP" {
				info.InternalIP = a.Address
				break
			}
		}
		for _, c := range item.Status.Conditions {
			if c.Type == "Ready" && c.Status == "True" {
				info.Ready = true
				break
			}
		}
		for k := range item.Metadata.Labels {
			if role, ok := strings.CutPrefix(k, "node-role.kubernetes.io/"); ok && role != "" {
				info.Roles = append(info.Roles, role)
			}
		}
		sort.Strings(info.Roles)
		for k, v := range item.Status.Allocatable {
			kl := strings.ToLower(k)
			if strings.Contains(kl, "gpu") || strings.Contains(kl, "npu") || strings.Contains(kl, "nvidia") || strings.Contains(kl, "amd") || strings.Contains(kl, "intel") {
				if info.GPUAllocatable == nil {
					info.GPUAllocatable = map[string]string{}
				}
				info.GPUAllocatable[k] = v
			}
		}
		out = append(out, info)
	}
	return out
}

func fetchPodCountsAndGPUUsage(ctx context.Context, kc *kube.Client) (running int, total int, dpGPUUsed map[string]int) {
	var raw struct {
		Items []struct {
			Spec struct {
				NodeName   string `json:"nodeName"`
				Containers []struct {
					Resources struct {
						Limits   map[string]string `json:"limits"`
						Requests map[string]string `json:"requests"`
					} `json:"resources"`
				} `json:"containers"`
			} `json:"spec"`
			Status struct {
				Phase string `json:"phase"`
			} `json:"status"`
		} `json:"items"`
	}
	if err := kc.GetJSON(ctx, "/api/v1/pods", &raw); err != nil {
		return 0, 0, nil
	}
	dpGPUUsed = map[string]int{}
	total = len(raw.Items)
	for _, p := range raw.Items {
		if p.Status.Phase == "Running" {
			running++
		}
		if p.Status.Phase == "Succeeded" || p.Status.Phase == "Failed" {
			continue
		}
		node := p.Spec.NodeName
		if node == "" {
			continue
		}
		for _, c := range p.Spec.Containers {
			seenKeys := map[string]bool{}
			for _, resMap := range []map[string]string{c.Resources.Limits, c.Resources.Requests} {
				for k, v := range resMap {
					kl := strings.ToLower(k)
					if seenKeys[kl] {
						continue
					}
					if kl == "nvidia.com/gpu" || kl == "amd.com/gpu" || strings.HasPrefix(kl, "gpu.intel.com/") {
						if cnt, err := strconv.Atoi(strings.TrimSpace(v)); err == nil && cnt > 0 {
							dpGPUUsed[node] += cnt
							seenKeys[kl] = true
						}
					}
				}
			}
		}
	}
	return running, total, dpGPUUsed
}

func fetchDRAClaims(ctx context.Context, kc *kube.Client) []api.DRAClaimInfo {
	var raw struct {
		Items []struct {
			Metadata struct {
				Namespace string `json:"namespace"`
				Name      string `json:"name"`
			} `json:"metadata"`
			Status struct {
				Allocation *struct {
					Devices struct {
						Results []struct {
							Driver string `json:"driver"`
							Pool   string `json:"pool"`
							Device string `json:"device"`
						} `json:"results"`
					} `json:"devices"`
					NodeSelector *struct {
						NodeSelectorTerms []struct {
							MatchExpressions []struct {
								Key      string   `json:"key"`
								Operator string   `json:"operator"`
								Values   []string `json:"values"`
							} `json:"matchExpressions"`
							MatchFields []struct {
								Key      string   `json:"key"`
								Operator string   `json:"operator"`
								Values   []string `json:"values"`
							} `json:"matchFields"`
						} `json:"nodeSelectorTerms"`
					} `json:"nodeSelector"`
				} `json:"allocation"`
				ReservedFor []struct {
					Resource string `json:"resource"`
					Name     string `json:"name"`
				} `json:"reservedFor"`
			} `json:"status"`
		} `json:"items"`
	}
	if err := kc.GetJSON(ctx, "/apis/resource.k8s.io/v1/resourceclaims", &raw); err != nil {
		return nil
	}
	out := make([]api.DRAClaimInfo, 0, len(raw.Items))
	for _, it := range raw.Items {
		if it.Status.Allocation == nil || len(it.Status.Allocation.Devices.Results) == 0 {
			continue
		}
		var nodeName string
		if ns := it.Status.Allocation.NodeSelector; ns != nil {
			for _, term := range ns.NodeSelectorTerms {
				for _, f := range term.MatchFields {
					if f.Key == "metadata.name" && len(f.Values) > 0 {
						nodeName = f.Values[0]
						break
					}
				}
				if nodeName == "" {
					for _, e := range term.MatchExpressions {
						if e.Key == "kubernetes.io/hostname" && len(e.Values) > 0 {
							nodeName = e.Values[0]
							break
						}
					}
				}
			}
		}
		consumers := make([]string, 0, len(it.Status.ReservedFor))
		for _, r := range it.Status.ReservedFor {
			if r.Name != "" {
				consumers = append(consumers, it.Metadata.Namespace+"/"+r.Name)
			}
		}
		for _, res := range it.Status.Allocation.Devices.Results {
			resolvedNode := nodeName
			if resolvedNode == "" && res.Pool != "" {
				resolvedNode = res.Pool
			}
			out = append(out, api.DRAClaimInfo{
				Namespace:     it.Metadata.Namespace,
				Name:          it.Metadata.Name,
				Driver:        res.Driver,
				Pool:          res.Pool,
				Device:        res.Device,
				NodeName:      resolvedNode,
				Allocated:     true,
				ConsumerPods:  consumers,
				ConsumerCount: len(consumers),
			})
		}
	}
	return out
}

func fetchBoundPVs(ctx context.Context, kc *kube.Client) int {
	var raw struct {
		Items []struct {
			Status struct {
				Phase string `json:"phase"`
			} `json:"status"`
		} `json:"items"`
	}
	if err := kc.GetJSON(ctx, "/api/v1/persistentvolumes", &raw); err != nil {
		return 0
	}
	bound := 0
	for _, pv := range raw.Items {
		if pv.Status.Phase == "Bound" {
			bound++
		}
	}
	return bound
}

func fetchDeviceClasses(ctx context.Context, kc *kube.Client) []string {
	var raw struct {
		Items []struct {
			Metadata struct {
				Name string `json:"name"`
			} `json:"metadata"`
		} `json:"items"`
	}
	if err := kc.GetJSON(ctx, "/apis/resource.k8s.io/v1/deviceclasses", &raw); err != nil {
		return nil
	}
	out := make([]string, 0, len(raw.Items))
	for _, it := range raw.Items {
		if it.Metadata.Name != "" {
			out = append(out, it.Metadata.Name)
		}
	}
	sort.Strings(out)
	return out
}

type rawAttrValue struct {
	String      *string  `json:"string,omitempty"`
	StringValue *string  `json:"stringValue,omitempty"`
	Int         *int64   `json:"int,omitempty"`
	IntValue    *int64   `json:"intValue,omitempty"`
	Bool        *bool    `json:"bool,omitempty"`
	Version     *string  `json:"version,omitempty"`
}

func (v rawAttrValue) text() string {
	switch {
	case v.String != nil:
		return *v.String
	case v.StringValue != nil:
		return *v.StringValue
	case v.Version != nil:
		return *v.Version
	case v.Int != nil:
		return strconv.FormatInt(*v.Int, 10)
	case v.IntValue != nil:
		return strconv.FormatInt(*v.IntValue, 10)
	case v.Bool != nil:
		return strconv.FormatBool(*v.Bool)
	default:
		return ""
	}
}

func fetchDRASlices(ctx context.Context, kc *kube.Client) []api.DRASliceInfo {
	var raw struct {
		Items []struct {
			Metadata struct {
				Name string `json:"name"`
			} `json:"metadata"`
			Spec struct {
				NodeName string `json:"nodeName"`
				Driver   string `json:"driver"`
				Devices  []struct {
					Name       string                  `json:"name"`
					Attributes map[string]rawAttrValue `json:"attributes"`
					Capacity   map[string]struct {
						Value string `json:"value"`
					} `json:"capacity"`
				} `json:"devices"`
			} `json:"spec"`
		} `json:"items"`
	}
	if err := kc.GetJSON(ctx, "/apis/resource.k8s.io/v1/resourceslices", &raw); err != nil {
		return nil
	}
	out := make([]api.DRASliceInfo, 0, len(raw.Items))
	for _, item := range raw.Items {
		nodeName := item.Spec.NodeName
		if nodeName == "" {
			nodeName = "cluster"
		}
		slice := api.DRASliceInfo{
			SliceName:   item.Metadata.Name,
			NodeName:    nodeName,
			Driver:      item.Spec.Driver,
			DeviceClass: inferDeviceClass(item.Spec.Driver),
		}
		for _, dev := range item.Spec.Devices {
			attrs := map[string]string{}
			for k, val := range dev.Attributes {
				if t := val.text(); t != "" {
					attrs[k] = t
				}
			}
			mem := ""
			if mCap, ok := dev.Capacity["memory"]; ok && mCap.Value != "" {
				mem = mCap.Value
			}
			vendor := firstNonEmpty(attrs["vendor"], inferVendor(item.Spec.Driver, attrs))
			productName := firstNonEmpty(attrs["productName"], attrs["model"])
			pciAddr := firstNonEmpty(attrs["pciAddress"], attrs["pcieBusID"])
			drvVer := firstNonEmpty(attrs["driverVersion"], attrs["cudaDriverVersion"])
			slice.Devices = append(slice.Devices, api.DRADeviceInfo{
				Name:          dev.Name,
				Vendor:        vendor,
				Model:         firstNonEmpty(attrs["model"], productName),
				ProductName:   productName,
				Architecture:  attrs["architecture"],
				UUID:          attrs["uuid"],
				PCIAddress:    pciAddr,
				KernelDriver:  attrs["kernelDriver"],
				DriverVersion: drvVer,
				Memory:        mem,
				Attributes:    attrs,
			})
		}
		out = append(out, slice)
	}
	return out
}

func inferDeviceClass(driver string) string {
	low := strings.ToLower(driver)
	switch {
	case low == "gpu.nvidia.com" || strings.Contains(low, "nvidia"):
		return "gpu.nvidia.com"
	case strings.Contains(low, "npu"):
		return "pits-intel-npu"
	default:
		return driver
	}
}

func inferVendor(driver string, attrs map[string]string) string {
	low := strings.ToLower(driver + " " + attrs["productName"] + " " + attrs["brand"])
	switch {
	case strings.Contains(low, "nvidia"):
		return "nvidia"
	case strings.Contains(low, "amd"):
		return "amd"
	case strings.Contains(low, "intel"):
		return "intel"
	default:
		return ""
	}
}

func firstNonEmpty(values ...string) string {
	for _, v := range values {
		if strings.TrimSpace(v) != "" {
			return v
		}
	}
	return ""
}
