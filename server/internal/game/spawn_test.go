package game

import (
	"io"
	"log/slog"
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The spawn director, asked what it decided rather than what it tends to do.
//
// Every assertion here is exact. The generator is seeded from the world seed and
// advanced only inside the locked tick, so "the same world produces the same creatures"
// is a property this file can test rather than a claim it has to trust — see
// TestTheSameWorldSpawnsTheSameCreatures, which is the one that fails if anybody
// reaches for a package-level rand or a wall clock.

// spawnGround is the terrain the director tests are scripted over: flat stone to
// groundTop, air above it, and nothing at all outside a square of legal columns.
//
// **The hole is the point.** Outside the square every voxel is reported as belonging to
// a chunk that has not arrived, which is what a column at the edge of streamed terrain
// looks like — solid to the collision and refused by the director. It is how a test says
// "there is nowhere legal over there" without having to model a lake.
type spawnGround struct {
	groundTop int64

	// legalTo is the half-width of the square of columns that have terrain in them.
	// Zero means every column does.
	legalTo int64

	// carved is a column the player has dug out: it answers air all the way down, so
	// there is no surface in it at all.
	carved *[2]int64

	// flooded is the surface of the water lying on the ground: every cell from
	// groundTop+1 up to it holds [world.Water]. Zero means dry.
	//
	// **This used to be a block id past the end of the palette, because there was no
	// such block yet.** The synthetic stand-in existed so that the director's headroom
	// rule could be tested before anything in the world was passable; worldgen 5 made
	// it real, and a fixture holding a shape the palette now has is a fixture that can
	// drift away from what it stands for.
	flooded int64

	// iced puts a lid of [world.Ice] on the water at flooded+1, which is what a tundra
	// lake actually looks like. Ice is *solid*, so the surface scan stops on it and
	// every other check in the director passes — it is the one case the headroom rule
	// below cannot catch, and the reason the floor is asked about by name.
	iced bool
}

func (w spawnGround) Block(x, y, z int64) (world.Block, bool) {
	if w.legalTo != 0 && (abs64(x) > w.legalTo || abs64(z) > w.legalTo) {
		return world.Air, false
	}
	if w.carved != nil && x == w.carved[0] && z == w.carved[1] {
		return world.Air, true
	}
	if y <= w.groundTop {
		return world.Stone, true
	}
	if w.flooded != 0 && y <= w.flooded {
		return world.Water, true
	}
	if w.iced && w.flooded != 0 && y == w.flooded+1 {
		return world.Ice, true
	}
	return world.Air, true
}
func (w spawnGround) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

// Solid is the production rule, unchanged and read from the palette — which is what
// makes this terrain worth having: everywhere else in these tests "not solid" and
// "air" are the same answer, and a director that relies on that cannot be caught
// doing it.
func (w spawnGround) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || world.Solid(block)
}

func abs64(v int64) int64 {
	if v < 0 {
		return -v
	}
	return v
}

// ---------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------

// spawnPasses runs n spawn passes back to back, with no ticks between them.
//
// The caps are arithmetic over the population and the connected players, so they are
// asked of the pass directly: driving them through ticks instead would mix in the two
// removals and the despawn grace, and a test that failed would not say which of the four
// rules had moved. The tick-level behaviour — the clock, the dawn, the grace — is tested
// through h.advance below, which is the other half of the same split.
func (h *vitalsHarness) spawnPasses(n int) {
	h.t.Helper()
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	for range n {
		h.sim.spawnPassLocked(h.sim.sortedPlayersLocked())
	}
}

// placeMobAt puts a draugr somewhere directly, the way the director would, and
// placeSpeciesAt does the same for a chosen species.
//
// The draugr is the default because it is the nocturnal one: most of what this file
// tests is a rule about the dark, and a creature the dawn does not touch would make
// those tests pass for the wrong reason.
func (h *vitalsHarness) placeMobAt(pos [3]float64) uint64 {
	return h.placeSpeciesAt(vnet.MobKindDraugr, pos)
}

func (h *vitalsHarness) placeSpeciesAt(kind vnet.MobKind, pos [3]float64) uint64 {
	h.t.Helper()
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	return h.spawnMobAtLocked(kind, pos)
}

// mobSighting is where one creature stood and which species it is.
//
// The kind travels with the position because the director now chooses both, and every
// rule below that measures a body — the separation, the reach of an aggro range — has to
// measure *that* creature's rather than a default one's.
type mobSighting struct {
	pos  [3]float64
	kind vnet.MobKind
}

// mobPositions is where every mob stands and what it is, keyed by identity.
func (h *vitalsHarness) mobPositions() map[uint64]mobSighting {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()

	out := make(map[uint64]mobSighting, len(h.sim.mobs))
	for id, m := range h.sim.mobs {
		out[id] = mobSighting{pos: m.pos, kind: m.kind}
	}
	return out
}

