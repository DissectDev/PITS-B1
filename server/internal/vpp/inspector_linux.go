//go:build linux

// Package vpp provides read-only live GoVPP binary socket inspection and bounded
// dpdk-input packet trace analysis on pits-core.
package vpp

import (
	"context"
	"fmt"
	"io"
	"os"
	"strings"
	"time"

	"go.fd.io/govpp"
	govppapi "go.fd.io/govpp/api"
	"go.fd.io/govpp/binapi/acl"
	interfaces "go.fd.io/govpp/binapi/interface"
	"go.fd.io/govpp/binapi/interface_types"
	"go.fd.io/govpp/binapi/ip"
	"go.fd.io/govpp/binapi/memclnt"
	"go.fd.io/govpp/binapi/nat44_ed"
	"go.fd.io/govpp/binapi/vlib"
	"go.fd.io/govpp/core"
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

func (in *Inspector) resolveSocketPath() string {
	if st, err := os.Stat(in.SocketPath); err == nil && st.Mode()&os.ModeSocket != 0 {
		return in.SocketPath
	}
	// On NixOS pits-core, VPP runs inside the RKE2 edge/gateway Pod with an emptyDir at /run/vpp.
	// Dynamically locate vpp_main via /proc/<pid>/root without invoking crictl or shell wrappers.
	entries, err := os.ReadDir("/proc")
	if err != nil {
		return in.SocketPath
	}
	relSock := strings.TrimPrefix(in.SocketPath, "/")
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		pid := entry.Name()
		if len(pid) == 0 || pid[0] < '0' || pid[0] > '9' {
			continue
		}
		comm, err := os.ReadFile("/proc/" + pid + "/comm")
		if err != nil || strings.TrimSpace(string(comm)) != "vpp_main" {
			continue
		}
		candidate := "/proc/" + pid + "/root/" + relSock
		if st, err := os.Stat(candidate); err == nil && st.Mode()&os.ModeSocket != 0 {
			return candidate
		}
	}
	return in.SocketPath
}

func (in *Inspector) connect() (*core.Connection, error) {
	sock := in.resolveSocketPath()
	st, err := os.Stat(sock)
	if err != nil {
		return nil, fmt.Errorf("stat VPP socket %s: %w", sock, err)
	}
	if st.Mode()&os.ModeSocket == 0 {
		return nil, fmt.Errorf("%s is not a Unix socket", sock)
	}
	conn, err := govpp.Connect(sock)
	if err != nil {
		return nil, fmt.Errorf("connect GoVPP %s: %w", sock, err)
	}
	return conn, nil
}

func cliInband(ctx context.Context, conn *core.Connection, cmd string) (string, error) {
	if strings.ContainsAny(cmd, "\r\n\x00") {
		return "", fmt.Errorf("invalid characters in VPP CLI request")
	}
	reply := &vlib.CliInbandReply{}
	if err := conn.Invoke(ctx, &vlib.CliInband{Cmd: cmd}, reply); err != nil {
		return "", err
	}
	if reply.Retval != 0 {
		return "", fmt.Errorf("VPP CLI retval %v: %s", govppapi.RetvalToVPPApiError(reply.Retval), strings.TrimSpace(reply.Reply))
	}
	return strings.ReplaceAll(reply.Reply, "\x00", ""), nil
}

func dumpMessages[D govppapi.Message](ctx context.Context, conn *core.Connection, req govppapi.Message, protoDetail D) ([]D, error) {
	ch, err := conn.NewAPIChannel()
	if err != nil {
		return nil, err
	}
	compatErr := ch.CheckCompatiblity(req, protoDetail)
	ch.Close()
	if compatErr != nil {
		return nil, fmt.Errorf("VPP binary compatibility %s: %w", req.GetMessageName(), compatErr)
	}
	stream, err := conn.NewStream(ctx)
	if err != nil {
		return nil, err
	}
	defer stream.Close()
	if err := stream.SendMsg(req); err != nil {
		return nil, err
	}
	if err := stream.SendMsg(&memclnt.ControlPing{}); err != nil {
		return nil, err
	}
	var out []D
	for {
		msg, err := stream.RecvMsg()
		if err != nil {
			return nil, err
		}
		if ping, ok := msg.(*memclnt.ControlPingReply); ok {
			return out, govppapi.RetvalToVPPApiError(ping.Retval)
		}
		typed, ok := msg.(D)
		if !ok {
			return nil, fmt.Errorf("unexpected VPP reply message %s", msg.GetMessageName())
		}
		out = append(out, typed)
	}
}

