package bridge

import (
	"encoding/json"
	"fmt"
	"net/netip"
	"strconv"
	"strings"
)

// SemanticRoutingTable mirrors the ConfigMap `edge/pits-core-routing-table`
// (`routingTable.json`) and `EdgeRouter.status.routingTable` published by Crossplane `EdgeRouter`.
type SemanticRoutingTable struct {
	SchemaVersion      string               `json:"schemaVersion"`
	Router             string               `json:"router"`
	ConfigMapName      string               `json:"configMapName"`
	Generation         int64                `json:"generation"`
	RouterGeneration   int64                `json:"routerGeneration"`
	SpecHash           string               `json:"specHash"`
	Hash               string               `json:"hash"`
	Verified           bool                 `json:"verified"`
	VerificationReason string               `json:"verificationReason"`
	RuntimeReady       bool                 `json:"runtimeReady"`
	ThunderboltReady   bool                 `json:"thunderboltReady"`
	Domains            []SemanticDomainInfo `json:"domains"`
	Entries            []SemanticRouteEntry `json:"entries"`
}

// SemanticDomainInfo summarizes one logical network domain published by EdgeRouter.
type SemanticDomainInfo struct {
	Domain            string   `json:"domain"`
	CIDRs             []string `json:"cidrs"`
	VLAN              int      `json:"vlan,omitempty"`
	Reachability      string   `json:"reachability"`
	Scope             string   `json:"scope"`
	FullSubnetRouted  bool     `json:"fullSubnetRouted"`
	RuntimeState      string   `json:"runtimeState"`
}

// SemanticRouteEntry represents one semantic routing & policy rule from EdgeRouter.
type SemanticRouteEntry struct {
	ID                string            `json:"id"`
	Domain            string            `json:"domain"`
	CIDR              string            `json:"cidr"`
	CIDRs             []string          `json:"cidrs"`
	VLAN              int               `json:"vlan,omitempty"`
	Gateway           string            `json:"gateway,omitempty"`
	GatewayAddress    string            `json:"gatewayAddress,omitempty"`
	GatewayInterface  string            `json:"gatewayInterface,omitempty"`
	NextHop           string            `json:"nextHop,omitempty"`
	Reachability      string            `json:"reachability"`
	Scope             string            `json:"scope"`
	NAD               string            `json:"nad,omitempty"`
	PodSelector       map[string]string `json:"podSelector,omitempty"`
	AllowedPeers      []string          `json:"allowedPeers,omitempty"`
	AllowedPorts      []int             `json:"allowedPorts,omitempty"`
	ControlPlanePorts []int             `json:"controlPlanePorts,omitempty"`
	FullSubnetRouted  bool              `json:"fullSubnetRouted"`
	RestrictedPurpose string            `json:"restrictedPurpose,omitempty"`
	ExcludedTraffic   []string          `json:"excludedTraffic,omitempty"`
	Description       string            `json:"description,omitempty"`
	RuntimeState      string            `json:"runtimeState,omitempty"`
	ExecutionContext  ExecutionContext  `json:"executionContext"`
}

// ExecutionContext captures where and how a route entry's traffic executes on pits-core.
type ExecutionContext struct {
	Mode             string            `json:"mode"`
	NetNS            string            `json:"netns"`
	NAD              string            `json:"nad,omitempty"`
	PodSelector      map[string]string `json:"podSelector,omitempty"`
	GatewayIP        string            `json:"gatewayIp,omitempty"`
	GatewayInterface string            `json:"gatewayInterface,omitempty"`
	GatewayAddress   string            `json:"gatewayAddress,omitempty"`
	VPPInterface     string            `json:"vppInterface,omitempty"`
}

// RoutePlan is the resolved forwarding instruction derived from SemanticRoutingTable.
type RoutePlan struct {
	Entry            *SemanticRouteEntry
	CanonicalTarget  string
	RequireMultusNS  bool
	ExcludedHostIPs  map[netip.Addr]bool
}

func (e SemanticRouteEntry) prefixes() []netip.Prefix {
	raw := make([]string, 0, 1+len(e.CIDRs))
	if e.CIDR != "" {
		raw = append(raw, e.CIDR)
	}
	raw = append(raw, e.CIDRs...)
	out := make([]netip.Prefix, 0, len(raw))
	seen := make(map[netip.Prefix]bool, len(raw))
	for _, s := range raw {
		p, err := netip.ParsePrefix(strings.TrimSpace(s))
		if err == nil && !seen[p] {
			seen[p] = true
			out = append(out, p)
		}
	}
	return out
}

func (e SemanticRouteEntry) allowsPort(port int) bool {
	ports := e.AllowedPorts
	if len(ports) == 0 {
		ports = e.ControlPlanePorts
	}
	if len(ports) == 0 {
		return allowedPorts[port]
	}
	for _, p := range ports {
		if p == port {
			return true
		}
	}
	return false
}

