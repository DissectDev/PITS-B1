// Package bridge implements an allowlisted Core <-> Lab VLAN 20 HTTP CONNECT
// tunnel on pits-core, replacing scripts/tinkerbell/labctl.sh (crictl + nsenter)
// with end-to-end mTLS passthrough.
package bridge

import (
	"context"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/netip"
	"strconv"
	"strings"
	"sync"
	"time"
)

var (
	labSubnet    = netip.MustParsePrefix("10.89.20.0/24")
	allowedPorts = map[int]bool{
		6443:  true, // Talos Kubernetes API
		50000: true, // Talos machined/apid gRPC
		7171:  true, // Tinkerbell iPXE HTTP
		7173:  true, // Tinkerbell static boot assets HTTP
		7180:  true, // Hook hardware inventory collector
		42113: true, // Tinkerbell gRPC
		4420:  true, // NVMe/TCP target probe
		30389: true, // Windows 11 VM RDP NodePort
	}
)

// Tunnel handles HTTP CONNECT requests strictly restricted to Lab VLAN 20 control endpoints
// and dynamically guided by EdgeRouter's semantic routing table subscription, plus optional
// explicitly registered external cluster endpoints.
type Tunnel struct {
	LabNetnsPath       string
	DialTimeout        time.Duration
	ExtraTargetChecker func(hostPort string) bool

	mu       sync.RWMutex
	table    *SemanticRoutingTable
	excluded map[netip.Addr]bool
	subState RoutingSubscriptionStatus
}

func NewTunnel(labNetnsPath string) *Tunnel {
	return &Tunnel{
		LabNetnsPath: labNetnsPath,
		DialTimeout:  10 * time.Second,
		subState: RoutingSubscriptionStatus{
			Source: "static-allowlist-fallback",
		},
	}
}

func (t *Tunnel) applyRoutingSnapshot(source, rv, hash string, table *SemanticRoutingTable) {
	if table == nil {
		return
	}
	now := time.Now().UTC()
	excluded := table.PolicyHostOnlyIPs()

	t.mu.Lock()
	defer t.mu.Unlock()
	t.table = table
	t.excluded = excluded
	t.subState.Subscribed = true
	t.subState.Source = source
	if rv != "" {
		t.subState.ResourceVersion = rv
	}
	if hash != "" {
		t.subState.Hash = hash
	}
	t.subState.Generation = table.Generation
	t.subState.SyncCount++
	t.subState.LastSyncAt = &now
	t.subState.LastError = ""
	t.subState.RoutingTable = table
}

func (t *Tunnel) recordSyncError(msg string) {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.subState.LastError = msg
}

// SetRoutingTable injects a semantic routing snapshot (for tests or out-of-band updates).
func (t *Tunnel) SetRoutingTable(source string, table *SemanticRoutingTable) {
	if table == nil {
		return
	}
	t.applyRoutingSnapshot(source, "", table.Hash, table)
}

// Status returns a snapshot of the EdgeRouter routing-table subscription state.
func (t *Tunnel) Status() RoutingSubscriptionStatus {
	t.mu.RLock()
	defer t.mu.RUnlock()
	return t.subState
}

// RoutingStatusJSON implements api.RoutingStatusProvider.
func (t *Tunnel) RoutingStatusJSON() any {
	return t.Status()
}

// ValidateTarget enforces strict CIDR + port allowlisting so the bridge cannot be abused as an open proxy.
func ValidateTarget(hostPort string) (string, error) {
	host, portStr, err := net.SplitHostPort(hostPort)
	if err != nil {
		return "", fmt.Errorf("invalid CONNECT target %q", hostPort)
	}
	ip, err := netip.ParseAddr(host)
	if err != nil || !labSubnet.Contains(ip) {
		return "", fmt.Errorf("target IP %q is outside allowlisted Lab subnet %s", host, labSubnet.String())
	}
	port, err := strconv.Atoi(portStr)
	if err != nil || !allowedPorts[port] {
		return "", fmt.Errorf("target port %q is not an allowlisted Lab control-plane port", portStr)
	}
	return net.JoinHostPort(ip.String(), strconv.Itoa(port)), nil
}