// spawnLog records where every creature the director made first stood.
//
// Taken after each tick rather than at the end, because a mob moves: the director runs
// after the mobs have been advanced, so one created on tick N has not been stepped when
// tick N ends, and its position then is exactly the spot the director chose. Reading it
// later would be asserting against wherever it had walked to.
func (h *vitalsHarness) spawnLog(ticks int) map[uint64]mobSighting {
	h.t.Helper()

	spawned := make(map[uint64]mobSighting)
	for range ticks {
		h.step()
		for id, seen := range h.mobPositions() {
			if _, known := spawned[id]; !known {
				spawned[id] = seen
			}
		}
	}
	return spawned
}

// plantCampfire puts a campfire in the registry directly.
//
// **The structure the campfire issue registers does not exist yet, and this test does
// not wait for it.** What this issue owns is the radius and the predicate that reads it;
// what that one owns is the item, the recipe and the placement rule. A registry entry of
// the right kind is all the predicate has ever looked at, so writing one here tests
// exactly the half that is finished.
func (h *vitalsHarness) plantCampfire(anchor [3]int32) {
	h.t.Helper()
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()

	held := &structure{
		structureID: h.sim.mintEntityID(),
		kind:        vnet.StructureKindCampfire,
		anchor:      anchor,
		facing:      vnet.FacingNorth,
		owner:       identity.PlayerID{1},
		chunk:       world.ChunkOf(int64(anchor[0]), int64(anchor[1]), int64(anchor[2])),
	}
	h.sim.structures[held.structureID] = held
}

// columnDistance is how far apart two positions are horizontally, which is the axis the
// ring is measured on.
func columnDistance(a, b [3]float64) float64 {
	return math.Hypot(a[0]-b[0], a[2]-b[2])
}

// ---------------------------------------------------------------------------
// Nothing exists because the server started
// ---------------------------------------------------------------------------

// The world starts empty and stays empty while nobody is in it.
//
// **This is the whole of what replacing the boot spawn means.** There used to be one
// draugr standing in a field from the moment the process came up, whether or not anybody
// ever connected. The ceiling is zero players' worth of creatures, and the despawn rule
// reaches the same answer from the other side.
func TestAnUnattendedWorldHoldsNoCreatures(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63})
	h.keepNight()

	h.advance(20 * DefaultTickRate)
	if got := h.mobCount(); got != 0 {
		t.Errorf("a world nobody has joined holds %d creatures", got)
	}
}

// And a world that had creatures in it loses them when the last player leaves.
func TestTheLastPlayerLeavingEmptiesTheWorld(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63})
	h.keepNight()
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	h.advance(10 * DefaultTickRate)
	if h.mobCount() == 0 {
		t.Fatal("ten seconds of night beside a player produced no creatures at all")
	}

	h.sim.Leave(player)
	// The grace, and then one tick more: removal is strictly past it.
	h.advance(int(h.sim.mobDespawnTicks) + 1)

	if got := h.mobCount(); got != 0 {
		t.Errorf("%d creatures remain in a world with nobody in it", got)
	}
}

// ---------------------------------------------------------------------------
// Nocturnal, and the species the sun does not stop
// ---------------------------------------------------------------------------

// keepDay stops the world in the middle of the afternoon, which is keepNight's opposite
// and exists for the same reason: a rule about the hour is not tested by whichever hour
// the harness happened to start at.
func (h *vitalsHarness) keepDay() {
	h.t.Helper()
	if err := h.sim.RestoreClock(NightStartTicks / 2); err != nil {
		h.t.Fatalf("RestoreClock: %v", err)
	}
}

// speciesOverPasses runs n spawn passes and reports which species the director chose.
//
// **The world is emptied after every pass**, so the per-player cap never becomes what
// decides the answer: six creatures is what one player may face at once, and a test
// about *which* species arrive has to be allowed to see more than six of them. The
// director is otherwise driven exactly as spawnPasses drives it.
func (h *vitalsHarness) speciesOverPasses(n int) map[vnet.MobKind]int {
	h.t.Helper()
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()

	seen := make(map[vnet.MobKind]int)
	for range n {
		h.sim.spawnPassLocked(h.sim.sortedPlayersLocked())
		for id, m := range h.sim.mobs {
			seen[m.kind]++
			delete(h.sim.mobs, id)
		}
	}
	return seen
}

