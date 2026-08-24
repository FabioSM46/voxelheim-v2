package game

import (
	"testing"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// The V3 invariants schemas/player.fbs documents, executed against the frames the
// simulation actually emits.
//
// The encoders in internal/protocol deliberately do not validate what they are handed —
// "the values are not re-validated, they come from the simulation, which is the only
// thing that produces them", stated there three times over — and that stays true. An
// outgoing check would cost every session a validation every tick, forever, to catch a
// programming error; and the tick has nowhere useful to put the failure, since dropping
// a snapshot is worse than sending one.
//
// What the convention leaves open is narrower and real: **nothing on this side executes
// the invariants the client enforces.** The schema documents them in prose and
// client/src/net/codec.rs refuses frames that break them, so a simulation that emitted a
// zero max_health or a misaligned durability vector would surface as a client that
// disconnects, not as a red test. These are that missing statement, placed where the
// values are produced rather than where they are laid out — which is also where the
// issues that give health and durability real behaviour will change them.

// newestSnapshot is the last EntitySnapshot this session was sent.
func newestSnapshot(t *testing.T, out *dropSink) *vnet.EntitySnapshot {
	t.Helper()

	var newest *vnet.EntitySnapshot
	for _, frame := range out.all() {
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		if envelope.PayloadType() != vnet.PayloadEntitySnapshot {
			continue
		}
		var payload flatbuffers.Table
		if !envelope.Payload(&payload) {
			t.Fatal("EntitySnapshot envelope has no payload")
		}
		snapshot := new(vnet.EntitySnapshot)
		snapshot.Init(payload.Bytes, payload.Pos)
		newest = snapshot
	}
	if newest == nil {
		t.Fatal("the session received no snapshot at all")
	}
	return newest
}

// checkVitals asserts every invariant schemas/player.fbs attaches to PlayerVitals, in
// the order the client's decoder checks them.
func checkVitals(t *testing.T, vitals *vnet.PlayerVitals) {
	t.Helper()

	if vitals == nil {
		t.Fatal("the snapshot carries no self_vitals")
	}

	switch vitals.LifeState() {
	case vnet.LifeStateAlive, vnet.LifeStateDead:
	default:
		t.Errorf("life_state = %s, want a known non-zero member", vitals.LifeState())
	}

	health, maxHealth := vitals.Health(), vitals.MaxHealth()
	if maxHealth == 0 {
		t.Error("max_health is zero, which is the division the client's health bar performs")
	}
	if health > maxHealth {
		t.Errorf("health %d exceeds max_health %d", health, maxHealth)
	}
	if vitals.LifeState() == vnet.LifeStateAlive && health == 0 {
		t.Error("an alive player has no health left")
	}
	if ticks := vitals.RespawnTicks(); ticks != 0 && vitals.LifeState() != vnet.LifeStateDead {
		t.Errorf("respawn_ticks = %d for a player who is not dead", ticks)
	}

	hunger, maxHunger := vitals.Hunger(), vitals.MaxHunger()
	if maxHunger == 0 {
		t.Error("max_hunger is zero, which is the division every hunger display performs")
	}
	if hunger > maxHunger {
		t.Errorf("hunger %d exceeds max_hunger %d", hunger, maxHunger)
	}

	level, experience, experienceToNext := vitals.Level(), vitals.Experience(), vitals.ExperienceToNext()
	if level == 0 {
		t.Error("level is zero, which is the absent value rather than a progression level")
	}
	if experienceToNext == 0 {
		t.Error("experience_to_next is zero, which is the division every experience display performs")
	}
	if experience > experienceToNext {
		t.Errorf("experience %d exceeds experience_to_next %d", experience, experienceToNext)
	}
}

// The vitals every snapshot carries are a value the client refuses to decode when it is
// wrong, so this is the test that fails instead of the connection.
func TestTheVitalsTheTickEmitsSatisfyTheContract(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	_, out := h.join(1, [3]float32{0.5, 64, 0.5})
	h.step()

	checkVitals(t, newestSnapshot(t, out).SelfVitals(nil))
}

// The same invariants on the other side of the state machine, which is the half a test
// written before death existed could not reach: a dead player's vitals are the shape the
// client's decoder has the most rules about — zero health, a countdown running, and a
// life state that must not read as Unknown.
func TestTheVitalsOfADeadPlayerSatisfyTheContract(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})

	h.sim.mu.Lock()
	player.damageLocked(PlayerMaxHealth)
	h.sim.mu.Unlock()
	h.step()

	vitals := newestSnapshot(t, out).SelfVitals(nil)
	checkVitals(t, vitals)

	// And the values are the ones death actually produces, so this cannot pass by
	// reporting a living player.
	if vitals == nil {
		t.Fatal("the snapshot carries no self_vitals")
	}
	if vitals.LifeState() != vnet.LifeStateDead {
		t.Errorf("life_state = %s, want Dead", vitals.LifeState())
	}
	if vitals.Health() != 0 {
		t.Errorf("health = %d, want 0", vitals.Health())
	}
	if vitals.RespawnTicks() == 0 {
		t.Error("a dead player carries no respawn countdown")
	}
}

