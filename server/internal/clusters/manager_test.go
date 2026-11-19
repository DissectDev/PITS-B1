package clusters

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"testing"

	"pits.io/server/internal/api"
	"pits.io/server/internal/kube"
)

func TestClusterRegisterAndDRAGPUDiscovery(t *testing.T) {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /version", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]string{"gitVersion": "v1.36.6"})
	})
	mux.HandleFunc("GET /api/v1/nodes", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"metadata": map[string]any{
						"name": "cp0",
						"labels": map[string]string{
							"node-role.kubernetes.io/control-plane": "",
						},
					},
					"status": map[string]any{
						"nodeInfo":   map[string]string{"kubeletVersion": "v1.34.3"},
						"addresses":  []map[string]string{{"type": "InternalIP", "address": "172.30.0.1"}},
						"conditions": []map[string]string{{"type": "Ready", "status": "True"}},
						"allocatable": map[string]string{
							"cpu":            "192",
							"nvidia.com/gpu": "8",
						},
					},
				},
			},
		})
	})
	mux.HandleFunc("GET /api/v1/pods", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{"status": map[string]string{"phase": "Running"}},
			},
		})
	})
	mux.HandleFunc("GET /api/v1/persistentvolumes", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{"items": []any{}})
	})
	mux.HandleFunc("GET /apis/resource.k8s.io/v1/deviceclasses", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{"metadata": map[string]string{"name": "gpu.nvidia.com"}},
			},
		})
	})
	mux.HandleFunc("GET /apis/resource.k8s.io/v1/resourceslices", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"metadata": map[string]string{"name": "cp0-gpu.nvidia.com-abc"},
					"spec": map[string]any{
						"nodeName": "cp0",
						"driver":   "gpu.nvidia.com",
						"devices": []map[string]any{
							{
								"name": "gpu-0",
								"attributes": map[string]any{
									"productName":   map[string]string{"string": "NVIDIA H20"},
									"architecture":  map[string]string{"string": "Hopper"},
									"driverVersion": map[string]string{"version": "580.126.20"},
									"uuid":          map[string]string{"string": "GPU-1111"},
									"pcieBusID":     map[string]string{"string": "0000:18:00.0"},
								},
								"capacity": map[string]any{
									"memory": map[string]string{"value": "97871Mi"},
								},
							},
						},
					},
				},
			},
		})
	})

	srv := httptest.NewServer(mux)
	defer srv.Close()

	dir := t.TempDir()
	regPath := filepath.Join(dir, "clusters.json")
	mgr := NewManager("", regPath, func(kcPath string) (*kube.Client, error) {
		return kube.NewFromKubeconfigBytes([]byte(`apiVersion: v1
kind: Config
clusters:
- name: vc
  cluster:
    server: ` + srv.URL + `
contexts:
- name: vc
  context:
    cluster: vc
    user: admin
current-context: vc
users:
- name: admin
  user:
    token: test-tok
`))
	})

	kcYAML := "apiVersion: v1\nkind: Config\nclusters:\n- name: vc\n  cluster:\n    server: " + srv.URL + "\ncontexts:\n- name: vc\n  context:\n    cluster: vc\n    user: admin\ncurrent-context: vc\nusers:\n- name: admin\n  user:\n    token: test-tok\n"
	reg, err := mgr.Register(api.ClusterRegisterRequest{
		Name:           "work82-vcluster",
		DisplayName:    "work82 vcluster-dev",
		Role:           "vcluster-gpu-dra",
		KubeconfigYAML: kcYAML,
	})
	if err != nil {
		t.Fatalf("Register failed: %v", err)
	}
	if reg.Name != "work82-vcluster" {
		t.Fatalf("unexpected name: %s", reg.Name)
	}

	report := mgr.DiscoverAll(context.Background())
	if report.TotalClusters != 1 || report.ReachableClusters != 1 || report.TotalGPUs != 1 {
		t.Fatalf("unexpected report totals: %+v", report)
	}
	cl := report.Clusters[0]
	if cl.Kind != "kubernetes-cluster" || cl.GitVersion != "v1.36.6" {
		t.Fatalf("unexpected cluster report: %+v", cl)
	}
	if len(cl.DRASlices) != 1 || cl.DRASlices[0].Devices[0].ProductName != "NVIDIA H20" {
		t.Fatalf("unexpected DRA slices: %+v", cl.DRASlices)
	}

	// Reload from disk and verify persistence
	mgr2 := NewManager("", regPath, nil)
	regs := mgr2.EffectiveRegistrations()
	if len(regs) != 1 || regs[0].Name != "work82-vcluster" {
		t.Fatalf("expected persisted registration after reload, got %+v", regs)
	}
}

