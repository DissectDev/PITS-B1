//go:build linux

package bridge

import (
	"context"
	"fmt"
	"net"
	"os"
	"runtime"
	"strings"

	"golang.org/x/sys/unix"
)

// resolveLabNetnsPath resolves "auto" (or "") on NixOS pits-core:
//  1. If the host netns itself owns an IP in 10.89.20.0/24, returns "" (direct dial).
//  2. Otherwise scans /proc/[0-9]*/ns/net (deduped by inode, skipping vpp_main) to
//     locate a live Multus macvlan container on Lab VLAN 20 (e.g. tinkerbell,
//     inventory-collector, workstation-sync-relay, or nvmet-*), replacing crictl+nsenter.
func resolveLabNetnsPath(configured string) (string, error) {
	if configured != "" && configured != "auto" {
		return configured, nil
	}
	if selfTrie, err := os.ReadFile("/proc/self/net/fib_trie"); err == nil && HasLocalLabIPv4(string(selfTrie)) {
		return "", nil
	}

	entries, err := os.ReadDir("/proc")
	if err != nil {
		return "", fmt.Errorf("scan /proc for Lab VLAN 20 netns: %w", err)
	}
	seenInodes := make(map[string]bool, 64)
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		pid := entry.Name()
		if len(pid) == 0 || pid[0] < '0' || pid[0] > '9' {
			continue
		}
		nsPath := "/proc/" + pid + "/ns/net"
		inode, err := os.Readlink(nsPath)
		if err != nil || seenInodes[inode] {
			continue
		}
		seenInodes[inode] = true

		if comm, cerr := os.ReadFile("/proc/" + pid + "/comm"); cerr == nil {
			if strings.TrimSpace(string(comm)) == "vpp_main" {
				continue
			}
		}
		trie, terr := os.ReadFile("/proc/" + pid + "/net/fib_trie")
		if terr != nil {
			continue
		}
		if HasLocalLabIPv4(string(trie)) {
			return nsPath, nil
		}
	}
	if configured == "" {
		// Fall back to direct dial when neither host nor container owns 10.89.20.x (e.g. routed test env)
		return "", nil
	}
	return "", fmt.Errorf("no active Lab VLAN 20 (10.89.20.0/24) container netns found under /proc")
}

func dialInNetns(ctx context.Context, netnsPath, addr string) (net.Conn, error) {
	resolvedNS, err := resolveLabNetnsPath(netnsPath)
	if err != nil {
		return nil, err
	}
	if resolvedNS == "" {
		var d net.Dialer
		return d.DialContext(ctx, "tcp", addr)
	}
	targetNS, err := os.Open(resolvedNS)
	if err != nil {
		return nil, fmt.Errorf("open lab netns %s: %w", resolvedNS, err)
	}
	defer targetNS.Close()

	type dialResult struct {
		conn net.Conn
		err  error
	}
	ch := make(chan dialResult, 1)
	go func() {
		runtime.LockOSThread()
		origNS, oerr := os.Open("/proc/self/task/" + fmt.Sprint(unix.Gettid()) + "/ns/net")
		if oerr != nil {
			runtime.UnlockOSThread()
			ch <- dialResult{err: oerr}
			return
		}
		if serr := unix.Setns(int(targetNS.Fd()), unix.CLONE_NEWNET); serr != nil {
			origNS.Close()
			runtime.UnlockOSThread()
			ch <- dialResult{err: fmt.Errorf("setns %s: %w", resolvedNS, serr)}
			return
		}
		var d net.Dialer
		conn, derr := d.DialContext(ctx, "tcp", addr)
		restoreErr := unix.Setns(int(origNS.Fd()), unix.CLONE_NEWNET)
		origNS.Close()
		if restoreErr == nil {
			runtime.UnlockOSThread()
		}
		ch <- dialResult{conn: conn, err: derr}
	}()
	res := <-ch
	return res.conn, res.err
}
