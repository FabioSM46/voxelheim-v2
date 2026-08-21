package game

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// joinAs is join with the identity stated rather than derived from the entity id.
//
// The whole of this file is about the two coming apart: the harness's join ties them
// together, which is exactly the assumption a reconnect breaks.
func (h *structureHarness) joinAs(entityID uint64, playerID identity.PlayerID, pos [3]float32) (*Player, *dropSink) {
	h.t.Helper()

	out := &dropSink{}
	player, err := h.sim.Join(entityID, playerID, pos, testAppearance(), nil, out.deliver)
	if err != nil {
		h.t.Fatalf("Join as %s: %v", playerID.Short(), err)
	}
	return player, out
}

// ownerOf is the owner_entity_id the newest snapshot carries for the one structure in it.
func ownerOf(t *testing.T, out *dropSink) uint64 {
	t.Helper()

	states := snapshotStructures(t, out)
	if len(states) != 1 {
		t.Fatalf("the snapshot carries %d structures, want exactly 1", len(states))
	}
	return states[0].OwnerEntityId()
}

// ---------------------------------------------------------------------------
// A camp outlives the session that built it
// ---------------------------------------------------------------------------

// The issue at the level a player experiences it: a tent is still theirs after they log
// out and back in, and the wire says so without ever naming an identity.
//
// Three readings of one field, from a *second* player's snapshot each time, because the
// question is what everyone else sees: the owner's live entity id, then 0 while they are
// gone, then the new entity id they came back with. The middle one is the V5 rule, and it
// is the one that used to be impossible — before this change the tent simply stopped
// being anybody's.
func TestAnOwnerLeavingAndReturningIsFollowedByTheWire(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	owner, _ := h.joinAs(1, testPlayerID(1), [3]float32{0.5, 64, 0.5})
	_, watching := h.joinAs(2, testPlayerID(2), [3]float32{0.5, 64, 0.5})

	planted := h.plantTent(owner, [3]int32{0, 63, 0})
	h.step()

	if got := ownerOf(t, watching); got != owner.entityID {
		t.Errorf("while the owner is connected the wire says owner %d, want %d", got, owner.entityID)
	}

	// The owner goes. The tent does not: it is keyed by an identity, and an identity is
	// not something a disconnect takes away.
	h.sim.Leave(owner)
	h.step()

	if standing := h.structures(); len(standing) != 1 || standing[0].structureID != planted.structureID {
		t.Fatalf("the tent did not survive its owner's disconnect: %d structures stand", len(standing))
	}
	if got := ownerOf(t, watching); got != 0 {
		t.Errorf("with the owner offline the wire says owner %d, want 0", got)
	}

	// Back, under a new entity id and the same identity — which is exactly what a
	// reconnect is: session.Registry mints a fresh number, session.Identities resolves
	// the same player behind it.
	const rejoinedAs = 3
	returned, _ := h.joinAs(rejoinedAs, testPlayerID(1), [3]float32{0.5, 64, 0.5})
	if returned.entityID == owner.entityID {
		t.Fatal("the rejoin reused the first entity id, so this test proves nothing")
	}
	h.step()

	if got := ownerOf(t, watching); got != rejoinedAs {
		t.Errorf("after the rejoin the wire says owner %d, want the new entity id %d", got, rejoinedAs)
	}
}

// Zero means "offline", and it has to mean that to *everyone at once* — including the
// owner's own session, which has no special knowledge of a camp it cannot currently claim.
//
// Split from the test above because it pins the other half of the V5 rule: no entity is
// ever numbered 0, so an offline owner matches nobody and no client can read somebody
// else's camp as its own.
func TestNoStructureIsEverAnnouncedAsOwnedByEntityZeroWhileItsOwnerIsConnected(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	owner, mine := h.joinAs(1, testPlayerID(1), [3]float32{0.5, 64, 0.5})

	h.plantTent(owner, [3]int32{0, 63, 0})
	h.step()

	if got := ownerOf(t, mine); got == 0 {
		t.Error("a connected owner's own snapshot calls their tent unowned")
	}
}

