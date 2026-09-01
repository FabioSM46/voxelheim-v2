package game

import (
	"io"
	"log/slog"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

func TestNormalKillBecomesOneContinuousOwnedCorpse(t *testing.T) {
	t.Parallel()
	h, player, _, id := armedAgainst(t, vnet.MobKindDraugr, [3]float64{0.5, 64, -1.5})
	resting := h.killWithTheStarterBlade(player, id)

	h.sim.mu.Lock()
	c := h.sim.corpses[id]
	_, stillCounted := h.sim.mobs[id]
	h.sim.mu.Unlock()
	if c == nil {
		t.Fatal("the completed death created no corpse")
	}
	if c.entityID != id || c.kind != vnet.MobKindDraugr || c.pos != resting {
		t.Errorf("corpse continuity = id %d kind %s pos %v; want %d Draugr %v", c.entityID, c.kind, c.pos, id, resting)
	}
	if !c.ownedBy(player) || c.state().Action != vnet.MobActionCorpse {
		t.Errorf("corpse owner/action = %+v/%s", c.owner, c.state().Action)
	}
	if stillCounted {
		t.Error("corpse remained in the live-mob ceiling")
	}
	if got := h.sim.DropCount(); got != 0 {
		t.Errorf("normal-mob loot created %d ground drops", got)
	}
}

// **The corpse is in the snapshot the killing blow produced, and the owner is told in that
// same frame that it can be opened.**
//
// This is the whole of #441 in one assertion, and it is deliberately about the *tick*
// rather than about the eventual state. A killed creature used to spend MobDeathDuration in
// Sim.mobs: the snapshot of the killing tick drew it as Dying with no health, listed
// nothing in accessible_loot_corpses, and OpenLoot refused it, for two and a half seconds
// after the player had earned it. Every one of those three now answers on the tick of the
// blow, and OpenLoot is checked here as well as the vector, because the vector is what the
// client draws with and OpenLoot is what actually decides.
func TestTheKillingTicksSnapshotAlreadyCarriesAnOpenableCorpse(t *testing.T) {
	t.Parallel()
	h, player, out, id := armedAgainst(t, vnet.MobKindDraugr, [3]float64{0.5, 64, -1.5})

	// Wounded to within one blow, so the kill lands on a tick this test names rather than
	// on whichever of three the cooldown happened to put it on.
	h.sim.mu.Lock()
	h.sim.mobs[id].health = RustySwordDamage
	h.sim.mu.Unlock()

	if err := h.swing(player, 0, uint32(h.tick)+1); err != nil {
		t.Fatalf("the killing swing was refused: %v", err)
	}
	h.step()

	var drawn bool
	for _, state := range newestSnapshotMobs(t, out) {
		if state.EntityID != id {
			continue
		}
		drawn = true
		if state.Action != vnet.MobActionCorpse || state.Health != 0 {
			t.Errorf("the killing tick draws %d as %s with %d health, want inert Corpse",
				id, state.Action, state.Health)
		}
	}
	if !drawn {
		t.Fatal("the killing tick's snapshot does not carry the body at all")
	}

	snapshot := newestSnapshot(t, out)
	if snapshot.AccessibleLootCorpsesLength() != 1 || snapshot.AccessibleLootCorpses(0) != id {
		t.Errorf("the killing tick advertises %d accessible corpses, want just %d",
			snapshot.AccessibleLootCorpsesLength(), id)
	}

	// And the server agrees when actually asked, which is the half a client cannot fake.
	if reason, err := player.OpenLoot(protocol.LootOpenRequest{CorpseID: id, ClientTick: uint32(h.tick) + 1}); err != nil {
		t.Fatalf("opening the corpse on the tick of the kill = %s, %v", reason, err)
	}
}

func TestSnapshotsAdvertiseOnlyCorpsesTheRecipientCanOpen(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	owner, ownerOut := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	_, otherOut := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	id := h.placeSpeciesAt(vnet.MobKindVargr, [3]float64{0.5, 64, -1.5})
	h.killWithTheStarterBlade(owner, id)
	h.step()
	ownerSnapshot := newestSnapshot(t, ownerOut)
	if ownerSnapshot.AccessibleLootCorpsesLength() != 1 || ownerSnapshot.AccessibleLootCorpses(0) != id {
		t.Fatalf("owner accessible corpse count/id = %d/%d, want 1/%d", ownerSnapshot.AccessibleLootCorpsesLength(), ownerSnapshot.AccessibleLootCorpses(0), id)
	}
	if got := newestSnapshot(t, otherOut).AccessibleLootCorpsesLength(); got != 0 {
		t.Fatalf("other character was advertised %d accessible corpses", got)
	}
	h.sim.mu.Lock()
	owner.pos[0] += EditReach + 2
	owner.chunk = chunkAt(owner.pos)
	h.sim.mu.Unlock()
	h.step()
	if got := newestSnapshot(t, ownerOut).AccessibleLootCorpsesLength(); got != 0 {
		t.Fatalf("out-of-reach owner was advertised %d accessible corpses", got)
	}
}

func TestCorpseRollIsStableAcrossRepeatedOpens(t *testing.T) {
	t.Parallel()
	h, player, _, id := armedAgainst(t, vnet.MobKindDraugr, [3]float64{0.5, 64, -1.5})
	h.killWithTheStarterBlade(player, id)

	h.sim.mu.Lock()
	want := h.sim.corpses[id].lootState(&h.sim.corpses[id].container)
	h.sim.mu.Unlock()
	for tick := uint32(1); tick <= 3; tick++ {
		if reason, err := player.OpenLoot(protocol.LootOpenRequest{CorpseID: id, ClientTick: tick}); err != nil {
			t.Fatalf("open %d = %s, %v", tick, reason, err)
		}
		h.sim.mu.Lock()
		got := h.sim.corpses[id].lootState(&h.sim.corpses[id].container)
		h.sim.mu.Unlock()
		if got.Revision != want.Revision || len(got.Entries) != len(want.Entries) || got.Entries[0] != want.Entries[0] {
			t.Fatalf("open %d rerolled or mutated %+v into %+v", tick, want, got)
		}
	}
}

func TestCorpseAccessBelongsToFirstTapAndSurvivesReconnect(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	owner, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	finisher, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	id := h.placeSpeciesAt(vnet.MobKindDraugr, [3]float64{0.5, 64, -1.5})
	if err := h.swing(owner, 0, 1); err != nil {
		t.Fatal(err)
	}
	h.step()
	h.killWithTheStarterBlade(finisher, id)

	if reason, err := finisher.OpenLoot(protocol.LootOpenRequest{CorpseID: id, ClientTick: 1}); err == nil || reason != vnet.RefusalReasonLootNotOwned {
		t.Fatalf("finisher open = %s, %v; want LootNotOwned", reason, err)
	}
	h.hurt(owner, PlayerMaxHealth)
	if reason, err := owner.OpenLoot(protocol.LootOpenRequest{CorpseID: id, ClientTick: 1}); err == nil || reason != vnet.RefusalReasonPlayerIsDead {
		t.Fatalf("dead owner open = %s, %v; want PlayerIsDead", reason, err)
	}
	h.advance(int(h.sim.deathTicks) + 1)
	if reason, err := owner.OpenLoot(protocol.LootOpenRequest{CorpseID: id, ClientTick: 1}); err != nil {
		t.Fatalf("respawned owner open = %s, %v", reason, err)
	}
	ownerKey := owner.partyMemberKey()
	h.sim.Leave(owner)
	reconnected, err := h.sim.JoinCharacter(3, ownerKey.playerID, ownerKey.characterID, "Astrid", [3]float32{0.5, 64, 0.5}, testAppearance(), nil, func([]byte) bool { return true })
	if err != nil {
		t.Fatalf("reconnect: %v", err)
	}
	if reason, err := reconnected.OpenLoot(protocol.LootOpenRequest{CorpseID: id, ClientTick: 1}); err != nil {
		t.Fatalf("owner reconnect open = %s, %v", reason, err)
	}
}

func TestLootTakeIsWholeAtomicAndLeavesAnInertCorpseUntilExpiry(t *testing.T) {
	t.Parallel()
	h, player, out, id := armedAgainst(t, vnet.MobKindVargr, [3]float64{0.5, 64, -1.5})
	h.killWithTheStarterBlade(player, id)
	if reason, err := player.OpenLoot(protocol.LootOpenRequest{CorpseID: id, ClientTick: 1}); err != nil {
		t.Fatalf("open = %s, %v", reason, err)
	}

	player.inventory.mu.Lock()
	for slot := range player.inventory.slots[:equipmentFirst] {
		player.inventory.slots[slot] = stackOf(ItemRustySword, 1)
	}
	before := player.inventory.slots
	player.inventory.mu.Unlock()
	if reason, err := player.TakeLoot(protocol.LootTakeRequest{CorpseID: id, EntryID: 1, Revision: 1, ClientTick: 1}); err == nil || reason != vnet.RefusalReasonInventoryFull {
		t.Fatalf("full take = %s, %v", reason, err)
	}
	player.inventory.mu.Lock()
	if player.inventory.slots != before {
		t.Error("refused whole-entry transfer partially changed the inventory")
	}
	player.inventory.slots[1] = inventoryStack{}
	player.inventory.mu.Unlock()

	if reason, err := player.TakeLoot(protocol.LootTakeRequest{CorpseID: id, EntryID: 1, Revision: 1, ClientTick: 2}); err != nil {
		t.Fatalf("take = %s, %v", reason, err)
	}
	h.sim.mu.Lock()
	c := h.sim.corpses[id]
	if c == nil {
		h.sim.mu.Unlock()
		t.Fatal("taking the final entry removed the corpse")
	}
	expiresTick := c.expiresTick
	entries := len(c.container.entries)
	h.sim.mu.Unlock()
	if entries != 0 {
		t.Fatalf("emptied corpse = %#v with %d entries; want a surviving empty body", c, entries)
	}
	if reason, err := player.TakeLoot(protocol.LootTakeRequest{CorpseID: id, EntryID: 1, Revision: 1, ClientTick: 3}); err == nil || reason != vnet.RefusalReasonCorpseUnavailable {
		t.Fatalf("duplicate take = %s, %v; want unavailable", reason, err)
	}

	h.step()
	snapshot := newestSnapshot(t, out)
	if got := snapshot.AccessibleLootCorpsesLength(); got != 0 {
		t.Fatalf("empty corpse remained in %d accessible-corpse entries", got)
	}
	var bodyStillDrawn bool
	for _, mob := range newestSnapshotMobs(t, out) {
		if mob.EntityID == id {
			bodyStillDrawn = mob.Action == vnet.MobActionCorpse
		}
	}
	if !bodyStillDrawn {
		t.Fatal("empty corpse left the authoritative mob snapshot")
	}
	h.sim.mu.Lock()
	openLootID := player.openLootID
	h.sim.mu.Unlock()
	if openLootID != 0 {
		t.Fatalf("empty corpse remained open as %d", openLootID)
	}
	h.step()
	_, _, closed := lootFrames(t, out)
	if len(closed) != 1 || closed[0] != id {
		t.Fatalf("loot closures = %v, want the emptied corpse once", closed)
	}

	h.advance(int(expiresTick-h.tick) - 1)
	h.sim.mu.Lock()
	_, beforeExpiry := h.sim.corpses[id]
	h.sim.mu.Unlock()
	if !beforeExpiry {
		t.Fatal("empty corpse disappeared before its original deadline")
	}
	h.step()
	h.sim.mu.Lock()
	_, afterExpiry := h.sim.corpses[id]
	h.sim.mu.Unlock()
	if afterExpiry {
		t.Fatal("empty corpse survived its original deadline")
	}
}

func TestBusyInventoryLeavesCorpseRetryable(t *testing.T) {
	t.Parallel()
	h, player, _, id := armedAgainst(t, vnet.MobKindVargr, [3]float64{0.5, 64, -1.5})
	h.killWithTheStarterBlade(player, id)
	if _, err := player.OpenLoot(protocol.LootOpenRequest{CorpseID: id, ClientTick: 1}); err != nil {
		t.Fatal(err)
	}
	player.inventory.mu.Lock()
	reason, err := player.TakeLoot(protocol.LootTakeRequest{CorpseID: id, EntryID: 1, Revision: 1, ClientTick: 1})
	player.inventory.mu.Unlock()
	if err == nil || reason != vnet.RefusalReasonInventoryBusy {
		t.Fatalf("busy take = %s, %v", reason, err)
	}
	h.sim.mu.Lock()
	entries := len(h.sim.corpses[id].container.entries)
	h.sim.mu.Unlock()
	if entries != 1 {
		t.Errorf("busy inventory consumed loot; %d entries remain", entries)
	}
}

func TestInventoryAndOpenLootStatesRetryIndependently(t *testing.T) {
	t.Parallel()
	var rejectInventory, rejectLoot bool
	sim, err := NewSim(DefaultTickRate, 8, testWorldSeed, dropTerrain{groundTop: 63}, refusedEdits{}, testEntityIDs(), slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatal(err)
	}
	deliver := func(frame []byte) bool {
		kind := vnet.GetRootAsEnvelope(frame, 0).PayloadType()
		switch kind {
		case vnet.PayloadInventoryState:
			return !rejectInventory
		case vnet.PayloadLootState:
			return !rejectLoot
		default:
			return true
		}
	}
	player, err := sim.JoinCharacter(1, testPlayerID(1), 1, "Astrid", [3]float32{0.5, 64, 0.5}, testAppearance(), nil, deliver)
	if err != nil {
		t.Fatal(err)
	}
	sim.mu.Lock()
	player.inventoryDirty = false
	sim.corpses[9] = &corpse{
		entityID: 9, kind: vnet.MobKindDraugr, pos: [3]float64{0.5, 64, 0.5}, chunk: player.chunk,
		owner: player.corpseOwner(), expiresTick: sim.corpseLifetimeTicks,
		container: corpseContainer{revision: 1, entries: []corpseEntry{
			{entryID: 1, stack: stackOf(ItemBone, 1)},
			{entryID: 2, stack: stackOf(ItemBone, 1)},
		}},
	}
	sim.mu.Unlock()
	if _, err := player.OpenLoot(protocol.LootOpenRequest{CorpseID: 9, ClientTick: 1}); err != nil {
		t.Fatal(err)
	}
	if _, err := player.TakeLoot(protocol.LootTakeRequest{CorpseID: 9, EntryID: 1, Revision: 1, ClientTick: 1}); err != nil {
		t.Fatal(err)
	}

	rejectInventory = true
	sim.Step(1)
	sim.mu.Lock()
	firstInventoryDirty, firstLootDirty := player.inventoryDirty, player.lootDirty
	sim.mu.Unlock()
	if !firstInventoryDirty || firstLootDirty {
		t.Fatalf("after inventory rejection dirty inventory/loot = %v/%v, want true/false", firstInventoryDirty, firstLootDirty)
	}

	rejectInventory, rejectLoot = false, true
	sim.mu.Lock()
	player.lootDirty = true
	sim.mu.Unlock()
	sim.Step(2)
	sim.mu.Lock()
	secondInventoryDirty, secondLootDirty := player.inventoryDirty, player.lootDirty
	sim.mu.Unlock()
	if secondInventoryDirty || !secondLootDirty {
		t.Fatalf("after loot rejection dirty inventory/loot = %v/%v, want false/true", secondInventoryDirty, secondLootDirty)
	}
}

func TestCorpseExpiresAtExactlyTenSimulationMinutes(t *testing.T) {
	t.Parallel()
	for _, rate := range []uint8{1, DefaultTickRate, 60, 255} {
		t.Run(time.Duration(rate).String(), func(t *testing.T) {
			h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
			h.sim.mu.Lock()
			h.sim.corpses[9] = &corpse{entityID: 9, kind: vnet.MobKindVargr, container: corpseContainer{entries: []corpseEntry{{entryID: 1, stack: stackOf(ItemVargrPelt, 1)}}, revision: 1}, expiresTick: h.sim.corpseLifetimeTicks}
			h.sim.mu.Unlock()
			h.advance(int(h.sim.corpseLifetimeTicks) - 1)
			h.sim.mu.Lock()
			_, before := h.sim.corpses[9]
			h.sim.mu.Unlock()
			if !before {
				t.Fatal("corpse expired before its exact deadline")
			}
			h.step()
			h.sim.mu.Lock()
			_, after := h.sim.corpses[9]
			h.sim.mu.Unlock()
			if after {
				t.Fatal("corpse survived its exact deadline")
			}
		})
	}
}

func TestDespawnLeavesNoCorpseOrLoot(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	id := h.placeSpeciesAt(vnet.MobKindDraugr, [3]float64{0.5, 64, 0.5})
	h.step()
	h.sim.mu.Lock()
	_, mobExists := h.sim.mobs[id]
	_, corpseExists := h.sim.corpses[id]
	h.sim.mu.Unlock()
	if mobExists || corpseExists || h.sim.DropCount() != 0 {
		t.Fatalf("dawn despawn left mob=%v corpse=%v drops=%d", mobExists, corpseExists, h.sim.DropCount())
	}
}
