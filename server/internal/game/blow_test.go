package game

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
	flatbuffers "github.com/google/flatbuffers/go"
)

// Read actual output, maintaining the last snapshot just as the receiver does. Every
// event must name only that snapshot's identities and copy its exact target position.
func disclosedBlows(t *testing.T, frames [][]byte) []protocol.BlowLanded {
	t.Helper()
	var snapshot *vnet.EntitySnapshot
	var blows []protocol.BlowLanded
	for _, frame := range frames {
		env := vnet.GetRootAsEnvelope(frame, 0)
		if env.PayloadType() != vnet.PayloadEntitySnapshot && env.PayloadType() != vnet.PayloadBlowLanded {
			continue
		}
		var tab flatbuffers.Table
		if !env.Payload(&tab) {
			t.Fatal("absent payload")
		}
		if env.PayloadType() == vnet.PayloadEntitySnapshot {
			snapshot = new(vnet.EntitySnapshot)
			snapshot.Init(tab.Bytes, tab.Pos)
			continue
		}
		var b vnet.BlowLanded
		b.Init(tab.Bytes, tab.Pos)
		if snapshot == nil || b.Tick() != snapshot.ServerTick() {
			t.Fatal("blow did not follow its own snapshot")
		}
		position := b.Position(nil)
		if position == nil {
			t.Fatal("blow position absent")
		}
		got := protocol.BlowLanded{Tick: b.Tick(), AttackerEntityID: b.AttackerEntityId(), TargetEntityID: b.TargetEntityId(), Kind: b.Kind(), Target: b.Target(), TargetMobKind: b.TargetMobKind(), Position: [3]float32{position.X(), position.Y(), position.Z()}}
		targetSeen, attackerSeen := false, got.AttackerEntityID == 0
		for i := 0; i < snapshot.EntitiesLength(); i++ {
			var entity vnet.EntityState
			snapshot.Entities(&entity, i)
			if entity.EntityId() == got.AttackerEntityID {
				attackerSeen = true
			}
			if got.Target == vnet.BlowTargetPlayer && entity.EntityId() == got.TargetEntityID {
				targetSeen = true
				p := entity.Pos(nil)
				if got.Position != [3]float32{p.X(), p.Y(), p.Z()} || got.TargetMobKind != vnet.MobKindUnknown {
					t.Fatal("player position/kind disclosure")
				}
			}
		}
		for i := 0; i < snapshot.MobsLength(); i++ {
			var mob vnet.MobState
			snapshot.Mobs(&mob, i)
			if mob.EntityId() == got.AttackerEntityID {
				attackerSeen = true
			}
			if got.Target == vnet.BlowTargetMob && mob.EntityId() == got.TargetEntityID {
				targetSeen = true
				p := mob.Pos(nil)
				if got.Position != [3]float32{p.X(), p.Y(), p.Z()} || got.TargetMobKind != mob.Kind() {
					t.Fatal("mob position/kind disclosure")
				}
			}
		}
		if !targetSeen || !attackerSeen {
			t.Fatal("event named an entity absent from snapshot")
		}
		blows = append(blows, got)
	}
	return blows
}

func TestBlowsReportTwoSameTickSwingsAndAKillExactlyOnce(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	a, out := h.join(1, [3]float32{.5, 64, .5})
	b, _ := h.join(2, [3]float32{.6, 64, .5})
	target := h.spawnDraugrAt([3]float32{.5, 64, -1.5})
	h.sim.mobs[target].health = 2 * RustySwordDamage
	for _, player := range []*Player{a, b} {
		if err := h.swing(player, 0, 1); err != nil {
			t.Fatal(err)
		}
	}
	if got := disclosedBlows(t, out.all()); len(got) != 0 {
		t.Fatal("intent emitted a blow before resolution")
	}
	h.step()
	blows := disclosedBlows(t, out.all())
	if len(blows) != 2 {
		t.Fatalf("got %d blows, want two", len(blows))
	}
	for i, blow := range blows {
		if blow.TargetEntityID != target || blow.AttackerEntityID != uint64(i+1) || blow.Kind != vnet.BlowKindMelee {
			t.Fatalf("wrong blow: %+v", blow)
		}
	}
	if h.mobHealth(target) != 0 {
		t.Fatal("killing damage changed")
	}
	h.step()
	if got := disclosedBlows(t, out.all()); len(got) != 2 {
		t.Fatal("contact replayed next tick")
	}
}

