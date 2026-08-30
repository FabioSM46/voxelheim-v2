package game

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The barrier: nothing that hunts a player spawns on warded ground, and nothing that
// hunts a player survives standing on it.
//
// **Both halves are asked of one predicate**, so most of what is below is written as a
// pair of worlds that differ only by a runestone. A twin that draws the same numbers and
// walks the same creatures is what turns "the ward suppressed this spawn" into an
// assertion rather than a statistic — and it is the only shape that can state the other
// half of the rule, that a world with no ward near it is unchanged down to the position.

// wardColumns raises one runestone directly, so the 3x3 square of columns centred on
// centre is warded, and returns that centre.
//
// A registry write for the reason plantCampfire is one: what this file tests is the
// barrier. Whether a player may raise a stone here — reach, headroom, the item in a slot,
// the cap on how many one player holds — is structure_test.go's subject, and a stone
// written here is exactly what rebuildWardsLocked reads.
func (h *vitalsHarness) wardColumns(centre world.Column, owner identity.PlayerID) world.Column {
	h.t.Helper()
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()

	stone := &structure{
		structureID: h.sim.mintEntityID(),
		kind:        vnet.StructureKindRunestone,
		anchor:      [3]int32{centre.CX * world.ChunkSize, 63, centre.CZ * world.ChunkSize},
		facing:      vnet.FacingNorth,
		owner:       owner,
		chunk:       world.Coord{X: centre.CX, Y: 1, Z: centre.CZ},
	}
	h.sim.structures[stone.structureID] = stone
	h.sim.rebuildWardsLocked()
	return centre
}

// warded is whether the ground in one column is claimed, asked of the simulation.
func (h *vitalsHarness) warded(col world.Column) bool {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	_, claimed := h.sim.wardOf(col)
	return claimed
}

// candidateDraw is what one call to candidateSpotLocked answered. The refusals are kept
// because the draws have to line up: the generator advances once per call whatever the
// verdict, so index i in two worlds is the same pair of random numbers.
type candidateDraw struct {
	pos [3]float64
	ok  bool
}

func (h *vitalsHarness) candidateDraws(p *Player, kind vnet.MobKind, n int) []candidateDraw {
	h.t.Helper()
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()

	draws := make([]candidateDraw, n)
	for i := range draws {
		pos, ok := h.sim.candidateSpotLocked(p, kind)
		draws[i] = candidateDraw{pos: pos, ok: ok}
	}
	return draws
}

// wardedTwins is the same world twice, one of them holding a runestone over centre, each
// with one player at the origin. Same seed, same terrain, same player: the only thing
// that differs is the claim.
func wardedTwins(t *testing.T, centre world.Column) (open, claimed *vitalsHarness, openPlayer, claimedPlayer *Player) {
	t.Helper()

	open = newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63})
	claimed = newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63})
	open.keepNight()
	claimed.keepNight()
	openPlayer, _ = open.join(1, [3]float32{0.5, 64, 0.5})
	claimedPlayer, _ = claimed.join(1, [3]float32{0.5, 64, 0.5})
	claimed.wardColumns(centre, identity.PlayerID{7})
	return open, claimed, openPlayer, claimedPlayer
}

// ---------------------------------------------------------------------------
// Nothing hostile arrives on warded ground
// ---------------------------------------------------------------------------

// The director refuses exactly the warded columns and no others.
//
// Both halves of the claim in one assertion, because they are one rule read from two
// sides: every spot the open world accepted in an unwarded column is accepted at the same
// position in the claimed one, and every spot it accepted inside the claim is refused
// there. A suppression that reached one column too far would fail the first; one that
// reached nowhere would fail the second.
func TestTheDirectorRefusesExactlyTheWardedColumns(t *testing.T) {
	t.Parallel()

	open, claimed, openPlayer, claimedPlayer := wardedTwins(t, world.Column{CX: 1, CZ: 0})

	const draws = 400
	openDraws := open.candidateDraws(openPlayer, vnet.MobKindDraugr, draws)
	claimedDraws := claimed.candidateDraws(claimedPlayer, vnet.MobKindDraugr, draws)

	var barred, unchanged int
	for i := range openDraws {
		was, now := openDraws[i], claimedDraws[i]
		if !was.ok {
			if now.ok {
				t.Fatalf("draw %d: the ward turned a refusal into the spot %v", i, now.pos)
			}
			continue
		}
		if claimed.warded(chunkAt(was.pos).Column()) {
			if now.ok {
				t.Errorf("draw %d: %v stands in a warded column and was accepted", i, now.pos)
			}
			barred++
			continue
		}
		if !now.ok || now.pos != was.pos {
			t.Errorf("draw %d: the unwarded spot %v became %v (accepted=%v)", i, was.pos, now.pos, now.ok)
		}
		unchanged++
	}

	if barred == 0 {
		t.Fatal("no draw ever landed inside the ward, so nothing here was tested")
	}
	if unchanged == 0 {
		t.Fatal("no draw landed outside the ward, so the suppression could be world-wide and pass")
	}
}

