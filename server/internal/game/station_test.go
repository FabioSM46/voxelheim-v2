package game

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The capital of the world every test in this package is built over.
//
// **Read from the generator rather than written down**, so nothing here asserts a literal
// position: internal/world decides where a settlement stands, and a hard-coded coordinate
// would keep passing after somebody moved a hut. What is asserted is the relationship — the
// ground under an anchor, the id derived from the column, the facing towards the middle.
func testCapital(t *testing.T) world.Settlement {
	t.Helper()

	capital, found := world.NearestSettlement(testWorldSeed, 0, 0)
	if !found {
		t.Fatal("the world every test here uses has no settlement near spawn")
	}
	return capital
}

// stationAnchors is every forge and campfire slot a settlement offers, with the ground
// voxel each one rests on.
func stationAnchors(t *testing.T, s world.Settlement) map[vnet.StructureKind][3]int64 {
	t.Helper()

	out := make(map[vnet.StructureKind][3]int64)
	for _, slot := range s.Anchors() {
		if kind, station := stationKind(slot.Kind); station {
			out[kind] = [3]int64{slot.X, slot.Y - 1, slot.Z}
		}
	}
	if len(out) == 0 {
		t.Fatalf("the %s offers no station slot at all", s.Kind)
	}
	return out
}

// look tells the simulation a chunk entered somebody's view, which is the only way a
// station is ever created.
func (h *structureHarness) look(coord world.Coord) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	h.sim.materialiseSettlementsLocked(coord)
}

// lookAtVoxel is [structureHarness.look] on the chunk one voxel falls in.
func (h *structureHarness) lookAtVoxel(voxel [3]int64) {
	h.look(world.ChunkOf(voxel[0], voxel[1], voxel[2]))
}

// pitchTent is [structureHarness.plantTent] without its assumption that the tent is all
// that stands.
func (h *structureHarness) pitchTent(p *Player, anchor [3]int32) {
	h.t.Helper()

	h.give(p, 0, ItemTent, 1)
	if _, _, err := p.PlaceStructure(placeRequest(0, anchor, vnet.FacingNorth)); err != nil {
		h.t.Fatalf("planting a tent at %v: %v", anchor, err)
	}
}

// ---------------------------------------------------------------------------
// The forge is there because somebody looked
// ---------------------------------------------------------------------------

// The whole feature in one test: looking at the chunks the capital's smithy and hall stand
// on produces a forge and a fire, each on the ground under its settlement anchor, each
// owned by nobody, each carrying an id the seed decides.
func TestLookingAtACapitalStandsItsForgeAndItsFireUp(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	capital := testCapital(t)
	anchors := stationAnchors(t, capital)
	if len(anchors) != 2 {
		t.Fatalf("the capital offers %d station kinds, want a forge and a campfire", len(anchors))
	}

	for _, ground := range anchors {
		h.lookAtVoxel(ground)
	}

	standing := h.structures()
	if len(standing) != len(anchors) {
		t.Fatalf("%d structures stand after looking at the capital, want %d", len(standing), len(anchors))
	}

	for _, held := range standing {
		ground, expected := anchors[held.kind]
		if !expected {
			t.Fatalf("a %s stands in the capital and no anchor asked for one", held.kind)
		}
		if got := held.anchorVoxel(); got != ground {
			t.Errorf("the %s is anchored at %v, want the voxel under its slot, %v", held.kind, got, ground)
		}
		if !held.worldOwned() {
			t.Errorf("the %s is owned by %s, want nobody", held.kind, held.owner.Short())
		}
		if want := worldStructureID(testWorldSeed, ground[0], ground[2]); held.structureID != want {
			t.Errorf("the %s has id %d, want the id its column derives, %d", held.kind, held.structureID, want)
		}
		if held.structureID&worldOwnedStructureBit == 0 {
			t.Errorf("the %s has id %d, which no minted id can be told apart from", held.kind, held.structureID)
		}
		if !knownFacing(held.facing) {
			t.Errorf("the %s faces %s, which is not a direction the contract allows", held.kind, held.facing)
		}
	}
}

// A chunk entering two views is one forge, and a chunk with no settlement in it is nothing
// at all. The idempotence is the id rather than a flag, which is what makes the hook safe
// on a failed send's retry.
func TestLookingAgainCreatesNothing(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	anchors := stationAnchors(t, testCapital(t))

	for range 3 {
		for _, ground := range anchors {
			h.lookAtVoxel(ground)
		}
	}
	h.look(world.Coord{X: 4000, Y: 2, Z: -4000})

	if standing := h.structures(); len(standing) != len(anchors) {
		t.Fatalf("%d structures stand after looking three times and at open country, want %d",
			len(standing), len(anchors))
	}
}