func TestCrossplaneDiscoveryAIStorageAuthFlowAndLabDevStack(t *testing.T) {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /version", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]string{"gitVersion": "v1.37.1"})
	})
	mux.HandleFunc("GET /api/v1/nodes", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{"items": []any{}})
	})
	mux.HandleFunc("GET /api/v1/pods", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{"items": []any{}})
	})
	mux.HandleFunc("GET /api/v1/persistentvolumes", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{"items": []any{}})
	})
	mux.HandleFunc("GET /apis/resource.k8s.io/v1/deviceclasses", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{"items": []any{}})
	})
	mux.HandleFunc("GET /apis/resource.k8s.io/v1/resourceslices", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{"items": []any{}})
	})
	mux.HandleFunc("GET /apis/apiextensions.crossplane.io/v1/compositeresourcedefinitions", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"metadata": map[string]any{"name": "xaistorageworkspaces.storage.ai.pits.io"},
					"spec": map[string]any{
						"group":      "storage.ai.pits.io",
						"names":      map[string]string{"kind": "XAIStorageWorkspace", "plural": "xaistorageworkspaces"},
						"claimNames": map[string]string{"kind": "AIStorageWorkspace", "plural": "aistorageworkspaces"},
						"versions":   []map[string]any{{"name": "v1alpha1", "served": true, "referenceable": true}},
					},
					"status": map[string]any{
						"conditions": []map[string]string{
							{"type": "Established", "status": "True"},
							{"type": "Offered", "status": "True"},
						},
					},
				},
				{
					"metadata": map[string]any{"name": "xauthflowbundles.iam.corp.io"},
					"spec": map[string]any{
						"group":      "iam.corp.io",
						"names":      map[string]string{"kind": "XAuthFlowBundle", "plural": "xauthflowbundles"},
						"claimNames": map[string]string{"kind": "AuthFlowBundle", "plural": "authflowbundles"},
						"versions":   []map[string]any{{"name": "v1alpha1", "served": true, "referenceable": true}},
					},
					"status": map[string]any{
						"conditions": []map[string]string{
							{"type": "Established", "status": "True"},
							{"type": "Offered", "status": "True"},
						},
					},
				},
				{
					"metadata": map[string]any{"name": "xlabdevstacks.lab.pits.io"},
					"spec": map[string]any{
						"group":      "lab.pits.io",
						"names":      map[string]string{"kind": "XLabDevStack", "plural": "xlabdevstacks"},
						"claimNames": map[string]string{"kind": "LabDevStack", "plural": "labdevstacks"},
						"versions":   []map[string]any{{"name": "v1alpha1", "served": true, "referenceable": true}},
					},
					"status": map[string]any{
						"conditions": []map[string]string{
							{"type": "Established", "status": "True"},
							{"type": "Offered", "status": "True"},
						},
					},
				},
			},
		})
	})
	mux.HandleFunc("GET /apis/apiextensions.crossplane.io/v1/compositions", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"metadata": map[string]any{"name": "aistorage-workspace-composition"},
					"spec": map[string]any{
						"mode":             "Pipeline",
						"compositeTypeRef": map[string]string{"apiVersion": "storage.ai.pits.io/v1alpha1", "kind": "XAIStorageWorkspace"},
					},
				},
			},
		})
	})
	mux.HandleFunc("GET /apis/pkg.crossplane.io/v1/providers", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"metadata": map[string]any{"name": "provider-keycloak"},
					"spec":     map[string]any{"package": "xpkg.upbound.io/crossplane-contrib/provider-keycloak:v3.2.3"},
					"status":   map[string]any{"conditions": []map[string]string{{"type": "Installed", "status": "True"}, {"type": "Healthy", "status": "True"}}},
				},
			},
		})
	})
	mux.HandleFunc("GET /apis/pkg.crossplane.io/v1/functions", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"metadata": map[string]any{"name": "function-auto-ready"},
					"spec":     map[string]any{"package": "xpkg.upbound.io/crossplane-contrib/function-auto-ready:v0.6.4"},
					"status":   map[string]any{"conditions": []map[string]string{{"type": "Installed", "status": "True"}, {"type": "Healthy", "status": "True"}}},
				},
			},
		})
	})
	mux.HandleFunc("GET /apis/storage.ai.pits.io/v1alpha1/aistorageworkspaces", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"apiVersion": "storage.ai.pits.io/v1alpha1",
					"kind":       "AIStorageWorkspace",
					"metadata":   map[string]any{"namespace": "pits-cicd", "name": "pits-agent-space"},
					"spec": map[string]any{
						"environment":    "lab-rustfs",
						"domain":         "mcp.lab.martensite.dev",
						"compositionRef": map[string]string{"name": "aistorage-workspace-composition"},
					},
					"status": map[string]any{
						"mcpEndpoint":      "https://mcp.lab.martensite.dev/mcp",
						"profileConfigMap": "pits-profile",
						"readyArchetypes":  []string{"ws://", "mem://", "kb://"},
						"conditions": []map[string]string{
							{"type": "Synced", "status": "True"},
							{"type": "Ready", "status": "True", "reason": "Available"},
						},
					},
				},
			},
		})
	})
	mux.HandleFunc("GET /apis/iam.corp.io/v1alpha1/authflowbundles", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"apiVersion": "iam.corp.io/v1alpha1",
					"kind":       "AuthFlowBundle",
					"metadata":   map[string]any{"namespace": "iam-system", "name": "corp-fido2-browser"},
					"spec":       map[string]any{"realm": "corp", "flowAlias": "corp-fido2-browser"},
					"status": map[string]any{
						"activeFlowAlias": "corp-fido2-browser-32922097",
						"conditions": []map[string]string{
							{"type": "Synced", "status": "True"},
							{"type": "Ready", "status": "True"},
						},
					},
				},
			},
		})
	})
	mux.HandleFunc("GET /apis/lab.pits.io/v1alpha1/labdevstacks", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"apiVersion": "lab.pits.io/v1alpha1",
					"kind":       "LabDevStack",
					"metadata":   map[string]any{"namespace": "pits-cicd", "name": "pits-lab-acceleration"},
					"spec":       map[string]any{"externalIp": "10.89.20.21"},
					"status": map[string]any{
						"conditions": []map[string]string{
							{"type": "Synced", "status": "True"},
							{"type": "Ready", "status": "False", "reason": "Creating"},
						},
					},
				},
			},
		})
	})

	srv := httptest.NewServer(mux)
	defer srv.Close()

	mgr := NewManager("", filepath.Join(t.TempDir(), "clusters.json"), func(kcPath string) (*kube.Client, error) {
		return kube.NewFromKubeconfigBytes([]byte("apiVersion: v1\nkind: Config\nclusters:\n- name: lab\n  cluster:\n    server: " + srv.URL + "\ncontexts:\n- name: lab\n  context:\n    cluster: lab\n    user: admin\ncurrent-context: lab\nusers:\n- name: admin\n  user:\n    token: test-tok\n"))
	})
	_, err := mgr.Register(api.ClusterRegisterRequest{
		Name:           "cluster-pits-lab",
		KubeconfigYAML: "apiVersion: v1\nkind: Config\nclusters:\n- name: lab\n  cluster:\n    server: " + srv.URL + "\ncontexts:\n- name: lab\n  context:\n    cluster: lab\n    user: admin\ncurrent-context: lab\nusers:\n- name: admin\n  user:\n    token: test-tok\n",
	})
	if err != nil {
		t.Fatalf("Register failed: %v", err)
	}

	report := mgr.DiscoverAll(context.Background())
	if report.TotalCrossplaneResources == 0 {
		t.Fatalf("expected non-zero TotalCrossplaneResources, got %+v", report)
	}
	xp := report.Clusters[0].Crossplane
	if xp == nil || !xp.Installed || len(xp.XRDs) != 3 || len(xp.Packages) != 2 {
		t.Fatalf("unexpected Crossplane inventory: %+v", xp)
	}
	var sawAI, sawAuth, sawLab bool
	for _, r := range xp.Resources {
		switch r.Kind {
		case "AIStorageWorkspace":
			sawAI = true
			if !r.Ready || r.Highlights["mcpEndpoint"] != "https://mcp.lab.martensite.dev/mcp" {
				t.Fatalf("unexpected AIStorageWorkspace resource: %+v", r)
			}
		case "AuthFlowBundle":
			sawAuth = true
			if !r.Ready || r.Highlights["activeFlowAlias"] != "corp-fido2-browser-32922097" {
				t.Fatalf("unexpected AuthFlowBundle resource: %+v", r)
			}
		case "LabDevStack":
			sawLab = true
			if r.Ready || r.ReadyReason != "Creating" || r.Highlights["externalIp"] != "10.89.20.21" {
				t.Fatalf("unexpected LabDevStack resource: %+v", r)
			}
		}
	}
	if !sawAI || !sawAuth || !sawLab {
		t.Fatalf("missing expected Crossplane resources: ai=%v auth=%v lab=%v (%+v)", sawAI, sawAuth, sawLab, xp.Resources)
	}
}

