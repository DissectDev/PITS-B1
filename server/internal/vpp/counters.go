package vpp

import (
	"strconv"
	"strings"
	"time"

	"pits.io/server/internal/api"
)

// InterfaceCounters holds parsed packet, byte, drop, and error counters for one VPP interface.
type InterfaceCounters struct {
	RxPackets uint64
	RxBytes   uint64
	TxPackets uint64
	TxBytes   uint64
	Drops     uint64
	RxErrors  uint64
	TxErrors  uint64
}

// ParseShowInterfaceCounters parses FD.io VPP `show interface` tabular CLI output into per-interface counters.
func ParseShowInterfaceCounters(raw string) map[string]InterfaceCounters {
	out := map[string]InterfaceCounters{}
	var current string

	for _, line := range strings.Split(raw, "\n") {
		trimmedRight := strings.TrimRight(line, " \t\r")
		if strings.TrimSpace(trimmedRight) == "" {
			continue
		}
		// Header line starts with "Name"
		if strings.HasPrefix(strings.TrimSpace(trimmedRight), "Name") && strings.Contains(trimmedRight, "Counter") {
			continue
		}

		// Non-indented lines begin a new interface block:
		// e.g. "TenGigabitEthernet3/2/0   2     up   1500/0/0/0  rx packets 12345"
		// or   "local0                    0    down   0/0/0/0"
		if !strings.HasPrefix(line, " ") && !strings.HasPrefix(line, "\t") {
			fields := strings.Fields(trimmedRight)
			if len(fields) == 0 {
				continue
			}
			current = fields[0]
			if _, exists := out[current]; !exists {
				out[current] = InterfaceCounters{}
			}
			// Sometimes counter name/value appears on the same line after MTU column (index >= 4)
			if len(fields) >= 6 {
				applyCounterFields(out, current, fields[4:])
			}
			continue
		}

		if current == "" {
			continue
		}
		fields := strings.Fields(trimmedRight)
		applyCounterFields(out, current, fields)
	}

	return out
}

func applyCounterFields(out map[string]InterfaceCounters, ifName string, fields []string) {
	if len(fields) < 2 {
		return
	}
	val, err := strconv.ParseUint(fields[len(fields)-1], 10, 64)
	if err != nil {
		return
	}
	counterName := strings.ToLower(strings.Join(fields[:len(fields)-1], " "))
	c := out[ifName]
	switch counterName {
	case "rx packets":
		c.RxPackets = val
	case "rx bytes":
		c.RxBytes = val
	case "tx packets":
		c.TxPackets = val
	case "tx bytes":
		c.TxBytes = val
	case "drops":
		c.Drops = val
	case "rx-error", "rx-miss", "rx-no-buf":
		c.RxErrors += val
	case "tx-error", "tx-no-buf":
		c.TxErrors += val
	}
	out[ifName] = c
}

// MergeInterfaceCounters applies parsed counters to report interfaces and populates report.Counters.
func MergeInterfaceCounters(report *api.VPPInspectReport, counters map[string]InterfaceCounters, sampledAt time.Time) {
	if report == nil || len(counters) == 0 {
		return
	}
	agg := &api.VPPAggregateCounters{
		SampledAt: sampledAt.UTC(),
	}
	for i := range report.Interfaces {
		c, ok := counters[report.Interfaces[i].Name]
		if !ok {
			continue
		}
		report.Interfaces[i].RxPackets = c.RxPackets
		report.Interfaces[i].RxBytes = c.RxBytes
		report.Interfaces[i].TxPackets = c.TxPackets
		report.Interfaces[i].TxBytes = c.TxBytes
		report.Interfaces[i].Drops = c.Drops
		report.Interfaces[i].RxErrors = c.RxErrors
		report.Interfaces[i].TxErrors = c.TxErrors

		agg.TotalRxPackets += c.RxPackets
		agg.TotalRxBytes += c.RxBytes
		agg.TotalTxPackets += c.TxPackets
		agg.TotalTxBytes += c.TxBytes
		agg.TotalDrops += c.Drops
		agg.TotalErrors += c.RxErrors + c.TxErrors
	}
	report.Counters = agg
}
