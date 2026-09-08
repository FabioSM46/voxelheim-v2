package game

import (
	"fmt"
	"math"
	"reflect"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func recoveryDungeon(t *testing.T, rate uint8, king bool) (*InstanceManager, InstanceSession, *Player, uint64) {
	t.Helper()
	manager := instanceTestManager(t, rate, 2)
	session, err := manager.Reenter(InstanceRuin{}, instanceTestCharacter(1))
	if err != nil {
		t.Fatal(err)
	}
	loadDungeon(t, session)
	if king {
		killMobInSession(t, session, vnet.MobKindVargrGuardian)
		manager.Step()
	}
	s := session.Sim
	id := s.dungeon.guardianID
	if king {
		id = s.dungeon.kingID
	}
	pos := s.mobs[id].pos
	spawn := [3]float32{float32(pos[0]), float32(pos[1]), float32(pos[2] + 3)}
	p, err := s.JoinCharacter(s.mintEntityID(), testPlayerID(1), 1, "Fighter", spawn, testAppearance(), nil, func([]byte) bool { return true })
	if err != nil {
		t.Fatal(err)
	}
	s.mu.Lock()
	s.startBossEncounterLocked(s.mobs[id], p)
	s.mu.Unlock()
	return manager, session, p, id
}

func TestDungeonAllDownReplacesOnlyTheLiveEncounter(t *testing.T) {
	for _, king := range []bool{false, true} {
		t.Run(fmt.Sprint(king), func(t *testing.T) {
			manager, session, p, id := recoveryDungeon(t, 20, king)
			s := session.Sim
			s.mu.Lock()
			old := s.mobs[id]
			home := old.pos
			old.health = 10
			old.encounter.phase = 2
			old.encounter.scheduleCursor = 4
			old.encounter.cooldowns[vnet.EncounterMoveKindBiteAndTear] = 100
			old.firstHit = newMobTap(p)
			old.pos[0] += 2
			p.dieLocked()
			s.mu.Unlock()
			manager.Step()
			s.mu.Lock()
			defer s.mu.Unlock()
			if s.mobs[id] != nil {
				t.Fatal("all-down retained damaged encounter")
			}
			if old.encounter != nil || old.firstHit != nil {
				t.Fatal("discard retained old pull or tap")
			}
			freshID := s.dungeon.guardianID
			if king {
				freshID = s.dungeon.kingID
			}
			fresh := s.mobs[freshID]
			if fresh == nil || fresh.encounter != nil || fresh.health != fresh.species().maxHealth || fresh.firstHit != nil || len(fresh.threat) != 0 || fresh.pos != home {
				t.Fatalf("reset did not restore a fresh boss: %+v", fresh)
			}
			if s.dungeon.progress.guardian != king || s.dungeon.progress.king {
				t.Fatal("wipe changed earned progress")
			}
			_, _, gate := world.InstanceEncounterAnchors(session.Seed)
			if s.terrain.Solid(gate.X, gate.Y, gate.Z) == king {
				t.Fatal("wipe changed gate")
			}
			if p.alive() || p.respawnTicks == 0 {
				t.Fatal("wipe bypassed resurrection timer")
			}
		})
	}
}

func TestDungeonCombatRejectsNewMembership(t *testing.T) {
	manager, session, _, _ := recoveryDungeon(t, 20, false)
	if _, err := manager.Join(session.ID, instanceTestCharacter(2)); err == nil {
		t.Fatal("combat admitted a new character")
	}
}

func TestDungeonAbandonedBossResetIgnoresDirectorPopulationBudget(t *testing.T) {
	for _, king := range []bool{false, true} {
		t.Run(fmt.Sprint(king), func(t *testing.T) {
			manager, session, p, id := recoveryDungeon(t, 20, king)
			s := session.Sim
			s.mu.Lock()
			old := s.mobs[id]
			home := old.pos
			old.health = 10
			s.mu.Unlock()
			s.Leave(p)
			manager.Leave(session.ID, instanceTestCharacter(1))
			// No connected bodies means the director's world ceiling is zero.
			// A placed encounter must still be replaced before the old one is removed.
			s.mu.Lock()
			bodies := len(s.players)
			s.mu.Unlock()
			if bodies != 0 {
				t.Fatal("fixture still has connected bodies")
			}
			manager.Step()
			s.mu.Lock()
			freshID := s.dungeon.guardianID
			if king {
				freshID = s.dungeon.kingID
			}
			fresh := s.mobs[freshID]
			reset := s.mobs[id] == nil && old.encounter == nil && fresh != nil && freshID != id &&
				fresh.health == fresh.species().maxHealth && fresh.encounter == nil && fresh.pos == home
			s.mu.Unlock()
			if !reset {
				t.Fatal("zero director budget prevented the abandoned boss reset")
			}
			if _, err := manager.Join(session.ID, instanceTestCharacter(2)); err != nil {
				t.Fatalf("fresh boss retained the combat admission block: %v", err)
			}
		})
	}
}

func TestDungeonDeathWaitsTenSecondsAndRisesInPlace(t *testing.T) {
	for _, rate := range []uint8{1, 20, 60} {
		t.Run(fmt.Sprint(rate), func(t *testing.T) {
			manager, session, p, _ := recoveryDungeon(t, rate, false)
			s := session.Sim
			s.mu.Lock()
			anchor := p.pos
			p.spawn = [3]float64{999, 999, 999}
			p.dieLocked()
			s.mu.Unlock()
			for tick := uint32(1); tick <= uint32(rate)*10; tick++ {
				manager.Step()
				s.mu.Lock()
				alive, left := p.alive(), p.respawnTicks
				position, protection := p.pos, p.protectionTicks
				s.mu.Unlock()
				if tick < uint32(rate)*10 {
					if alive || left != uint32(rate)*10-tick {
						t.Fatalf("tick%d alive%v left%d", tick, alive, left)
					}
				} else {
					if !alive || left != 0 || protection == 0 {
						t.Fatal("ten-second recovery missing or unprotected")
					}
					if math.Hypot(position[0]-anchor[0], position[2]-anchor[2]) > 0.01 {
						t.Fatal("dungeon respawn moved to a home")
					}
				}
			}
		})
	}
}

func TestDungeonWipeWaitsForTheLivingBodyOutsideTheLootRoster(t *testing.T) {
	manager, session, p, id := recoveryDungeon(t, 20, false)
	s := session.Sim
	// Set up a second admission before the pull, without adding them to its party.
	s.mu.Lock()
	s.mobs[id].encounter = nil
	s.mu.Unlock()
	if _, err := manager.Join(session.ID, instanceTestCharacter(2)); err != nil {
		t.Fatal(err)
	}
	arrival, _ := world.InstanceAnchors(session.Seed)
	other, err := s.JoinCharacter(s.mintEntityID(), testPlayerID(2), 1, "Observer", [3]float32{float32(arrival.X) + .5, float32(arrival.Y), float32(arrival.Z) + .5}, testAppearance(), nil, func([]byte) bool { return true })
	if err != nil {
		t.Fatal(err)
	}
	s.mu.Lock()
	s.startBossEncounterLocked(s.mobs[id], p)
	if len(s.mobs[id].encounter.roster) != 1 {
		t.Fatal("fixture added observer to loot roster")
	}
	s.mobs[id].health = 10
	p.dieLocked()
	s.mu.Unlock()
	manager.Step()
	s.mu.Lock()
	held := s.mobs[id]
	s.mu.Unlock()
	if held == nil || held.health != 10 {
		t.Fatal("living observer did not prevent all-down wipe")
	}
	s.mu.Lock()
	other.dieLocked()
	s.mu.Unlock()
	manager.Step()
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.mobs[id] != nil {
		t.Fatal("last body falling did not reset boss")
	}
}

func TestDungeonWipeRetiresPlayerShotsBeforeTheFreshPull(t *testing.T) {
	manager, session, p, id := recoveryDungeon(t, 20, false)
	s := session.Sim
	s.mu.Lock()
	m := s.mobs[id]
	shot := s.mintEntityID()
	s.projectiles[shot] = &projectile{entityID: shot, kind: vnet.ProjectileKindArrow, owner: p.entityID, pos: m.pos, vel: [3]float64{0, 0, 10}, ticksLeft: 100}
	s.projectileOwners[shot] = p
	p.dieLocked()
	s.mu.Unlock()
	manager.Step()
	manager.Step()
	s.mu.Lock()
	defer s.mu.Unlock()
	fresh := s.mobs[s.dungeon.guardianID]
	if len(s.projectiles) != 0 || len(s.projectileOwners) != 0 || fresh == nil || fresh.health != fresh.species().maxHealth || fresh.firstHit != nil {
		t.Fatal("abandoned projectile reached new pull")
	}
}

func TestDungeonReconnectKeepsDeathAndProtectionDeadlines(t *testing.T) {
	manager, session, p, _ := recoveryDungeon(t, 20, false)
	s := session.Sim
	s.mu.Lock()
	p.dieLocked()
	s.mu.Unlock()
	life := p.Record() // charges the ordinary penalty exactly once
	originalSlots := life.Slots
	deadline := life.recovery.deathUntil
	anchor := life.recovery.anchor
	key := instanceTestCharacter(1)
	entry := PortalEntry{Session: session, Character: key, Return: [3]float32{99, 64, 99}}
	reconnect := func(wait int) {
		t.Helper()
		life = p.Record()
		s.Leave(p)
		manager.DisconnectPortal(entry, life)
		for range wait {
			manager.Step()
		}
		restored, visit, err := manager.ResumePortal(key)
		if err != nil || restored == nil || visit == nil {
			t.Fatalf("resume failed%v", err)
		}
		// Persisted safe-return placement must not overwrite transient death anchor.
		restored.Pos = [3]float64{99, 64, 99}
		p, err = s.JoinCharacter(s.mintEntityID(), testPlayerID(1), 1, "Fighter", [3]float32{99, 64, 99}, testAppearance(), restored, func([]byte) bool { return true })
		if err != nil {
			t.Fatal(err)
		}
		if p.pos != anchor {
			t.Fatal("resume lost in-place anchor")
		}
		if p.Record().Slots != originalSlots {
			t.Fatal("reconnect charged death penalty again")
		}
		entry = *visit
	}
	reconnect(40)
	if p.alive() || uint64(p.respawnTicks) != deadline-s.currentTick {
		t.Fatal("early reconnect revived or restarted timer")
	}
	reconnect(60)
	if p.alive() || uint64(p.respawnTicks) != deadline-s.currentTick {
		t.Fatal("repeated reconnect changed deadline")
	}
	reconnect(110)
	if !p.alive() || p.protectionTicks != s.protectionTicks-10 {
		t.Fatalf("late resume protection%d want%d", p.protectionTicks, s.protectionTicks-10)
	}
	reconnect(5)
	if p.protectionTicks != s.protectionTicks-15 {
		t.Fatal("reconnect renewed immunity")
	}
	reconnect(int(s.protectionTicks))
	if p.protectionTicks != 0 {
		t.Fatal("expired protection renewed")
	}
}

func TestDungeonCombatRechecksAnAcceptedOfferAndLeavesBindingUntouched(t *testing.T) {
	manager, open, request, join := portalHarness(t, 3)
	owner, friend := join(), join()
	inviteAndAccept(t, owner, friend, friend.name)
	entry := enterSavedRun(t, manager, owner, request)
	offer := manager.EnterPortal(friend, request).Offer
	if offer.ID == 0 {
		t.Fatal("fixture produced no pre-combat offer")
	}
	decision := manager.EnterPortal(owner, request)
	if decision.Outcome != PortalAdmitted {
		t.Fatal("owner could not return before combat")
	}
	s := entry.Session.Sim
	loadDungeon(t, entry.Session)
	s.mu.Lock()
	king := s.mobs[s.dungeon.kingID]
	at := king.pos
	s.mu.Unlock()
	if err := open.Transfer(owner, s, [3]float32{float32(at[0]), float32(at[1]), float32(at[2] + 3)}); err != nil {
		t.Fatal(err)
	}
	s.mu.Lock()
	s.startBossEncounterLocked(king, owner)
	roster := append([]corpseOwner(nil), king.encounter.roster...)
	s.mu.Unlock()
	decision = manager.AnswerEntryOffer(friend, protocol.InstanceEntryAnswer{OfferID: offer.ID, Accept: true})
	if decision.Outcome != PortalRefused || decision.Reason != vnet.RefusalReasonInstanceUnavailable {
		t.Fatalf("combat accepted old offer: %+v", decision)
	}
	key := InstanceCharacter{friend.playerID, friend.characterID}
	if _, bound := manager.Bound(entry.Session.Ruin, key); bound {
		t.Fatal("failed crossing bound character")
	}
	held, _ := manager.Lookup(entry.Session.ID)
	if len(held.Members) != 1 {
		t.Fatal("failed crossing changed occupants")
	}
	if !reflect.DeepEqual(roster, king.encounter.roster) {
		t.Fatal("admission changed loot roster")
	}
	if decision = manager.EnterPortal(friend, request); decision.Outcome != PortalRefused || decision.Reason != vnet.RefusalReasonInstanceUnavailable {
		t.Fatal("combat offered a new entry")
	}
}

func TestDungeonRememberedLifeCanResumeButCannotPreventAnAllDownWipe(t *testing.T) {
	for _, resume := range []bool{false, true} {
		t.Run(fmt.Sprint(resume), func(t *testing.T) {
			manager, open, request, join := portalHarness(t, 3)
			p, q := join(), join()
			inviteAndAccept(t, p, q, q.name)
			a := manager.EnterPortal(p, request)
			b := manager.EnterPortal(q, request)
			if a.Outcome != PortalAdmitted || b.Outcome != PortalAdmitted || a.Entry.Session.ID != b.Entry.Session.ID {
				t.Fatal("party failed initial admission")
			}
			s := a.Entry.Session.Sim
			loadDungeon(t, a.Entry.Session)
			id := s.dungeon.guardianID
			at := s.mobs[id].pos
			for _, who := range []*Player{p, q} {
				if err := open.Transfer(who, s, [3]float32{float32(at[0]), float32(at[1]), float32(at[2] + 3)}); err != nil {
					t.Fatal(err)
				}
			}
			s.mu.Lock()
			s.startBossEncounterLocked(s.mobs[id], p)
			s.mobs[id].health = 10
			s.mu.Unlock()
			life := p.Record()
			s.Leave(p)
			manager.DisconnectPortal(a.Entry, life)
			if resume {
				restored, entry, err := manager.ResumePortal(a.Entry.Character)
				if err != nil || entry == nil || restored == nil || *restored != life || entry.Session.Sim != s {
					t.Fatal("combat blocked or changed authoritative resume")
				}
				if _, err := manager.Join(a.Entry.Session.ID, instanceTestCharacter(999)); err == nil {
					t.Fatal("resume exemption admitted a stranger")
				}
			} else {
				s.mu.Lock()
				q.dieLocked()
				s.mu.Unlock()
				manager.Step()
				s.mu.Lock()
				defer s.mu.Unlock()
				if s.mobs[id] != nil {
					t.Fatal("disconnected healthy life prevented all-down reset")
				}
			}
		})
	}
}

func TestDungeonRecoveryMetadataCannotControlAnotherWorld(t *testing.T) {
	manager, session, p, _ := recoveryDungeon(t, 20, false)
	session.Sim.mu.Lock()
	p.dieLocked()
	session.Sim.mu.Unlock()
	life := p.Record()
	life.Pos = [3]float64{.5, 64, .5}
	open := newVitalsHarness(t, 20, dropTerrain{groundTop: 63})
	outsider, _ := open.joinLife(2, [3]float32{.5, 64, .5}, &life)
	if !outsider.alive() || outsider.protectionTicks != 0 || outsider.pos != life.Pos {
		t.Fatal("dungeon metadata affected open world")
	}
	other, err := manager.Reenter(InstanceRuin{1, 1}, instanceTestCharacter(2))
	if err != nil {
		t.Fatal(err)
	}
	newcomer, err := other.Sim.JoinCharacter(other.Sim.mintEntityID(), testPlayerID(2), 1, "Other", [3]float32{.5, 64, .5}, testAppearance(), &life, func([]byte) bool { return true })
	if err != nil {
		t.Fatal(err)
	}
	if !newcomer.alive() || newcomer.protectionTicks != 0 || newcomer.pos != life.Pos {
		t.Fatal("metadata affected another instance")
	}
}