// The daylight brings the vargr and never the draugr.
//
// **The director asks the registry which species this hour allows; it does not ask what
// time it is and then name a creature.** That is the whole of the change the vargr
// forced: "nothing spawns in daylight" was true of a world whose only species was
// nocturnal, and it was a statement about the draugr wearing a clock's clothes.
func TestOnlyTheSpeciesTheHourAllowsArrivesInDaylight(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63})
	h.keepDay()
	h.join(1, [3]float32{0.5, 64, 0.5})

	if IsNight(h.sim.TickOfDay()) {
		t.Fatalf("the world is at tick %d of its day, which this test needs to be daylight", h.sim.TickOfDay())
	}

	seen := h.speciesOverPasses(200)
	if seen[vnet.MobKindVargr] == 0 {
		t.Error("two hundred passes of daylight beside a player produced no vargr at all")
	}
	for kind, count := range seen {
		if mobRegistry[kind].nocturnal {
			t.Errorf("%d %s arrived under the sun, and the dark is what is supposed to bring one", count, kind)
		}
	}
}

// And the night brings both, which is what makes the daylight rule a rule about the
// species rather than a rule about spawning at all.
func TestTheNightBringsEverySpecies(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63})
	h.keepNight()
	h.join(1, [3]float32{0.5, 64, 0.5})

	seen := h.speciesOverPasses(200)
	for kind := range mobRegistry {
		if seen[kind] == 0 {
			t.Errorf("two hundred passes of night produced no %s", kind)
		}
	}
}

// The dawn takes what it finds idle, on the tick after the night ends.
func TestDawnTakesADraugrThatIsHuntingNobody(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63})
	// Two ticks short of the end of the night. The clock advances at the *top* of Step,
	// so the first tick below lands on NightEndTicks-1 — the last tick of the night —
	// and the second lands on NightEndTicks, which is the first that is not.
	if err := h.sim.RestoreClock(NightEndTicks - 2); err != nil {
		t.Fatalf("RestoreClock: %v", err)
	}
	h.join(1, [3]float32{0.5, 64, 0.5})
	// Far enough out to have nobody to hunt: the ring's own inner radius, which is
	// twice the aggro range.
	idle := h.placeMobAt([3]float64{40.5, 64, 0.5})

	h.step()
	if !IsNight(h.sim.TickOfDay()) {
		t.Fatalf("the world is at tick %d of its day, which this test needs to be night", h.sim.TickOfDay())
	}
	if _, alive := h.mobState(idle); !alive {
		t.Fatal("the last tick of the night took a draugr that had done nothing wrong")
	}

	h.step()
	if _, alive := h.mobState(idle); alive {
		t.Error("a draugr with nothing to hunt survived the dawn")
	}
}

// The dawn is nothing to a vargr, hunting or not.
//
// The other half of "nocturnal is a property of the creature": the removal reads the
// same registry field the spawn rule does, so a species the sun does not keep out is a
// species the sun does not take away either.
func TestTheDawnLeavesAVargrWhereItStands(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63})
	if err := h.sim.RestoreClock(NightEndTicks - 2); err != nil {
		t.Fatalf("RestoreClock: %v", err)
	}
	h.join(1, [3]float32{0.5, 64, 0.5})
	// Out past every aggro range in the registry, so it is hunting nobody — which is
	// exactly the state the dawn takes a draugr in.
	idle := h.placeSpeciesAt(vnet.MobKindVargr, [3]float64{40.5, 64, 0.5})

	h.advance(2)
	if IsNight(h.sim.TickOfDay()) {
		t.Fatal("the world is still in the night this test needs it out of")
	}
	m, alive := h.mobState(idle)
	if !alive {
		t.Fatal("the dawn took a vargr, and nothing about a vargr is nocturnal")
	}
	if m.target != 0 {
		t.Fatalf("the vargr is hunting %d; this test needs one the dawn could have taken", m.target)
	}

	// And a while later, so this is the rule rather than the removal being a tick late.
	h.advance(5 * DefaultTickRate)
	if _, alive := h.mobState(idle); !alive {
		t.Error("a vargr standing in broad daylight was removed anyway")
	}
}

// And leaves alone the one that is hunting somebody — until it stops.
func TestDawnSparesADraugrThatIsHuntingSomebody(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63})
	if err := h.sim.RestoreClock(NightEndTicks - 2); err != nil {
		t.Fatalf("RestoreClock: %v", err)
	}
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	hunter := h.placeMobAt([3]float64{4.5, 64, 0.5})

	// Two ticks: the last of the night and the first of the day.
	h.advance(2)
	if IsNight(h.sim.TickOfDay()) {
		t.Fatal("the world is still in the night this test needs it out of")
	}
	m, alive := h.mobState(hunter)
	if !alive {
		t.Fatal("the dawn took a draugr that was mid-hunt")
	}
	if m.target != player.entityID {
		t.Fatalf("the draugr is hunting %d, not the player it was placed beside", m.target)
	}

	// The hunt is what is keeping it, so ending the hunt ends it. A corpse is not prey.
	h.hurt(player, PlayerMaxHealth)
	h.step()
	if _, alive := h.mobState(hunter); alive {
		t.Error("a draugr that lost its target in daylight is still standing")
	}
}

