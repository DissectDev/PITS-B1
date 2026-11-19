package vpp

import (
	"testing"
	"time"

	"pits.io/server/internal/api"
)

func TestParseShowInterfaceCountersAndMerge(t *testing.T) {
	raw := `              Name               Idx    State  MTU (L3/IP4/IP6/MPLS)     Counter          Count
GigabitEthernet5b/0/0             1      up          1500/0/0/0     rx packets               128450
                                                                    rx bytes              145002310
                                                                    tx packets                94120
                                                                    tx bytes               48201100
                                                                    drops                        17
                                                                    rx-miss                       2
                                                                    tx-error                      1
TenGigabitEthernet3/2/0.20        3      up          1500/0/0/0     rx packets                40000
                                                                    rx bytes               32000000
                                                                    tx packets                50000
                                                                    tx bytes               64000000
                                                                    drops                         3
local0                            0     down          0/0/0/0       drops                         0
`
	parsed := ParseShowInterfaceCounters(raw)
	wan, ok := parsed["GigabitEthernet5b/0/0"]
	if !ok {
		t.Fatalf("expected GigabitEthernet5b/0/0 in parsed counters")
	}
	if wan.RxPackets != 128450 || wan.RxBytes != 145002310 || wan.TxPackets != 94120 || wan.TxBytes != 48201100 {
		t.Fatalf("unexpected WAN counters: %+v", wan)
	}
	if wan.Drops != 17 || wan.RxErrors != 2 || wan.TxErrors != 1 {
		t.Fatalf("unexpected WAN drops/errors: %+v", wan)
	}

	report := &api.VPPInspectReport{
		Interfaces: []api.VPPInterface{
			{Index: 1, Name: "GigabitEthernet5b/0/0", AdminUp: true, LinkUp: true},
			{Index: 3, Name: "TenGigabitEthernet3/2/0.20", AdminUp: true, LinkUp: true},
		},
	}
	MergeInterfaceCounters(report, parsed, time.Unix(1700000000, 0).UTC())
	if report.Counters == nil {
		t.Fatalf("expected aggregate counters")
	}
	if report.Counters.TotalRxPackets != 168450 || report.Counters.TotalTxBytes != 112201100 {
		t.Fatalf("unexpected aggregate rx/tx: %+v", report.Counters)
	}
	if report.Counters.TotalDrops != 20 || report.Counters.TotalErrors != 3 {
		t.Fatalf("unexpected aggregate drops/errors: %+v", report.Counters)
	}
}
