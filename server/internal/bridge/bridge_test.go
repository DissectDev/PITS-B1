package bridge

import "testing"

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

func TestHasLocalLabIPv4ParsesKernelFibTrie(t *testing.T) {
	tinkPodTrie := `Main:
  +-- 0.0.0.0/0 3 0 5
     +-- 10.42.128.0/24 2 0 2
        |-- 10.42.128.44
           /32 host LOCAL
     +-- 10.89.20.0/24 2 0 2
        |-- 10.89.20.0
           /32 link BROADCAST
        |-- 10.89.20.10
           /32 host LOCAL
        |-- 10.89.20.255
           /32 link BROADCAST
`
	if !HasLocalLabIPv4(tinkPodTrie) {
		t.Fatal("expected tinkerbell 10.89.20.10 fib_trie to match Lab VLAN 20 local IP")
	}

	hostTrie := `Main:
  +-- 0.0.0.0/0 3 0 5
     +-- 192.168.71.0/24 2 0 2
        |-- 192.168.71.35
           /32 host LOCAL
`
	if HasLocalLabIPv4(hostTrie) {
		t.Fatal("expected management-only host fib_trie not to match Lab VLAN 20")
	}
}
