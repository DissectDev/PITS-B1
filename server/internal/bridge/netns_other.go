//go:build !linux

package bridge

import (
	"context"
	"net"
)

func dialInNetns(ctx context.Context, _ string, addr string) (net.Conn, error) {
	var d net.Dialer
	return d.DialContext(ctx, "tcp", addr)
}
