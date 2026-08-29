package game

import (
	"math"
	"testing"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func armMobBlow(t *testing.T, h *vitalsHarness, mobID uint64, target *Player) {
	t.Helper()
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	m := h.sim.mobs[mobID]
	m.action = vnet.MobActionWindup
	m.actionTicks = 1
	m.target = target.entityID
}

func mobHits(t *testing.T, frames [][]byte) []struct {
	id  uint64
	pos [3]float32
} {
	t.Helper()
	var hits []struct {
		id  uint64
		pos [3]float32
	}
	for _, frame := range frames {
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		if envelope.PayloadType() != vnet.PayloadMobHit {
			continue
		}
		var table flatbuffers.Table
		if !envelope.Payload(&table) {
			t.Fatal("MobHit payload is absent")
		}
		var hit vnet.MobHit
		hit.Init(table.Bytes, table.Pos)
		pos := hit.AttackerPos(nil)
		if pos == nil {
			t.Fatal("MobHit attacker position is absent")
		}
		hits = append(hits, struct {
			id  uint64
			pos [3]float32
		}{hit.AttackerEntityId(), [3]float32{pos.X(), pos.Y(), pos.Z()}})
	}
	return hits
}

func TestALandedMobBlowSendsItsAttackerBeforeTheSnapshot(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	mobPos := [3]float32{0.5, 64, -1.0}
	mobID := h.spawnDraugrAt(mobPos)
	armMobBlow(t, h, mobID, player)
	before := len(out.all())

	healthBefore := h.vitals(player).Health
	h.step()
	if got := h.vitals(player).Health; got >= healthBefore {
		t.Fatalf("health after landed blow = %d, want less than %d", got, healthBefore)
	}

	frames := out.all()[before:]
	hits := mobHits(t, frames)
	if len(hits) != 1 {
		t.Fatalf("landed blow sent %d MobHit events, want one", len(hits))
	}
	if hits[0].id != mobID || hits[0].pos != mobPos {
		t.Errorf("MobHit = id %d pos %v, want id %d pos %v", hits[0].id, hits[0].pos, mobID, mobPos)
	}
	for axis, value := range hits[0].pos {
		if math.IsNaN(float64(value)) || math.IsInf(float64(value), 0) {
			t.Errorf("attacker position axis %d is not finite: %v", axis, value)
		}
	}

	firstHit, firstSnapshot := -1, -1
	for index, frame := range frames {
		switch vnet.GetRootAsEnvelope(frame, 0).PayloadType() {
		case vnet.PayloadMobHit:
			if firstHit == -1 {
				firstHit = index
			}
		case vnet.PayloadEntitySnapshot:
			if firstSnapshot == -1 {
				firstSnapshot = index
			}
		}
	}
	if firstHit == -1 || firstSnapshot == -1 || firstHit >= firstSnapshot {
		t.Errorf("frame order has MobHit at %d and snapshot at %d; hit must lead", firstHit, firstSnapshot)
	}
}

func TestAMobHitRetriesWithoutRepeatingTheDamage(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	mobID := h.spawnDraugrAt([3]float32{0.5, 64, -1.0})
	armMobBlow(t, h, mobID, player)
	out.setFull(true)

	healthBefore := h.vitals(player).Health
	h.step()
	healthAfter := h.vitals(player).Health
	if healthAfter >= healthBefore {
		t.Fatalf("full outbound queue prevented authoritative damage: %d -> %d", healthBefore, healthAfter)
	}
	if hits := mobHits(t, out.all()); len(hits) != 0 {
		t.Fatalf("full queue accepted %d MobHit events", len(hits))
	}

	out.setFull(false)
	h.step()
	if got := h.vitals(player).Health; got != healthAfter {
		t.Errorf("retry repeated damage: health = %d, want %d", got, healthAfter)
	}
	if hits := mobHits(t, out.all()); len(hits) != 1 || hits[0].id != mobID {
		t.Fatalf("retried MobHit events = %+v, want one from %d", hits, mobID)
	}
	h.advance(3)
	if hits := mobHits(t, out.all()); len(hits) != 1 {
		t.Errorf("accepted MobHit was sent %d times, want once", len(hits))
	}
}

func TestPendingMobHitsAreBoundedAndKeepTheNewestBlows(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.sim.mu.Lock()
	for id := uint64(1); id <= maxPendingMobHits+2; id++ {
		player.recordMobHitLocked(protocol.MobHit{AttackerEntityID: id})
	}
	count := len(player.pendingMobHits)
	oldest := player.pendingMobHits[0].AttackerEntityID
	newest := player.pendingMobHits[len(player.pendingMobHits)-1].AttackerEntityID
	h.sim.mu.Unlock()

	if count != maxPendingMobHits {
		t.Fatalf("pending MobHit count = %d, want %d", count, maxPendingMobHits)
	}
	if oldest != 3 {
		t.Errorf("oldest pending attacker = %d, want 3", oldest)
	}
	if newest != maxPendingMobHits+2 {
		t.Errorf("newest pending attacker = %d, want %d", newest, maxPendingMobHits+2)
	}
}

func TestNonDamageAndProtectedDamageSendNoMobHit(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	h.sim.mu.Lock()
	player.damageLocked(0)
	player.damageLocked(1) // Direct damage stands for fall/environmental damage.
	player.protectionTicks = 2
	h.sim.mu.Unlock()
	mobID := h.spawnDraugrAt([3]float32{0.5, 64, -1.0})
	armMobBlow(t, h, mobID, player)
	healthBeforeProtectedBlow := h.vitals(player).Health

	h.step()
	if got := h.vitals(player).Health; got != healthBeforeProtectedBlow {
		t.Errorf("protected blow changed health from %d to %d", healthBeforeProtectedBlow, got)
	}
	if hits := mobHits(t, out.all()); len(hits) != 0 {
		t.Errorf("zero, environmental or protected damage sent %d MobHit events", len(hits))
	}
}

// ---------------------------------------------------------------------------
// A blow does not cross a block.
// ---------------------------------------------------------------------------

// walledTerrain is [dropTerrain] with a slab of extra solids standing on it, which is
// the whole of what a wall is here: the ground the bodies stand on, plus the voxels
// somebody built between them.
type walledTerrain struct {
	dropTerrain
	wall func(x, y, z int64) bool
}

func (w walledTerrain) Block(x, y, z int64) (world.Block, bool) {
	if w.wall != nil && w.wall(x, y, z) {
		return world.Stone, true
	}
	return w.dropTerrain.Block(x, y, z)
}

func (w walledTerrain) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || world.Solid(block)
}

