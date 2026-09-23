package game

import (
	"fmt"
	"log/slog"
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The dungeon is group content (#1332): sized for three to five, and a delver alone meets
// the dungeon sized for three. These tests hold the proof the issue asks for — a solo
// delver at the iron reference dies, without `/immortal`, to either boss and to one placed
// pack before killing it.
//
// **The solo delver is the #1099 stander**: the iron blade and rusty armour, closing to
// reach and swinging whenever energy allows, and never stepping aside. It is the #1298
// bot's shape — the one kind of solo player the #1298 run could only carry through the
// fights under `/immortal`. A perfect scripted reader, which leaves every announced region
// in time and so is never struck, is not what "practically impossible" is about and is not
// what this proves: a reader never dies to anything, whatever the balance. The review
// record gives its solo times beside this.

// The two bosses: a solo iron stander meets a boss sized for three and is killed over and
// over — every death a wipe that puts the boss back whole — without ever finishing it,
// through the harness's whole thirty simulated minutes.
func TestASoloDelverDiesToEitherBossBeforeKillingIt(t *testing.T) {
	for _, boss := range []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing} {
		cfg := playtestConfig{boss: boss, party: 1, kit: kitIron, policy: policyStander}
		t.Run(cfg.String(), func(t *testing.T) {
			r := newPlaytest(t, cfg).run()
			if r.killed || r.deaths == 0 || r.scale.members != bossScaleMinMembers {
				t.Fatalf("killed=%v deaths=%d members=%d; want a boss sized for three that the solo delver dies to and never kills\n%s",
					r.killed, r.deaths, r.scale.members, r)
			}
		})
	}
}

// soloPackFight wakes one placed group for a lone iron stander standing in its hall — the
// hall's other group taken away first, so it is one pack and no more — and plays the
// fight until the delver dies or the pack is dead. It answers the pack's size, how many
// of it the delver killed, and whether and when the delver died.
func soloPackFight(t *testing.T, group, sibling int) (pack, killed int, died bool, seconds float64) {
	t.Helper()
	m, err := NewInstanceManager(DefaultTickRate, 3, 1, testEntityIDs(), slog.New(slog.DiscardHandler))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(m.Close)
	session, err := m.Reenter(InstanceRuin{}, instanceTestCharacter(1))
	if err != nil {
		t.Fatal(err)
	}
	loadDungeon(t, session)
	s := session.Sim

	s.mu.Lock()
	desc := &s.dungeon.descent
	for _, id := range desc.groups[sibling] {
		if mob := s.mobs[id]; mob != nil {
			s.discardMobLocked(mob)
		}
	}
	desc.groups[sibling] = nil
	spawn := zoneCentre(s, group)
	s.mu.Unlock()

	at := [3]float32{float32(spawn[0]), float32(spawn[1]), float32(spawn[2])}
	life := playtestLife(t, at, kitIron, 1)
	character := instanceTestCharacter(1)
	p, err := s.JoinCharacter(s.mintEntityID(), character.PlayerID, character.CharacterID, "Delver",
		at, testAppearance(), &life, func([]byte) bool { return true })
	if err != nil {
		t.Fatal(err)
	}

	var ids []uint64
	var clientTick uint32
	const limit = 120 // simulated seconds; the pack ends it long before
	for tick := 1; tick <= limit*int(DefaultTickRate); tick++ {
		m.Step()
		s.mu.Lock()
		if ids == nil && desc.woken[group] {
			ids = append(ids, desc.groups[group]...)
		}
		if !p.alive() {
			s.mu.Unlock()
			return len(ids), killed, true, float64(tick) / float64(DefaultTickRate)
		}
		// Counted only while the delver lives: the wipe its death causes puts every
		// survivor back under a fresh identity, which would read as a kill.
		killed = 0
		var target *mob
		for _, id := range ids {
			mob := s.mobs[id]
			if mob == nil {
				killed++
				continue
			}
			if target == nil || boxDistance(playerBox(p.pos), mob.species().body.boxAt(mob.pos)) <
				boxDistance(playerBox(p.pos), target.species().body.boxAt(target.pos)) {
				target = mob
			}
		}
		pos := p.pos
		var them box
		if target != nil {
			them = target.species().body.boxAt(target.pos)
		}
		s.mu.Unlock()
		if ids != nil && target == nil {
			return len(ids), killed, false, float64(tick) / float64(DefaultTickRate)
		}
		if target == nil {
			continue
		}

		me := playerBox(pos)
		centre, aim := boxCentre(me), boxCentre(them)
		dx, dy, dz := aim[0]-centre[0], aim[1]-centre[1], aim[2]-centre[2]
		yaw := wrapAngle(math.Atan2(-dx, -dz))
		gap := boxDistance(me, them)
		moveZ := float32(0)
		if gap > playtestStand {
			moveZ = 1 // forward along the facing
		}
		clientTick++
		_ = p.Submit(protocol.PlayerInput{ClientTick: clientTick, MoveZ: moveZ, Yaw: float32(yaw),
			Pitch: float32(math.Atan2(dy, math.Hypot(dx, dz)))})
		if gap <= SwordReach-0.05 {
			// Refused while the blade recovers or the reserve is short: that is the pace.
			_, _ = p.Attack(protocol.AttackRequest{Slot: mainHandSlot, ClientTick: clientTick})
		}
	}
	t.Fatalf("group %d: neither the delver nor the pack fell in %d seconds", group, limit)
	return 0, 0, false, 0
}

// One placed pack: a solo delver wakes a pack of three — one for each member the dungeon
// is sized for — and dies to it before the pack is dead, in the draugr's hall and in the
// vargr's.
func TestASoloDelverDiesToOnePackBeforeKillingIt(t *testing.T) {
	for _, hall := range []struct {
		name           string
		group, sibling int
	}{{"draugr", 0, 1}, {"vargr", 2, 3}} {
		t.Run(hall.name, func(t *testing.T) {
			pack, killed, died, seconds := soloPackFight(t, hall.group, hall.sibling)
			t.Logf("a pack of %d %s: the solo delver killed %d and died=%v after %.1f s", pack, hall.name, killed, died, seconds)
			if pack != bossScaleMinMembers || !died || killed >= pack {
				t.Fatalf("pack of %d, %d killed, died=%v; want a pack of three the solo delver dies to first", pack, killed, died)
			}
		})
	}
}

// The clamp both halves of the balance read, at every party size a run can hold and past
// both ends: a boss's members and a pack's size are the same number, three to five.
func TestTheDungeonIsSizedForThreeToFive(t *testing.T) {
	guardian := mobRegistry[vnet.MobKindVargrGuardian]
	for members, want := range []int{3, 3, 3, 3, 4, 5, 5} {
		levels := make([]uint16, members)
		for i := range levels {
			levels[i] = 1
		}
		scale := bossScaleFor(guardian, levels)
		if dungeonMembers(members) != want || int(scale.members) != want || dungeonPackSize(members) != want ||
			scale.maxHealth != guardian.maxHealth*uint16(want) {
			t.Errorf("%d members: sized as %d, boss %+v, pack %d; want %d", members, dungeonMembers(members), scale, dungeonPackSize(members), want)
		}
	}
	if bossScaleMaxMembers != MaxPartySize {
		t.Fatalf("the scale stops at %d members, want the largest party, %d", bossScaleMaxMembers, MaxPartySize)
	}
	// Every placed group the layout draws is one pack of the largest size, so the largest
	// pack stands on its own slots.
	for seed := int64(0); seed < 4; seed++ {
		slots := map[int]int{}
		for _, a := range world.InstanceDungeonAnchors(seed) {
			if a.Kind == world.AnchorInstanceMinorSpawn && a.Index != world.CaveBurrowGroup {
				slots[a.Index]++
			}
		}
		if fmt.Sprint(slots) != fmt.Sprint(map[int]int{0: 5, 1: 5, 2: 5, 3: 5, world.SandBuriedGroup: 5}) {
			t.Fatalf("seed %d: placed groups hold %v slots, want five each", seed, slots)
		}
	}
}
