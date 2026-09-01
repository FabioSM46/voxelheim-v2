package game

import (
	"math"
	"testing"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The people the seed put there, asked about from four directions: where they come from,
// what they do with a tick, what they are on the wire, and what nothing can do to them.
//
// **Nothing here writes a coordinate down.** internal/world decides where a village
// stands and which slots its buildings offer; a literal position would keep passing after
// somebody moved a hut. What is asserted is the relationship — the slot the person stands
// in, the id derived from its column, the bearing that points at the middle — which is
// station_test.go's rule for the same generator.

// residentAnchors is every resident slot a settlement offers.
func residentAnchors(t *testing.T, s world.Settlement) []world.PlacedAnchor {
	t.Helper()

	var out []world.PlacedAnchor
	for _, slot := range s.Anchors() {
		if _, lives := residentRole(slot.Kind); lives {
			out = append(out, slot)
		}
	}
	if len(out) == 0 {
		t.Fatalf("the %s offers no resident slot at all", s.Kind)
	}
	return out
}

// residents is everybody standing, keyed by identity.
func (h *structureHarness) residents() map[uint64]*resident {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()

	out := make(map[uint64]*resident, len(h.sim.residents))
	for id, r := range h.sim.residents {
		out[id] = r
	}
	return out
}

// lookAtEveryone tells the simulation that every chunk the capital's resident slots fall
// in has entered somebody's view, which is the only way a resident is ever created.
func (h *structureHarness) lookAtEveryone(anchors []world.PlacedAnchor) {
	for _, slot := range anchors {
		h.look(world.ChunkOf(slot.X, slot.Y, slot.Z))
	}
}

// ---------------------------------------------------------------------------
// They are there because somebody looked
// ---------------------------------------------------------------------------

// The whole materialisation in one test: looking at the chunks the capital's slots fall
// in produces one person per slot, each standing in their own doorway, each with an id,
// a name and a face the seed decides, each facing the middle of the village.
func TestLookingAtACapitalStandsItsPeopleUp(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	capital := testCapital(t)
	anchors := residentAnchors(t, capital)

	h.lookAtEveryone(anchors)

	standing := h.residents()
	if len(standing) != len(anchors) {
		t.Fatalf("%d residents stand after looking at the capital, want one per slot, %d", len(standing), len(anchors))
	}

	for _, slot := range anchors {
		role, _ := residentRole(slot.Kind)
		id := residentID(testWorldSeed, slot.X, slot.Z)

		r, arrived := standing[id]
		if !arrived {
			t.Fatalf("the %s slot at (%d, %d) produced nobody with the id its column derives", slot.Kind, slot.X, slot.Z)
		}
		if r.role != role {
			t.Errorf("the person in the %s slot is a %s", slot.Kind, r.role)
		}
		if id&residentBit == 0 {
			t.Errorf("%s has id %d, which no minted id can be told apart from", r.name, id)
		}

		// The floor, not the ground under it: a station takes the block below its slot
		// because it rests on one, and a person stands in the air the drawing left them.
		wantPos := [3]float64{float64(slot.X) + 0.5, float64(slot.Y), float64(slot.Z) + 0.5}
		if r.pos != wantPos {
			t.Errorf("%s stands at %v, want the centre of their own slot, %v", r.name, r.pos, wantPos)
		}
		if want := world.ChunkOf(slot.X, slot.Y, slot.Z); r.chunk != want {
			t.Errorf("%s is filed under chunk %v, want the one they stand in, %v", r.name, r.chunk, want)
		}
		if want := yawOfFacing(facingTowards(slot.X, slot.Z, capital.CentreX, capital.CentreZ)); r.restYaw != want {
			t.Errorf("%s rests at yaw %.4f, want the bearing of their anchor, %.4f", r.name, r.restYaw, want)
		}
		if r.yaw != r.restYaw {
			t.Errorf("%s arrives at yaw %.4f rather than at rest, %.4f", r.name, r.yaw, r.restYaw)
		}
		if got := [2]int64{capital.CentreX, capital.CentreZ}; r.home != got {
			t.Errorf("%s calls %v home, want the settlement centre %v", r.name, r.home, got)
		}
		if err := r.appearance.Validate(); err != nil {
			t.Errorf("%s wears an appearance no client may be sent: %v", r.name, err)
		}
	}
}

// **Nobody joins Sim.mobs, and that is the whole safety argument.** Combat, the
// projectiles, the spawn director and the corpse maker every one of them read that map
// and nothing else; a resident is invulnerable, unlootable, un-aggroable and never
// despawned because it is not in it, rather than because each of those four learned a
// new exception.
func TestResidentsAreNotCreatures(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	h.lookAtEveryone(residentAnchors(t, testCapital(t)))

	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()

	if len(h.sim.residents) == 0 {
		t.Fatal("nobody stands in the capital")
	}
	stablemasters := 0
	for _, r := range h.sim.residents {
		if r.role == vnet.ResidentRoleStablemaster {
			stablemasters++
		}
	}
	if stablemasters != 1 {
		t.Errorf("the capital has %d stablemasters in the resident registry, want one", stablemasters)
	}
	if len(h.sim.mobs) != 0 {
		t.Errorf("%d creatures appeared when a village was looked at", len(h.sim.mobs))
	}
	if len(h.sim.corpses) != 0 {
		t.Errorf("%d corpses appeared when a village was looked at", len(h.sim.corpses))
	}
	if _, registered := mobByKind(vnet.MobKindVillager); registered {
		t.Error("MobKind.Villager has a species row; a resident has no health, reach or loot to put in one")
	}
}

// A chunk entering two views is one person, not two. The idempotence is the id rather
// than a flag, which is what makes the hook safe to run again on every chunk that
// re-enters a view — and what a restart rests on.
func TestLookingAgainStandsNobodyNew(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	anchors := residentAnchors(t, testCapital(t))

	h.lookAtEveryone(anchors)
	first := h.residents()

	for range 3 {
		h.lookAtEveryone(anchors)
	}

	again := h.residents()
	if len(again) != len(first) {
		t.Fatalf("%d residents stand after four passes, want the %d of the first", len(again), len(first))
	}
	for id, was := range first {
		is, still := again[id]
		if !still {
			t.Fatalf("%s left the world when somebody looked twice", was.name)
		}
		if is != was {
			t.Errorf("%s was replaced by a second allocation of the same id", was.name)
		}
	}
}

// Nothing about a resident is written down, so a restart is a second simulation over the
// same seed — and it has to name the same people. This is that test: two simulations,
// same world, same everybody.
func TestASecondSimulationOverTheSameSeedNamesTheSamePeople(t *testing.T) {
	t.Parallel()

	anchors := residentAnchors(t, testCapital(t))

	first := newStructureHarness(t)
	first.lookAtEveryone(anchors)
	before := first.residents()

	second := newStructureHarness(t)
	second.lookAtEveryone(anchors)
	after := second.residents()

	if len(after) != len(before) {
		t.Fatalf("the restarted world holds %d residents, want %d", len(after), len(before))
	}
	for id, was := range before {
		is, back := after[id]
		if !back {
			t.Fatalf("%s did not come back after the restart", was.name)
		}
		if is.name != was.name || is.role != was.role || is.appearance != was.appearance ||
			is.pos != was.pos || is.restYaw != was.restYaw || is.home != was.home {
			t.Errorf("entity %d came back as %s the %s, was %s the %s", id, is.name, is.role, was.name, was.role)
		}
	}
}

// The resident slots are all at ground level, so a chunk of open sky over the capital
// holds nobody. The hook is per chunk and the answer has to be per chunk.
func TestAChunkWithNobodyInItStandsNobodyUp(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	slot := residentAnchors(t, testCapital(t))[0]

	h.look(world.ChunkOf(slot.X, slot.Y+256, slot.Z))

	if standing := h.residents(); len(standing) != 0 {
		t.Errorf("%d residents stand in the sky above the capital", len(standing))
	}
}

// Every furnishing anchor is claimed by exactly one materialiser. The paddock slots are
// deliberately scenic anchors for the later horse issue and furnish nothing in this one.
//
// The guard against the failure that costs nothing to make and is invisible afterwards: a
// seventh resident slot appended to [world.AnchorKind] and forgotten here is a slot that
// silently produces nobody, and the settlement still builds.
func TestEveryFurnishingAnchorIsEitherAStationOrAPerson(t *testing.T) {
	t.Parallel()

	for kind, name := range map[world.AnchorKind]string{
		world.AnchorNone:         "no anchor",
		world.AnchorForge:        "forge",
		world.AnchorCampfire:     "campfire",
		world.AnchorSmith:        "smith",
		world.AnchorCarpenter:    "carpenter",
		world.AnchorCook:         "cook",
		world.AnchorTrader:       "trader",
		world.AnchorVillager:     "villager",
		world.AnchorGuard:        "guard",
		world.AnchorStablemaster: "stablemaster",
		world.AnchorPaddock:      "paddock",
	} {
		if got := kind.String(); got != name {
			t.Fatalf("AnchorKind %d is %q, want %q — this table has fallen behind the vocabulary", kind, got, name)
		}
		_, station := stationKind(kind)
		_, lives := residentRole(kind)
		switch {
		case kind == world.AnchorNone || kind == world.AnchorPaddock:
			if station || lives {
				t.Errorf("%s must furnish nothing in this issue", name)
			}
		case station && lives:
			t.Errorf("the %s slot is claimed by both materialisers", name)
		case !station && !lives:
			t.Errorf("the %s slot is claimed by neither materialiser and silently produces nothing", name)
		}
	}
}

// ---------------------------------------------------------------------------
// The names and the faces
// ---------------------------------------------------------------------------

// The name table is what a client draws over a head, so every entry has to be something
// the font has and the contract allows. ASCII is the load-bearing half: since #481 the
// client's own build fails on a non-ASCII literal, and a name it cannot draw would arrive
// as a row of missing glyphs.
func TestEveryNameIsOneAClientCanDraw(t *testing.T) {
	t.Parallel()

	seen := make(map[string]int, len(residentNames))
	for index, name := range residentNames {
		if name == "" {
			t.Errorf("name %d is empty, and a resident always has one", index)
		}
		if len(name) > protocol.ResidentNameMaxBytes {
			t.Errorf("%q is %d bytes, past the contract's %d", name, len(name), protocol.ResidentNameMaxBytes)
		}
		for _, r := range name {
			if r < 0x20 || r > 0x7E {
				t.Errorf("%q carries %q, which the client's font has no glyph for", name, r)
			}
		}
		if first, repeated := seen[name]; repeated {
			t.Errorf("%q is entry %d and entry %d; sixty-four names means sixty-four", name, first, index)
		}
		seen[name] = index
	}
	if len(residentNames) != 64 {
		t.Fatalf("the table holds %d names, and the index is six bits of a hash", len(residentNames))
	}
}

// Every face this world can hand out is one the wire allows, and the check is exhaustive
// over the palettes rather than over a sample of columns: [residentAppearance] draws six
// indices out of one hash, so what has to hold is that every combination validates.
func TestEveryFaceAResidentCanWearIsOneTheContractAllows(t *testing.T) {
	t.Parallel()

	for _, skin := range residentSkinTones {
		for _, hair := range residentHairColors {
			for _, shirt := range residentShirtColors {
				for _, trousers := range residentTrouserColors {
					for _, shoes := range residentShoeColors {
						for _, model := range residentHairModels {
							worn := protocol.Appearance{
								SkinColor: skin, HairColor: hair, ShirtColor: shirt,
								TrousersColor: trousers, ShoesColor: shoes, HairModel: model,
							}
							if err := worn.Validate(); err != nil {
								t.Fatalf("%+v is not wearable: %v", worn, err)
							}
						}
					}
				}
			}
		}
	}
}

// A name and a face are functions of the column and of nothing else, and they are
// functions of *different* offsets: two people with the same name do not therefore share
// a face. The sweep is what makes the second half more than an assertion about one pair.
func TestNamesAndFacesAreDrawnIndependently(t *testing.T) {
	t.Parallel()

	byName := make(map[string]map[protocol.Appearance]bool)
	for x := int64(-40); x < 40; x++ {
		for z := int64(-40); z < 40; z++ {
			name := residentName(testWorldSeed, x, z)
			if name != residentName(testWorldSeed, x, z) {
				t.Fatalf("the name at (%d, %d) is not a function of the column", x, z)
			}
			worn := residentAppearance(testWorldSeed, x, z)
			if worn != residentAppearance(testWorldSeed, x, z) {
				t.Fatalf("the face at (%d, %d) is not a function of the column", x, z)
			}
			if byName[name] == nil {
				byName[name] = make(map[protocol.Appearance]bool)
			}
			byName[name][worn] = true
		}
	}

	for name, faces := range byName {
		if len(faces) == 1 {
			t.Errorf("every %s in 6400 columns wears the same face; the two offsets have collapsed", name)
		}
	}
}

// ---------------------------------------------------------------------------
// The one thing a resident does
// ---------------------------------------------------------------------------

// standResidentAt puts somebody at a chosen position, through the same three derivations
// the materialiser uses.
//
// It exists because the behaviour, combat and wire tests need a person *next to a
// player*, and where a village stands is the generator's decision rather than a test's.
// The materialisation tests above are what pin the derivation itself; this is deliberately
// not a second copy of it.
func (h *vitalsHarness) standResidentAt(role vnet.ResidentRole, pos [3]float64, restYaw float64) *resident {
	h.t.Helper()

	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()

	x, z := int64(math.Floor(pos[0])), int64(math.Floor(pos[2]))
	r := &resident{
		entityID:   residentID(h.sim.worldSeed, x, z),
		role:       role,
		name:       residentName(h.sim.worldSeed, x, z),
		appearance: residentAppearance(h.sim.worldSeed, x, z),
		pos:        pos,
		yaw:        restYaw,
		restYaw:    restYaw,
		home:       [2]int64{x, z},
		chunk:      chunkAt(pos),
	}
	h.sim.residents[r.entityID] = r
	return r
}

// turn runs the resident pass alone, without a tick, so a test can ask what the turning
// does without gravity moving the player it is turning toward.
func (h *vitalsHarness) turn(n int) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()

	players := h.sim.sortedPlayersLocked()
	for range n {
		h.sim.advanceResidentsLocked(players)
	}
}