func TestDRAResourceClaimsAndGPUOccupancy(t *testing.T) {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /version", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]string{"gitVersion": "v1.36.6"})
	})
	mux.HandleFunc("GET /api/v1/nodes", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"metadata": map[string]any{"name": "gpu-node-01"},
					"status": map[string]any{
						"nodeInfo":   map[string]string{"kubeletVersion": "v1.36.6"},
						"addresses":  []map[string]string{{"type": "InternalIP", "address": "10.89.20.41"}},
						"conditions": []map[string]string{{"type": "Ready", "status": "True"}},
						"allocatable": map[string]string{
							"nvidia.com/gpu": "4",
						},
					},
				},
			},
		})
	})
	mux.HandleFunc("GET /api/v1/pods", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"spec": map[string]any{
						"nodeName": "gpu-node-01",
						"containers": []map[string]any{
							{"resources": map[string]any{"limits": map[string]string{"nvidia.com/gpu": "1"}}},
						},
					},
					"status": map[string]string{"phase": "Running"},
				},
				{
					"spec": map[string]any{
						"nodeName": "gpu-node-01",
						"containers": []map[string]any{
							{"resources": map[string]any{"limits": map[string]string{"nvidia.com/gpu": "2"}}},
						},
					},
					"status": map[string]string{"phase": "Succeeded"},
				},
			},
		})
	})
	mux.HandleFunc("GET /api/v1/persistentvolumes", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{"items": []any{}})
	})
	mux.HandleFunc("GET /apis/resource.k8s.io/v1/deviceclasses", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{"metadata": map[string]string{"name": "gpu.nvidia.com"}},
				{"metadata": map[string]string{"name": "pits-intel-npu"}},
			},
		})
	})
	mux.HandleFunc("GET /apis/resource.k8s.io/v1/resourceslices", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"metadata": map[string]string{"name": "gpu-node-01-nvidia"},
					"spec": map[string]any{
						"nodeName": "gpu-node-01",
						"driver":   "gpu.nvidia.com",
						"devices": []map[string]any{
							{"name": "gpu-0", "attributes": map[string]any{"productName": map[string]string{"string": "NVIDIA H20"}}},
							{"name": "gpu-1", "attributes": map[string]any{"productName": map[string]string{"string": "NVIDIA H20"}}},
							{"name": "gpu-2", "attributes": map[string]any{"productName": map[string]string{"string": "NVIDIA H20"}}},
							{"name": "gpu-3", "attributes": map[string]any{"productName": map[string]string{"string": "NVIDIA H20"}}},
						},
					},
				},
			},
		})
	})
	mux.HandleFunc("GET /apis/resource.k8s.io/v1/resourceclaims", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"metadata": map[string]string{"namespace": "ai-train", "name": "vllm-claim-0"},
					"status": map[string]any{
						"allocation": map[string]any{
							"devices": map[string]any{
								"results": []map[string]string{
									{"driver": "gpu.nvidia.com", "pool": "gpu-node-01", "device": "gpu-0"},
									{"driver": "gpu.nvidia.com", "pool": "gpu-node-01", "device": "gpu-1"},
								},
							},
							"nodeSelector": map[string]any{
								"nodeSelectorTerms": []map[string]any{
									{
										"matchFields": []map[string]any{
											{"key": "metadata.name", "operator": "In", "values": []string{"gpu-node-01"}},
										},
									},
								},
							},
						},
						"reservedFor": []map[string]string{
							{"resource": "pods", "name": "vllm-worker-0"},
						},
					},
				},
				{
					// NPU claim should be captured in DRAClaims but not counted toward GPU occupancy
					"metadata": map[string]string{"namespace": "default", "name": "npu-claim-0"},
					"status": map[string]any{
						"allocation": map[string]any{
							"devices": map[string]any{
								"results": []map[string]string{
									{"driver": "npu.pits.home.arpa", "pool": "netboot-001", "device": "npu-0"},
								},
							},
							"nodeSelector": map[string]any{
								"nodeSelectorTerms": []map[string]any{
									{
										"matchExpressions": []map[string]any{
											{"key": "kubernetes.io/hostname", "operator": "In", "values": []string{"netboot-001"}},
										},
									},
								},
							},
						},
						"reservedFor": []map[string]string{
							{"resource": "pods", "name": "openvino-job-1"},
						},
					},
				},
				{
					// Unallocated claim must be ignored
					"metadata": map[string]string{"namespace": "ai-train", "name": "pending-claim"},
					"status":   map[string]any{},
				},
			},
		})
	})

	srv := httptest.NewServer(mux)
	defer srv.Close()

	kcYAML := "apiVersion: v1\nkind: Config\nclusters:\n- name: dra\n  cluster:\n    server: " + srv.URL + "\ncontexts:\n- name: dra\n  context:\n    cluster: dra\n    user: admin\ncurrent-context: dra\nusers:\n- name: admin\n  user:\n    token: test-tok\n"
	mgr := NewManager("", filepath.Join(t.TempDir(), "clusters.json"), func(_ string) (*kube.Client, error) {
		return kube.NewFromKubeconfigBytes([]byte(kcYAML))
	})
	if _, err := mgr.Register(api.ClusterRegisterRequest{
		Name:           "dra-gpu-cluster",
		KubeconfigYAML: kcYAML,
	}); err != nil {
		t.Fatalf("Register failed: %v", err)
	}

	report := mgr.DiscoverAll(context.Background())
	if report.TotalGPUs != 4 || report.AllocatedGPUs != 2 {
		t.Fatalf("expected 4 total / 2 allocated GPUs, got %+v", report)
	}
	cl := report.Clusters[0]
	if len(cl.DRAClaims) != 3 {
		t.Fatalf("expected 3 allocated DRA claim entries (2 GPU + 1 NPU), got %+v", cl.DRAClaims)
	}
	if cl.AllocatedGPUs != 2 || cl.GPUOccupancyPct != 50.0 {
		t.Fatalf("expected 2 allocated GPUs (50%%), got allocated=%d pct=%.2f", cl.AllocatedGPUs, cl.GPUOccupancyPct)
	}
	if cl.DRAClaims[0].NodeName != "gpu-node-01" || len(cl.DRAClaims[0].ConsumerPods) != 1 || cl.DRAClaims[0].ConsumerPods[0] != "ai-train/vllm-worker-0" {
		t.Fatalf("unexpected GPU DRA claim entry: %+v", cl.DRAClaims[0])
	}
	if cl.DRAClaims[2].NodeName != "netboot-001" || cl.DRAClaims[2].ConsumerPods[0] != "default/openvino-job-1" {
		t.Fatalf("unexpected NPU DRA claim entry: %+v", cl.DRAClaims[2])
	}
	// Succeeded pod must be excluded from DevicePlugin GPU usage
	if cl.DevicePluginGPUUsed["gpu-node-01"] != 1 {
		t.Fatalf("expected DevicePluginGPUUsed[gpu-node-01]=1 (excluding Succeeded), got %+v", cl.DevicePluginGPUUsed)
	}
}