// A tent is somewhere to come back to, and that is the sentence the entity id broke: the
// player who pitched it came back with a new number and respawned at the world spawn,
// beside a tent that was no longer theirs to take down.
func TestARejoinedPlayerRespawnsAtTheirOwnTent(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	first, _ := h.joinAs(1, testPlayerID(1), [3]float32{0.5, 64, 0.5})

	anchor := [3]int32{2, 63, 0}
	h.give(first, 0, ItemTent, 1)
	if _, _, err := first.PlaceStructure(placeRequest(0, anchor, vnet.FacingNorth)); err != nil {
		t.Fatalf("PlaceStructure: %v", err)
	}

	h.sim.Leave(first)
	returned, _ := h.joinAs(7, testPlayerID(1), [3]float32{0.5, 64, 0.5})

	// Through the record, which is where respawnPositionLocked is actually consulted: a
	// dead player is captured as their respawn would have left them.
	h.sim.mu.Lock()
	returned.damageLocked(PlayerMaxHealth)
	h.sim.mu.Unlock()

	want := [3]float64{float64(anchor[0]) + 0.5, float64(anchor[1]) + 1, float64(anchor[2]) + 0.5}
	if got := returned.Record().Pos; got != want {
		t.Errorf("a rejoined player comes back at %v, want their tent at %v", got, want)
	}
}

// Ownership refuses the same people before and after the reconnect, which is the half of
// the rule a naive fix breaks in the other direction: keying by identity must not make a
// camp claimable by whoever happens to hold its old entity id.
func TestOnlyTheIdentityThatBuiltACampCanTakeItDown(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	owner, _ := h.joinAs(1, testPlayerID(1), [3]float32{0.5, 64, 0.5})
	other, _ := h.joinAs(2, testPlayerID(2), [3]float32{0.5, 64, 0.5})

	planted := h.plantTent(owner, [3]int32{0, 63, 0})
	remove := protocol.RemoveStructureRequest{StructureID: planted.structureID}

	if err := other.RemoveStructure(remove); err == nil {
		t.Error("another player took down a tent that was not theirs")
	}

	h.sim.Leave(owner)

	// The dangerous case: a new session inheriting the departed owner's entity id. Under
	// the old rule this player *was* the owner as far as the registry could tell.
	inheritor, _ := h.joinAs(owner.entityID, testPlayerID(99), [3]float32{0.5, 64, 0.5})
	if err := inheritor.RemoveStructure(remove); err == nil {
		t.Error("a player who inherited the owner's entity id took their tent down")
	}
	h.sim.Leave(inheritor)

	// And the owner themselves, back under a new number, still can.
	returned, _ := h.joinAs(42, testPlayerID(1), [3]float32{0.5, 64, 0.5})
	if err := returned.RemoveStructure(remove); err != nil {
		t.Errorf("the owner could not take down their own tent after reconnecting: %v", err)
	}
	if standing := h.structures(); len(standing) != 0 {
		t.Errorf("%d structures still stand after the owner removed theirs", len(standing))
	}
}

// One live session to an identity, refused here as well as upstream. The index that
// resolves an owner's entity id holds one entry per identity, and an overwrite would
// point every one of the displaced player's structures at a session that had ended.
func TestOneIdentityCannotBeTwoPlayersAtOnce(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	h.joinAs(1, testPlayerID(1), [3]float32{0.5, 64, 0.5})

	out := &dropSink{}
	if _, err := h.sim.Join(2, testPlayerID(1), [3]float32{0.5, 64, 0.5}, testAppearance(), nil, out.deliver); err == nil {
		t.Fatal("one identity joined twice")
	}
	if h.sim.Count() != 1 {
		t.Errorf("%d players are in the simulation, want 1", h.sim.Count())
	}
}

// ---------------------------------------------------------------------------
// A camp on disk, and back
// ---------------------------------------------------------------------------

