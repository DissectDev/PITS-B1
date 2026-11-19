package bridge

import (
	"net/netip"
	"strings"
)

// HasLocalLabIPv4 parses Linux /proc/<pid>/net/fib_trie content and reports whether
// that network namespace owns a local IPv4 interface address inside 10.89.20.0/24
// (excluding the gateway address 10.89.20.1 and host-only /32 policy gateways).
func HasLocalLabIPv4(fibTrie string) bool {
	return HasLocalLabIPv4WithOptions(fibTrie, nil, true)
}

// HasLocalLabIPv4WithOptions checks whether fibTrie owns a non-excluded local IPv4 address
// in 10.89.20.0/24. When requireConnectedSubnet is true, isolated /32 policy-gateway IPs
// without a connected broadcast entry (e.g. tb-lab-gw 10.89.20.9/32) are ignored.
func HasLocalLabIPv4WithOptions(fibTrie string, excluded map[netip.Addr]bool, requireConnectedSubnet bool) bool {
	lines := strings.Split(fibTrie, "\n")
	var lastIP netip.Addr
	hasLocalHostIP := false
	hasBroadcastOrSubnet := false

	for _, raw := range lines {
		line := strings.TrimSpace(raw)
		if strings.HasPrefix(line, "|-- ") {
			addrStr := strings.TrimSpace(strings.TrimPrefix(line, "|-- "))
			ip, err := netip.ParseAddr(addrStr)
			if err == nil && labSubnet.Contains(ip) &&
				ip.String() != "10.89.20.1" &&
				!excluded[ip] {
				lastIP = ip
			} else {
				lastIP = netip.Addr{}
			}
			continue
		}
		if !lastIP.IsValid() {
			continue
		}
		if strings.Contains(line, "/32 link BROADCAST") {
			hasBroadcastOrSubnet = true
			continue
		}
		if strings.Contains(line, "/32 host LOCAL") &&
			lastIP.String() != "10.89.20.0" &&
			lastIP.String() != "10.89.20.255" {
			hasLocalHostIP = true
		}
	}

	if !hasLocalHostIP {
		return false
	}
	if requireConnectedSubnet {
		return hasBroadcastOrSubnet
	}
	return true
}