// Mobs are the other half of the V3 snapshot, and nothing creates one yet. Asserting the
// vector is empty rather than skipping it is what makes this test start covering mob
// invariants the moment something does.
func TestEveryMobTheTickEmitsSatisfiesTheContract(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	_, out := h.join(1, [3]float32{0.5, 64, 0.5})
	h.step()

	snapshot := newestSnapshot(t, out)
	for i := range snapshot.MobsLength() {
		var mob vnet.MobState
		if !snapshot.Mobs(&mob, i) {
			t.Fatalf("mob %d is missing from a snapshot that claims to hold it", i)
		}
		if mob.EntityId() == 0 {
			t.Errorf("mob %d carries the reserved entity id 0", i)
		}
		if mob.Pos(nil) == nil || mob.Vel(nil) == nil {
			t.Errorf("mob %d carries no position or no velocity", i)
		}
		if mob.Kind() == vnet.MobKindUnknown {
			t.Errorf("mob %d has an unknown kind", i)
		}
		if mob.Action() == vnet.MobActionUnknown {
			t.Errorf("mob %d has an unknown action", i)
		}
		if mob.MaxHealth() == 0 || mob.Health() > mob.MaxHealth() {
			t.Errorf("mob %d is %d/%d, want a non-zero maximum and no more health than it",
				i, mob.Health(), mob.MaxHealth())
		}
	}
}

// Structures are the V4 addition to the snapshot, and the invariants schemas/player.fbs
// attaches to them are the ones a client's decoder refuses a frame over.
//
// Written against the invariant rather than against today's contents, exactly as the
// durability test below is: the vector is asserted even when it is empty, so this starts
// covering a third kind the moment one exists.
func TestEveryStructureTheTickEmitsSatisfiesTheContract(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	h.plantTent(player, [3]int32{0, 63, 0})
	h.step()

	snapshot := newestSnapshot(t, out)
	if snapshot.StructuresLength() == 0 {
		t.Fatal("the snapshot carries no structures, so this test asserts nothing")
	}
	for i := range snapshot.StructuresLength() {
		var held vnet.StructureState
		if !snapshot.Structures(&held, i) {
			t.Fatalf("structure %d is missing from a snapshot that claims to hold it", i)
		}
		if held.StructureId() == 0 {
			t.Errorf("structure %d carries the reserved id 0", i)
		}
		if held.OwnerEntityId() == 0 {
			t.Errorf("structure %d has no owner, and every structure was placed by somebody", i)
		}
		if held.Anchor(nil) == nil {
			t.Errorf("structure %d carries no anchor, and the origin is a real location", i)
		}
		if held.Kind() == vnet.StructureKindUnknown {
			t.Errorf("structure %d has an unknown kind", i)
		}
		if held.Facing() == vnet.FacingUnknown {
			t.Errorf("structure %d faces no direction", i)
		}
	}
}

