package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strings"
)

// This is the only machine artifact exported by the manual workflow. It contains
// controlled scenario observations, never raw error text, addresses, keys or paths.
type hoardReport struct {
	Version         int
	Success         bool
	FailureCategory string
	Members         []hoardMemberReport
}

type hoardMemberReport struct {
	Member              int
	Evidence            hoardEvidence
	Deaths              int
	PortalPlacements    int
	StuckAssists        int
	DevelopmentCommands int
	ImmortalityCommands int
}

func (pt *party) hoardComplete() bool {
	if len(pt.members) != 3 {
		return false
	}
	crafts, repairs := 0, 0
	for _, r := range pt.members {
		h := r.hoard
		if !h.Bootstrap || !h.KingDone || !h.RuneOut {
			return false
		}
		for i := range h.ChestDone {
			if !h.ChestDone[i] || !h.Restored[i] {
				return false
			}
		}
		if h.Crafted {
			crafts++
		}
		if h.Repaired {
			repairs++
		}
	}
	return crafts == 1 && repairs == 1
}

func writeHoardReport(out io.Writer, pt *party, failure error) {
	report := hoardReport{Version: 1, Success: failure == nil && pt.hoardComplete()}
	switch {
	case errors.Is(failure, context.DeadlineExceeded):
		report.FailureCategory = "deadline"
	case errors.Is(failure, context.Canceled):
		report.FailureCategory = "cancelled"
	case errors.Is(failure, errDied):
		report.FailureCategory = "member died"
	case failure != nil:
		report.FailureCategory = "route or wire assertion or persistence"
	case !report.Success:
		report.FailureCategory = "incomplete evidence"
	}
	for i, r := range pt.members {
		member := hoardMemberReport{Member: i + 1, Evidence: r.hoard, Deaths: r.stats.totalDeaths()}
		r.stats.mu.Lock()
		member.PortalPlacements = r.stats.portalPlacements
		member.StuckAssists = len(r.stats.assists)
		member.DevelopmentCommands = len(r.stats.commands)
		for _, command := range r.stats.commands {
			if strings.HasPrefix(command, "/immortal") {
				member.ImmortalityCommands++
				report.Success = false
				report.FailureCategory = "forbidden immortality command"
			}
		}
		r.stats.mu.Unlock()
		report.Members = append(report.Members, member)
	}
	encoded, err := json.Marshal(report)
	if err != nil {
		// The report consists solely of JSON-safe scalar fields. Fail closed if changed.
		_, _ = fmt.Fprintln(out, `HOARD_EVIDENCE {"Version":1,"Success":false,"Members":[]}`)
		return
	}
	_, _ = fmt.Fprintf(out, "HOARD_EVIDENCE %s\n", encoded)
}