func (w walledTerrain) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

// The reproduction, arranged so the wall is the only thing that changes.
//
// A draugr stands 2.5 blocks off, which is 1.9 between the two bodies and inside the
// species' 2.0 attackRange — the control below is what proves that, by letting the same
// swing land over the same distance with the voxels at z = -1 left as air. Two blocks
// tall so the state machine's one-block hop cannot clear it, though nothing here steps
// long enough for that to matter.
func walledOff() walledTerrain {
	return walledTerrain{
		dropTerrain: dropTerrain{groundTop: 63},
		wall:        func(_, y, z int64) bool { return z == -1 && (y == 64 || y == 65) },
	}
}

var (
	walledPlayerSpawn = [3]float32{0.5, 64, 0.5}
	walledMobSpawn    = [3]float32{0.5, 64, -2.0}
)

func TestAMobsBlowDoesNotCrossASolidBlock(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, walledOff())
	player, out := h.join(1, walledPlayerSpawn)
	mobID := h.spawnDraugrAt(walledMobSpawn)
	armMobBlow(t, h, mobID, player)
	before := len(out.all())

	healthBefore := h.vitals(player).Health
	h.step()

	if got := h.vitals(player).Health; got != healthBefore {
		t.Errorf("health after a blow through a wall = %d, want it unchanged at %d", got, healthBefore)
	}
	if hits := mobHits(t, out.all()[before:]); len(hits) != 0 {
		t.Errorf("a blow through a wall sent %d MobHit events, want none", len(hits))
	}

	// Abandoned rather than landed, and abandoned without the recovery an attack pays:
	// the swing never happened, so it costs nothing and the creature goes back to
	// walking into the block.
	m, alive := h.mobState(mobID)
	if !alive {
		t.Fatal("the draugr is gone")
	}
	if m.action != vnet.MobActionChase {
		t.Errorf("the draugr is in %v after the wall took its swing, want %v", m.action, vnet.MobActionChase)
	}
}

// The control, and it is the load-bearing half of the pair: without it the test above
// passes just as well when the mob is out of range, out of the world, or asleep.
func TestTheSameBlowLandsWithTheBlockTakenAway(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, walledPlayerSpawn)
	mobID := h.spawnDraugrAt(walledMobSpawn)
	armMobBlow(t, h, mobID, player)
	before := len(out.all())

	healthBefore := h.vitals(player).Health
	h.step()

	if got := h.vitals(player).Health; got >= healthBefore {
		t.Errorf("health after an unobstructed blow = %d, want less than %d", got, healthBefore)
	}
	if hits := mobHits(t, out.all()[before:]); len(hits) != 1 {
		t.Errorf("an unobstructed blow sent %d MobHit events, want one", len(hits))
	}
}

// The other half of the gate: a wall does not merely abandon a committed swing, it stops
// one being committed. A creature standing against a block it cannot see past chases —
// which is the same answer the navigation already gives, and now the same answer twice.
func TestAWallStopsTheWindupBeingCommittedAtAll(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name    string
		terrain Terrain
		want    vnet.MobAction
	}{
		{name: "walled off", terrain: walledOff(), want: vnet.MobActionChase},
		{name: "in the open", terrain: dropTerrain{groundTop: 63}, want: vnet.MobActionWindup},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newVitalsHarness(t, DefaultTickRate, tc.terrain)
			h.keepNight()
			player, _ := h.join(1, walledPlayerSpawn)
			mobID := h.spawnDraugrAt(walledMobSpawn)

			h.sim.mu.Lock()
			h.sim.mobs[mobID].target = player.entityID
			h.sim.mu.Unlock()

			h.step()

			m, alive := h.mobState(mobID)
			if !alive {
				t.Fatal("the draugr is gone")
			}
			if m.action != tc.want {
				t.Errorf("the draugr is in %v after one tick, want %v", m.action, tc.want)
			}
		})
	}
}