// The capture is what the file is written from, so its four fields and its order are what
// a restart depends on.
func TestStructuresCapturesEveryFieldInIdentityOrder(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	owner, _ := h.joinAs(1, testPlayerID(1), [3]float32{0.5, 64, 0.5})

	h.give(owner, 0, ItemTent, 1)
	if _, _, err := owner.PlaceStructure(placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingWest)); err != nil {
		t.Fatalf("planting the tent: %v", err)
	}
	h.give(owner, 1, ItemForge, 1)
	if _, _, err := owner.PlaceStructure(placeRequest(1, [3]int32{2, 63, 0}, vnet.FacingSouth)); err != nil {
		t.Fatalf("planting the forge: %v", err)
	}

	captured := h.sim.Structures()
	if len(captured) != 2 {
		t.Fatalf("captured %d structures, want 2", len(captured))
	}

	standing := h.structures()
	for i, held := range standing {
		want := Structure{Kind: held.kind, Anchor: held.anchor, Facing: held.facing, Owner: held.owner}
		if captured[i] != want {
			t.Errorf("captured structure %d is %+v, want %+v", i, captured[i], want)
		}
	}
	if captured[0].Kind == captured[1].Kind {
		t.Error("both captured structures are the same kind, so the kind assertion proves little")
	}

	// Twice from one unchanged world, because the registry is a map and the store writes
	// what this returns: without the sort, two saves of the same camp would differ by a
	// hash seed and "byte-identical" would be luck.
	again := h.sim.Structures()
	for i := range captured {
		if again[i] != captured[i] {
			t.Errorf("a second capture of an unchanged world differs at %d: %+v, want %+v", i, again[i], captured[i])
		}
	}
}

// The round trip through the two halves this package owns: capture, restore into a fresh
// simulation, capture again. Equal captures mean nothing was lost and nothing invented.
func TestACampRestoresToTheCampItWas(t *testing.T) {
	t.Parallel()

	first := newStructureHarness(t)
	owner, _ := first.joinAs(1, testPlayerID(1), [3]float32{0.5, 64, 0.5})
	first.give(owner, 0, ItemTent, 1)
	if _, _, err := owner.PlaceStructure(placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingEast)); err != nil {
		t.Fatalf("PlaceStructure: %v", err)
	}
	saved := first.sim.Structures()

	// A second simulation, sharing nothing with the first: a restart is not a re-entry.
	second := newStructureHarness(t)
	if err := second.sim.RestoreStructures(saved); err != nil {
		t.Fatalf("RestoreStructures: %v", err)
	}

	got := second.sim.Structures()
	if len(got) != len(saved) {
		t.Fatalf("restored %d structures, want %d", len(got), len(saved))
	}
	for i := range saved {
		if got[i] != saved[i] {
			t.Errorf("structure %d restored as %+v, want %+v", i, got[i], saved[i])
		}
	}
}

// A fire crosses a restart on exactly the terms a tent does, and the file that carries it
// is the file this build already writes.
//
// **The whole of what the campfire added to persistence is a kind byte.** The record layout
// is fixed-width and the kind already occupies one byte of it, so nothing about the format
// changed and [persist.StructuresVersion] stays where it is — pinned in the persist package
// beside the encoder it describes. What is asserted here is the half game owns: a placed
// fire is captured, restored into a simulation that shares nothing with the first, and
// stands there as a campfire with its anchor and its owner intact.
func TestAFireCrossesARestartTheWayATentDoes(t *testing.T) {
	t.Parallel()

	first := newStructureHarness(t)
	owner, _ := first.joinAs(1, testPlayerID(1), [3]float32{0.5, 64, 0.5})
	first.give(owner, 0, ItemCampfire, 1)
	if _, _, err := owner.PlaceStructure(placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingSouth)); err != nil {
		t.Fatalf("planting the fire: %v", err)
	}
	// Two of them, because a camp is allowed several and a restore that quietly kept one
	// would be the tent's rule leaking into a kind that does not have it.
	first.give(owner, 1, ItemCampfire, 1)
	if _, _, err := owner.PlaceStructure(placeRequest(1, [3]int32{2, 63, 0}, vnet.FacingNorth)); err != nil {
		t.Fatalf("planting the second fire: %v", err)
	}
	saved := first.sim.Structures()

	second := newStructureHarness(t)
	if err := second.sim.RestoreStructures(saved); err != nil {
		t.Fatalf("RestoreStructures: %v", err)
	}

	got := second.sim.Structures()
	if len(got) != 2 {
		t.Fatalf("restored %d structures, want the two fires that were lit", len(got))
	}
	for i := range saved {
		if got[i] != saved[i] {
			t.Errorf("structure %d restored as %+v, want %+v", i, got[i], saved[i])
		}
		if got[i].Kind != vnet.StructureKindCampfire {
			t.Errorf("structure %d restored as a %s, want a campfire", i, got[i].Kind)
		}
	}

	// And the restored fire is a fire to the director too, which is the only thing a
	// campfire does: the predicate reads the registry, so a kind that came back wrong
	// would come back invisible to it.
	second.sim.mu.Lock()
	near := second.sim.nearACampfireLocked([3]float64{0.5, 64, 0.5})
	second.sim.mu.Unlock()
	if !near {
		t.Error("a restored fire is not ground the spawn director keeps clear")
	}
}

