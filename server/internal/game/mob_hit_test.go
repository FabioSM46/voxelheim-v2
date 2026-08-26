package game

import (
	"math"
	"testing"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
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
