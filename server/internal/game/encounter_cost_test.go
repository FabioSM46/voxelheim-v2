package game

import (
	"fmt"
	"math"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// The first dungeon's server cost: wall time of the instance manager's step while a party
// fights each boss stage, against the tick interval the server runs at. This is a
// measurement harness for docs/reviews/dungeon-acceptance-1037.md, not a timing assertion:
// shared CI hosts say nothing about the reference machine, so nothing here can fail on time.
//
// The step timed is the whole InstanceManager.Step for a session holding the dungeon and
// its party: movement, collision, the encounter scheduler, hazards, damage and snapshots.
// The scripted players' own decoding and decisions run outside the timed call.

const (
	serverCostEnv    = "VOXELHEIM_SERVER_COST"
	serverCostDirEnv = "VOXELHEIM_SERVER_COST_DIR"

	// serverCostSeconds is how long each stage is fought: long enough for an evading party
	// to see a stage's whole repertoire, including the rituals.
	serverCostSeconds = 120
)

type serverCost struct {
	name  string
	ticks []int64
}

// nearestRank is the nearest-rank percentile of sorted nanoseconds: the smallest sample with
// at least p of all samples at or below it.
func nearestRank(sorted []int64, p float64) int64 {
	if len(sorted) == 0 {
		return 0
	}
	rank := max(int(math.Ceil(p*float64(len(sorted)))), 1)
	return sorted[rank-1]
}

func (c serverCost) row() string {
	sorted := slices.Clone(c.ticks)
	slices.Sort(sorted)
	var total int64
	for _, v := range sorted {
		total += v
	}
	micro := func(ns int64) string { return fmt.Sprintf("%.1f", float64(ns)/1e3) }
	budget := time.Second.Nanoseconds() / int64(DefaultTickRate)
	return strings.Join([]string{
		c.name, fmt.Sprint(len(sorted)),
		micro(total / int64(max(len(sorted), 1))), micro(nearestRank(sorted, 0.5)), micro(nearestRank(sorted, 0.95)),
		micro(nearestRank(sorted, 0.99)), micro(sorted[len(sorted)-1]), micro(budget),
		fmt.Sprintf("%.3f", float64(nearestRank(sorted, 0.99))/float64(budget)),
	}, ",")
}

func TestNearestRankPercentile(t *testing.T) {
	samples := []int64{1, 2, 3, 4, 5, 6, 7, 8, 9, 10}
	for _, c := range []struct {
		p    float64
		want int64
	}{{0.5, 5}, {0.95, 10}, {0.99, 10}, {0.1, 1}, {0.11, 2}, {0, 1}} {
		if got := nearestRank(samples, c.p); got != c.want {
			t.Errorf("nearestRank(%v) = %d, want %d", c.p, got, c.want)
		}
	}
	// One slow outlier among a hundred is the maximum, not the 99th percentile.
	hundred := make([]int64, 100)
	for i := range hundred {
		hundred[i] = 35
	}
	hundred[99] = 3_000_000
	if got := nearestRank(hundred, 0.99); got != 35 {
		t.Errorf("p99 of 99 fast and one slow sample = %d, want 35", got)
	}
}

func TestFirstDungeonServerCost(t *testing.T) {
	if os.Getenv(serverCostEnv) == "" {
		t.Skipf("measurement harness for docs/reviews/dungeon-acceptance-1037.md; set %s=1 to run it", serverCostEnv)
	}
	var costs []serverCost

	// The baseline: the same session and party with both bosses already dead, stepped for
	// as long as a fight.
	cleared := func(party int) serverCost {
		pt := newPlaytest(t, playtestConfig{boss: vnet.MobKindDraugrKing, party: party, kit: kitIron, policy: policyEvader})
		pt.s.mu.Lock()
		king := pt.s.mobs[pt.s.dungeon.kingID]
		killed := king != nil && pt.s.damageMobLocked(king, king.health)
		pt.s.mu.Unlock()
		if !killed {
			t.Fatal("the king survived the baseline kill")
		}
		pt.timed = true
		for range serverCostSeconds * int(DefaultTickRate) {
			pt.step()
		}
		return serverCost{name: fmt.Sprintf("cleared/party%d", party), ticks: pt.stepNanos}
	}

	// A discarded warm-up, so the first recorded scenario does not pay for the process's
	// first allocations.
	cleared(4)

	for _, boss := range []struct {
		kind   vnet.MobKind
		stages uint8
	}{{vnet.MobKindVargrGuardian, 2}, {vnet.MobKindDraugrKing, 3}} {
		for stage := uint8(1); stage <= boss.stages; stage++ {
			for _, setup := range []struct{ party, ranged int }{{1, 1}, {4, 2}} {
				cfg := playtestConfig{boss: boss.kind, party: setup.party, ranged: setup.ranged, kit: kitIron,
					policy: policyEvader, stage: stage, seconds: serverCostSeconds, timed: true}
				result := runPlaytest(t, cfg)
				costs = append(costs, serverCost{name: fmt.Sprintf("%s/party%d/stage%d", playtestBossName(boss.kind), setup.party, stage), ticks: result.stepNanos})
			}
		}
	}
	costs = append(costs, cleared(1), cleared(4))

	var b strings.Builder
	fmt.Fprintf(&b, "# %s/%s, %d logical CPUs, %s\n", runtime.GOOS, runtime.GOARCH, runtime.NumCPU(), runtime.Version())
	b.WriteString("scenario,ticks,mean_us,p50_us,p95_us,p99_us,max_us,tick_interval_us,p99_share_of_interval\n")
	for _, c := range costs {
		if len(c.ticks) == 0 {
			t.Fatalf("%s timed no steps", c.name)
		}
		b.WriteString(c.row())
		b.WriteByte('\n')
	}
	t.Log("\n" + b.String())
	if dir := os.Getenv(serverCostDirEnv); dir != "" {
		if err := os.WriteFile(filepath.Join(dir, "dungeon-server-cost-1037.csv"), []byte(b.String()), 0o644); err != nil {
			t.Fatal(err)
		}
	}
}