// **No id on disk, so every id is fresh** — and fresh means "from the counter that names
// everything else", not "the same numbers again". A restored structure that reused an id
// would collide with the next player, drop or draugr the counter hands out, and every
// consumer reads that as one entity changing kind.
func TestRestoredStructuresAreMintedFreshIdsThatCollideWithNothing(t *testing.T) {
	t.Parallel()

	stored := []Structure{
		{Kind: vnet.StructureKindTent, Anchor: [3]int32{0, 63, 0}, Facing: vnet.FacingNorth, Owner: testPlayerID(1)},
		{Kind: vnet.StructureKindForge, Anchor: [3]int32{4, 63, 0}, Facing: vnet.FacingNorth, Owner: testPlayerID(1)},
	}

	h := newStructureHarness(t)
	if err := h.sim.RestoreStructures(stored); err != nil {
		t.Fatalf("RestoreStructures: %v", err)
	}

	seen := map[uint64]struct{}{}
	for _, held := range h.structures() {
		if held.structureID == 0 {
			t.Error("a restored structure holds id 0, which names nothing")
		}
		if _, twice := seen[held.structureID]; twice {
			t.Errorf("id %d names two restored structures", held.structureID)
		}
		seen[held.structureID] = struct{}{}
	}

	// And the counter has moved past them: the next thing the world names cannot be one
	// of the structures that were just restored.
	owner, _ := h.joinAs(1, testPlayerID(1), [3]float32{0.5, 64, 0.5})
	h.give(owner, 0, ItemForge, 1)
	if _, _, err := owner.PlaceStructure(placeRequest(0, [3]int32{-4, 63, 0}, vnet.FacingNorth)); err != nil {
		t.Fatalf("planting after a restore: %v", err)
	}
	for _, held := range h.structures() {
		if _, restored := seen[held.structureID]; !restored {
			continue
		}
		if held.kind == vnet.StructureKindForge && held.anchor == [3]int32{-4, 63, 0} {
			t.Error("a newly placed structure reused a restored id")
		}
	}
}

// Every shape of a stored camp this build will not restore, and the rule is [Life]'s:
// refused whole, never repaired, never partly believed. The simulation is left exactly as
// it was, so the caller can start with no structures and keep the file.
func TestRestoreStructuresRefusesACampWholeOrNotAtAll(t *testing.T) {
	t.Parallel()

	sound := func() []Structure {
		return []Structure{
			{Kind: vnet.StructureKindTent, Anchor: [3]int32{0, 63, 0}, Facing: vnet.FacingNorth, Owner: testPlayerID(1)},
			{Kind: vnet.StructureKindForge, Anchor: [3]int32{4, 63, 0}, Facing: vnet.FacingEast, Owner: testPlayerID(2)},
		}
	}

	// The guard the whole table rests on: if this stopped restoring, every case below
	// would pass for the wrong reason.
	if err := newStructureHarness(t).sim.RestoreStructures(sound()); err != nil {
		t.Fatalf("a sound camp was refused: %v", err)
	}

	damage := map[string]func([]Structure) []Structure{
		"a kind this build cannot place": func(c []Structure) []Structure {
			c[1].Kind = vnet.StructureKindUnknown
			return c
		},
		"a kind nobody has ever defined": func(c []Structure) []Structure {
			c[0].Kind = vnet.StructureKind(200)
			return c
		},
		"a facing that is not a direction": func(c []Structure) []Structure {
			c[0].Facing = vnet.Facing(9)
			return c
		},
		"the absent facing FlatBuffers reads as zero": func(c []Structure) []Structure {
			c[1].Facing = vnet.FacingUnknown
			return c
		},
		"an owner that names nobody": func(c []Structure) []Structure {
			c[0].Owner = identity.PlayerID{}
			return c
		},
		// One tent to a player is a placement rule, so a file with two is not one this
		// server wrote — and restoring it would give somebody two answers to "where do I
		// come back to", which is the choice the rule exists to make.
		"a second tent for one player": func(c []Structure) []Structure {
			c[1] = Structure{Kind: vnet.StructureKindTent, Anchor: [3]int32{8, 63, 0}, Facing: vnet.FacingNorth, Owner: c[0].Owner}
			return c
		},
	}

	for name, break_ := range damage {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			h := newStructureHarness(t)
			if err := h.sim.RestoreStructures(break_(sound())); err == nil {
				t.Fatal("RestoreStructures accepted a camp this build cannot have written")
			}
			if standing := h.structures(); len(standing) != 0 {
				t.Errorf("a refused camp left %d structures standing; it is refused whole", len(standing))
			}
		})
	}
}