// ---------------------------------------------------------------------------
// Where a creature may stand
// ---------------------------------------------------------------------------

// Every spawn lands in the ring, on the surface, and no two land on top of each other.
func TestEverySpawnLandsInTheRingOnLegalGround(t *testing.T) {
	t.Parallel()

	const groundTop = 63

	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: groundTop})
	h.keepNight()
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	origin := h.position(player)

	spawned := h.spawnLog(60 * DefaultTickRate)
	if len(spawned) == 0 {
		t.Fatal("a minute of night beside a player produced no creatures at all")
	}

	for id, seen := range spawned {
		pos := seen.pos
		// Standing on the surface, which for this terrain is one block above the ground.
		if pos[1] != groundTop+1 {
			t.Errorf("creature %d stands at y=%v, not on the surface at %d", id, pos[1], groundTop+1)
		}
		// Centred in its column, like every other entity this simulation places.
		if pos[0] != math.Floor(pos[0])+0.5 || pos[2] != math.Floor(pos[2])+0.5 {
			t.Errorf("creature %d stands at %v, not centred in a column", id, pos)
		}

		// In the ring, measured between columns — which is how the draw is made. The
		// half-block centring is what the tolerance below accounts for.
		distance := columnDistance(pos, origin)
		if distance < MobSpawnRingInner-1 || distance > MobSpawnRingOuter+1 {
			t.Errorf("creature %d stands %v blocks from the player, outside the %d..%d ring",
				id, distance, MobSpawnRingInner, MobSpawnRingOuter)
		}
		// Past *its own* aggro range by a wide margin, which is what the inner radius is
		// chosen for: nothing may arrive already hunting, whatever it is.
		if aggro := mobRegistry[seen.kind].aggroRange; distance <= aggro {
			t.Errorf("%s %d arrived %v blocks away, inside an aggro range of %v",
				seen.kind, id, distance, aggro)
		}
	}

	// And no two of them arrived within the separation of each other. Compared at their
	// spawn positions, because that is where the rule was applied, and with each one's
	// own body, because that is the box the director measured.
	for a, first := range spawned {
		for b, second := range spawned {
			if a >= b {
				continue
			}
			gap := boxDistance(mobRegistry[first.kind].body.boxAt(first.pos),
				mobRegistry[second.kind].body.boxAt(second.pos))
			if gap < MobSpawnSeparation {
				t.Errorf("creatures %d and %d spawned %v blocks apart, inside a separation of %v",
					a, b, gap, MobSpawnSeparation)
			}
		}
	}
}

// A column with no terrain in it is not somewhere to stand.
//
// Two shapes of the same refusal: terrain the server has not composed, which reads solid
// to the collision and must not read as ground here, and a column a player has dug out,
// which is air all the way down and has no surface at all.
func TestASpawnNeedsGroundUnderAClearSky(t *testing.T) {
	t.Parallel()

	// Legal columns only within 40 blocks of the origin. The ring runs 32..72, so most
	// of it is over terrain that has not arrived.
	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63, legalTo: 40})
	h.keepNight()
	h.join(1, [3]float32{0.5, 64, 0.5})

	spawned := h.spawnLog(120 * DefaultTickRate)
	if len(spawned) == 0 {
		t.Fatal("no creature ever found the legal square")
	}
	for id, seen := range spawned {
		x, z := int64(math.Floor(seen.pos[0])), int64(math.Floor(seen.pos[2]))
		if abs64(x) > 40 || abs64(z) > 40 {
			t.Errorf("creature %d stands at %v, on terrain the server does not hold", id, seen.pos)
		}
	}
}

// The player's own excavation counts, because legality is read through the same seam the
// collision reads.
func TestADugOutColumnIsNotSomewhereToStand(t *testing.T) {
	t.Parallel()

	// One legal column at exactly 40 blocks out, and it is carved away. Nothing may
	// ever spawn.
	carved := [2]int64{40, 0}
	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63, legalTo: 40, carved: &carved})
	h.keepNight()
	h.join(1, [3]float32{0.5, 64, 0.5})

	spawned := h.spawnLog(120 * DefaultTickRate)
	for id, seen := range spawned {
		if int64(math.Floor(seen.pos[0])) == carved[0] && int64(math.Floor(seen.pos[2])) == carved[1] {
			t.Errorf("creature %d stands in the column the player dug out, at %v", id, seen.pos)
		}
	}
}

// A creature is not stood in a fluid, however happily the collision would let it wade in.
//
// The criterion is "the two blocks above it are air", and while nothing in this world was
// passable, air was indistinguishable from what the surface scan already guarantees. This
// terrain is the distinction: two blocks of water over ground that is otherwise perfectly
// legal, on which the scan finds the lake bed, the headroom is not solid, the spot is in
// the ring, in view, clear of bodies and nowhere near a fire — and every one of those
// answers is right. Only the block itself says no.
func TestNothingSpawnsInsideAFluid(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63, flooded: 65})
	h.keepNight()
	h.join(1, [3]float32{0.5, 66, 0.5})

	for id, seen := range h.spawnLog(120 * DefaultTickRate) {
		t.Errorf("creature %d was stood in the water at %v", id, seen.pos)
	}
}