// Inspect reads interfaces, addresses, ACLs, bindings, NAT44-ED outputs/mappings, and FIB routes.
func (in *Inspector) Inspect(ctx context.Context) (*api.VPPInspectReport, error) {
	conn, err := in.connect()
	if err != nil {
		return nil, err
	}
	defer conn.Disconnect()

	ver, _ := cliInband(ctx, conn, "show version")
	allIdx := interface_types.InterfaceIndex(^uint32(0))
	swIfs, err := dumpMessages(ctx, conn, &interfaces.SwInterfaceDump{SwIfIndex: allIdx}, &interfaces.SwInterfaceDetails{})
	if err != nil {
		return nil, err
	}
	acls, err := dumpMessages(ctx, conn, &acl.ACLDump{ACLIndex: ^uint32(0)}, &acl.ACLDetails{})
	if err != nil {
		return nil, err
	}
	bindings, err := dumpMessages(ctx, conn, &acl.ACLInterfaceListDump{SwIfIndex: allIdx}, &acl.ACLInterfaceListDetails{})
	if err != nil {
		return nil, err
	}
	natStatic, err := dumpMessages(ctx, conn, &nat44_ed.Nat44StaticMappingDump{}, &nat44_ed.Nat44StaticMappingDetails{})
	if err != nil {
		return nil, err
	}
	routes, err := dumpMessages(ctx, conn, &ip.IPRouteDump{Table: ip.IPTable{TableID: 0}}, &ip.IPRouteDetails{})
	if err != nil {
		return nil, err
	}

	report := &api.VPPInspectReport{
		CollectedAt: time.Now().UTC(),
		SocketPath:  in.SocketPath,
		Version:     strings.TrimSpace(ver),
		RawSections: map[string]int{
			"interfaces": len(swIfs),
			"acls":       len(acls),
			"bindings":   len(bindings),
			"natStatic":  len(natStatic),
			"routes":     len(routes),
		},
	}
	nameByIdx := map[uint32]string{}
	ifSvc := interfaces.NewServiceClient(conn)
	for _, row := range swIfs {
		idx := uint32(row.SwIfIndex)
		name := strings.TrimRight(row.InterfaceName, "\x00")
		nameByIdx[idx] = name
		addrs, _ := dumpMessages(ctx, conn, &ip.IPAddressDump{SwIfIndex: row.SwIfIndex}, &ip.IPAddressDetails{})
		var addrList []string
		for _, a := range addrs {
			addrList = append(addrList, a.Prefix.String())
		}
		var vrf uint32
		if tbl, terr := ifSvc.SwInterfaceGetTable(ctx, &interfaces.SwInterfaceGetTable{SwIfIndex: row.SwIfIndex}); terr == nil {
			vrf = tbl.VrfID
		}
		report.Interfaces = append(report.Interfaces, api.VPPInterface{
			Index:     idx,
			Name:      name,
			AdminUp:   row.Flags&interface_types.IF_STATUS_API_FLAG_ADMIN_UP != 0,
			LinkUp:    row.Flags&interface_types.IF_STATUS_API_FLAG_LINK_UP != 0,
			MAC:       row.L2Address.String(),
			MTU:       row.Mtu[0],
			Addresses: addrList,
			VRFID:     vrf,
		})
	}
	for _, a := range acls {
		var rules []string
		for _, r := range a.R {
			rules = append(rules, fmt.Sprintf("permit=%v src=%s dst=%s proto=%d dport=%d-%d",
				r.IsPermit, r.SrcPrefix.String(), r.DstPrefix.String(), r.Proto,
				r.DstportOrIcmpcodeFirst, r.DstportOrIcmpcodeLast))
		}
		report.ACLs = append(report.ACLs, api.VPPACL{
			Index: a.ACLIndex,
			Tag:   strings.TrimRight(a.Tag, "\x00"),
			Rules: rules,
		})
	}
	for _, b := range bindings {
		idx := uint32(b.SwIfIndex)
		nIn := int(b.NInput)
		var inACLs, outACLs []uint32
		for i, id := range b.Acls {
			if i < nIn {
				inACLs = append(inACLs, id)
			} else {
				outACLs = append(outACLs, id)
			}
		}
		report.Bindings = append(report.Bindings, api.VPPACLBinding{
			InterfaceIndex: idx,
			InterfaceName:  nameByIdx[idx],
			InputACLs:      inACLs,
			OutputACLs:     outACLs,
		})
	}
	if outStream, nerr := nat44_ed.NewServiceClient(conn).Nat44EdOutputInterfaceGet(ctx, &nat44_ed.Nat44EdOutputInterfaceGet{}); nerr == nil {
		defer outStream.Close()
		for {
			item, _, rerr := outStream.Recv()
			if rerr == io.EOF || rerr != nil {
				break
			}
			idx := uint32(item.SwIfIndex)
			if n := nameByIdx[idx]; n != "" {
				report.NATOutputs = append(report.NATOutputs, n)
			} else {
				report.NATOutputs = append(report.NATOutputs, fmt.Sprintf("sw_if_index:%d", idx))
			}
		}
	}
	for _, sm := range natStatic {
		report.NATStatic = append(report.NATStatic, fmt.Sprintf("%s:%d -> %s:%d proto=%d",
			sm.ExternalIPAddress.String(), sm.ExternalPort, sm.LocalIPAddress.String(), sm.LocalPort, sm.Protocol))
	}
	for _, rt := range routes {
		report.Routes = append(report.Routes, rt.Route.Prefix.String())
	}
	return report, nil
}