// residentYaw is where somebody is looking.
func (h *vitalsHarness) residentYaw(id uint64) float64 {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	return h.sim.residents[id].yaw
}

// yawFrom is the heading that points from one position at another, which is the mob
// integrator's formula and therefore the one a resident has to agree with.
func yawFrom(from, to [3]float64) float64 {
	return wrapAngle(math.Atan2(from[0]-to[0], from[2]-to[2]))
}

func TestAResidentTurnsTowardAPlayerWhoComesNear(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	r := h.standResidentAt(vnet.ResidentRoleSmith, [3]float64{0.5, 64, 0.5}, 0)
	player, _ := h.join(1, [3]float32{3.5, 64, 0.5})

	// A second of turning is more than enough for half a turn at ResidentTurnRate, so
	// what this asserts is where the heading settles rather than how it got there.
	h.turn(int(DefaultTickRate))

	want := yawFrom(r.pos, player.pos)
	if got := h.residentYaw(r.entityID); math.Abs(wrapAngle(got-want)) > 1e-9 {
		t.Errorf("%s looks along %.4f, want %.4f — the player standing three blocks away", r.name, got, want)
	}
}

func TestAResidentIgnoresAPlayerBeyondTheNoticeRadius(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	r := h.standResidentAt(vnet.ResidentRoleGuard, [3]float64{0.5, 64, 0.5}, math.Pi/2)

	// Body to body and comfortably outside six blocks, which is the measurement
	// nearestNoticedLocked takes rather than centre to centre.
	h.join(1, [3]float32{0.5, 64, 12.5})
	h.turn(int(DefaultTickRate))

	if got := h.residentYaw(r.entityID); math.Abs(wrapAngle(got-r.restYaw)) > 1e-9 {
		t.Errorf("%s looks along %.4f with nobody near, want their anchor's %.4f", r.name, got, r.restYaw)
	}
}