// And not stood on the lid of one either.
//
// **Ice is the case the headroom rule cannot answer**, which is what makes this a
// second test rather than a variant of the one above. It is solid, so the downward
// scan stops on it and calls it the surface; the two cells above it are honest air;
// the spot is in the ring, in view and nowhere near a fire. Every check the director
// makes says yes except the one that reads what the floor is made of.
func TestNothingSpawnsOnTheIceOverAFluid(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63, flooded: 65, iced: true})
	h.keepNight()
	h.join(1, [3]float32{0.5, 68, 0.5})

	for id, seen := range h.spawnLog(120 * DefaultTickRate) {
		t.Errorf("creature %d was stood on the ice at %v", id, seen.pos)
	}
}

// The ice terrain has to be a world the director would otherwise accept, or the test
// above passes because the spot was illegal for some other reason entirely.
func TestTheIcedGroundIsOtherwiseLegalGround(t *testing.T) {
	t.Parallel()

	iced := spawnGround{groundTop: 63, flooded: 65, iced: true}
	if block, resident := iced.Block(40, 66, 0); !resident || block != world.Ice {
		t.Fatalf("the lid is block %d (resident %t), want Ice", block, resident)
	}
	if !iced.Solid(40, 66, 0) {
		t.Fatal("the ice lid is not solid, so the surface scan would never stop on it")
	}
	for _, y := range [2]int64{67, 68} {
		if block, resident := iced.Block(40, y, 0); !resident || block != world.Air {
			t.Fatalf("the cell at y=%d over the ice is block %d (resident %t), want Air", y, block, resident)
		}
	}
	// Which leaves exactly one reason to refuse it, and the same terrain without the
	// lid has none: a spot on the stone floor of a dry column is legal.
	dry := spawnGround{groundTop: 63}
	if block, resident := dry.Block(40, 63, 0); !resident || !standableFloor(block) {
		t.Fatalf("the dry floor is block %d (resident %t), which the director would refuse", block, resident)
	}
	if standableFloor(world.Ice) {
		t.Fatal("standableFloor accepts ice, so the test above proves nothing")
	}
}

// Nothing spawns on the ground a campfire keeps.
func TestNothingSpawnsInsideTheCampfireRadius(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63})
	h.keepNight()
	h.join(1, [3]float32{0.5, 64, 0.5})
	// Out in the ring, where creatures would otherwise land.
	fire := [3]int32{45, 63, 0}
	h.plantCampfire(fire)

	spawned := h.spawnLog(120 * DefaultTickRate)
	if len(spawned) == 0 {
		t.Fatal("no creature ever spawned, so nothing here was tested")
	}
	for id, seen := range spawned {
		if got := distanceToVoxel(seen.pos, [3]int64{int64(fire[0]), int64(fire[1]), int64(fire[2])}); got <= CampfireSafeRadius {
			t.Errorf("creature %d stands %v blocks from the fire, inside %v", id, got, CampfireSafeRadius)
		}
	}
}

// And the predicate is correct with no fire in the world, which is why this issue does
// not wait on the one that builds them.
func TestTheCampfireRuleIsCorrectWithNoCampfires(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63})
	h.sim.mu.Lock()
	near := h.sim.nearACampfireLocked([3]float64{0.5, 64, 0.5})
	h.sim.mu.Unlock()

	if near {
		t.Error("a world with no structures at all reports a campfire nearby")
	}
}

// ---------------------------------------------------------------------------
// The caps
// ---------------------------------------------------------------------------

// One player's streamed cube never holds more than MobsPerPlayer creatures.
func TestThePerPlayerCapBoundsWhatOnePlayerFaces(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63})
	h.keepNight()
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	// Far more passes than the cap, so a cap that is not enforced overshoots it.
	h.spawnPasses(200)

	h.sim.mu.Lock()
	inView := h.sim.mobsInViewLocked(player)
	total := len(h.sim.mobs)
	h.sim.mu.Unlock()

	if inView > MobsPerPlayer {
		t.Errorf("%d creatures stand in one player's cube, over a cap of %d", inView, MobsPerPlayer)
	}
	if inView != MobsPerPlayer {
		t.Errorf("%d creatures after 200 passes: the cap is never reached, so it is not what stopped them", inView)
	}
	if total != inView {
		t.Errorf("the world holds %d creatures and the cube holds %d; nothing here should be outside it", total, inView)
	}
}