func TestDevicePluginGPUFallbackOccupancyWithoutDRAClaims(t *testing.T) {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /version", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]string{"gitVersion": "v1.35.8"})
	})
	mux.HandleFunc("GET /api/v1/nodes", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"metadata": map[string]any{"name": "legacy-gpu-01"},
					"status": map[string]any{
						"conditions":  []map[string]string{{"type": "Ready", "status": "True"}},
						"allocatable": map[string]string{"nvidia.com/gpu": "4"},
					},
				},
			},
		})
	})
	mux.HandleFunc("GET /api/v1/pods", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"spec": map[string]any{
						"nodeName": "legacy-gpu-01",
						"containers": []map[string]any{
							{"resources": map[string]any{"limits": map[string]string{"nvidia.com/gpu": "2"}}},
						},
					},
					"status": map[string]string{"phase": "Running"},
				},
				{
					"spec": map[string]any{
						"nodeName": "legacy-gpu-01",
						"containers": []map[string]any{
							{"resources": map[string]any{"requests": map[string]string{"nvidia.com/gpu": "1"}}},
						},
					},
					"status": map[string]string{"phase": "Pending"},
				},
				{
					"spec": map[string]any{
						"nodeName": "legacy-gpu-01",
						"containers": []map[string]any{
							{"resources": map[string]any{"limits": map[string]string{"nvidia.com/gpu": "1"}}},
						},
					},
					"status": map[string]string{"phase": "Failed"},
				},
			},
		})
	})
	mux.HandleFunc("GET /api/v1/persistentvolumes", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{"items": []any{}})
	})
	mux.HandleFunc("GET /apis/resource.k8s.io/v1/deviceclasses", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{"items": []any{}})
	})
	mux.HandleFunc("GET /apis/resource.k8s.io/v1/resourceslices", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{"items": []any{}})
	})
	mux.HandleFunc("GET /apis/resource.k8s.io/v1/resourceclaims", func(w http.ResponseWriter, _ *http.Request) {
		http.NotFound(w, nil)
	})

	srv := httptest.NewServer(mux)
	defer srv.Close()

	kcYAML := "apiVersion: v1\nkind: Config\nclusters:\n- name: dp\n  cluster:\n    server: " + srv.URL + "\ncontexts:\n- name: dp\n  context:\n    cluster: dp\n    user: admin\ncurrent-context: dp\nusers:\n- name: admin\n  user:\n    token: test-tok\n"
	mgr := NewManager("", filepath.Join(t.TempDir(), "clusters.json"), func(_ string) (*kube.Client, error) {
		return kube.NewFromKubeconfigBytes([]byte(kcYAML))
	})
	if _, err := mgr.Register(api.ClusterRegisterRequest{
		Name:           "dp-gpu-cluster",
		KubeconfigYAML: kcYAML,
	}); err != nil {
		t.Fatalf("Register failed: %v", err)
	}

	report := mgr.DiscoverAll(context.Background())
	if report.TotalGPUs != 4 || report.AllocatedGPUs != 3 {
		t.Fatalf("expected 4 total / 3 allocated GPUs via DevicePlugin fallback, got %+v", report)
	}
	cl := report.Clusters[0]
	if cl.GPUOccupancyPct != 75.0 {
		t.Fatalf("expected 75%% GPU occupancy, got %.2f", cl.GPUOccupancyPct)
	}
}

