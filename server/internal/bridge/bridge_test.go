package bridge

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"pits.io/server/internal/kube"
)

const sampleRoutingTableJSON = `{
  "schemaVersion": "platform.edge.router/v1alpha1",
  "router": "edge/pits-core",
  "configMapName": "pits-core-routing-table",
  "generation": 25,
  "routerGeneration": 18,
  "specHash": "2e36950272afa7bf30c2b9cda675b460ff5a1b38f1464cbc9f802881932b60f1",
  "hash": "835fb7bfba9575d44ff159cf7d8f4aa46f0a2a5eb38a1d2f5c754fd2e5d0e73c",
  "verified": true,
  "verificationReason": "Verified",
  "runtimeReady": true,
  "thunderboltReady": true,
  "entries": [
    {
      "id": "core",
      "domain": "core",
      "cidr": "10.88.0.0/16",
      "cidrs": ["10.88.0.0/16", "10.18.18.0/24"],
      "vlan": 10,
      "gateway": "10.88.0.1",
      "reachability": "vpp-lcp-vlan10",
      "scope": "core-lan",
      "fullSubnetRouted": true,
      "runtimeState": "up",
      "executionContext": {"mode": "vpp-vlan-l3", "netns": "vpp"}
    },
    {
      "id": "lab-control-plane",
      "domain": "lab",
      "cidr": "10.89.20.0/24",
      "cidrs": ["10.89.20.0/24"],
      "vlan": 20,
      "gateway": "10.89.20.1",
      "gatewayAddress": "10.89.20.1/24",
      "reachability": "multus-vlan20",
      "scope": "control-plane-vlan",
      "nad": "pits-net/lab-vlan20",
      "allowedPorts": [4420, 6443, 7171, 7173, 7180, 30389, 42113, 50000],
      "fullSubnetRouted": true,
      "runtimeState": "up",
      "executionContext": {"mode": "multus-netns", "netns": "multus-vlan20", "nad": "pits-net/lab-vlan20"}
    },
    {
      "id": "thunderbolt-storage-gw",
      "domain": "thunderbolt",
      "cidr": "10.90.0.0/16",
      "cidrs": ["10.90.0.0/16"],
      "vlan": 20,
      "gateway": "10.89.20.9",
      "gatewayAddress": "10.89.20.9/32",
      "gatewayInterface": "tb-lab-gw",
      "reachability": "host-policy-tproxy",
      "scope": "host-policy-tproxy",
      "allowedPorts": [4420, 8420],
      "fullSubnetRouted": false,
      "runtimeState": "up",
      "executionContext": {"mode": "host-policy-tproxy", "netns": "host", "gatewayInterface": "tb-lab-gw", "gatewayAddress": "10.89.20.9/32"}
    },
    {
      "id": "wan-uplink",
      "domain": "wan",
      "cidr": "0.0.0.0/0",
      "cidrs": ["0.0.0.0/0"],
      "reachability": "vpp-wan-uplink",
      "scope": "wan-egress",
      "fullSubnetRouted": true,
      "runtimeState": "up",
      "executionContext": {"mode": "vpp-wan-uplink", "netns": "vpp"}
    }
  ]
}`

func TestValidateTargetAllowlist(t *testing.T) {
	allowed := []string{
		"10.89.20.21:6443",
		"10.89.20.24:50000",
		"10.89.20.10:7173",
		"10.89.20.24:30389",
	}
	for _, target := range allowed {
		if _, err := ValidateTarget(target); err != nil {
			t.Fatalf("expected %s to be allowed, got: %v", target, err)
		}
	}
	rejected := []string{
		"127.0.0.1:6443",
		"192.168.71.35:22",
		"10.88.1.1:80",
		"10.89.20.21:22",
		"example.com:6443",
	}
	for _, target := range rejected {
		if _, err := ValidateTarget(target); err != nil {
			continue
		}
		t.Fatalf("expected %s to be rejected by Lab Bridge allowlist", target)
	}
}