// Two simulations of one world derive one forge, which is the whole of "a restart puts it
// back": nothing is written down, and the second reaches the same id.
func TestARestartDerivesTheSameStations(t *testing.T) {
	t.Parallel()

	anchors := stationAnchors(t, testCapital(t))
	ids := func() map[uint64]vnet.StructureKind {
		h := newStructureHarness(t)
		for _, ground := range anchors {
			h.lookAtVoxel(ground)
		}
		out := make(map[uint64]vnet.StructureKind)
		for _, held := range h.structures() {
			out[held.structureID] = held.kind
		}
		return out
	}

	before, after := ids(), ids()
	if len(before) != len(anchors) {
		t.Fatalf("the first world stood %d stations up, want %d", len(before), len(anchors))
	}
	for id, kind := range before {
		if after[id] != kind {
			t.Errorf("station %d was a %s and came back as %s", id, kind, after[id])
		}
	}
}

// The derived range and the minted range cannot meet, and **that is what a client's decoder
// rests on**: schemas/player.fbs requires a structure id to be unique against every player,
// drop, mob and structure in one snapshot, and a client that meets a collision ends the
// session rather than dropping the frame.
func TestADerivedIdIsNeverOneTheCounterCanMint(t *testing.T) {
	t.Parallel()

	mint := testEntityIDs()
	for range 1000 {
		if id := mint(); id&worldOwnedStructureBit != 0 {
			t.Fatalf("the counter minted %d, which carries the world-owned bit", id)
		}
	}
	for x := int64(-40); x <= 40; x++ {
		id := worldStructureID(testWorldSeed, x*97, x*-89)
		if id&worldOwnedStructureBit == 0 || id == 0 {
			t.Fatalf("the derived id %d does not carry the world-owned bit", id)
		}
	}
}

// Every station faces the middle of the place it stands in; North is the answer only when
// there is no direction to face.
func TestAStationFacesTheMiddleOfItsSettlement(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name string
		x, z int64
		want vnet.Facing
	}{
		{"west of it", -10, 0, vnet.FacingEast},
		{"east of it", 10, 0, vnet.FacingWest},
		{"north of it", 0, -10, vnet.FacingSouth},
		{"south of it", 0, 10, vnet.FacingNorth},
		{"diagonal, further along x", -10, -4, vnet.FacingEast},
		{"diagonal, further along z", -4, -10, vnet.FacingSouth},
		{"in the middle", 0, 0, vnet.FacingNorth},
	}
	for _, c := range cases {
		if got := facingTowards(c.x, c.z, 0, 0); got != c.want {
			t.Errorf("%s: facing %s, want %s", c.name, got, c.want)
		}
	}
}

// ---------------------------------------------------------------------------
// What the owner field decides, now that one structure has none
// ---------------------------------------------------------------------------

// Nobody can take a village forge home, and the refusal is the one the registry already
// made: a player is not its owner. No live player is ever the zero identity, so the
// existing comparison answers correctly with nothing added to it.
func TestAWorldOwnedStationIsNobodysToRemove(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	anchors := stationAnchors(t, testCapital(t))
	forge, offered := anchors[vnet.StructureKindForge]
	if !offered {
		t.Fatal("the capital offers no forge slot")
	}
	h.lookAtVoxel(forge)

	// Standing on the anvil, so distance is not what refuses this.
	player, _ := h.join(1, [3]float32{float32(forge[0]) + 0.5, float32(forge[1]) + 1, float32(forge[2]) + 0.5})
	station := h.only()

	if err := player.RemoveStructure(protocol.RemoveStructureRequest{StructureID: station.structureID}); err == nil {
		t.Fatal("a player took the village forge down")
	}
	if standing := h.structures(); len(standing) != 1 {
		t.Fatalf("%d structures stand after the refused removal, want the forge still up", len(standing))
	}
}

// A station is nobody's tent, so it is not where anybody wakes up and it does not spend the
// one tent a player is allowed.
func TestAWorldOwnedStationIsNobodysTent(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	anchors := stationAnchors(t, testCapital(t))
	for _, ground := range anchors {
		h.lookAtVoxel(ground)
	}

	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	if held, standing := h.sim.tentOfLocked(player.playerID); standing {
		t.Fatalf("structure %d counts as this player's tent", held.structureID)
	}

	// And the one-tent rule still lets the first real tent go up.
	h.pitchTent(player, [3]int32{0, 63, 0})
	if standing := h.structures(); len(standing) != len(anchors)+1 {
		t.Fatalf("%d structures stand after the tent went up, want %d", len(standing), len(anchors)+1)
	}
}