// Overlapping footprints are *accepted*, and this test is the reason written down: a
// forge inside its owner's tent is legal to place today, so refusing it on load would
// turn a camp this server built and drew into an unloadable file — the exact data loss
// the structures file exists to prevent.
//
// If structures ever stop being allowed to overlap, the rule belongs in PlaceStructure
// first; this test should fail then, and be changed then.
func TestARestoredCampMayOverlapExactlyAsAPlacedOneMay(t *testing.T) {
	t.Parallel()

	// Built through the authoritative path, so what is restored below is provably a camp
	// this server can produce rather than one the test invented.
	live := newStructureHarness(t)
	owner, _ := live.joinAs(1, testPlayerID(1), [3]float32{0.5, 64, 0.5})
	live.give(owner, 0, ItemTent, 1)
	if _, _, err := owner.PlaceStructure(placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingNorth)); err != nil {
		t.Fatalf("planting the tent: %v", err)
	}
	live.give(owner, 1, ItemForge, 1)
	if _, _, err := owner.PlaceStructure(placeRequest(1, [3]int32{0, 63, 0}, vnet.FacingNorth)); err != nil {
		t.Fatalf("a forge on the tent's own anchor was refused, so the premise of this test has changed: %v", err)
	}

	overlapping := live.sim.Structures()
	if len(overlapping) != 2 {
		t.Fatalf("%d structures stand, want the tent and the forge", len(overlapping))
	}

	restored := newStructureHarness(t)
	if err := restored.sim.RestoreStructures(overlapping); err != nil {
		t.Fatalf("a camp this server placed was refused on load: %v", err)
	}
	if got := restored.sim.StructureCount(); got != 2 {
		t.Errorf("%d structures came back, want 2", got)
	}
}

// A restore is for an empty world, and the refusal is stated rather than assumed:
// restoring into a world that already has a camp would silently drop what was standing.
func TestRestoreStructuresRefusesAWorldThatIsAlreadyBuiltIn(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	owner, _ := h.joinAs(1, testPlayerID(1), [3]float32{0.5, 64, 0.5})
	planted := h.plantTent(owner, [3]int32{0, 63, 0})

	stored := []Structure{{Kind: vnet.StructureKindForge, Anchor: [3]int32{9, 63, 9}, Facing: vnet.FacingNorth, Owner: testPlayerID(2)}}
	if err := h.sim.RestoreStructures(stored); err == nil {
		t.Fatal("a camp was restored into a world that already had one")
	}
	if only := h.only(); only.structureID != planted.structureID {
		t.Error("the refused restore replaced what was standing")
	}
}

// ---------------------------------------------------------------------------
// When the camp gets written
// ---------------------------------------------------------------------------

// The dirty flag is what keeps a world nobody is building in from rewriting a
// byte-identical file every interval for the life of the process — and what makes sure
// every way a camp can change does reach the disk.
func TestEveryChangeToTheCampMarksItForWriting(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	owner, _ := h.joinAs(1, testPlayerID(1), [3]float32{0.5, 64, 0.5})

	// A fresh simulation owes the disk nothing.
	if _, dirty := h.sim.TakeDirtyStructures(); dirty {
		t.Error("an untouched world asked to be written")
	}

	planted := h.plantTent(owner, [3]int32{0, 63, 0})
	camp, dirty := h.sim.TakeDirtyStructures()
	if !dirty {
		t.Fatal("placing a tent did not mark the camp for writing")
	}
	if len(camp) != 1 {
		t.Errorf("the camp to write holds %d structures, want 1", len(camp))
	}
	// Taken means taken: a second pass with nothing new must cost no I/O.
	if _, again := h.sim.TakeDirtyStructures(); again {
		t.Error("the camp was still dirty after being taken")
	}

	if err := owner.RemoveStructure(protocol.RemoveStructureRequest{StructureID: planted.structureID}); err != nil {
		t.Fatalf("RemoveStructure: %v", err)
	}
	if _, dirty := h.sim.TakeDirtyStructures(); !dirty {
		t.Error("removing a structure did not mark the camp for writing")
	}

	// And a collapse, which is the change nobody asked for: the ground under a footprint
	// stopped being solid.
	h.plantTent(owner, [3]int32{0, 63, 0})
	h.sim.TakeDirtyStructures()
	if fallen := h.sim.collapseStructuresAt([3]int64{0, 63, 0}); len(fallen) != 1 {
		t.Fatalf("%d structures collapsed, want 1", len(fallen))
	}
	if _, dirty := h.sim.TakeDirtyStructures(); !dirty {
		t.Error("a collapse did not mark the camp for writing")
	}
}