// A passive species is not what the barrier is about, and the director treats a claimed
// world exactly like an open one when asked for somewhere to put a deer.
func TestAWardRefusesNoSpotToAPassiveSpecies(t *testing.T) {
	t.Parallel()

	centre := world.Column{CX: 1, CZ: 0}
	open, claimed, openPlayer, claimedPlayer := wardedTwins(t, centre)

	const draws = 400
	openDraws := open.candidateDraws(openPlayer, vnet.MobKindDeer, draws)
	claimedDraws := claimed.candidateDraws(claimedPlayer, vnet.MobKindDeer, draws)

	var inside int
	for i := range openDraws {
		if openDraws[i] != claimedDraws[i] {
			t.Fatalf("draw %d: %+v in the open world, %+v in the warded one", i, openDraws[i], claimedDraws[i])
		}
		if openDraws[i].ok && claimed.warded(chunkAt(openDraws[i].pos).Column()) {
			inside++
		}
	}
	if inside == 0 {
		t.Fatal("no deer spot ever landed inside the ward, so the exemption was never exercised")
	}
}

// ---------------------------------------------------------------------------
// Nothing hostile survives standing on it
// ---------------------------------------------------------------------------

// A hostile creature that is in a warded column when a tick starts is gone when it ends,
// and it left no body behind.
func TestAHostileCreatureInAWardIsGoneOnTheNextTick(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.keepNight()
	h.join(1, [3]float32{0.5, 64, 0.5})
	h.wardColumns(world.Column{CX: 0, CZ: 0}, identity.PlayerID{7})

	id := h.spawnDraugrAt([3]float32{8.5, 64, 8.5})
	if _, live := h.mobState(id); !live {
		t.Fatal("the draugr was not placed")
	}

	h.step()

	if _, live := h.mobState(id); live {
		t.Error("the draugr is still standing in a warded column after a tick")
	}
	h.sim.mu.Lock()
	body := h.sim.corpses[id]
	h.sim.mu.Unlock()
	if body != nil {
		t.Error("a ward removal left a corpse, so nobody killed it and somebody may loot it")
	}
}

// The other order, which a player can actually cause: the creature is standing there and
// the stone goes up around it.
func TestARunestoneRaisedAroundACreatureRemovesIt(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.keepNight()
	h.join(1, [3]float32{0.5, 64, 0.5})

	id := h.spawnDraugrAt([3]float32{8.5, 64, 8.5})
	h.advance(3)
	if _, live := h.mobState(id); !live {
		t.Fatal("the draugr left an unwarded world before the stone went up")
	}

	h.wardColumns(world.Column{CX: 0, CZ: 0}, identity.PlayerID{7})
	h.step()

	if _, live := h.mobState(id); live {
		t.Error("the draugr survived the runestone raised around it")
	}
}

