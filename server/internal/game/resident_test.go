package game

import (
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The people the seed put there: where they come from, what names them, what they look
// like, and why nothing that hunts can reach them.
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

// Every word in the anchor vocabulary is claimed by exactly one of the two materialisers.
//
// The guard against the failure that costs nothing to make and is invisible afterwards: a
// seventh resident slot appended to [world.AnchorKind] and forgotten here is a slot that
// silently produces nobody, and the settlement still builds.
func TestEveryAnchorIsEitherAStationOrAPerson(t *testing.T) {
	t.Parallel()

	for kind, name := range map[world.AnchorKind]string{
		world.AnchorNone:      "no anchor",
		world.AnchorForge:     "forge",
		world.AnchorCampfire:  "campfire",
		world.AnchorSmith:     "smith",
		world.AnchorCarpenter: "carpenter",
		world.AnchorCook:      "cook",
		world.AnchorTrader:    "trader",
		world.AnchorVillager:  "villager",
		world.AnchorGuard:     "guard",
	} {
		if got := kind.String(); got != name {
			t.Fatalf("AnchorKind %d is %q, want %q — this table has fallen behind the vocabulary", kind, got, name)
		}
		_, station := stationKind(kind)
		_, lives := residentRole(kind)
		switch {
		case kind == world.AnchorNone:
			if station || lives {
				t.Error("AnchorNone is the uninitialised zero and must furnish nothing")
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

// yawFrom is the heading that points from one position at another, which is the mob
// integrator's formula and therefore the one a bearing has to agree with.
func yawFrom(from, to [3]float64) float64 {
	return wrapAngle(math.Atan2(from[0]-to[0], from[2]-to[2]))
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