// Somebody who noticed you and then watched you leave goes back to facing their door.
// The return is the same arithmetic as the notice, which is what keeps one rate honest.
func TestAResidentGoesBackToItsAnchorsFacingOnceNobodyIsNear(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	r := h.standResidentAt(vnet.ResidentRoleCook, [3]float64{0.5, 64, 0.5}, math.Pi)
	player, _ := h.join(1, [3]float32{3.5, 64, 0.5})

	h.turn(int(DefaultTickRate))
	if got := h.residentYaw(r.entityID); math.Abs(wrapAngle(got-r.restYaw)) < 0.5 {
		t.Fatalf("%s never turned away from rest at all (%.4f)", r.name, got)
	}

	h.sim.mu.Lock()
	player.pos = [3]float64{0.5, 64, 40.5}
	h.sim.mu.Unlock()
	h.turn(int(DefaultTickRate))

	if got := h.residentYaw(r.entityID); math.Abs(wrapAngle(got-r.restYaw)) > 1e-9 {
		t.Errorf("%s is still looking along %.4f after the player left, want %.4f", r.name, got, r.restYaw)
	}
}

// The nearest of them, not the first the loop happens to reach.
func TestAResidentTurnsTowardTheNearestPlayer(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	r := h.standResidentAt(vnet.ResidentRoleTrader, [3]float64{0.5, 64, 0.5}, 0)
	h.join(1, [3]float32{4.5, 64, 0.5})
	near, _ := h.join(2, [3]float32{0.5, 64, -2.5})

	h.turn(int(DefaultTickRate))

	want := yawFrom(r.pos, near.pos)
	if got := h.residentYaw(r.entityID); math.Abs(wrapAngle(got-want)) > 1e-9 {
		t.Errorf("%s looks along %.4f, want the nearer player's %.4f", r.name, got, want)
	}
}

