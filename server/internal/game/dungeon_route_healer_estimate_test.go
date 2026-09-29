package game

import (
	"math"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// The first dungeon's route for a party that brings a healer (#1370).
//
// **What #1361 measured.** At level 21 on one build, a party of three with one healer
// cleared in 26.0 minutes and a party of five with one in 23.8, against 21.4 and 21.8 for
// the same parties holding only blades. Nearly all of it was the bosses.
//
// **Why, and it is arithmetic rather than behaviour.** A healer holds a sceptre in the hand
// a blade would be in. An orb is worth [OrbDamage] where an iron blade is worth
// [IronSwordDamage], and both cost [AttackEnergyCost] of the same reserve, so they land at
// the same energy-bound pace: a sceptre is a fifth of a blade. The dungeon still counts
// its holder as a whole member — a boss's health and a pack's size read who is inside and
// never what they hold (boss_scale.go) — so a party of three with a healer brings three
// members' health to the fight and 2.2 members' blades.
//
// **The estimate** is dungeon_route_estimate_test.go's with that one substitution: every
// kill takes the members over the blades as long. It is the same clean clear — nobody is
// struck, so the healer never has an ally to heal and every launch goes to the creature —
// which makes it the *fastest* a healer party can be, as the all-blade estimate is for a
// party of blades. The walk is nobody's blade and does not move; the siege moves little,
// because most of its length is its breathers.
//
// **The band is the iron-blade reference's, and a healer party is not held to it.** The
// owner's decision on #1370, 2026-09-29: no balance value changes. 17–23 minutes is what
// a party of blades takes (TestTheRouteTakesSeventeenToTwentyThreeMinutes); a party that
// trades a blade for a healer trades time for the health that healer restores, and the
// estimate below is what the trade costs. With one healer, four and five stay inside the
// band and three leaves it by about a minute.

// partyBlades is how many iron blades a party of members with healers among them swings as
// against a creature of kind.
func partyBlades(members, healers int, kind vnet.MobKind) float64 {
	return float64(members-healers) + float64(healers)*OrbBladeShare(kind)
}

// healerBossSeconds is the readers' kill of a boss of kind with healers of the party
// holding sceptres: the measured all-blade kill, longer by the members over the blades.
func healerBossSeconds(members, healers int, kind vnet.MobKind) float64 {
	kill := energyReaderKills[members].vargr
	if kind == vnet.MobKindDraugrKing {
		kill = energyReaderKills[members].draugr
	}
	return kill * float64(members) / partyBlades(members, healers, kind)
}

// healerPlacedSeconds is placedSeconds for a party with healers among it.
func healerPlacedSeconds(members, healers int) float64 {
	seconds := 0.0
	for _, species := range dungeonGroupSpecies {
		blows := dungeonPackSize(members) * tieredBlows(species.kind)
		seconds += bladeSeconds(blows, members, partyBlades(members, healers, species.kind))
	}
	return seconds
}

// healerRouteSeconds is the whole route for a party of members with healers among it.
func healerRouteSeconds(t *testing.T, seed int64, members, healers int) float64 {
	t.Helper()
	return routeWalkSeconds(t, seed) +
		healerBossSeconds(members, healers, vnet.MobKindVargrGuardian) +
		healerBossSeconds(members, healers, vnet.MobKindDraugrKing) +
		healerPlacedSeconds(members, healers) +
		siegeBladeSeconds(t, seed, members, partyBlades(members, healers, vnet.MobKindCaveSpider))
}

// A sceptre lands at a blade's pace and is worth a fifth of one: exactly on both bosses,
// and a sixth on the scorpion, whose shell rounds an orb's 4.8 down to 4. Both halves are
// what the estimate rests on: a faster blade, a slower sceptre or a heavier orb moves the
// share, and this fails until the estimate is re-read.
func TestASceptreIsAFifthOfAnIronBlade(t *testing.T) {
	refill := float64(AttackEnergyCost) / EnergyRegenPerSecond
	if SceptreCooldown.Seconds() > refill || SwordCooldown.Seconds() > refill {
		t.Fatalf("a cooldown outlasts the %.2f s refill: the two weapons no longer share a pace", refill)
	}
	for _, kind := range []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing} {
		if got := OrbBladeShare(kind); math.Abs(got-0.2) > 0.01 {
			t.Errorf("a sceptre is %.3f of an iron blade against a %s, want 0.2", got, kind)
		}
	}
	for _, species := range dungeonGroupSpecies {
		if got := OrbBladeShare(species.kind); got < 0.16 || got > 0.21 {
			t.Errorf("a sceptre is %.3f of an iron blade against a %s, want a sixth to a fifth", got, species.kind)
		}
	}
	if got := OrbBladeShare(vnet.MobKindCaveSpider); got < 0.16 || got > 0.21 {
		t.Errorf("a sceptre is %.3f of an iron blade against a spider, want a sixth to a fifth", got)
	}
}