// Trace reads or arms dpdk-input packet traces and counts ACL drop hits.
func (in *Inspector) Trace(ctx context.Context, req api.VPPTraceRequest) (*api.VPPTraceResult, error) {
	conn, err := in.connect()
	if err != nil {
		return nil, err
	}
	defer conn.Disconnect()

	limit := req.PacketCount
	if limit <= 0 || limit > 200 {
		limit = 100
	}
	raw, err := cliInband(ctx, conn, fmt.Sprintf("show trace max %d", limit))
	if err != nil {
		return nil, err
	}
	if req.ClearAfter {
		_, _ = cliInband(ctx, conn, "clear trace")
	}
	packets := strings.Split(raw, "Packet ")
	total := 0
	matchedDrops := 0
	for _, pkt := range packets[1:] {
		total++
		if req.FilterIP != "" && !strings.Contains(pkt, req.FilterIP) {
			continue
		}
		if strings.Contains(pkt, "acl-plugin-in-ip4-fa") && (strings.Contains(pkt, "error-drop") || strings.Contains(pkt, "ACL deny")) {
			matchedDrops++
		}
	}
	excerpt := raw
	if len(excerpt) > 4096 {
		excerpt = excerpt[:4096]
	}
	return &api.VPPTraceResult{
		CapturedAt:   time.Now().UTC(),
		TotalPackets: total,
		MatchedDrops: matchedDrops,
		Excerpt:      excerpt,
	}, nil
}

// ArmTrace clears existing traces and arms dpdk-input capture for N packets.
func (in *Inspector) ArmTrace(ctx context.Context, count int) error {
	conn, err := in.connect()
	if err != nil {
		return err
	}
	defer conn.Disconnect()
	if count <= 0 || count > 200 {
		count = 100
	}
	if _, err := cliInband(ctx, conn, "clear trace"); err != nil {
		return err
	}
	_, err = cliInband(ctx, conn, fmt.Sprintf("trace add dpdk-input %d", count))
	return err
}