// A body on the ground is not company. It is also the only thing this entity class reads
// a player's life state for.
func TestADeadPlayerIsNotNoticed(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	r := h.standResidentAt(vnet.ResidentRoleVillager, [3]float64{0.5, 64, 0.5}, 0)
	player, _ := h.join(1, [3]float32{0.5, 64, 3.5})

	h.sim.mu.Lock()
	player.dieLocked()
	h.sim.mu.Unlock()

	h.turn(int(DefaultTickRate))

	if got := h.residentYaw(r.entityID); math.Abs(wrapAngle(got-r.restYaw)) > 1e-9 {
		t.Errorf("%s turned to %.4f to watch a corpse, want their anchor's %.4f", r.name, got, r.restYaw)
	}
}

// The rate is a rate: one tick moves the heading by at most one tick's worth of it.
func TestAResidentTurnsNoFasterThanItsRate(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	r := h.standResidentAt(vnet.ResidentRoleSmith, [3]float64{0.5, 64, 0.5}, 0)

	// Directly behind, so the heading has half a turn to travel and cannot arrive on the
	// first tick at any rate this world would accept.
	h.join(1, [3]float32{0.5, 64, 3.5})

	before := h.residentYaw(r.entityID)
	h.turn(1)
	moved := math.Abs(wrapAngle(h.residentYaw(r.entityID) - before))

	if step := ResidentTurnRate * h.sim.dt; moved > step+1e-9 {
		t.Errorf("one tick turned %s by %.4f radians, past the %.4f a rate of %.1f allows", r.name, moved, step, ResidentTurnRate)
	}
}