// One id names one thing. The counter that mints them is shared, so this is a test of
// that sharing rather than of arithmetic — and it is the invariant a client reads as "an
// entity changed kind" when it breaks.
func TestNoIdNamesTwoThingsInOneSnapshot(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	h.plantTent(player, [3]int32{0, 63, 0})
	if _, ok := h.sim.spawnDrop(ItemStone, 1, [3]int64{1, 64, 1}); !ok {
		t.Fatal("the drop this test needs was refused")
	}
	h.sim.mu.Lock()
	if _, made := h.sim.spawnMobLocked(vnet.MobKindDraugr, [3]float64{2.5, 64, 2.5}); !made {
		h.sim.mu.Unlock()
		t.Fatal("the draugr this test needs was refused")
	}
	h.sim.mu.Unlock()
	h.step()

	snapshot := newestSnapshot(t, out)
	seen := make(map[uint64]string)
	claim := func(id uint64, kind string) {
		t.Helper()
		if previous, taken := seen[id]; taken {
			t.Errorf("id %d names both a %s and a %s in one snapshot", id, previous, kind)
			return
		}
		seen[id] = kind
	}

	for i := range snapshot.EntitiesLength() {
		var entity vnet.EntityState
		if snapshot.Entities(&entity, i) {
			claim(entity.EntityId(), "player")
		}
	}
	for i := range snapshot.DropsLength() {
		var drop vnet.ItemDropState
		if snapshot.Drops(&drop, i) {
			claim(drop.EntityId(), "drop")
		}
	}
	for i := range snapshot.MobsLength() {
		var mob vnet.MobState
		if snapshot.Mobs(&mob, i) {
			claim(mob.EntityId(), "mob")
		}
	}
	for i := range snapshot.StructuresLength() {
		var held vnet.StructureState
		if snapshot.Structures(&held, i) {
			claim(held.StructureId(), "structure")
		}
	}

	// Four kinds, so a snapshot that quietly stopped carrying one would not pass this by
	// having nothing to collide.
	if len(seen) < 4 {
		t.Fatalf("the snapshot named %d entities, want a player, a drop, a mob and a structure", len(seen))
	}
}

// The three inventory vectors describe the same slots or they describe nothing, and each
// slot has to be a shape the contract allows. Today every slot holds a resource and every
// durability is zero; the assertion is written against the invariant rather than against
// that fact, so it keeps meaning something once a durable item exists.
func TestTheInventoryTheSimulationEmitsSatisfiesTheDurabilityContract(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	h.spawn(ItemStone, 1, [3]int64{1, 64, 0})
	h.advance(dropPickupDelayTicks + 1)

	if got := heldCount(player.InventoryState(), ItemStone); got != 1 {
		t.Fatalf("the player holds %d Stone, want the 1 that was dropped for them", got)
	}

	var seen int
	for _, frame := range out.all() {
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		if envelope.PayloadType() != vnet.PayloadInventoryState {
			continue
		}
		var payload flatbuffers.Table
		if !envelope.Payload(&payload) {
			t.Fatal("InventoryState envelope has no payload")
		}
		var state vnet.InventoryState
		state.Init(payload.Bytes, payload.Pos)
		seen++

		slots := state.StacksLength() / 2
		if state.StacksLength()%2 != 0 {
			t.Fatalf("stacks holds %d scalars, want complete pairs", state.StacksLength())
		}
		if state.DurabilityLength() != slots || state.MaxDurabilityLength() != slots {
			t.Fatalf("%d slots but %d durability and %d maximum entries",
				slots, state.DurabilityLength(), state.MaxDurabilityLength())
		}

		for slot := range slots {
			itemID, count := state.Stacks(slot*2), state.Stacks(slot*2+1)
			durability, maxDurability := state.Durability(slot), state.MaxDurability(slot)

			if maxDurability == 0 {
				if durability != 0 {
					t.Errorf("slot %d has durability %d with no maximum", slot, durability)
				}
				continue
			}
			if durability > maxDurability {
				t.Errorf("slot %d is %d/%d durability", slot, durability, maxDurability)
			}
			if itemID == 0 || count != 1 {
				t.Errorf("slot %d holds (%d, %d) and is durable; a durable item is one whole item",
					slot, itemID, count)
			}
		}
	}
	if seen == 0 {
		t.Fatal("the session received no inventory state at all")
	}
}