func TestDRAClaimDeduplicationPoolFallbackAndMixedContainerResources(t *testing.T) {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /version", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]string{"gitVersion": "v1.37.1"})
	})
	mux.HandleFunc("GET /api/v1/nodes", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"metadata": map[string]any{"name": "gpu-node-02"},
					"status": map[string]any{
						"conditions":  []map[string]string{{"type": "Ready", "status": "True"}},
						"allocatable": map[string]string{"amd.com/gpu": "2"},
					},
				},
			},
		})
	})
	mux.HandleFunc("GET /api/v1/pods", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"spec": map[string]any{
						"nodeName": "gpu-node-02",
						"containers": []map[string]any{
							{
								"resources": map[string]any{
									"limits":   map[string]string{"memory": "8Gi"},
									"requests": map[string]string{"amd.com/gpu": "1"},
								},
							},
						},
					},
					"status": map[string]string{"phase": "Running"},
				},
			},
		})
	})
	mux.HandleFunc("GET /api/v1/persistentvolumes", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{"items": []any{}})
	})
	mux.HandleFunc("GET /apis/resource.k8s.io/v1/deviceclasses", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{"items": []any{}})
	})
	mux.HandleFunc("GET /apis/resource.k8s.io/v1/resourceslices", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"metadata": map[string]string{"name": "gpu-node-02-gpu"},
					"spec": map[string]any{
						"nodeName": "gpu-node-02",
						"driver":   "gpu.amd.com",
						"devices": []map[string]any{
							{"name": "gpu-0"},
							{"name": "gpu-1"},
						},
					},
				},
			},
		})
	})
	mux.HandleFunc("GET /apis/resource.k8s.io/v1/resourceclaims", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"metadata": map[string]string{"namespace": "ml", "name": "claim-a"},
					"status": map[string]any{
						"allocation": map[string]any{
							"devices": map[string]any{
								"results": []map[string]string{
									{"driver": "gpu.amd.com", "pool": "gpu-node-02", "device": "gpu-0"},
									{"driver": "gpu.amd.com", "pool": "gpu-node-02", "device": "gpu-1"},
								},
							},
						},
						"reservedFor": []map[string]string{
							{"resource": "pods", "name": "trainer-0"},
						},
					},
				},
				{
					// Duplicate device reference on a shared claim should deduplicate to 2 allocated GPUs (100%, not 150%)
					"metadata": map[string]string{"namespace": "ml", "name": "claim-b-shared"},
					"status": map[string]any{
						"allocation": map[string]any{
							"devices": map[string]any{
								"results": []map[string]string{
									{"driver": "gpu.amd.com", "pool": "gpu-node-02", "device": "gpu-0"},
								},
							},
						},
						"reservedFor": []map[string]string{
							{"resource": "pods", "name": "observer-0"},
						},
					},
				},
			},
		})
	})

	srv := httptest.NewServer(mux)
	defer srv.Close()

	kcYAML := "apiVersion: v1\nkind: Config\nclusters:\n- name: amd\n  cluster:\n    server: " + srv.URL + "\ncontexts:\n- name: amd\n  context:\n    cluster: amd\n    user: admin\ncurrent-context: amd\nusers:\n- name: admin\n  user:\n    token: test-tok\n"
	mgr := NewManager("", filepath.Join(t.TempDir(), "clusters.json"), func(_ string) (*kube.Client, error) {
		return kube.NewFromKubeconfigBytes([]byte(kcYAML))
	})
	if _, err := mgr.Register(api.ClusterRegisterRequest{
		Name:           "amd-dra-cluster",
		KubeconfigYAML: kcYAML,
	}); err != nil {
		t.Fatalf("Register failed: %v", err)
	}

	report := mgr.DiscoverAll(context.Background())
	cl := report.Clusters[0]
	if cl.DevicePluginGPUUsed["gpu-node-02"] != 1 {
		t.Fatalf("expected requests fallback DevicePluginGPUUsed[gpu-node-02]=1, got %+v", cl.DevicePluginGPUUsed)
	}
	if cl.AllocatedGPUs != 2 || cl.GPUOccupancyPct != 100.0 {
		t.Fatalf("expected deduplicated 2 allocated GPUs (100%%), got allocated=%d pct=%.2f", cl.AllocatedGPUs, cl.GPUOccupancyPct)
	}
	if cl.DRAClaims[0].NodeName != "gpu-node-02" {
		t.Fatalf("expected pool fallback NodeName=gpu-node-02, got %+v", cl.DRAClaims[0])
	}
}