// Nobody killed it, so nobody is paid for it — and the claims it was carrying leave with
// it rather than outliving it.
func TestAWardRemovalPaysNoLootAndNoExperience(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.keepNight()
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	// Yaw 0 looks along -Z, so this draugr is directly ahead and inside reach.
	id := h.spawnDraugrAt([3]float32{0.5, 64, -1.5})

	// A real blow through the authoritative path, so the tap is the one combat sets.
	if err := h.swing(player, 0, 1); err != nil {
		t.Fatalf("the swing was refused: %v", err)
	}
	h.step()

	wounded := h.mob(id)
	if wounded == nil {
		t.Fatal("one blow of the starter blade killed the draugr outright")
	}
	if wounded.firstHit == nil {
		t.Fatal("the blow left no tap, so this test cannot say a ward discards one")
	}
	before := experienceOf(player)

	h.wardColumns(world.Column{CX: 0, CZ: 0}, identity.PlayerID{7})
	h.step()

	if _, live := h.mobState(id); live {
		t.Fatal("the wounded draugr survived the ward")
	}
	if wounded.firstHit != nil || wounded.encounter != nil {
		t.Error("the ward removal left an earned claim on the creature it took away")
	}
	h.sim.mu.Lock()
	body, drops := h.sim.corpses[id], len(h.sim.drops)
	h.sim.mu.Unlock()
	if body != nil {
		t.Error("a ward removal left a lootable corpse for the player who had wounded it")
	}
	if drops != 0 {
		t.Errorf("a ward removal put %d items on the ground", drops)
	}
	if got := experienceOf(player); got != before {
		t.Errorf("experience went from %d to %d across a ward removal", before, got)
	}
}

// A deer may walk into a village and live, for as long as it likes.
func TestAPassiveCreatureLivesInsideAWard(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.keepNight()
	h.join(1, [3]float32{0.5, 64, 0.5})
	h.wardColumns(world.Column{CX: 0, CZ: 0}, identity.PlayerID{7})

	id := h.spawnMobAt(vnet.MobKindDeer, [3]float32{12.5, 64, 12.5})
	h.advance(200)

	m, live := h.mobState(id)
	if !live {
		t.Fatal("the deer was removed from warded ground")
	}
	if !h.warded(chunkAt(m.pos).Column()) {
		t.Fatalf("the deer walked out of the ward to %v, so its survival proves nothing", m.pos)
	}
}

// ---------------------------------------------------------------------------
// The boundary
// ---------------------------------------------------------------------------

// A draugr that chases a player across the boundary dies at it, and the player it was
// chasing is never touched.
func TestACreatureChasingAPlayerDiesAtTheBoundary(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.keepNight()
	// The claim covers columns -4..-2, so its edge on x is the boundary at -32: the
	// player stands well inside it and the draugr starts inside its aggro range and out.
	h.wardColumns(world.Column{CX: -3, CZ: 0}, identity.PlayerID{7})
	player, _ := h.join(1, [3]float32{-40.5, 64, 0.5})
	full := h.vitals(player).Health

	id := h.spawnDraugrAt([3]float32{-28.5, 64, 0.5})
	h.step()
	if got := h.mob(id); got == nil || got.target != player.entityID {
		t.Fatal("the draugr never took up the chase, so nothing here crosses a boundary")
	}
	if h.warded(chunkAt(h.mob(id).pos).Column()) {
		t.Fatal("the draugr started on warded ground, so it never crossed anything")
	}

	var crossed bool
	for range 120 {
		h.step()
		if _, live := h.mobState(id); !live {
			crossed = true
			break
		}
	}
	if !crossed {
		t.Fatal("the draugr chased the player for six seconds without dying at the barrier")
	}
	if got := h.vitals(player).Health; got != full {
		t.Errorf("the player inside the ward is on %d health, want the %d they walked in with", got, full)
	}
}

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

// The same world removes the same creatures at the same ward, which is what says the rule
// reads nothing but the simulation's own state.
func TestTheSameWorldRemovesTheSameCreaturesAtAWard(t *testing.T) {
	t.Parallel()

	run := func() map[uint64]mobSighting {
		h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63})
		h.keepNight()
		h.join(1, [3]float32{0.5, 64, 0.5})
		h.wardColumns(world.Column{CX: 1, CZ: 0}, identity.PlayerID{7})
		h.placeMobAt([3]float64{40.5, 64, 0.5})
		h.advance(200)
		return h.mobPositions()
	}

	first, second := run(), run()
	if len(first) != len(second) {
		t.Fatalf("one run left %d creatures and the other %d", len(first), len(second))
	}
	for id, seen := range first {
		other, alive := second[id]
		if !alive {
			t.Errorf("creature %d survived one run and not the other", id)
			continue
		}
		if other != seen {
			t.Errorf("creature %d stood at %+v in one run and %+v in the other", id, seen, other)
		}
	}
}