func TestMissAndInvalidIntentProduceNoBlow(t *testing.T) {
	for _, slot := range []uint8{0, 39, 255} {
		h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
		p, out := h.join(1, [3]float32{.5, 64, .5})
		h.spawnDraugrAt([3]float32{.5, 64, 6}) // Behind and beyond sword reach.
		_, _ = p.Attack(protocol.AttackRequest{Slot: slot, ClientTick: 1})
		h.step()
		if len(disclosedBlows(t, out.all())) != 0 {
			t.Fatalf("slot %d emitted a miss", slot)
		}
	}
}

func TestProjectileContactReportsOnceAndHealingAndTerrainDoNot(t *testing.T) {
	for _, kind := range []vnet.ProjectileKind{vnet.ProjectileKindArrow, vnet.ProjectileKindEnergyOrb} {
		for _, hit := range []bool{false, true} {
			h := newVitalsHarness(t, DefaultTickRate, projectileTerrain{})
			owner, out := h.join(1, [3]float32{.5, 64, .5})
			var target uint64
			if hit {
				target = h.spawnDraugrAt([3]float32{.5, 64, -1.5})
			}
			spawnTestProjectile(t, h, kind, owner, [3]float64{0, 0, -1}, ArrowSpeed)
			h.advance(6)
			blows := disclosedBlows(t, out.all())
			if !hit {
				if len(blows) != 0 {
					t.Fatal("projectile miss emitted blow")
				}
				continue
			}
			want := vnet.BlowKindArrow
			damage := uint16(ArrowDamage)
			if kind == vnet.ProjectileKindEnergyOrb {
				want, damage = vnet.BlowKindEnergyOrb, OrbDamage
			}
			if len(blows) != 1 || blows[0].Kind != want || blows[0].TargetEntityID != target {
				t.Fatalf("projectile blows: %+v", blows)
			}
			if h.mobHealth(target) != draugrRow.maxHealth-damage {
				t.Fatal("projectile damage changed")
			}
		}
	}
	h := newVitalsHarness(t, DefaultTickRate, projectileTerrain{})
	owner, out := h.join(1, [3]float32{.5, 64, .5})
	healed, _ := h.join(2, [3]float32{.5, 64, -1.5})
	healed.health = 20
	spawnTestProjectile(t, h, vnet.ProjectileKindEnergyOrb, owner, [3]float64{0, 0, -1}, OrbSpeed)
	h.advance(4)
	if healed.health != 20+OrbHeal || len(disclosedBlows(t, out.all())) != 0 {
		t.Fatal("healing changed or emitted blow")
	}
}

func TestMobBlowsReportProtectedMissBlockedAndKillingOutcomes(t *testing.T) {
	for _, mode := range []string{"ordinary", "blocked", "killed", "protected", "out-of-reach"} {
		t.Run(mode, func(t *testing.T) {
			h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
			h.keepNight()
			p, out := h.join(1, [3]float32{.5, 64, .5})
			id := h.spawnDraugrAt([3]float32{.5, 64, -1})
			armMobBlow(t, h, id, p)
			switch mode {
			case "blocked":
				equipShield(t, p, WoodenShieldMaxDurability)
				p.Block(true)
			case "killed":
				p.health = 1
			case "protected":
				p.protectionTicks = 10
			case "out-of-reach":
				p.pos[2] = 20
			}
			before := p.health
			h.step()
			blows := disclosedBlows(t, out.all())
			if mode == "protected" || mode == "out-of-reach" {
				if len(blows) != 0 || p.health != before {
					t.Fatal("refused mob blow emitted or damaged")
				}
			} else if len(blows) != 1 || blows[0].Kind != vnet.BlowKindMobMelee || blows[0].TargetEntityID != p.entityID || blows[0].AttackerEntityID != id || p.health >= before {
				t.Fatalf("mob contact: %+v", blows)
			}
		})
	}
}

