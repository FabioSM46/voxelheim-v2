package game

import (
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Water as terrain a creature pays for, driven through the tick the way the loop
// drives it.
//
// **Every assertion here is about the relationship [SwimSpeed] names, never about a
// number this file writes down.** What is being claimed is that a creature's speed in
// water is `min(registry speed, SwimSpeed)` — a cap, so a species already slower keeps
// its own number — and that the answer is recomputed from the body's box every tick,
// so leaving the water costs nothing and nothing is carried out of it. Pinning "the
// draugr moves 0.129 blocks a tick" would restate the arithmetic and would survive a
// rule that had stopped reading the box at all.
//
// The vertical is deliberately absent, because the vertical did not change: a creature
// still walks along the bed rather than floating. See [mob.physics], where that is a
// decision rather than a gap.

// fordTerrain is flat stone up to groundTop with a shallow channel of water lying on
// it, one block deep, in the columns from fordMinX to fordMaxX inclusive — the stream a
// creature walks *through* rather than the lake it walks into. The floor under the
// water is the same stone as the bank, so nothing here changes what the creature is
// standing on; only what its box overlaps while it crosses.
//
// absent, when set, is the chunk the tick could not read: [Terrain.Block] answers "not
// resident" for it, which is what a creature at the edge of loaded terrain sees.
type fordTerrain struct {
	groundTop int64
	fordMinX  int64
	fordMaxX  int64
	absent    func(x, y, z int64) bool
}

func (w fordTerrain) Block(x, y, z int64) (world.Block, bool) {
	if w.absent != nil && w.absent(x, y, z) {
		return world.Air, false
	}
	switch {
	case y <= w.groundTop:
		return world.Stone, true
	case y == w.groundTop+1 && x >= w.fordMinX && x <= w.fordMaxX:
		return world.Water, true
	default:
		return world.Air, true
	}
}

func (w fordTerrain) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

func (w fordTerrain) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || world.Solid(block)
}

// The fixture has to be the world it claims to be, or the crossing below passes for a
// reason that has nothing to do with water.
func TestTheFordFixtureIsWaterLyingOnUnbrokenGround(t *testing.T) {
	t.Parallel()

	ford := fordTerrain{groundTop: 63, fordMinX: 4, fordMaxX: 4}
	for _, tc := range []struct {
		x, y  int64
		block world.Block
		solid bool
		fluid bool
	}{
		// The bank: stone underfoot, air above it.
		{0, 63, world.Stone, true, false},
		{0, 64, world.Air, false, false},
		// The channel: the same stone floor, water standing on it, air over that.
		{4, 63, world.Stone, true, false},
		{4, 64, world.Water, false, true},
		{4, 65, world.Air, false, false},
	} {
		block, _ := ford.Block(tc.x, tc.y, 0)
		if block != tc.block || ford.Solid(tc.x, tc.y, 0) != tc.solid || ford.Fluid(tc.x, tc.y, 0) != tc.fluid {
			t.Errorf("(%d,%d) is block %d (solid %t, fluid %t), want block %d (solid %t, fluid %t)",
				tc.x, tc.y, block, ford.Solid(tc.x, tc.y, 0), ford.Fluid(tc.x, tc.y, 0),
				tc.block, tc.solid, tc.fluid)
		}
	}
}

// Every registered species is capped at SwimSpeed in water and untouched out of it.
//
// **A sweep rather than three tests, and the expectation is computed from the row.**
// The claim is about the rule, so a species added tomorrow is covered by the same
// assertion instead of needing a fourth copy of it — and the dry half is what says a
// creature on land still moves at exactly the number the registry holds, for every
// species, which is the half a regression would reach first.
func TestEverySpeciesIsCappedAtTheSwimSpeedInWaterAndUnchangedOnLand(t *testing.T) {
	t.Parallel()

	lake := lakeWorld{bedTop: 40, waterTop: 60}
	dry := dropTerrain{groundTop: 63}

	for kind, def := range mobRegistry {
		submerged := &mob{kind: kind, pos: [3]float64{0.5, 45, 0.5}}
		if got, want := submerged.speedIn(lake), min(def.speed, SwimSpeed); got != want {
			t.Errorf("a %s in water travels at %v, want min(%v, %v) = %v",
				kind, got, def.speed, SwimSpeed, want)
		}

		ashore := &mob{kind: kind, pos: [3]float64{0.5, 64, 0.5}}
		if got := ashore.speedIn(dry); got != def.speed {
			t.Errorf("a %s on land travels at %v, want the registry's %v", kind, got, def.speed)
		}
	}
}