// A failed write puts the camp back in the queue, which is the contract the caller in
// cmd/voxelheimd keeps and the reason taking is safe at all.
func TestMarkStructuresDirtyPutsAFailedWriteBackInTheQueue(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	owner, _ := h.joinAs(1, testPlayerID(1), [3]float32{0.5, 64, 0.5})
	h.plantTent(owner, [3]int32{0, 63, 0})

	if _, dirty := h.sim.TakeDirtyStructures(); !dirty {
		t.Fatal("the placed tent was not dirty")
	}
	h.sim.MarkStructuresDirty()
	if _, dirty := h.sim.TakeDirtyStructures(); !dirty {
		t.Error("a camp put back in the queue was not offered again")
	}
}

// A restore does **not** mark the camp dirty, and that is load-bearing twice: every
// restart would otherwise rewrite a byte-identical file, and a start that failed for some
// unrelated reason would already have overwritten the camp it could not use.
func TestARestoredCampOwesTheDiskNothing(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	stored := []Structure{{Kind: vnet.StructureKindTent, Anchor: [3]int32{0, 63, 0}, Facing: vnet.FacingNorth, Owner: testPlayerID(1)}}
	if err := h.sim.RestoreStructures(stored); err != nil {
		t.Fatalf("RestoreStructures: %v", err)
	}

	if _, dirty := h.sim.TakeDirtyStructures(); dirty {
		t.Error("a freshly restored camp asked to be written straight back out")
	}
}

// The tent's whole promise, on the far side of a restart: a camp that came back from a
// file is what its owner comes back to.
//
// Split from the restart test in cmd/voxelheimd on purpose — that one proves the bytes
// cross the process boundary and the wire names the reconnected session; this one proves
// what the camp is *for*, which needs a damage path cmd cannot reach without widening
// game's exported API for a single assertion.
func TestAPlayerRespawnsAtATentThatCameBackFromDisk(t *testing.T) {
	t.Parallel()

	anchor := [3]int32{2, 63, 0}
	owner := testPlayerID(1)

	// Captured from a simulation that placed it, so what is restored below is a camp this
	// server produced rather than one the test invented.
	built := newStructureHarness(t)
	placing, _ := built.joinAs(1, owner, [3]float32{0.5, 64, 0.5})
	built.give(placing, 0, ItemTent, 1)
	if _, _, err := placing.PlaceStructure(placeRequest(0, anchor, vnet.FacingNorth)); err != nil {
		t.Fatalf("PlaceStructure: %v", err)
	}
	saved := built.sim.Structures()

	// A different process, in every way a test can express one: a new simulation, a new
	// entity id counter, and the same identity arriving with a new number.
	restarted := newStructureHarness(t)
	if err := restarted.sim.RestoreStructures(saved); err != nil {
		t.Fatalf("RestoreStructures: %v", err)
	}
	returned, _ := restarted.joinAs(500, owner, [3]float32{0.5, 64, 0.5})

	restarted.sim.mu.Lock()
	returned.damageLocked(PlayerMaxHealth)
	restarted.sim.mu.Unlock()

	want := [3]float64{float64(anchor[0]) + 0.5, float64(anchor[1]) + 1, float64(anchor[2]) + 0.5}
	if got := returned.Record().Pos; got != want {
		t.Errorf("after a restart the player comes back at %v, want the restored tent at %v", got, want)
	}
}
