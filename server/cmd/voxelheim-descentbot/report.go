package main

import (
	"fmt"
	"io"
	"sort"
	"strings"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// The human time is the bot's own clock with its two boss fights replaced. The bot does not
// evade, so it fights both bosses under /immortal; those fights are swapped for a solo iron
// reader's kill times. Everything else stands as the bot walked, waited and fought it.
//
// **A solo run is not held to the band any more (#1332)**: the dungeon is sized for three to
// five, and one player meets it sized for three, so the verdict below is expected to read
// OUTSIDE for this solo bot. The band is TestTheRouteTakesSeventeenToTwentyThreeMinutes's,
// at three, four and five.

// soloIronReaderKills are a solo iron reader's kill times in seconds against bosses sized for
// three, under the energy economy — the #1099 harness at zero delay, measured on #1332 —
// the Vargr and then the Draugr. A scripted reader is never struck, so these are a floor on
// what a person alone could do rather than a time anybody is expected to reach.
var soloIronReaderKills = [2]float64{629.75, 988.90}

// humanBand is the acceptance band for one clear by a party of three to five.
var humanBand = [2]time.Duration{17 * time.Minute, 23 * time.Minute}

func humanEstimate(total, guardianFight, kingFight time.Duration) time.Duration {
	bosses := time.Duration((soloIronReaderKills[0] + soloIronReaderKills[1]) * float64(time.Second))
	return total - guardianFight - kingFight + bosses
}

func writeReport(out io.Writer, r *runner, failure error) {
	s := r.stats
	var b strings.Builder
	fmt.Fprintf(&b, "voxelheim-descentbot report\n")
	fmt.Fprintf(&b, "server: %s\n", r.server.commandLine)
	fmt.Fprintf(&b, "instance seed: %d\n", r.lay.seed)
	if failure != nil {
		fmt.Fprintf(&b, "RESULT: did not finish: %v\n", failure)
	} else {
		fmt.Fprintf(&b, "RESULT: cleared, portal to portal\n")
	}
	fmt.Fprintf(&b, "\nparts of the route (wall clock, deaths):\n")
	s.mu.Lock()
	totalDeaths := 0
	for _, p := range s.phases {
		end := p.end
		if end.IsZero() {
			end = time.Now()
		}
		fmt.Fprintf(&b, "  %-18s %8.1f s  %d deaths\n", p.name, end.Sub(p.start).Seconds(), p.deaths)
		totalDeaths += p.deaths
	}
	fmt.Fprintf(&b, "deaths: %d\n", totalDeaths)
	fmt.Fprintf(&b, "blows taken: %d\n", s.taken)
	fmt.Fprintf(&b, "\ncreatures (seen / killed by the bot):\n")
	kinds := make([]vnet.MobKind, 0, len(s.seen))
	for kind := range s.seen {
		kinds = append(kinds, kind)
	}
	sort.Slice(kinds, func(i, j int) bool { return kinds[i] < kinds[j] })
	for _, kind := range kinds {
		fmt.Fprintf(&b, "  %-14s %3d / %3d\n", vnet.EnumNamesMobKind[kind], len(s.seen[kind]), s.killed[kind])
	}
	fmt.Fprintf(&b, "\nunder /immortal:\n")
	var immortal time.Duration
	for phase, d := range s.immortal {
		fmt.Fprintf(&b, "  %-18s %8.1f s\n", phase, d.Seconds())
		immortal += d
	}
	fmt.Fprintf(&b, "portal placements (/teleport beside the open world's portal): %d\n", s.portalPlacements)
	fmt.Fprintf(&b, "stuck assists (/teleport): %d %v\n", len(s.assists), s.assists)
	fmt.Fprintf(&b, "creatures left unreachable: %d %v\n", len(s.unreachedMobs), s.unreachedMobs)
	fmt.Fprintf(&b, "development commands sent: %d\n", len(s.commands))
	s.mu.Unlock()

	fmt.Fprintf(&b, "\nrune inscription read %v, world.InstanceRuneOrder %v, lit runes per stone %v\n",
		r.rune.read, r.rune.want, r.rune.lit)
	fmt.Fprintf(&b, "curtain cobwebs cut: %d\n", r.websCut)
	for _, key := range []string{"grille lever to checkpoint", "twin lever run", "guardian fight", "king fight"} {
		if d, ok := r.timing[key]; ok {
			fmt.Fprintf(&b, "%s: %.1f s\n", key, d.Seconds())
		}
	}
	for _, label := range []string{"open world, before the portal", "inside, on arrival", "back in the open world"} {
		if rss, ok := r.rss[label]; ok {
			fmt.Fprintf(&b, "server resident set, %s: %.1f MiB\n", label, float64(rss)/(1<<20))
		}
	}
	if total, ok := r.timing["total"]; ok {
		human := humanEstimate(total, r.timing["guardian fight"], r.timing["king fight"])
		fmt.Fprintf(&b, "\nbot run, portal to portal: %.1f min (%.1f min of it under /immortal)\n", total.Minutes(), immortal.Minutes())
		verdict := "inside"
		if human < humanBand[0] || human > humanBand[1] {
			verdict = "OUTSIDE"
		}
		fmt.Fprintf(&b, "estimated solo human clear: %.1f min, %s the %v–%v band\n", human.Minutes(), verdict, humanBand[0], humanBand[1])
	}
	_, _ = io.WriteString(out, b.String())
}