// The model against the fights it explains. #1361 ran every composition twice on one
// build, once with blades only and once with the last member holding a sceptre
// (docs/reviews/dungeon-healer-1361.md), so each pair gives a measured slowdown that owes
// nothing to how the bots fight: they fight the same way in both. The estimate's slowdown
// is the members over the blades, and it is held to the measured one within a twentieth.
//
// The bots are struck and the healer answers it, so a few of every hundred launches went
// to an ally rather than the boss (33 of 508 at three, 17 of 456 at five); the estimate's
// healer never has to, which is the direction every residual but one leans.
func TestTheHealerSlowdownIsTheOneMeasured(t *testing.T) {
	for _, fight := range []struct {
		kind             vnet.MobKind
		members          int
		blades, healered float64
	}{
		{vnet.MobKindVargrGuardian, 3, 206.5, 284.7},
		{vnet.MobKindDraugrKing, 3, 331.0, 469.2},
		{vnet.MobKindVargrGuardian, 5, 205.7, 248.2},
		{vnet.MobKindDraugrKing, 5, 343.4, 404.7},
	} {
		measured := fight.healered / fight.blades
		model := float64(fight.members) / partyBlades(fight.members, 1, fight.kind)
		t.Logf("%s, %d members, one healer: measured ×%.3f, estimated ×%.3f (%+.1f%%)",
			playtestBossName(fight.kind), fight.members, measured, model, (model/measured-1)*100)
		if math.Abs(model/measured-1) > 0.05 {
			t.Errorf("%s, %d members: the estimate slows the kill ×%.3f, the run measured ×%.3f",
				playtestBossName(fight.kind), fight.members, model, measured)
		}
	}
}

// What one healer costs the route, at every party size and rotation: 23.9 minutes for
// three, 22.7 for four and 22.0 for five, against 20.0 with blades. Four and five stay in
// the iron reference's band; three leaves it, by less than a minute. That is recorded, not
// required: the band is not the healer party's (see the top of this file), and this pins
// where the trade lands so that a change to a blade, an orb, a row or a wave that moves it
// is seen moving it.
func TestOneHealerCostsTheRouteThis(t *testing.T) {
	recorded := map[int]float64{3: 23.9, 4: 22.7, 5: 22.0}
	for seed := int64(0); seed < 4; seed++ {
		for members := bossScaleMinMembers; members <= bossScaleMaxMembers; members++ {
			blades := time.Duration(healerRouteSeconds(t, seed, members, 0) * float64(time.Second))
			total := time.Duration(healerRouteSeconds(t, seed, members, 1) * float64(time.Second))
			t.Logf("seed %d, %d members: %.1f min with blades, %.1f min with one healer (+%.1f)",
				seed, members, blades.Minutes(), total.Minutes(), (total - blades).Minutes())
			if math.Abs(total.Minutes()-recorded[members]) > 0.2 || total <= blades {
				t.Fatalf("seed %d, %d members with a healer: %.1f min, recorded as %.1f",
					seed, members, total.Minutes(), recorded[members])
			}
		}
	}
}

// With no healer the role-aware estimate is the all-blade one, term for term.
func TestAPartyOfBladesIsTheReferenceEstimate(t *testing.T) {
	for members := bossScaleMinMembers; members <= bossScaleMaxMembers; members++ {
		want := routeWalkSeconds(t, 0) + energyReaderKills[members].vargr + energyReaderKills[members].draugr +
			placedSeconds(members) + siegeSeconds(t, 0, members)
		if got := healerRouteSeconds(t, 0, members, 0); math.Abs(got-want) > 1e-6 {
			t.Fatalf("%d blades: %.3f s, the reference estimate is %.3f s", members, got, want)
		}
	}
}