func TestBlowProjectionSuppressesAbsentTargetsAndAnonymousSources(t *testing.T) {
	h := newVitalsHarnessAt(t, DefaultTickRate, dropTerrain{groundTop: 63}, 1)
	attacker, near := h.join(1, [3]float32{.5, 64, .5})
	_, far := h.join(2, [3]float32{200, 64, 200})
	target := h.spawnDraugrAt([3]float32{.5, 64, -1.5})
	if err := h.swing(attacker, 0, 1); err != nil {
		t.Fatal(err)
	}
	h.step()
	if len(disclosedBlows(t, near.all())) != 1 || len(disclosedBlows(t, far.all())) != 0 {
		t.Fatal("interest filter failed")
	}
	// Exercise the actual projection with a moved body and with party-only membership:
	// the frozen contact has no coordinate to leak and no party exception to apply.
	h.sim.blows = []landedBlow{{attacker: 1, target: target, kind: vnet.BlowKindArrow, targetKind: vnet.BlowTargetMob}}
	snapshot := protocol.EntitySnapshot{Tick: 7, Mobs: []protocol.MobState{{EntityID: target, Kind: vnet.MobKindDraugr, Pos: [3]float32{4, 65, -3}}}}
	frames := append([][]byte{protocol.EncodeEntitySnapshot(snapshot)}, h.sim.blowFramesLocked(snapshot)...)
	blows := disclosedBlows(t, frames)
	if len(blows) != 1 || blows[0].AttackerEntityID != 0 {
		t.Fatal("hidden projectile source disclosed")
	}
	snapshot.Mobs = nil
	if len(h.sim.blowFramesLocked(snapshot)) != 0 {
		t.Fatal("target absent after interest exit still disclosed")
	}
}

func TestDroppedBlowSnapshotDoesNotReplay(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	p, out := h.join(1, [3]float32{.5, 64, .5})
	h.spawnDraugrAt([3]float32{.5, 64, -1.5})
	deliver := p.deliverSnapshot
	attempted := 0
	p.deliverSnapshot = func(_ []byte, _ world.Column, following [][]byte) bool { attempted += len(following); return false }
	if err := h.swing(p, 0, 1); err != nil {
		t.Fatal(err)
	}
	h.step()
	if attempted != 1 || len(disclosedBlows(t, out.all())) != 0 {
		t.Fatal("failed snapshot orphaned its event")
	}
	p.deliverSnapshot = deliver
	h.step()
	if len(disclosedBlows(t, out.all())) != 0 {
		t.Fatal("failed snapshot replayed its stale contact")
	}
}

func TestTerrainImpactAndEnvironmentalDamageHaveNoBlow(t *testing.T) {
	wall := int64(-2)
	for _, kind := range []vnet.ProjectileKind{vnet.ProjectileKindArrow, vnet.ProjectileKindEnergyOrb} {
		h := newVitalsHarness(t, DefaultTickRate, projectileTerrain{wallZ: &wall})
		p, out := h.join(1, [3]float32{.5, 64, .5})
		spawnTestProjectile(t, h, kind, p, [3]float64{0, 0, -1}, ArrowSpeed)
		h.advance(6)
		if len(disclosedBlows(t, out.all())) != 0 {
			t.Fatal("terrain impact reported as a landed blow")
		}
		p.damageLocked(3)
		h.step()
		if len(disclosedBlows(t, out.all())) != 0 {
			t.Fatal("environmental damage reported as a blow")
		}
	}
}

func TestOfflineProjectileOwnerIsAnonymousInARealSnapshot(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, projectileTerrain{})
	owner, _ := h.join(1, [3]float32{.5, 64, .5})
	_, out := h.join(2, [3]float32{4, 64, .5})
	target := h.spawnDraugrAt([3]float32{.5, 64, -1.5})
	spawnTestProjectile(t, h, vnet.ProjectileKindArrow, owner, [3]float64{0, 0, -1}, ArrowSpeed)
	h.sim.Leave(owner)
	h.advance(5)
	blows := disclosedBlows(t, out.all())
	if len(blows) != 1 || blows[0].AttackerEntityID != 0 || blows[0].TargetEntityID != target {
		t.Fatalf("offline impact = %+v", blows)
	}
}
