package bridge

import (
	"net/netip"
	"strings"
)

// HasLocalLabIPv4 parses Linux /proc/<pid>/net/fib_trie content and reports whether
// that network namespace owns a local IPv4 interface address inside 10.89.20.0/24
// (excluding the gateway address 10.89.20.1 owned by VPP LCP).
func HasLocalLabIPv4(fibTrie string) bool {
	lines := strings.Split(fibTrie, "\n")
	var lastLabIP netip.Addr
	for _, raw := range lines {
		line := strings.TrimSpace(raw)
		if strings.HasPrefix(line, "|-- ") {
			addrStr := strings.TrimSpace(strings.TrimPrefix(line, "|-- "))
			ip, err := netip.ParseAddr(addrStr)
			if err == nil && labSubnet.Contains(ip) && ip.String() != "10.89.20.1" && ip.String() != "10.89.20.0" && ip.String() != "10.89.20.255" {
				lastLabIP = ip
			} else {
				lastLabIP = netip.Addr{}
			}
			continue
		}
		if lastLabIP.IsValid() && strings.Contains(line, "/32 host LOCAL") {
			return true
		}
	}
	return false
}
