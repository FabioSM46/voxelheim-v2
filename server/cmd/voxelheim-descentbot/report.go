package main

import (
	"fmt"
	"io"
	"sort"
	"strings"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// The human time is the party's own clock with its two boss fights replaced by the energy
// readers' kill times for a party of that size (#1332's measurement, the numbers
// TestTheRouteTakesSeventeenToTwentyThreeMinutes adds). The bots swing whenever energy
// allows and never step aside, so their boss fights carry the time their deaths cost; a
// reader is never struck, which is what makes it the estimate's figure for a fight. Every
// other part of the route stands as the party walked, waited, died and fought it.
//
// The raw clock is reported beside the estimate and is the number with nothing replaced.

// readerKills are the iron readers' boss kill times in seconds under the energy economy —
// the Vargr and then the Draugr — for the party sizes they were measured at. A party of one
// meets the bosses sized for three (629.75 s and 988.90 s, a reader alone who is never
// struck); two was never measured, so a pair has no estimate.
var readerKills = map[int][2]float64{
	1: {629.75, 988.90},
	3: {206.65, 324.70},
	4: {204.65, 324.70},
	5: {204.55, 324.70},
}

// humanBand is the acceptance band for one clear by a party of three to five.
var humanBand = [2]time.Duration{17 * time.Minute, 23 * time.Minute}

// humanEstimate is the clock with both boss fights replaced by the readers' kills for a
// party of members, and false where no reader was measured at that size.
func humanEstimate(members int, total, guardianFight, kingFight time.Duration) (time.Duration, bool) {
	kills, ok := readerKills[members]
	if !ok {
		return 0, false
	}
	bosses := time.Duration((kills[0] + kills[1]) * float64(time.Second))
	return total - guardianFight - kingFight + bosses, true
}

func writeReport(out io.Writer, pt *party, failure error) {
	lead := pt.leaderRunner()
	var b strings.Builder
	fmt.Fprintf(&b, "voxelheim-descentbot report\n")
	fmt.Fprintf(&b, "server: %s\n", lead.server.commandLine)
	names := make([]string, len(pt.members))
	for i, m := range pt.members {
		names[i] = m.c.name
	}
	fmt.Fprintf(&b, "party: %d (%s)\n", len(pt.members), strings.Join(names, ", "))
	fmt.Fprintf(&b, "instance seed: %d\n", lead.lay.seed)
	if failure != nil {
		fmt.Fprintf(&b, "RESULT: did not finish: %v\n", failure)
	} else {
		fmt.Fprintf(&b, "RESULT: cleared, portal to portal\n")
	}

	fmt.Fprintf(&b, "\nparts of the route (party wall clock; deaths per member; wipes):\n")
	parts := lead.route()
	for _, p := range parts {
		deaths := make([]string, len(pt.members))
		reached := false
		for i, m := range pt.members {
			n := m.stats.deathsIn(p.name)
			deaths[i] = fmt.Sprint(n)
			if m.stats.reached(p.name) {
				reached = true
			}
		}
		if !reached {
			continue
		}
		clock := "   —   "
		if d, ok := lead.timing[p.name]; ok {
			clock = fmt.Sprintf("%6.1f s", d.Seconds())
		}
		fmt.Fprintf(&b, "  %-18s %s  deaths [%s]  wipes %d\n", p.name, clock, strings.Join(deaths, " "), pt.wipesIn(p.name))
	}
	totalDeaths := 0
	for _, m := range pt.members {
		n := m.stats.totalDeaths()
		totalDeaths += n
		m.stats.mu.Lock()
		fmt.Fprintf(&b, "%-7s deaths %d, blows taken %d, portal placements %d, stuck assists %d %v\n",
			m.c.name, n, m.stats.taken, m.stats.portalPlacements, len(m.stats.assists), m.stats.assists)
		if len(m.stats.unreachedMobs) > 0 {
			fmt.Fprintf(&b, "        creatures left unreachable: %v\n", m.stats.unreachedMobs)
		}
		m.stats.mu.Unlock()
	}
	fmt.Fprintf(&b, "deaths, whole party: %d\n", totalDeaths)

	fmt.Fprintf(&b, "\ncreatures (seen / killed by the party):\n")
	t := lead.stats.tally
	t.mu.Lock()
	kinds := make([]vnet.MobKind, 0, len(t.seen))
	for kind := range t.seen {
		kinds = append(kinds, kind)
	}
	sort.Slice(kinds, func(i, j int) bool { return kinds[i] < kinds[j] })
	for _, kind := range kinds {
		fmt.Fprintf(&b, "  %-14s %3d / %3d\n", vnet.EnumNamesMobKind[kind], len(t.seen[kind]), t.killed[kind])
	}
	t.mu.Unlock()

	commands, immortal := 0, 0
	for _, m := range pt.members {
		m.stats.mu.Lock()
		commands += len(m.stats.commands)
		for _, line := range m.stats.commands {
			if strings.HasPrefix(line, "/immortal") {
				immortal++
			}
		}
		m.stats.mu.Unlock()
	}
	fmt.Fprintf(&b, "development commands sent: %d (/immortal: %d)\n", commands, immortal)

	fmt.Fprintf(&b, "\nrune inscription read %v, world.InstanceRuneOrder %v, lit runes per stone %v\n",
		lead.rune.read, lead.rune.want, lead.rune.lit)
	fmt.Fprintf(&b, "curtain cobwebs cut: %d\n", lead.websCut)
	for _, key := range []string{"grille lever to checkpoint", "twin lever run"} {
		if d, ok := lead.timing[key]; ok {
			fmt.Fprintf(&b, "%s: %.1f s\n", key, d.Seconds())
		}
	}
	for _, key := range []string{"guardian fight", "king fight"} {
		if d, ok := pt.fightTime(key); ok {
			fmt.Fprintf(&b, "%s, first pull to kill: %.1f s\n", key, d.Seconds())
		}
	}
	for _, label := range []string{"open world, before the portal", "inside, on arrival", "back in the open world"} {
		if rss, ok := lead.rss[label]; ok {
			fmt.Fprintf(&b, "server resident set, %s: %.1f MiB\n", label, float64(rss)/(1<<20))
		}
	}
	if failure == nil {
		guardian, _ := pt.fightTime("guardian fight")
		king, _ := pt.fightTime("king fight")
		fmt.Fprintf(&b, "\nparty run, portal to portal: %.1f min\n", pt.total.Minutes())
		if human, ok := humanEstimate(len(pt.members), pt.total, guardian, king); ok {
			verdict := "inside"
			if human < humanBand[0] || human > humanBand[1] {
				verdict = "OUTSIDE"
			}
			fmt.Fprintf(&b, "estimated human clear for a party of %d: %.1f min, %s the %v–%v band\n",
				len(pt.members), human.Minutes(), verdict, humanBand[0], humanBand[1])
		}
	}
	_, _ = io.WriteString(out, b.String())
}