// PolicyHostOnlyIPs collects host-namespace interface IPs attached to restricted
// policy gateways (e.g. `tb-lab-gw` `10.89.20.9/32` with `fullSubnetRouted=false`)
// so host fib_trie inspection never mistakes them for full-subnet Lab VLAN 20 connectivity.
func (t *SemanticRoutingTable) PolicyHostOnlyIPs() map[netip.Addr]bool {
	out := make(map[netip.Addr]bool)
	if t == nil {
		return out
	}
	for _, entry := range t.Entries {
		isPolicyOnly := !entry.FullSubnetRouted ||
			strings.EqualFold(entry.Scope, "host-policy-tproxy") ||
			strings.EqualFold(entry.ExecutionContext.Mode, "host-policy-tproxy")
		if !isPolicyOnly {
			continue
		}
		candidates := []string{
			entry.Gateway,
			entry.GatewayAddress,
			entry.NextHop,
			entry.ExecutionContext.GatewayIP,
			entry.ExecutionContext.GatewayAddress,
		}
		for _, c := range candidates {
			c = strings.TrimSpace(c)
			if c == "" {
				continue
			}
			if p, err := netip.ParsePrefix(c); err == nil {
				out[p.Addr()] = true
				continue
			}
			if addr, err := netip.ParseAddr(c); err == nil {
				out[addr] = true
			}
		}
	}
	return out
}

// MatchControlPlaneTarget resolves a destination (ip, port) using EdgeRouter's semantic entries.
// Currently the mTLS bridge serves Lab VLAN 20 control-plane & storage endpoints (`domain=lab`).
func (t *SemanticRoutingTable) MatchControlPlaneTarget(ip netip.Addr, port int) (SemanticRouteEntry, error) {
	if t == nil || len(t.Entries) == 0 {
		return SemanticRouteEntry{}, fmt.Errorf("semantic routing table is empty")
	}

	var best *SemanticRouteEntry
	bestBits := -1

	for i := range t.Entries {
		e := &t.Entries[i]
		for _, pfx := range e.prefixes() {
			// Skip 0.0.0.0/0 WAN default route so non-lab destinations fail closed.
			if pfx.Bits() == 0 {
				continue
			}
			if pfx.Contains(ip) && pfx.Bits() > bestBits {
				best = e
				bestBits = pfx.Bits()
			}
		}
	}

	if best == nil {
		return SemanticRouteEntry{}, fmt.Errorf("target IP %s is not covered by any EdgeRouter internal domain", ip)
	}
	if best.RuntimeState != "" && !strings.EqualFold(best.RuntimeState, "up") {
		return SemanticRouteEntry{}, fmt.Errorf("EdgeRouter domain %q (%s) runtimeState is %q", best.Domain, best.ID, best.RuntimeState)
	}
	if !best.FullSubnetRouted || strings.EqualFold(best.Scope, "host-policy-tproxy") {
		return SemanticRouteEntry{}, fmt.Errorf("EdgeRouter route %q (%s) is policy-restricted (%s) and does not permit general control-plane bridging", best.ID, best.CIDR, best.Scope)
	}
	if best.Domain != "lab" && !strings.EqualFold(best.Reachability, "multus-vlan20") {
		return SemanticRouteEntry{}, fmt.Errorf("EdgeRouter route %q belongs to isolated domain %q (outside bridgeable lab domain)", best.ID, best.Domain)
	}
	if !best.allowsPort(port) {
		return SemanticRouteEntry{}, fmt.Errorf("target port %d is not permitted on EdgeRouter route %q (%s)", port, best.ID, best.CIDR)
	}
	for _, ex := range best.ExcludedTraffic {
		if strings.EqualFold(ex, ip.String()+":"+strconv.Itoa(port)) {
			return SemanticRouteEntry{}, fmt.Errorf("target %s:%d is explicitly excluded by EdgeRouter route %q", ip, port, best.ID)
		}
	}

	return *best, nil
}

// ParseConfigMapJSON extracts a SemanticRoutingTable from a Kubernetes ConfigMap JSON payload.
func ParseConfigMapJSON(raw []byte) (*SemanticRoutingTable, string, string, error) {
	var cm struct {
		Metadata struct {
			ResourceVersion string `json:"resourceVersion"`
		} `json:"metadata"`
		Data map[string]string `json:"data"`
	}
	if err := json.Unmarshal(raw, &cm); err != nil {
		return nil, "", "", fmt.Errorf("decode routing-table ConfigMap: %w", err)
	}
	rtJSON := strings.TrimSpace(cm.Data["routingTable.json"])
	if rtJSON == "" {
		return nil, cm.Metadata.ResourceVersion, cm.Data["hash"], fmt.Errorf("ConfigMap missing data['routingTable.json']")
	}
	var table SemanticRoutingTable
	if err := json.Unmarshal([]byte(rtJSON), &table); err != nil {
		return nil, cm.Metadata.ResourceVersion, cm.Data["hash"], fmt.Errorf("decode data['routingTable.json']: %w", err)
	}
	hash := table.Hash
	if hash == "" {
		hash = cm.Data["hash"]
	}
	return &table, cm.Metadata.ResourceVersion, hash, nil
}

// ParseEdgeRouterJSON extracts a SemanticRoutingTable from `EdgeRouter.status.routingTable` as fallback.
func ParseEdgeRouterJSON(raw []byte) (*SemanticRoutingTable, string, error) {
	var er struct {
		Metadata struct {
			ResourceVersion string `json:"resourceVersion"`
		} `json:"metadata"`
		Status struct {
			RoutingTable *SemanticRoutingTable `json:"routingTable"`
		} `json:"status"`
	}
	if err := json.Unmarshal(raw, &er); err != nil {
		return nil, "", fmt.Errorf("decode EdgeRouter resource: %w", err)
	}
	if er.Status.RoutingTable == nil {
		return nil, er.Metadata.ResourceVersion, fmt.Errorf("EdgeRouter status.routingTable missing")
	}
	return er.Status.RoutingTable, er.Metadata.ResourceVersion, nil
}