func TestHasLocalLabIPv4IgnoresHostPolicyGateway32(t *testing.T) {
	tinkPodTrie := `Main:
  +-- 0.0.0.0/0 3 0 5
     +-- 10.42.128.0/24 2 0 2
        |-- 10.42.128.44
           /32 host LOCAL
     +-- 10.89.20.0/24 2 0 2
        |-- 10.89.20.0
           /32 link BROADCAST
        |-- 10.89.20.12
           /32 host LOCAL
        |-- 10.89.20.255
           /32 link BROADCAST
`
	if !HasLocalLabIPv4(tinkPodTrie) {
		t.Fatal("expected Multus VLAN 20 pod 10.89.20.12/24 fib_trie to match")
	}

	// Host with tb-lab-gw (10.89.20.9/32 scope global) has /32 host LOCAL but no connected /24 BROADCAST.
	tbHostTrie := `Main:
  +-- 0.0.0.0/0 3 0 5
     +-- 10.89.20.9
        |-- 10.89.20.9
           /32 host LOCAL
     +-- 10.89.20.25
        /32 link UNICAST
`
	if HasLocalLabIPv4(tbHostTrie) {
		t.Fatal("expected host with isolated tb-lab-gw 10.89.20.9/32 NOT to match connected Lab VLAN 20")
	}

	excluded := map[netip.Addr]bool{netip.MustParseAddr("10.89.20.9"): true}
	if HasLocalLabIPv4WithOptions(tbHostTrie, excluded, false) {
		t.Fatal("expected excluded policy gateway IP 10.89.20.9 to be ignored even with requireConnectedSubnet=false")
	}
}

func TestSemanticRoutingResolutionFromEdgeRouter(t *testing.T) {
	var table SemanticRoutingTable
	if err := json.Unmarshal([]byte(sampleRoutingTableJSON), &table); err != nil {
		t.Fatalf("unmarshal sample routing table: %v", err)
	}

	tun := NewTunnel("auto")
	tun.SetRoutingTable("unit-test", &table)

	plan, err := tun.ResolveTarget("10.89.20.21:6443")
	if err != nil {
		t.Fatalf("expected 10.89.20.21:6443 to resolve via lab-control-plane, got: %v", err)
	}
	if !plan.RequireMultusNS {
		t.Fatal("expected RequireMultusNS=true for multus-vlan20 execution context")
	}
	if !plan.ExcludedHostIPs[netip.MustParseAddr("10.89.20.9")] {
		t.Fatal("expected tb-lab-gw 10.89.20.9 to be excluded as policy host-only IP")
	}

	for _, denied := range []string{"10.90.0.21:4420", "10.88.0.1:443", "8.8.8.8:443", "10.89.20.21:22"} {
		if _, derr := tun.ResolveTarget(denied); derr == nil {
			t.Fatalf("expected semantic routing to deny %s", denied)
		}
	}
}

func TestK8sWatchSubscriberUpdatesAndHandlesBookmarkAndGone(t *testing.T) {
	var watchCalls atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case r.URL.Path == "/api/v1/namespaces/edge/configmaps/pits-core-routing-table":
			cm := map[string]any{
				"metadata": map[string]any{"resourceVersion": "100"},
				"data": map[string]string{
					"hash":              "835fb7bfba9575d44ff159cf7d8f4aa46f0a2a5eb38a1d2f5c754fd2e5d0e73c",
					"routingTable.json": sampleRoutingTableJSON,
				},
			}
			_ = json.NewEncoder(w).Encode(cm)
		case r.URL.Path == "/api/v1/namespaces/edge/configmaps" && r.URL.Query().Get("watch") == "true":
			call := watchCalls.Add(1)
			w.Header().Set("Content-Type", "application/json")
			fl, _ := w.(http.Flusher)
			if call == 1 {
				updatedJSON := strings.Replace(sampleRoutingTableJSON, `"generation": 25`, `"generation": 26`, 1)
				updatedJSON = strings.Replace(updatedJSON, `"835fb7bfba9575d44ff159cf7d8f4aa46f0a2a5eb38a1d2f5c754fd2e5d0e73c"`, `"hash-gen-26"`, 1)
				modObj, _ := json.Marshal(map[string]any{
					"type": "MODIFIED",
					"object": map[string]any{
						"metadata": map[string]any{"resourceVersion": "101"},
						"data": map[string]string{
							"hash":              "hash-gen-26",
							"routingTable.json": updatedJSON,
						},
					},
				})
				_, _ = fmt.Fprintf(w, "%s\n", modObj)
				_, _ = fmt.Fprintln(w, `{"type":"BOOKMARK","object":{"metadata":{"resourceVersion":"102"}}}`)
				if fl != nil {
					fl.Flush()
				}
				return
			}
			// Keep subsequent watch open briefly until context closes cleanly.
			<-r.Context().Done()
		default:
			http.NotFound(w, r)
		}
	}))
	defer srv.Close()

	kc := kube.NewFromTransport(srv.URL, "test-token", srv.Client().Transport)
	tun := NewTunnel("auto")

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	tun.StartSubscriber(ctx, kc)

	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		st := tun.Status()
		if st.Subscribed && st.Generation == 26 && st.Hash == "hash-gen-26" {
			return
		}
		time.Sleep(25 * time.Millisecond)
	}
	t.Fatalf("subscriber did not reach generation 26 via K8s Watch; last status=%+v", tun.Status())
}