// The world ceiling refuses a spawn the per-player cap would have allowed.
//
// The mobs are placed outside every streamed cube, so the per-player count is zero and
// the only thing that can refuse is the ceiling.
func TestTheWorldCeilingBoundsWhatThePerPlayerCapCannotSee(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63})
	h.keepNight()
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	// Exactly the ceiling for one player, all of it far away.
	for i := range MobsPerPlayerWorldwide {
		h.placeMobAt([3]float64{10000.5, 64, float64(i) * 10})
	}

	h.sim.mu.Lock()
	inView := h.sim.mobsInViewLocked(player)
	h.sim.mu.Unlock()
	if inView != 0 {
		t.Fatalf("%d of the placed creatures are inside the player's cube; this test needs none to be", inView)
	}

	h.spawnPasses(50)
	if got := h.mobCount(); got != MobsPerPlayerWorldwide {
		t.Errorf("the world holds %d creatures against a ceiling of %d", got, MobsPerPlayerWorldwide)
	}

	// A second player raises it, and the same passes now produce creatures.
	h.join(2, [3]float32{0.5, 64, 0.5})
	h.spawnPasses(50)
	if got := h.mobCount(); got <= MobsPerPlayerWorldwide {
		t.Errorf("a second player raised the ceiling to %d and the world still holds %d",
			2*MobsPerPlayerWorldwide, got)
	}
	if got := h.mobCount(); got > 2*MobsPerPlayerWorldwide {
		t.Errorf("the world holds %d creatures against a ceiling of %d", got, 2*MobsPerPlayerWorldwide)
	}
}

// And a disconnect lowers it: what was allowed a moment ago is refused.
func TestADisconnectLowersTheWorldCeiling(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63})
	h.keepNight()
	h.join(1, [3]float32{0.5, 64, 0.5})
	second, _ := h.join(2, [3]float32{0.5, 64, 0.5})

	// Above one player's ceiling and below two players', so which of the two applies is
	// the whole of what this test turns on. Far away, where the per-player cap cannot
	// see them.
	for i := range MobsPerPlayerWorldwide + 1 {
		h.placeMobAt([3]float64{10000.5, 64, float64(i) * 10})
	}
	before := h.mobCount()

	h.spawnPasses(50)
	if h.mobCount() <= before {
		t.Fatalf("two players' ceiling of %d refused every spawn from %d creatures",
			2*MobsPerPlayerWorldwide, before)
	}

	h.sim.Leave(second)
	after := h.mobCount()
	h.spawnPasses(50)

	if got := h.mobCount(); got != after {
		t.Errorf("a disconnect left a ceiling of %d and the world grew from %d to %d",
			MobsPerPlayerWorldwide, after, got)
	}
}

// ---------------------------------------------------------------------------
// Distance
// ---------------------------------------------------------------------------

// A creature nobody is streaming is removed, and not before the grace has run.
func TestAMobOutsideEveryCubeIsRemovedAfterTheGrace(t *testing.T) {
	t.Parallel()

	h := newVitalsHarnessAt(t, DefaultTickRate, spawnGround{groundTop: 63}, 1)
	h.keepNight()
	h.join(1, [3]float32{0.5, 64, 0.5})
	far := h.placeMobAt([3]float64{10000.5, 64, 0.5})

	// One tick short of the grace: still there.
	h.advance(int(h.sim.mobDespawnTicks))
	if _, alive := h.mobState(far); !alive {
		t.Fatalf("a creature was removed inside its %d-tick grace", h.sim.mobDespawnTicks)
	}

	h.step()
	if _, alive := h.mobState(far); alive {
		t.Error("a creature outside every streamed cube survived the grace")
	}
}

// Walking back into somebody's cube resets it, so a border is not a despawn line.
func TestSteppingBackIntoACubeResetsTheGrace(t *testing.T) {
	t.Parallel()

	h := newVitalsHarnessAt(t, DefaultTickRate, spawnGround{groundTop: 63}, 1)
	h.keepNight()
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	wanderer := h.placeMobAt([3]float64{10000.5, 64, 0.5})

	h.advance(int(h.sim.mobDespawnTicks) - 1)

	// The player, not the mob: moving the watcher is the same event from the other side,
	// and it is the one a player actually causes.
	h.sim.mu.Lock()
	player.pos = [3]float64{10000.5, 64, 0.5}
	h.sim.mu.Unlock()
	h.step()

	h.sim.mu.Lock()
	unseen := h.sim.mobs[wanderer].unseenTicks
	h.sim.mu.Unlock()
	if unseen != 0 {
		t.Errorf("the grace stands at %d ticks after the player walked back into range", unseen)
	}

	h.advance(int(h.sim.mobDespawnTicks))
	if _, alive := h.mobState(wanderer); !alive {
		t.Error("a creature the player is standing next to was removed anyway")
	}
}

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