// A species already slower than the water keeps its own speed.
//
// **This is the test that tells a cap from a scale, and it is the reason the rule is
// written as one.** Every registered species is above SwimSpeed on land today, so a
// `speed *= 0.6` would pass every other assertion in this file — it would slow all
// three creatures, in water, by a plausible-looking amount. Only a species *under* the
// cap separates the two: a cap leaves it alone, a scale takes six tenths of what was
// already slow, and a future modifier that had already halved a creature's speed would
// compound the same way. mobRegistry holds no such row, so one is borrowed for the
// length of this test — the pattern boss_encounter_test.go uses, and not parallel for
// the same reason.
func TestASpeciesSlowerThanTheCapKeepsItsOwnSpeedInWater(t *testing.T) {
	definition := mobRegistry[vnet.MobKindDraugr]
	wading := definition
	// Half the cap, so a scale and a cap cannot agree by accident.
	wading.speed = SwimSpeed / 2
	mobRegistry[vnet.MobKindDraugr] = wading
	t.Cleanup(func() { mobRegistry[vnet.MobKindDraugr] = definition })

	m := &mob{kind: vnet.MobKindDraugr, pos: [3]float64{0.5, 45, 0.5}}
	if got := m.speedIn(lakeWorld{bedTop: 40, waterTop: 60}); got != wading.speed {
		t.Errorf("a creature that walks at %v travels at %v in water, want its own %v: "+
			"the water is a cap on the speed, not a scale applied to it",
			wading.speed, got, wading.speed)
	}
}

// The water query reads the body it is asking about.
//
// **Two creatures standing on the same block, one of them in the water.** The channel's
// edge falls inside a vargr's 0.9-block box and outside a draugr's 0.6, so at x=3.6 the
// wider creature's box overlaps the channel and the narrower creature's does not — the
// same discrimination [TestTheStepProbeReadsTheProbingBodyFromTheRegistry] makes for the
// step probe, and for the same reason: a box spelled here, or the player's box borrowed,
// would answer the draugr's question for every species and no test in this file that put
// a creature squarely in a lake would notice.
func TestTheWaterQueryReadsTheBodyItIsAskingAbout(t *testing.T) {
	t.Parallel()

	ford := fordTerrain{groundTop: 63, fordMinX: 4, fordMaxX: 4}
	const standing = 3.6

	for kind, want := range map[vnet.MobKind]float64{
		// [3.15, 4.05] reaches into the channel; capped.
		vnet.MobKindVargr: SwimSpeed,
		// [3.30, 3.90] stops short of it; the registry's own speed.
		vnet.MobKindDraugr: draugrRow.speed,
	} {
		def := mobRegistry[kind]
		m := &mob{kind: kind, pos: [3]float64{standing, 64, 0.5}}
		if got := m.speedIn(ford); got != want {
			t.Errorf("a %s (%v wide) standing at x=%v travels at %v, want %v",
				kind, def.body.width, standing, got, want)
		}
	}
}

