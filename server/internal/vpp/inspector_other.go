//go:build !linux

package vpp

import (
	"context"
	"fmt"

	"pits.io/server/internal/api"
)

type Inspector struct {
	SocketPath string
}

func NewInspector(socketPath string) *Inspector {
	if socketPath == "" {
		socketPath = api.DefaultVPPSocket
	}
	return &Inspector{SocketPath: socketPath}
}

func (in *Inspector) Inspect(_ context.Context) (*api.VPPInspectReport, error) {
	return nil, fmt.Errorf("GoVPP Unix binary socket (%s) requires Linux host pits-core", in.SocketPath)
}

func (in *Inspector) Trace(_ context.Context, _ api.VPPTraceRequest) (*api.VPPTraceResult, error) {
	return nil, fmt.Errorf("GoVPP packet trace requires Linux host pits-core")
}

func (in *Inspector) ArmTrace(_ context.Context, _ int) error {
	return fmt.Errorf("GoVPP packet trace requires Linux host pits-core")
}