func TestDRASharedClaimDeduplicationAndMixedLimitsRequestsGPUAccounting(t *testing.T) {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /version", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]string{"gitVersion": "v1.37.1"})
	})
	mux.HandleFunc("GET /api/v1/nodes", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"metadata": map[string]any{"name": "gpu-node-a"},
					"status": map[string]any{
						"conditions":  []map[string]string{{"type": "Ready", "status": "True"}},
						"allocatable": map[string]string{"nvidia.com/gpu": "1", "amd.com/gpu": "1"},
					},
				},
			},
		})
	})
	mux.HandleFunc("GET /api/v1/pods", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					// Container sets CPU/memory in limits, GPU only in requests -> must still be counted
					"spec": map[string]any{
						"nodeName": "gpu-node-a",
						"containers": []map[string]any{
							{
								"resources": map[string]any{
									"limits":   map[string]string{"cpu": "4", "memory": "8Gi"},
									"requests": map[string]string{"amd.com/gpu": "1"},
								},
							},
						},
					},
					"status": map[string]string{"phase": "Running"},
				},
				{
					// Unscheduled pod without nodeName must be ignored
					"spec": map[string]any{
						"nodeName": "",
						"containers": []map[string]any{
							{"resources": map[string]any{"limits": map[string]string{"nvidia.com/gpu": "1"}}},
						},
					},
					"status": map[string]string{"phase": "Pending"},
				},
			},
		})
	})
	mux.HandleFunc("GET /api/v1/persistentvolumes", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{"items": []any{}})
	})
	mux.HandleFunc("GET /apis/resource.k8s.io/v1/deviceclasses", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{"items": []any{}})
	})
	mux.HandleFunc("GET /apis/resource.k8s.io/v1/resourceslices", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"metadata": map[string]string{"name": "gpu-node-a-nvidia"},
					"spec": map[string]any{
						"nodeName": "gpu-node-a",
						"driver":   "gpu.nvidia.com",
						"devices": []map[string]any{
							{"name": "gpu-0", "attributes": map[string]any{"productName": map[string]string{"string": "NVIDIA RTX 5090"}}},
						},
					},
				},
			},
		})
	})
	mux.HandleFunc("GET /apis/resource.k8s.io/v1/resourceclaims", func(w http.ResponseWriter, _ *http.Request) {
		// Two claims referencing the exact same physical GPU (shared) -> deduplicated to 1 allocated GPU (100%)
		_ = json.NewEncoder(w).Encode(map[string]any{
			"items": []map[string]any{
				{
					"metadata": map[string]string{"namespace": "ai", "name": "claim-a"},
					"status": map[string]any{
						"allocation": map[string]any{
							"devices": map[string]any{
								"results": []map[string]string{
									{"driver": "gpu.nvidia.com", "pool": "gpu-node-a", "device": "gpu-0"},
								},
							},
						},
						"reservedFor": []map[string]string{{"resource": "pods", "name": "pod-a"}},
					},
				},
				{
					"metadata": map[string]string{"namespace": "ai", "name": "claim-b"},
					"status": map[string]any{
						"allocation": map[string]any{
							"devices": map[string]any{
								"results": []map[string]string{
									{"driver": "gpu.nvidia.com", "pool": "gpu-node-a", "device": "gpu-0"},
								},
							},
						},
						"reservedFor": []map[string]string{{"resource": "pods", "name": "pod-b"}},
					},
				},
			},
		})
	})

	srv := httptest.NewServer(mux)
	defer srv.Close()

	kcYAML := "apiVersion: v1\nkind: Config\nclusters:\n- name: dedup\n  cluster:\n    server: " + srv.URL + "\ncontexts:\n- name: dedup\n  context:\n    cluster: dedup\n    user: admin\ncurrent-context: dedup\nusers:\n- name: admin\n  user:\n    token: test-tok\n"
	mgr := NewManager("", filepath.Join(t.TempDir(), "clusters.json"), func(_ string) (*kube.Client, error) {
		return kube.NewFromKubeconfigBytes([]byte(kcYAML))
	})
	if _, err := mgr.Register(api.ClusterRegisterRequest{
		Name:           "dedup-cluster",
		KubeconfigYAML: kcYAML,
	}); err != nil {
		t.Fatalf("Register failed: %v", err)
	}

	report := mgr.DiscoverAll(context.Background())
	cl := report.Clusters[0]
	if cl.GPUCount != 1 || cl.AllocatedGPUs != 1 || cl.GPUOccupancyPct != 100.0 {
		t.Fatalf("expected deduplicated 1/1 GPU (100%%), got count=%d alloc=%d pct=%.2f", cl.GPUCount, cl.AllocatedGPUs, cl.GPUOccupancyPct)
	}
	// Pool fallback should resolve NodeName when nodeSelector is omitted
	if len(cl.DRAClaims) != 2 || cl.DRAClaims[0].NodeName != "gpu-node-a" {
		t.Fatalf("expected pool fallback nodeName=gpu-node-a, got %+v", cl.DRAClaims)
	}
	// Mixed limits(cpu/mem)+requests(amd.com/gpu) must still be recorded in DevicePluginGPUUsed
	if cl.DevicePluginGPUUsed["gpu-node-a"] != 1 {
		t.Fatalf("expected DevicePluginGPUUsed[gpu-node-a]=1, got %+v", cl.DevicePluginGPUUsed)
	}
}