// The same world, given the same ticks, produces the same creatures in the same places.
//
// **This is the test the whole design is arranged around.** A package-level rand, a
// reading of the wall clock, or a generator advanced outside the lock all pass every
// other test in this file and fail this one. It asserts exact positions rather than a
// distribution, which is only possible because none of those three is anywhere near the
// director.
func TestTheSameWorldSpawnsTheSameCreatures(t *testing.T) {
	t.Parallel()

	// The world seed is what is under test, so it is a parameter here rather than the
	// package's shared one.
	run := func(seed int64) map[uint64]mobSighting {
		t.Helper()

		sim, err := NewSim(DefaultTickRate, 8, seed, spawnGround{groundTop: 63}, refusedEdits{},
			testEntityIDs(), slog.New(slog.NewTextHandler(io.Discard, nil)))
		if err != nil {
			t.Fatalf("NewSim: %v", err)
		}

		h := &vitalsHarness{t: t, sim: sim}
		h.keepNight()
		h.join(1, [3]float32{0.5, 64, 0.5})
		return h.spawnLog(40 * DefaultTickRate)
	}

	first, second := run(20250820), run(20250820)
	if len(first) == 0 {
		t.Fatal("no creature spawned, so nothing here was compared")
	}
	if len(first) != len(second) {
		t.Fatalf("the same world spawned %d creatures and then %d", len(first), len(second))
	}
	for id, seen := range first {
		other, made := second[id]
		if !made {
			t.Errorf("creature %d was made by one run of the world and not the other", id)
			continue
		}
		if seen != other {
			t.Errorf("creature %d was a %s at %v in one run of the world and a %s at %v in the other",
				id, seen.kind, seen.pos, other.kind, other.pos)
		}
	}

	// And a different world is a different night. Without this the test above would
	// pass just as happily against a generator that ignored its seed entirely.
	other := run(20250821)
	same := true
	for id, seen := range first {
		if elsewhere, made := other[id]; !made || elsewhere != seen {
			same = false
			break
		}
	}
	if same && len(other) == len(first) {
		t.Error("two different world seeds produced identical spawns; the seed is not reaching the generator")
	}
}

// ---------------------------------------------------------------------------
// The numbers, and the relationships between them
// ---------------------------------------------------------------------------

// The relationships the constants exist inside, asserted where every one of them is
// visible.
//
// These are the numbers whose *pairing* is the design: an inner radius under the aggro
// range spawns creatures that are already hunting, an outer radius past the streamed
// cube spawns them on terrain nobody has been sent, and a separation wider than the ring
// would leave the ring unfillable. Each is a one-line comparison, and each is the line
// that would have caught somebody tuning one number without the other.
func TestTheSpawnGeometryHangsTogether(t *testing.T) {
	t.Parallel()

	// Every row, not one species'. The inner radius is what keeps a creature from
	// arriving already hunting, and it has to be true of the widest aggro range in the
	// registry rather than of whichever one somebody had in mind.
	for kind, def := range mobRegistry {
		if MobSpawnRingInner <= def.aggroRange {
			t.Errorf("the ring starts at %d blocks, inside a %s's aggro range of %v: it would arrive already hunting",
				MobSpawnRingInner, kind, def.aggroRange)
		}
		if MobSpawnSeparation < def.body.width {
			t.Errorf("a separation of %v is narrower than a %s's body of %v, so two of them could overlap",
				MobSpawnSeparation, kind, def.body.width)
		}
	}
	if MobSpawnRingInner >= MobSpawnRingOuter {
		t.Errorf("the ring runs from %d to %d, which is not a ring", MobSpawnRingInner, MobSpawnRingOuter)
	}
	if MobSpawnSeparation >= MobSpawnRingOuter-MobSpawnRingInner {
		t.Errorf("a separation of %v is wider than the %d-block ring it has to fit inside",
			MobSpawnSeparation, MobSpawnRingOuter-MobSpawnRingInner)
	}
	if MobsPerPlayerWorldwide <= MobsPerPlayer {
		t.Errorf("a world ceiling of %d per player under a per-cube cap of %d makes the ceiling bind first",
			MobsPerPlayerWorldwide, MobsPerPlayer)
	}
	// The fire against the *draugr's* aggro range rather than the widest in the
	// registry, and the asymmetry is deliberate rather than an oversight. What actually
	// keeps anything from arriving at the fire's edge is MobSpawnRingInner, checked
	// above against every row; CampfireSafeRadius is a second line, it belongs to the
	// campfire issue, and whether it should widen to cover the vargr's twenty blocks is
	// a balance decision for whoever owns that constant — not something to make true
	// here by loosening the assertion.
	if CampfireSafeRadius < draugrRow.aggroRange {
		t.Errorf("a fire keeps %v blocks clear against a draugr's aggro range of %v: you could be reached across the middle of it",
			CampfireSafeRadius, draugrRow.aggroRange)
	}

	// The outer radius against the cube the default view distance streams. The worst
	// case is a player at the very edge of their chunk looking the other way, which
	// leaves view distance × ChunkSize blocks of guaranteed cube on that axis — and the
	// ring is measured on the diagonal, so each axis has to hold the whole of it.
	guaranteed := DefaultViewDistance * world.ChunkSize
	if MobSpawnRingOuter > guaranteed {
		t.Errorf("the ring reaches %d blocks and the default view distance guarantees only %d",
			MobSpawnRingOuter, guaranteed)
	}
}