// A creature at the edge of loaded terrain is not in water.
//
// The conservative direction, and the existing one: [Terrain.Fluid] answers false for a
// chunk the tick could not read, so a creature standing where the world has not
// generated yet runs at its land speed rather than being slowed by water nobody has
// seen. The fixture is a ford whose channel is unreadable — water if it were resident,
// and therefore the one arrangement in which the two answers differ.
func TestACreatureAtTheEdgeOfLoadedTerrainIsNotInWater(t *testing.T) {
	t.Parallel()

	unread := fordTerrain{
		groundTop: 63,
		fordMinX:  0,
		fordMaxX:  0,
		absent:    func(_, y, _ int64) bool { return y > 63 },
	}
	m := &mob{kind: vnet.MobKindDraugr, pos: [3]float64{0.5, 64, 0.5}}
	if got := m.speedIn(unread); got != draugrRow.speed {
		t.Errorf("a draugr over an unreadable chunk travels at %v, want the registry's %v",
			got, draugrRow.speed)
	}

	// The same ford, read: the assertion above is only worth anything if the water is
	// there to be found once the chunk resolves.
	loaded := unread
	loaded.absent = nil
	if got := m.speedIn(loaded); got != SwimSpeed {
		t.Errorf("the same column, loaded, gives %v, want the cap %v — the fixture is not "+
			"water and the test above proves nothing", got, SwimSpeed)
	}
}

// A creature crossing a ford is slowed exactly while its box is in the water.
//
// **Per tick, against the box the creature actually occupied at the start of it.** The
// expectation is not a number but the same overlap question the rule asks, so what is
// being pinned is the coupling: the tick a draugr's body first touches the channel is
// the tick it slows, the tick its body clears it is the tick it is back at 3.2, and
// there is no run-up, no decay and nothing carried across the bank. Nothing is stored
// on the mob for the crossing to accumulate into — see [mob.speedIn].
func TestACreatureIsSlowedOnlyWhileItsBoxOverlapsTheFord(t *testing.T) {
	t.Parallel()

	terrain := fordTerrain{groundTop: 63, fordMinX: 4, fordMaxX: 4}
	h := newVitalsHarness(t, DefaultTickRate, terrain)
	h.keepNight()
	// Twelve blocks away and straight along +X, so the chase is under way from the
	// first tick, the ford is crossed well before the swing's reach, and the whole
	// movement is on one axis.
	h.join(1, [3]float32{12.5, 64, 0.5})
	id := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})

	// One tick to choose a target: the step taken on it is the one the loop below
	// reads its first position from.
	h.step()

	const dt = 1.0 / float64(DefaultTickRate)
	wet, dryAfterWet := 0, 0
	for range 40 {
		before := h.mob(id).pos
		want := draugrRow.speed
		if overlapsFluid(terrain, draugrRow.body.boxAt(before)) {
			want = SwimSpeed
			wet++
		} else if wet > 0 {
			dryAfterWet++
		}

		h.step()

		if got := h.mob(id).pos[0] - before[0]; math.Abs(got-want*dt) > 1e-9 {
			t.Fatalf("from x=%v the draugr moved %v this tick, want %v (%v blocks a second)",
				before[0], got, want*dt, want)
		}
	}

	if wet == 0 {
		t.Fatal("the draugr never reached the ford: the crossing this test is about did not happen")
	}
	if dryAfterWet == 0 {
		t.Fatal("the draugr never left the ford: the tick it returns to full speed was never taken")
	}
}

// The crossing is a pure function of the inputs.
//
// Same seed, same terrain, same script, same positions — bit for bit. The water rule
// reads the terrain and the registry and nothing else: no clock, no package-level
// `rand`, and no state on the mob that a second run could start from differently.
func TestTheFordCrossingIsIdenticalOnTwoRunsOfTheSameInputs(t *testing.T) {
	t.Parallel()

	crossing := func() [][3]float64 {
		h := newVitalsHarness(t, DefaultTickRate, fordTerrain{groundTop: 63, fordMinX: 4, fordMaxX: 4})
		h.keepNight()
		h.join(1, [3]float32{12.5, 64, 0.5})
		id := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})

		track := make([][3]float64, 0, 40)
		for range 40 {
			h.step()
			track = append(track, h.mob(id).pos)
		}
		return track
	}

	first, second := crossing(), crossing()
	for tick := range first {
		if first[tick] != second[tick] {
			t.Fatalf("tick %d: %v then %v", tick, first[tick], second[tick])
		}
	}
}