// Never moves — the AC that is a whole clause of the design rather than a detail. There is
// no integrator that could move one, and this is what says so out loud.
func TestAResidentNeverMoves(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	r := h.standResidentAt(vnet.ResidentRoleGuard, [3]float64{0.5, 64, 0.5}, 0)
	was := r.pos

	h.join(1, [3]float32{1.5, 64, 0.5})
	h.advance(int(DefaultTickRate) * 3)

	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	if h.sim.residents[r.entityID].pos != was {
		t.Errorf("%s walked from %v to %v", r.name, was, h.sim.residents[r.entityID].pos)
	}
}

// turnToward takes the short way round, which is the wrap and not the subtraction.
func TestTurnTowardTakesTheShortWayRound(t *testing.T) {
	t.Parallel()

	for _, c := range []struct {
		name                 string
		from, to, step, want float64
	}{
		// The two that would fail on a bare subtraction: 3.1 to -3.1 is a twenty-fifth
		// of a turn across the seam, not very nearly a whole one the other way.
		{"a step across the seam", 3.1, -3.1, 0.05, wrapAngle(3.15)},
		{"a step back across it", -3.1, 3.1, 0.05, wrapAngle(-3.15)},
		{"the seam inside one step, snaps", 3.1, -3.1, 0.1, -3.1},
		{"inside one step, snaps", 0.5, 0.55, 0.1, 0.55},
		{"already there", -1.0, -1.0, 0.1, -1.0},
		{"a step counter-clockwise", 0, 1.0, 0.1, 0.1},
		{"a step clockwise", 0, -1.0, 0.1, -0.1},
	} {
		if got := turnToward(c.from, c.to, c.step); math.Abs(wrapAngle(got-c.want)) > 1e-9 {
			t.Errorf("%s: turnToward(%.4f, %.4f, %.2f) = %.4f, want %.4f", c.name, c.from, c.to, c.step, got, c.want)
		}
	}
}