// A ring draw is in the ring or it is nothing, and it is never anywhere else.
func TestARingDrawIsEitherInTheRingOrRefused(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63})

	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()

	var accepted int
	for range 20000 {
		dx, dz, ok := h.sim.ringOffsetLocked()
		if !ok {
			if dx != 0 || dz != 0 {
				t.Fatalf("a refused draw returned an offset of (%d, %d)", dx, dz)
			}
			continue
		}
		accepted++
		distance := dx*dx + dz*dz
		if distance < MobSpawnRingInner*MobSpawnRingInner || distance > MobSpawnRingOuter*MobSpawnRingOuter {
			t.Fatalf("an accepted draw of (%d, %d) is %v blocks out, outside the %d..%d ring",
				dx, dz, math.Sqrt(float64(distance)), MobSpawnRingInner, MobSpawnRingOuter)
		}
	}

	// The square the draw is made from is bigger than the ring by a known amount, so a
	// wildly different acceptance rate means the draw has stopped being uniform over it.
	if accepted < 10000 || accepted > 16000 {
		t.Errorf("%d of 20000 draws landed in the ring; the annulus covers about 62%% of the square", accepted)
	}
}

// ---------------------------------------------------------------------------
// Against the real generator
// ---------------------------------------------------------------------------

// The director places creatures on terrain the world actually generates.
//
// **Every other test in this file is scripted, which is what makes them exact — and it
// is also what makes this one necessary.** A fixture answers whatever it was written to
// answer; the real cache answers with residency, with chunk boundaries every 32 blocks,
// and with a surface that moves under the terrain function. Two of the director's rules
// are only meaningfully exercised here: that the downward scan finds the *generated*
// surface, and that a chunk the cache has not composed is refused rather than read as
// ground.
func TestTheDirectorPlacesCreaturesOnGeneratedTerrain(t *testing.T) {
	t.Parallel()

	const seed = 4242
	const viewDistance = 3

	chunks := world.NewCache(seed, 2, 4096)
	spawn := world.SpawnAt(seed)
	center := world.ChunkOf(int64(spawn[0]), int64(spawn[1]), int64(spawn[2]))

	// The cube a session would have streamed by the time it is standing still. The tick
	// may never generate terrain, so a director asked before this has been done answers
	// "nowhere legal" — correctly, and the test would then be measuring nothing.
	for y := center.Y - viewDistance; y <= center.Y+viewDistance; y++ {
		for z := center.Z - viewDistance; z <= center.Z+viewDistance; z++ {
			for x := center.X - viewDistance; x <= center.X+viewDistance; x++ {
				if _, _, err := chunks.Get(t.Context(), world.Coord{X: x, Y: y, Z: z}); err != nil {
					t.Fatalf("generating chunk (%d, %d, %d): %v", x, y, z, err)
				}
			}
		}
	}

	terrain := NewCacheTerrain(chunks)
	sim, err := NewSim(DefaultTickRate, viewDistance, seed, terrain, refusedEdits{},
		testEntityIDs(), slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}

	h := &vitalsHarness{t: t, sim: sim}
	h.keepNight()
	h.join(1, spawn)

	spawned := h.spawnLog(60 * DefaultTickRate)
	if len(spawned) == 0 {
		t.Fatal("a minute of night on real terrain produced no creatures at all")
	}

	for id, seen := range spawned {
		pos := seen.pos
		x := int64(math.Floor(pos[0]))
		y := int64(math.Floor(pos[1]))
		z := int64(math.Floor(pos[2]))

		// Standing on something, with room to stand: the two questions the rule is.
		if !terrain.Solid(x, y-1, z) {
			t.Errorf("creature %d at %v is standing on nothing", id, pos)
		}
		if terrain.Solid(x, y, z) || terrain.Solid(x, y+1, z) {
			t.Errorf("creature %d at %v is standing inside the terrain", id, pos)
		}
		// And the block under it is terrain the server holds rather than a chunk that
		// has not arrived, which is the failure "solid" would otherwise hide.
		if block, resident := terrain.Block(x, y-1, z); !resident || block == world.Air {
			t.Errorf("creature %d at %v stands on a block that is %v, resident=%v", id, pos, block, resident)
		}
	}
}