// ResolveTarget validates hostPort against the subscribed EdgeRouter SemanticRoutingTable
// (falling back to static Lab VLAN 20 allowlisting before the first K8s Watch sync).
func (t *Tunnel) ResolveTarget(hostPort string) (RoutePlan, error) {
	host, portStr, err := net.SplitHostPort(hostPort)
	if err != nil {
		return RoutePlan{}, fmt.Errorf("invalid CONNECT target %q", hostPort)
	}
	ip, err := netip.ParseAddr(host)
	if err != nil {
		return RoutePlan{}, fmt.Errorf("invalid CONNECT target IP %q", host)
	}
	port, err := strconv.Atoi(portStr)
	if err != nil || port <= 0 || port > 65535 {
		return RoutePlan{}, fmt.Errorf("invalid CONNECT target port %q", portStr)
	}

	t.mu.RLock()
	table := t.table
	excluded := t.excluded
	t.mu.RUnlock()

	if table != nil && len(table.Entries) > 0 {
		entry, merr := table.MatchControlPlaneTarget(ip, port)
		if merr != nil {
			return RoutePlan{}, merr
		}
		requireMultus := strings.EqualFold(entry.ExecutionContext.Mode, "multus-netns") ||
			strings.EqualFold(entry.Reachability, "multus-vlan20")
		return RoutePlan{
			Entry:           &entry,
			CanonicalTarget: net.JoinHostPort(ip.String(), strconv.Itoa(port)),
			RequireMultusNS: requireMultus,
			ExcludedHostIPs: excluded,
		}, nil
	}

	canonical, verr := ValidateTarget(hostPort)
	if verr != nil {
		if t.ExtraTargetChecker != nil && t.ExtraTargetChecker(hostPort) {
			return RoutePlan{
				CanonicalTarget: net.JoinHostPort(ip.String(), strconv.Itoa(port)),
			}, nil
		}
		return RoutePlan{}, verr
	}
	return RoutePlan{
		CanonicalTarget: canonical,
	}, nil
}

// ServeConnect upgrades an HTTP CONNECT request into a bidirectional TCP stream.
func (t *Tunnel) ServeConnect(w http.ResponseWriter, r *http.Request) {
	plan, err := t.ResolveTarget(r.Host)
	if err != nil {
		http.Error(w, err.Error(), http.StatusForbidden)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), t.DialTimeout)
	defer cancel()

	upstream, err := dialInNetns(ctx, t.LabNetnsPath, plan.CanonicalTarget, plan)
	if err != nil {
		http.Error(w, fmt.Sprintf("dial lab endpoint %s: %v", plan.CanonicalTarget, err), http.StatusBadGateway)
		return
	}

	hj, ok := w.(http.Hijacker)
	if !ok {
		upstream.Close()
		http.Error(w, "HTTP connection does not support hijacking", http.StatusInternalServerError)
		return
	}
	clientConn, clientBuf, err := hj.Hijack()
	if err != nil {
		upstream.Close()
		return
	}
	if _, err := io.WriteString(clientConn, "HTTP/1.1 200 Connection Established\r\n\r\n"); err != nil {
		clientConn.Close()
		upstream.Close()
		return
	}
	// Flush any buffered bytes sent immediately after CONNECT.
	if clientBuf != nil && clientBuf.Reader.Buffered() > 0 {
		buffered := make([]byte, clientBuf.Reader.Buffered())
		n, _ := clientBuf.Read(buffered)
		if n > 0 {
			_, _ = upstream.Write(buffered[:n])
		}
	}

	var wg sync.WaitGroup
	wg.Add(2)
	go func() {
		defer wg.Done()
		_, _ = io.Copy(upstream, clientConn)
		if tc, ok := upstream.(*net.TCPConn); ok {
			_ = tc.CloseWrite()
		}
	}()
	go func() {
		defer wg.Done()
		_, _ = io.Copy(clientConn, upstream)
		if tc, ok := clientConn.(*net.TCPConn); ok {
			_ = tc.CloseWrite()
		}
	}()
	wg.Wait()
	clientConn.Close()
	upstream.Close()
}