// The four bearings are the integrator's formula evaluated at the four unit vectors,
// rather than four numbers somebody typed.
func TestTheAnchorBearingsAreTheOnesTheIntegratorWouldCompute(t *testing.T) {
	t.Parallel()

	origin := [3]float64{0, 0, 0}
	for facing, ahead := range map[vnet.Facing][3]float64{
		vnet.FacingNorth: {0, 0, -1},
		vnet.FacingEast:  {1, 0, 0},
		vnet.FacingSouth: {0, 0, 1},
		vnet.FacingWest:  {-1, 0, 0},
	} {
		want := yawFrom(origin, ahead)
		if got := yawOfFacing(facing); math.Abs(wrapAngle(got-want)) > 1e-9 {
			t.Errorf("%s is yaw %.4f, want %.4f", facing, got, want)
		}
	}
}

// ---------------------------------------------------------------------------
// Nothing can touch them
// ---------------------------------------------------------------------------

// A swing that would have killed a draugr at the same spot does nothing at all, because
// the scan it runs never sees a collection a resident is in.
func TestASwingAtAResidentHitsNothing(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	// Yaw 0 looks along -Z, so this is the position combat_test.go's landing swing uses.
	r := h.standResidentAt(vnet.ResidentRoleSmith, [3]float64{0.5, 64, -1.5}, 0)

	if err := h.swing(player, 0, 1); err != nil {
		t.Fatalf("the swing was refused: %v", err)
	}
	h.step()

	h.sim.mu.Lock()
	still, standing := h.sim.residents[r.entityID]
	mobs, corpses := len(h.sim.mobs), len(h.sim.corpses)
	h.sim.mu.Unlock()

	if !standing {
		t.Fatal("a swing removed a resident from the world")
	}
	if still.pos != r.pos {
		t.Errorf("a swing moved %s to %v", still.name, still.pos)
	}
	if mobs != 0 || corpses != 0 {
		t.Errorf("a swing at a resident produced %d creatures and %d corpses", mobs, corpses)
	}
	for _, frame := range out.all() {
		if kind := vnet.GetRootAsEnvelope(frame, 0).PayloadType(); kind == vnet.PayloadMobHit {
			t.Error("a swing at a resident reported a hit")
		}
	}
	if states := newestSnapshotMobs(t, out); len(states) != 1 || states[0].Health != residentHealth {
		t.Errorf("the snapshot carries %+v, want one resident at full health", states)
	}
}

// ---------------------------------------------------------------------------
// What a session is told
// ---------------------------------------------------------------------------

// residentAppearances is every resident description this session was sent.
func (s *dropSink) residentAppearances(t *testing.T) []protocol.ResidentAppearance {
	t.Helper()

	var sent []protocol.ResidentAppearance
	for _, frame := range s.all() {
		env := vnet.GetRootAsEnvelope(frame, 0)
		if env.PayloadType() != vnet.PayloadResidentAppearance {
			continue
		}
		var table flatbuffers.Table
		if !env.Payload(&table) {
			t.Fatal("the resident appearance payload is absent")
		}
		var payload vnet.ResidentAppearance
		payload.Init(table.Bytes, table.Pos)

		one := protocol.ResidentAppearance{EntityID: payload.EntityId(), Role: payload.Role()}
		if name := payload.Name(); name != nil {
			one.HasName, one.Name = true, string(name)
		}
		if worn := payload.Appearance(nil); worn != nil {
			one.HasAppearance = true
			one.Appearance = protocol.Appearance{
				SkinColor:     worn.SkinColor(),
				ShirtColor:    worn.ShirtColor(),
				TrousersColor: worn.TrousersColor(),
				ShoesColor:    worn.ShoesColor(),
				HairModel:     worn.HairModel(),
				HairColor:     worn.HairColor(),
			}
		}
		sent = append(sent, one)
	}
	return sent
}

