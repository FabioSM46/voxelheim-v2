package game

import (
	"fmt"
	"math"
	"os"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The first dungeon's route, estimated rather than played (#1294, re-based on #1332).
//
// **What the estimate is.** A scripted full clear at the #1099 reference — iron-blade
// readers — timed from the layout and the balance, never from a wall clock, for the
// parties the dungeon is made for: three, four and five.
//
//   - **Walking** is the shortest walk over whole blocks between the route's stops
//     ([walkingSteps] over the solved layout) at [WalkSpeed]: the rune stones in the
//     seed's order, the guardian, the chasm's edge, the grille's lever, the checkpoint
//     past it, both twin levers, the checkpoint past the sand hall's door, the king, and
//     the return shortcut to the exit. Plus the fall down the chasm, from rest under
//     [Gravity], and the swim from under it to the shore at [SwimSpeed].
//   - **The bosses** are measured, not derived: [energyReaderKills], the #1099 harness's
//     iron readers under #1127's energy economy.
//   - **The lesser creatures** are the blows each needs at the tier — its tiered health
//     over an iron blade after the species' armour — shared by the party, at the pace
//     energy allows ([readerSwingSeconds]) and at the share of it those same readers
//     achieved on the guardian ([readerPace]).
//   - **The siege** is the wave schedule itself: the production descent run tick by tick
//     with every wave put down as soon as the party's blows can do it, until the last.
//
// It is an estimate of a clean clear: nobody dies, nobody waits for anybody, and the walk
// is straight between stops. Deaths only lengthen a run, so the band's lower edge is the
// one this protects; the upper edge is room for a party that is not perfect.

// energyReaderKills are the iron readers' boss kill times, in seconds, for parties of
// three, four and five, measured by the #1099 harness (encounter_playtest_test.go) at
// zero delay and level one under the energy economy, at the boss rows #1332 set.
// TestTheRouteBossKillsAreTheHarnesss reproduces them.
//
// **Why measured again.** The #1099 figures this estimate used to read — 222.65 and
// 355.45 seconds solo, 190.50 and 284.95 for four — predate #1127, when a swing cost
// nothing and a reader landed one every [SwordCooldown]. A swing now costs
// [AttackEnergyCost] of a reserve refilling [EnergyRegenPerSecond], so a reader lands one
// every two seconds, and the same harness at the #1099 rows measured 524.6 s and 816.7 s
// for three — the gap #1328 found, and the #1298 bot's 532.9 s and 812.9 s solo agree with
// it. The rows were then re-set so the energy-bound kills land in the approved design's
// three to four and five to six minutes (species.go), and these are what that measured.
var energyReaderKills = map[int]struct{ vargr, draugr float64 }{
	3: {206.65, 324.70},
	4: {204.65, 324.70},
	5: {204.55, 324.70},
}

// readerSwingSeconds is the time between one reader's blows once the reserve is spent:
// the cooldown, or the reserve's refill of one swing's cost, whichever is longer. Two
// seconds, the refill — the pace the energy economy set.
func readerSwingSeconds() float64 {
	return max(SwordCooldown.Seconds(), float64(AttackEnergyCost)/EnergyRegenPerSecond)
}

// readerPace is how much of the energy-bound pace a reader of a party of members achieves
// in a real fight: the guardian's per-member health over the blade's energy-bound damage
// rate, against the kill the readers were measured at. About 1.02 for three to five. It is
// no longer the 0.71–0.83 "uptime" the cooldown-bound readers had, because the reserve
// refills while a reader steps aside, so a dodge costs no blow; the few percent over one
// is the full reserve's opening burst of four swings.
func readerPace(members int) float64 {
	vargr, _ := mobByKind(vnet.MobKindVargrGuardian)
	per := float64((&mob{kind: vnet.MobKindVargrGuardian}).armoured(IronSwordDamage))
	pure := float64(vargr.maxHealth) / per * readerSwingSeconds()
	return pure / energyReaderKills[members].vargr
}

// tieredBlows is how many iron blows one tiered creature of kind takes to die.
func tieredBlows(kind vnet.MobKind) int {
	def, _ := mobByKind(kind)
	m := &mob{kind: kind, tiered: true}
	per := m.armoured(IronSwordDamage)
	// armoured floors a connecting blow at one, so per is never zero.
	return int((uint32(tierScaled(def.maxHealth, dungeonHealthTier)) + uint32(per) - 1) / uint32(per))
}

// blowSeconds is how long a party of members takes to land blows among them.
func blowSeconds(blows, members int) float64 {
	return float64(blows) * readerSwingSeconds() / (float64(members) * readerPace(members))
}

// routeWalkSeconds is the route's walking, falling and swimming for one seed.
func routeWalkSeconds(t *testing.T, seed int64) float64 {
	t.Helper()
	terrain := newDescentTerrain(t, seed)
	arrival, exit := world.InstanceAnchors(seed)
	guardian, king, gate := world.InstanceEncounterAnchors(seed)
	cps := routeCheckpoints(t, seed)
	var grille world.PlacedAnchor
	var twins, stones []world.PlacedAnchor
	for _, a := range world.InstanceDungeonAnchors(seed) {
		if a.Kind != world.AnchorInstanceMechanism {
			continue
		}
		switch a.Index {
		case world.GrillePuzzle:
			grille = a
		case world.TwinLeverPuzzle:
			twins = append(twins, a)
		case world.RunePuzzle:
			stones = append(stones, a)
		}
	}
	cell := func(a world.PlacedAnchor) [3]int64 { return [3]int64{a.X, a.Y, a.Z} }
	beside := func(a world.PlacedAnchor) [3]int64 {
		for _, d := range [][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
			if standableCell(terrain, a.X+d[0], a.Y, a.Z+d[1]) {
				return [3]int64{a.X + d[0], a.Y, a.Z + d[1]}
			}
		}
		t.Fatalf("seed %d: nowhere to stand beside %+v", seed, a)
		return [3]int64{}
	}
	steps := 0
	walk := func(from, to [3]int64) {
		n := walkingSteps(terrain, from, to)
		if n < 0 {
			t.Fatalf("seed %d: %v is not reachable from %v", seed, to, from)
		}
		steps += n
	}
	at := cell(arrival)
	for _, k := range world.InstanceRuneOrder(seed) {
		walk(at, beside(stones[k]))
		at = beside(stones[k])
	}
	walk(at, cell(guardian))
	// The guardian's arena is open floor to the trapdoor, so the edge is the straight line.
	edge := math.Hypot(float64(gate.X-guardian.X), float64(gate.Z-guardian.Z))
	fall := math.Sqrt(2 * float64(guardian.Y-cps[0].Y) / Gravity)
	swim := math.Hypot(float64(gate.X-cps[0].X), float64(gate.Z-cps[0].Z)) / SwimSpeed
	walk(cell(cps[0]), leverStand(t, terrain, grille))
	walk(leverStand(t, terrain, grille), cell(cps[1]))
	walk(cell(cps[1]), leverStand(t, terrain, twins[0]))
	walk(leverStand(t, terrain, twins[0]), leverStand(t, terrain, twins[1]))
	walk(leverStand(t, terrain, twins[1]), cell(cps[2]))
	walk(cell(cps[2]), cell(king))
	walk(cell(king), cell(exit))
	return (float64(steps)+edge)/WalkSpeed + fall + swim
}

// placedSeconds is the time a party of members spends killing its pack of every placed
// group at the tier.
func placedSeconds(members int) float64 {
	blows := 0
	for _, species := range dungeonGroupSpecies {
		blows += dungeonPackSize(members) * tieredBlows(species.kind)
	}
	return blowSeconds(blows, members)
}

// siegeSeconds runs the production wave schedule for a party of members standing in the
// cavern, putting every wave down as soon as its blows are landed, and answers how long
// the cave holds them from the first wave to the last spider's death.
func siegeSeconds(t *testing.T, seed int64, members int) float64 {
	t.Helper()
	s := newWavesSim(t, seed)
	cave := triggerCentre(t, s, world.CaveTrigger)
	party := make([]*Player, members)
	for i := range party {
		party[i] = delver(uint64(500+i), cave)
	}
	rate := math.Round(1 / s.dt)
	perSpider := tieredBlows(vnet.MobKindCaveSpider)
	downAt := map[uint64][]uint64{}
	var first uint64
	desc := &s.dungeon.descent
	for tick := uint64(1); tick < uint64(3600*rate); tick++ {
		released := len(desc.groups[world.CaveBurrowGroup])
		s.advanceDungeonDescentLocked(tick, party)
		if wave := desc.groups[world.CaveBurrowGroup][released:]; len(wave) > 0 {
			if first == 0 {
				first = tick
			}
			kill := tick + uint64(math.Ceil(blowSeconds(len(wave)*perSpider, members)*rate))
			downAt[kill] = append(downAt[kill], wave...)
		}
		for _, id := range downAt[tick] {
			if m := s.mobs[id]; m != nil && !s.damageMobLocked(m, m.health) {
				t.Fatalf("a spider survived its blows")
			}
		}
		if s.dungeonGroupClearedLocked(world.CaveBurrowGroup) {
			return float64(tick-first) / rate
		}
	}
	t.Fatalf("seed %d: the siege never ended for %d members", seed, members)
	return 0
}

// The whole route, for every party of three to five at every rotation, lands in the
// band. It fails the moment the balance — a tier, a wave, the schedule, a boss's measured
// kill or the layout's length — drifts out of it. A party of one or two meets the dungeon
// sized for three and is not held to the band: it is not what the dungeon is for (#1332).
func TestTheRouteTakesSeventeenToTwentyThreeMinutes(t *testing.T) {
	const low, high = 17 * time.Minute, 23 * time.Minute
	for seed := int64(0); seed < 4; seed++ {
		walk := routeWalkSeconds(t, seed)
		for members := bossScaleMinMembers; members <= bossScaleMaxMembers; members++ {
			bosses := energyReaderKills[members].vargr + energyReaderKills[members].draugr
			placed := placedSeconds(members)
			siege := siegeSeconds(t, seed, members)
			total := time.Duration((walk + bosses + placed + siege) * float64(time.Second))
			t.Logf("seed %d, %d members: walk %.0f s, bosses %.0f s, halls %.0f s, siege %.0f s = %.1f min",
				seed, members, walk, bosses, placed, siege, total.Minutes())
			if total < low || total > high {
				t.Fatalf("seed %d, %d members: the route takes %.1f min, outside %v–%v",
					seed, members, total.Minutes(), low, high)
			}
		}
	}
}

// The estimate reads the numbers it says it reads: a tier or a schedule change moves it.
func TestTheRouteEstimateFollowsTheBalance(t *testing.T) {
	base := placedSeconds(4)
	if got := blowSeconds(tieredBlows(vnet.MobKindDraugr), bossScaleMinMembers); got <= 0 || math.IsInf(got, 0) {
		t.Fatal("a draugr takes no time to kill")
	}
	// Every dungeon species, through the same armour path the blade takes: the only thing
	// this pins is the tier's multiplier over the row.
	for _, kind := range []vnet.MobKind{vnet.MobKindDraugr, vnet.MobKindVargr, vnet.MobKindScorpion, vnet.MobKindCaveSpider} {
		def, _ := mobByKind(kind)
		per := uint32((&mob{kind: kind}).armoured(IronSwordDamage))
		want := int((uint32(def.maxHealth)*uint32(dungeonHealthTier) + per - 1) / per)
		if got := tieredBlows(kind); got != want {
			t.Fatalf("a tiered %s takes %d blows, want %d", kind, got, want)
		}
	}
	if base <= 0 || siegeSeconds(t, 0, 4) < float64(spiderWaveCount-1)*spiderWaveBreather.Seconds() {
		t.Fatal(fmt.Sprint("the siege is shorter than its breathers: ", siegeSeconds(t, 0, 4)))
	}
}

// The boss kills the estimate reads are the harness's, at the rows the registry carries. A
// measurement, opt-in like the #1099 matrix, because six full fights cost about ten seconds
// of wall time:
//
//	cd server && VOXELHEIM_PLAYTEST=1 go test ./internal/game -run TestTheRouteBossKillsAreTheHarnesss -v
//
// The harness is deterministic at 20 Hz, so a drift of more than a tick is a changed fight.
func TestTheRouteBossKillsAreTheHarnesss(t *testing.T) {
	if os.Getenv(playtestEnv) == "" {
		t.Skipf("measurement behind energyReaderKills; set %s=1 to run it", playtestEnv)
	}
	for members := bossScaleMinMembers; members <= bossScaleMaxMembers; members++ {
		want := energyReaderKills[members]
		for boss, seconds := range map[vnet.MobKind]float64{vnet.MobKindVargrGuardian: want.vargr, vnet.MobKindDraugrKing: want.draugr} {
			r := newPlaytest(t, playtestConfig{boss: boss, party: members, kit: kitIron}).run()
			t.Logf("%s, %d iron readers: %.2f s, %d hits", playtestBossName(boss), members, r.killSeconds, r.hits)
			if !r.killed || math.Abs(r.killSeconds-seconds) > 1/float64(DefaultTickRate) {
				t.Errorf("%s, %d members: killed=%v in %.2f s, the estimate reads %.2f s", playtestBossName(boss), members, r.killed, r.killSeconds, seconds)
			}
		}
	}
}