// A forge is a place, not a possession, and the village's is a place too: this is craft.go's
// existing rule answering a structure nobody owns.
func TestAVillageForgeIsAStationToWorkAt(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	forge, offered := stationAnchors(t, testCapital(t))[vnet.StructureKindForge]
	if !offered {
		t.Fatal("the capital offers no forge slot")
	}
	h.lookAtVoxel(forge)

	beside := [3]float64{float64(forge[0]) + 1, float64(forge[1]) + 1, float64(forge[2])}
	away := [3]float64{float64(forge[0]) + 40, float64(forge[1]) + 1, float64(forge[2])}

	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	if !h.sim.stationWithinLocked(vnet.StructureKindForge, playerBox(beside), ForgeCraftRadius) {
		t.Error("standing beside the village forge is not standing at a forge")
	}
	if h.sim.stationWithinLocked(vnet.StructureKindForge, playerBox(away), ForgeCraftRadius) {
		t.Error("the village forge reaches forty blocks")
	}
}

// Digging the ground out from under a village forge leaves it standing there. **The exploit
// this closes is duplication, not floating furniture**: the seed puts a station back the
// next time its chunk is looked at, so a collapse that dropped a forge item would hand out
// one crafted station per break.
func TestDiggingUnderAWorldOwnedStationBringsNothingDown(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	forge, offered := stationAnchors(t, testCapital(t))[vnet.StructureKindForge]
	if !offered {
		t.Fatal("the capital offers no forge slot")
	}
	h.lookAtVoxel(forge)
	station := h.only()

	cells, _, known := footprintOf(station.kind, station.facing, station.anchorVoxel())
	if !known {
		t.Fatalf("a %s has no footprint", station.kind)
	}
	for _, cell := range cells {
		if collapsed := h.sim.collapseStructuresAt(cell); len(collapsed) != 0 {
			t.Fatalf("breaking %v brought %d world-owned structures down", cell, len(collapsed))
		}
	}
	if standing := h.structures(); len(standing) != 1 {
		t.Fatalf("%d structures stand after the whole footprint was broken, want the forge still up", len(standing))
	}
}

// ---------------------------------------------------------------------------
// What crosses the wire, and what reaches the disk
// ---------------------------------------------------------------------------

// The feature at the level a client sees it: a session standing in the capital is sent its
// forge and its fire in the ordinary structure vector, carrying the owner value V5 already
// reserved.
func TestASnapshotInTheCapitalCarriesItsForgeAndItsFire(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	anchors := stationAnchors(t, testCapital(t))
	for _, ground := range anchors {
		h.lookAtVoxel(ground)
	}

	forge := anchors[vnet.StructureKindForge]
	_, watching := h.join(1, [3]float32{float32(forge[0]) + 0.5, float32(forge[1]) + 1, float32(forge[2]) + 0.5})
	h.step()

	states := snapshotStructures(t, watching)
	if len(states) != len(anchors) {
		t.Fatalf("the snapshot carries %d structures, want %d", len(states), len(anchors))
	}

	seen := make(map[vnet.StructureKind]bool)
	for _, state := range states {
		if state.OwnerEntityId() != 0 {
			t.Errorf("a world-owned station names owner %d, want 0", state.OwnerEntityId())
		}
		if state.StructureId() == 0 {
			t.Error("a world-owned station carries id 0, which the contract forbids")
		}
		if state.Anchor(new(vnet.BlockCoord)) == nil {
			t.Fatal("a structure crossed the wire with no anchor")
		}
		seen[state.Kind()] = true
	}
	for kind := range anchors {
		if !seen[kind] {
			t.Fatalf("the snapshot carries no %s", kind)
		}
	}
}

// Walking into a village writes nothing. The dirty flag decides whether the autosave touches
// the disk at all, so a station setting it would have every player who looks at the capital
// rewrite structures.bin — with a record the next start refuses.
func TestStandingAStationUpDoesNotDirtyTheCamp(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	for _, ground := range stationAnchors(t, testCapital(t)) {
		h.lookAtVoxel(ground)
	}

	if camp, dirty := h.sim.TakeDirtyStructures(); dirty {
		t.Fatalf("looking at the capital marked a camp of %d structures for writing", len(camp))
	}
}