// A resident travels in the MobState vector like a creature, and every field of that row
// is a constant: Villager, Idle, full health, nobody targeted.
func TestAResidentIsInTheSnapshotAsAnIdleVillager(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	_, out := h.join(1, [3]float32{0.5, 64, 0.5})
	r := h.standResidentAt(vnet.ResidentRoleCarpenter, [3]float64{2.5, 64, 0.5}, 0)

	h.step()

	states := newestSnapshotMobs(t, out)
	if len(states) != 1 {
		t.Fatalf("the snapshot carries %d mob rows, want the one resident", len(states))
	}
	got := states[0]
	if got.EntityID != r.entityID {
		t.Errorf("the row names entity %d, want %d", got.EntityID, r.entityID)
	}
	if got.Kind != vnet.MobKindVillager {
		t.Errorf("a resident is drawn as %s, want Villager", got.Kind)
	}
	if got.Action != vnet.MobActionIdle {
		t.Errorf("a resident is %s, want Idle", got.Action)
	}
	if got.Health != got.MaxHealth || got.Health == 0 {
		t.Errorf("a resident has %d of %d health, want a full bar", got.Health, got.MaxHealth)
	}
	if got.TargetEntityID != 0 {
		t.Errorf("a resident is hunting entity %d", got.TargetEntityID)
	}
	if want := toWire(r.pos); got.Pos != want {
		t.Errorf("the row puts %s at %v, want %v", r.name, got.Pos, want)
	}
}

// The description is sent once per time the entity is in view, exactly as a player's is:
// a name and a role are not part of a snapshot, and a frame per tick per villager is what
// the once-per-view bookkeeping exists to prevent.
func TestAResidentIsDescribedOnceWhenItComesIntoView(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	_, out := h.join(1, [3]float32{0.5, 64, 0.5})
	r := h.standResidentAt(vnet.ResidentRoleTrader, [3]float64{2.5, 64, 0.5}, 0)

	h.advance(10)

	sent := out.residentAppearances(t)
	if len(sent) != 1 {
		t.Fatalf("ten ticks described %s %d times, want 1", r.name, len(sent))
	}
	if sent[0].EntityID != r.entityID {
		t.Errorf("the description names entity %d, want %d", sent[0].EntityID, r.entityID)
	}
	if !sent[0].HasName || sent[0].Name != r.name {
		t.Errorf("the description carries name %q, want %q", sent[0].Name, r.name)
	}
	if sent[0].Role != r.role {
		t.Errorf("the description carries role %s, want %s", sent[0].Role, r.role)
	}
	if !sent[0].HasAppearance || sent[0].Appearance != r.appearance {
		t.Errorf("the description carries %+v, want %+v", sent[0].Appearance, r.appearance)
	}
}

// A resident on a chunk this session has never been sent is somebody standing on terrain
// the client does not hold — so no row, and no description either.
func TestAResidentOutsideTheViewCubeIsNeitherSentNorDescribed(t *testing.T) {
	t.Parallel()

	h := newVitalsHarnessAt(t, DefaultTickRate, dropTerrain{groundTop: 63}, 1)
	_, out := h.join(1, [3]float32{0.5, 64, 0.5})
	h.standResidentAt(vnet.ResidentRoleGuard, [3]float64{600.5, 64, 0.5}, 0)

	h.advance(3)

	if states := newestSnapshotMobs(t, out); len(states) != 0 {
		t.Errorf("the snapshot carries %d rows for somebody outside the cube", len(states))
	}
	if sent := out.residentAppearances(t); len(sent) != 0 {
		t.Errorf("%d descriptions were sent for somebody outside the cube", len(sent))
	}
}

// ---------------------------------------------------------------------------
// Nothing opens
// ---------------------------------------------------------------------------

// **A trade opens a stall; everything else is refused, and every refusal is the same
// frame.** Four causes arrive at that one answer — a role that keeps no stall, a role
// that does but stands too far away, an id that names nothing at all, and the player's
// own — and they are four sentences for an operator and one code for the player. The
// uniformity is the fail-closed default: a probe must learn nothing about the world it
// could not already see.
//
// **The four are one table because the property is that they are indistinguishable**,
// and a property asserted in four places is one that can drift apart in three of them.
// [TestAddressingSomebodyOutOfReachIsRefusedTheSameWay] still owns the question of
// whether reach is *measured* correctly — body to body against [EditReach] — and the row
// here overlaps it deliberately, because the distance case has to be inside the
// comparison or the comparison is not being made.
//
// Each case also asserts that no stall is open afterwards. Before #459 there was nothing
// to open and the returned error was the whole of it; now a refusal that left a session
// behind would be a window a client never asked for.
func TestOnlyATradeOpensAndEveryOtherAddressIsRefusedTheSameWay(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	// A separate column per resident, because the id is a function of the column.
	villager := h.standResidentAt(vnet.ResidentRoleVillager, [3]float64{0.5, 64, 1.5}, 0)
	guard := h.standResidentAt(vnet.ResidentRoleGuard, [3]float64{1.5, 64, 1.5}, 0)
	// A role that does keep a stall, stood past the reach: the one refusal here whose
	// cause is the distance rather than the person.
	far := h.standResidentAt(vnet.ResidentRoleSmith, [3]float64{2.5, 64, EditReach + 4.5}, 0)

	for index, one := range []struct {
		what string
		id   uint64
	}{
		{"a villager", villager.entityID},
		{"a guard", guard.entityID},
		{"a smith past the reach", far.entityID},
		{"an id nobody holds", 0xDEAD_BEEF},
		{"the player's own id", player.entityID},
	} {
		reason, err := player.InteractNPC(protocol.NpcInteractRequest{EntityID: one.id, ClientTick: uint32(index) + 1})
		if err == nil {
			t.Errorf("addressing %s opened something", one.what)
		}
		if reason != vnet.RefusalReasonNotAVendor {
			t.Errorf("addressing %s is refused %s, want NotAVendor — a probe must learn nothing", one.what, reason)
		}
		if open := h.openStall(player); open != 0 {
			t.Errorf("addressing %s left stall %d open", one.what, open)
		}
	}
}

// Reach is measured body to body against [EditReach], the same distance every other
// interaction in this package uses — and the refusal is still NotAVendor, so a client
// cannot map the answers to a rangefinder.
func TestAddressingSomebodyOutOfReachIsRefusedTheSameWay(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	far := h.standResidentAt(vnet.ResidentRoleTrader, [3]float64{0.5, 64, EditReach + 4.5}, 0)

	reason, err := player.InteractNPC(protocol.NpcInteractRequest{EntityID: far.entityID, ClientTick: 1})
	if err == nil {
		t.Fatal("a resident across the village answered")
	}
	if reason != vnet.RefusalReasonNotAVendor {
		t.Errorf("somebody out of reach is refused %s, want NotAVendor", reason)
	}
}

// The vendor roles are named in one place, so #459 changes what happens on a true rather
// than having to rediscover which roles it applies to.
func TestOnlyTheTradesCouldEverKeepAStall(t *testing.T) {
	t.Parallel()

	for role, could := range map[vnet.ResidentRole]bool{
		vnet.ResidentRoleUnknown:      false,
		vnet.ResidentRoleVillager:     false,
		vnet.ResidentRoleGuard:        false,
		vnet.ResidentRoleSmith:        true,
		vnet.ResidentRoleCarpenter:    true,
		vnet.ResidentRoleCook:         true,
		vnet.ResidentRoleTrader:       true,
		vnet.ResidentRoleStablemaster: true,
	} {
		if got := vendorRole(role); got != could {
			t.Errorf("vendorRole(%s) = %v, want %v", role, got, could)
		}
	}
}
