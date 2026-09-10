package game

import (
	"errors"
	"fmt"
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func rewardStack(item ItemID, count, durability, maxDurability uint16) protocol.InventoryStack {
	return protocol.InventoryStack{ItemID: uint16(item), Count: count, Durability: durability, MaxDurability: maxDurability}
}

func rewardPlayer(t *testing.T) (*vitalsHarness, *Player) {
	t.Helper()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	p, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	return h, p
}

func reserveReward(p *Player, durable Life, grant BossRewardGrant) (*BossRewardClaim, BossRewardImage, error) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	return p.reserveBossRewardLocked(durable, grant, nil)
}

// mustReserveReward uses the player's own capture as the durable baseline, which is
// what a Store barrier returns for a character with nothing unsaved.
func mustReserveReward(t *testing.T, p *Player, grant BossRewardGrant) (*BossRewardClaim, BossRewardImage) {
	t.Helper()
	claim, image, err := reserveReward(p, p.Record(), grant)
	if err != nil {
		t.Fatalf("reserving %+v: %v", grant, err)
	}
	return claim, image
}

type rewardPack struct {
	slots      slotTable
	silver     uint32
	experience uint32
	epoch      uint64
}

func rewardPackOf(p *Player) rewardPack {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	return rewardPack{p.inventory.slots, p.inventory.silver, p.experience, p.bossRewardEpoch}
}

func setRewardPurse(p *Player, silver, experience uint32) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	p.inventory.silver, p.experience = silver, experience
}

func rewardBusy(p *Player) bool {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	return p.rewardInventoryBusyLocked()
}

// riseAgain waits out a death and ends the respawn protection, so the next hurt is a
// real death rather than a blow the protection window absorbs.
func riseAgain(t *testing.T, h *vitalsHarness, p *Player) {
	t.Helper()
	for range 20 * int(DefaultTickRate) {
		h.sim.mu.Lock()
		alive := p.alive()
		if alive {
			p.protectionTicks = 0
		}
		h.sim.mu.Unlock()
		if alive {
			return
		}
		h.step()
	}
	t.Fatal("the player never respawned")
}

func TestBossRewardReservationImagesTheGrantWithoutTouchingTheLivePack(t *testing.T) {
	t.Parallel()
	_, p := rewardPlayer(t)
	setRewardPurse(p, 10, 100)
	before := rewardPackOf(p)
	grant := BossRewardGrant{
		Entries: []protocol.InventoryStack{
			rewardStack(ItemBone, 3, 0, 0),
			rewardStack(ItemRustySword, 1, 40, RustySwordMaxDurability),
		},
		Silver: 7, Experience: 30,
	}
	claim, image := mustReserveReward(t, p, grant)

	if got := rewardPackOf(p); got != before {
		t.Fatalf("reservation changed the live pack: %+v, want %+v", got, before)
	}
	bones, blade := 0, false
	for slot, stack := range image.Life.Slots {
		if stack.ItemID == uint16(ItemBone) {
			bones += int(stack.Count)
		}
		if slot != 0 && stack == grant.Entries[1] {
			blade = true
		}
	}
	if bones != 3 || !blade || image.Life.Slots[0] != before.slots.stored()[0] {
		t.Fatalf("image slots = %+v, want the starter blade, three bones and the worn blade", image.Life.Slots)
	}
	if image.Life.Silver != 17 || image.Life.Experience != 130 || image.Life.BossRewardEpoch != 1 {
		t.Fatalf("image purse/experience/epoch = %d/%d/%d, want 17/130/1", image.Life.Silver, image.Life.Experience, image.Life.BossRewardEpoch)
	}
	if image.Entries != 0b11 || !image.Silver || !image.Experience {
		t.Fatalf("image receipt = %+v, want both entries, silver and experience", image)
	}
	sealed, err := claim.Seal()
	if err != nil || sealed != image {
		t.Fatalf("Seal = %v, image changed %v", err, sealed != image)
	}
	if _, err := claim.Seal(); !errors.Is(err, ErrBossRewardClaim) {
		t.Fatalf("second Seal = %v, want refused", err)
	}
}

func TestBossRewardReservationRefusalsInstallNothing(t *testing.T) {
	t.Parallel()
	bone := rewardStack(ItemBone, 1, 0, 0)
	many := make([]protocol.InventoryStack, maxBossRewardEntries+1)
	for i := range many {
		many[i] = bone
	}
	for _, tc := range []struct {
		name    string
		grant   BossRewardGrant
		prepare func(t *testing.T, h *vitalsHarness, p *Player, durable *Life)
	}{
		{name: "nothing granted"},
		{name: "an unregistered item", grant: BossRewardGrant{Entries: []protocol.InventoryStack{rewardStack(ItemID(math.MaxUint16), 1, 0, 0)}}},
		{name: "the empty item", grant: BossRewardGrant{Entries: []protocol.InventoryStack{rewardStack(ItemNone, 1, 0, 0)}}},
		{name: "an empty entry", grant: BossRewardGrant{Entries: []protocol.InventoryStack{rewardStack(ItemBone, 0, 0, 0)}}},
		{name: "wear on a wearless item", grant: BossRewardGrant{Entries: []protocol.InventoryStack{rewardStack(ItemBone, 1, 3, 0)}}},
		{name: "wear past the maximum", grant: BossRewardGrant{Entries: []protocol.InventoryStack{rewardStack(ItemRustySword, 1, RustySwordMaxDurability+1, RustySwordMaxDurability)}}},
		{name: "more entries than the receipt mask", grant: BossRewardGrant{Entries: many, Partial: true}},
		{name: "a purse that cannot hold the silver", grant: BossRewardGrant{Silver: math.MaxUint32, Experience: 1},
			prepare: func(_ *testing.T, _ *vitalsHarness, p *Player, _ *Life) { setRewardPurse(p, 1, 0) }},
		{name: "a whole grant that does not fit", grant: BossRewardGrant{Entries: []protocol.InventoryStack{bone}, Experience: 1},
			prepare: func(t *testing.T, _ *vitalsHarness, p *Player, _ *Life) { packWithEmptySlots(t, p, 0) }},
		{name: "a partial grant of which nothing fits", grant: BossRewardGrant{Entries: []protocol.InventoryStack{bone}, Partial: true},
			prepare: func(t *testing.T, _ *vitalsHarness, p *Player, _ *Life) { packWithEmptySlots(t, p, 0) }},
		{name: "a stale durable epoch", grant: BossRewardGrant{Experience: 1},
			prepare: func(_ *testing.T, _ *vitalsHarness, _ *Player, durable *Life) { durable.BossRewardEpoch = 1 }},
		{name: "an invalid durable baseline", grant: BossRewardGrant{Experience: 1},
			prepare: func(_ *testing.T, _ *vitalsHarness, _ *Player, durable *Life) { durable.Pos[0] = math.NaN() }},
		{name: "a dead character", grant: BossRewardGrant{Experience: 1},
			prepare: func(_ *testing.T, h *vitalsHarness, p *Player, _ *Life) { h.hurt(p, PlayerMaxHealth) }},
		{name: "a leaving character", grant: BossRewardGrant{Experience: 1},
			prepare: func(_ *testing.T, h *vitalsHarness, p *Player, _ *Life) {
				h.sim.mu.Lock()
				p.leaving = true
				h.sim.mu.Unlock()
			}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			h, p := rewardPlayer(t)
			durable := p.Record()
			if tc.prepare != nil {
				tc.prepare(t, h, p, &durable)
			}
			before := rewardPackOf(p)
			claim, _, err := reserveReward(p, durable, tc.grant)
			if err == nil || claim != nil {
				t.Fatalf("reservation = %v, %v; want refused", claim, err)
			}
			if got := rewardPackOf(p); got != before {
				t.Errorf("a refused reservation changed the character: %+v, want %+v", got, before)
			}
			if rewardBusy(p) {
				t.Error("a refused reservation left the inventory busy")
			}
		})
	}
}

func TestAPartialBossRewardStepsOverWhatDoesNotFitInEntryOrder(t *testing.T) {
	t.Parallel()
	_, p := rewardPlayer(t)
	packWithEmptySlots(t, p, 1)
	entries := []protocol.InventoryStack{
		rewardStack(ItemBone, 2, 0, 0),
		rewardStack(ItemRustySword, 1, 40, RustySwordMaxDurability),
		rewardStack(ItemBone, 1, 0, 0),
	}
	if claim, _, err := reserveReward(p, p.Record(), BossRewardGrant{Entries: entries}); err == nil || claim != nil {
		t.Fatal("a whole grant that does not fit was reserved")
	}
	_, image := mustReserveReward(t, p, BossRewardGrant{Entries: entries, Partial: true})
	if image.Entries != 0b101 {
		t.Errorf("entry mask = %b, want the two bones around the blade", image.Entries)
	}
	if got := image.Life.Slots[0]; got != rewardStack(ItemBone, 3, 0, 0) {
		t.Errorf("slot 0 = %+v, want both bone entries merged", got)
	}
}

func TestBossRewardReservationFoldsDurableExperienceIntoItsBaseline(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name                   string
		durable, live, wantNow uint32
	}{
		{"offline experience already durable", 400, 100, 400},
		{"live experience ahead of the disk", 50, 100, 100},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			_, p := rewardPlayer(t)
			setRewardPurse(p, 0, tc.live)
			durable := p.Record()
			durable.Experience = tc.durable
			_, image, err := reserveReward(p, durable, BossRewardGrant{Experience: 50})
			if err != nil {
				t.Fatal(err)
			}
			if got := rewardPackOf(p).experience; got != tc.wantNow {
				t.Errorf("live experience = %d, want %d", got, tc.wantNow)
			}
			if image.Life.Experience != tc.wantNow+50 {
				t.Errorf("image experience = %d, want %d", image.Life.Experience, tc.wantNow+50)
			}
		})
	}
}

// Each case is one inventory or silver change a pending reward must refuse, and the
// same change succeeding once the reward is gone: that second half is what proves the
// reward, and not some other precondition, was what held it.
func TestAPendingBossRewardRefusesEveryInventoryAndSilverMutation(t *testing.T) {
	t.Parallel()
	trade := func(onRight bool) func(t *testing.T) (*Player, func() error) {
		return func(t *testing.T) (*Player, func() error) {
			h, left, right := tradeTransactionPlayers(t)
			stone, logs := stackOf(ItemStone, 8), stackOf(ItemLog, 4)
			putTradeStack(left, 0, stone)
			putTradeStack(right, 0, logs)
			fundTradePurse(left, 100)
			subject := left
			if onRight {
				subject = right
			}
			return subject, func() error {
				h.sim.mu.Lock()
				defer h.sim.mu.Unlock()
				result := settlePlayerTradeLocked(
					playerTradeSide{player: left, offers: oneTradeOffer(0, 0, stone), silver: 30},
					playerTradeSide{player: right, offers: oneTradeOffer(0, 0, logs)},
				)
				if result != playerTradeSettlementComplete {
					return fmt.Errorf("settlement = %d", result)
				}
				return nil
			}
		}
	}
	command := func(line string) func(t *testing.T) (*Player, func() error) {
		return func(t *testing.T) (*Player, func() error) {
			_, p, _ := commandPlayer(t, true)
			return p, func() error {
				outcome, err := p.Chat(line)
				if err == nil && outcome.Inventory == nil {
					err = errors.New(outcome.PrivateText)
				}
				return err
			}
		}
	}
	loot := func(silver bool, all bool) func(t *testing.T) (*Player, func() error) {
		return func(t *testing.T) (*Player, func() error) {
			h, p := rewardPlayer(t)
			if silver {
				standSilverCorpse(t, h, p, 25)
			} else {
				standCorpse(t, h, p, stackOf(ItemBone, 1))
			}
			tick := uint32(1)
			return p, func() error {
				tick++
				if all {
					_, err := p.TakeAllLoot(protocol.LootTakeAllRequest{CorpseID: takeAllCorpse, Revision: 1, ClientTick: tick})
					return err
				}
				_, err := p.TakeLoot(protocol.LootTakeRequest{CorpseID: takeAllCorpse, EntryID: 1, Revision: 1, ClientTick: tick})
				return err
			}
		}
	}
	for _, tc := range []struct {
		name  string
		setup func(t *testing.T) (*Player, func() error)
	}{
		{"inventory move", func(t *testing.T) (*Player, func() error) {
			_, p := rewardPlayer(t)
			putTradeStack(p, 3, stackOf(ItemStone, 5))
			return p, func() error {
				_, err := p.MoveInventory(protocol.InventoryMoveRequest{From: 3, To: 4, Count: 5})
				return err
			}
		}},
		{"consumption", func(t *testing.T) (*Player, func() error) {
			h := newStructureHarness(t)
			p, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.give(p, 4, ItemRawMeat, 2)
			h.sim.mu.Lock()
			p.hunger = 10
			h.sim.mu.Unlock()
			tick := uint32(0)
			return p, func() error {
				tick++
				_, _, err := p.Consume(protocol.ConsumeRequest{Slot: 4, ClientTick: tick})
				return err
			}
		}},
		{"crafting", func(t *testing.T) (*Player, func() error) {
			h := newStructureHarness(t)
			p, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			for id, r := range recipeTable {
				if r.station != vnet.StructureKindUnknown {
					continue
				}
				h.stockPack(p, r.ingredients...)
				tick := uint32(0)
				return p, func() error {
					tick++
					_, err := p.Craft(protocol.CraftRequest{Recipe: id, ClientTick: tick})
					return err
				}
			}
			t.Fatal("no recipe is craftable without a station")
			return nil, nil
		}},
		{"repair", func(t *testing.T) (*Player, func() error) {
			h := newStructureHarness(t)
			p, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.stockPack(p, ingredient{ItemSharpeningStone, 1})
			h.equipWorn(p, 1, ItemRustySword, 49)
			tick := uint32(0)
			return p, func() error {
				tick++
				_, err := p.Repair(protocol.RepairRequest{KitSlot: 0, TargetSlot: 1, ClientTick: tick})
				return err
			}
		}},
		{"taking one loot entry", loot(false, false)},
		{"taking all loot", loot(false, true)},
		{"taking a corpse's silver", loot(true, true)},
		{"a vendor purchase", func(t *testing.T) (*Player, func() error) {
			h, p, _, r := stall(t, vnet.ResidentRoleSmith)
			h.fund(p, 50)
			h.step()
			tick := uint32(1)
			return p, func() error {
				tick++
				_, err := p.Trade(tradeFor(r, ItemPickaxe, 1, true, 1, tick))
				return err
			}
		}},
		{"dropping a stack", func(t *testing.T) (*Player, func() error) {
			_, p := rewardPlayer(t)
			putTradeStack(p, 3, stackOf(ItemStone, 17))
			tick := uint32(0)
			return p, func() error {
				tick++
				_, err := p.DropItem(protocol.DropItemRequest{Slot: 3, ClientTick: tick})
				return err
			}
		}},
		{"collecting a drop", func(t *testing.T) (*Player, func() error) {
			h, p := rewardPlayer(t)
			if _, spawned := h.sim.spawnDrop(ItemStone, 1, [3]int64{1, 64, 0}); !spawned {
				t.Fatal("the drop was not spawned")
			}
			return p, func() error {
				h.advance(dropPickupDelayTicks + 1)
				return nil
			}
		}},
		{"planting a structure", func(t *testing.T) (*Player, func() error) {
			h := newStructureHarness(t)
			p, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.give(p, 0, ItemCampfire, 1)
			return p, func() error {
				_, _, err := p.PlaceStructure(placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingNorth))
				return err
			}
		}},
		{"a development item grant", command("/additem 1 2")},
		{"a development silver grant", command("/additem 35 1000")},
		{"a trade settlement for the reward's owner", trade(false)},
		{"a trade settlement for the counterparty", trade(true)},
		{"a bow launch", func(t *testing.T) (*Player, func() error) {
			h, p := rewardPlayer(t)
			putTradeStack(p, 0, stackOf(ItemBow, 1))
			putTradeStack(p, 1, stackOf(ItemArrow, 2))
			h.aimAt(p, math.Pi/2, math.Pi/6)
			tick := uint32(0)
			return p, func() error {
				h.sim.mu.Lock()
				pending := p.pendingSwing != nil
				h.sim.mu.Unlock()
				if !pending {
					tick++
					if _, err := p.Attack(protocol.AttackRequest{Slot: 0, ClientTick: tick}); err != nil {
						return err
					}
				}
				h.sim.mu.Lock()
				p.resolveAttackLocked()
				h.sim.mu.Unlock()
				return nil
			}
		}},
		{"shield wear", func(t *testing.T) (*Player, func() error) {
			h, p, _ := raisedShield(t)
			mobID := h.spawnDraugrAt([3]float32{0.5, 64, -1.0})
			h.aimAt(p, 0, 0)
			return p, func() error {
				armMobBlow(t, h, mobID, p)
				h.step()
				return nil
			}
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			p, mutate := tc.setup(t)
			claim, _ := mustReserveReward(t, p, BossRewardGrant{Experience: 1})
			before := rewardPackOf(p)
			_ = mutate()
			if got := rewardPackOf(p); got.slots != before.slots || got.silver != before.silver {
				t.Fatalf("a pending boss reward let the pack change:\n got %+v\nwant %+v", got, before)
			}
			if err := p.AbortBossReward(claim); err != nil {
				t.Fatalf("Abort: %v", err)
			}
			if err := mutate(); err != nil {
				t.Fatalf("the same mutation without a reward was refused: %v", err)
			}
			if got := rewardPackOf(p); got.slots == before.slots && got.silver == before.silver {
				t.Fatal("the mutation changed nothing without a reward either, so the refusal proves nothing")
			}
		})
	}
}

func TestAPendingBossRewardStillJudgesAMeleeSwing(t *testing.T) {
	t.Parallel()
	h, p := rewardPlayer(t)
	mustReserveReward(t, p, BossRewardGrant{Experience: 1})
	h.sim.mu.Lock()
	armed, sampled := p.armedForAttackLocked(0)
	h.sim.mu.Unlock()
	if !sampled || armed.meleeDamage != RustySwordDamage {
		t.Fatalf("melee during a pending reward = %+v sampled %v, want the starter blade's damage", armed, sampled)
	}
}

func TestDeathsBeforePublicationWearThePublishedImageExactlyOnce(t *testing.T) {
	t.Parallel()
	h, p := rewardPlayer(t)
	equipLeatherSet(t, p)
	blade := rewardStack(ItemRustySword, 1, 50, RustySwordMaxDurability)
	claim, _ := mustReserveReward(t, p, BossRewardGrant{Entries: []protocol.InventoryStack{blade}, Silver: 5})
	once := wornByDeath(RustySwordMaxDurability)
	twice := wornByDeath(once)

	h.hurt(p, PlayerMaxHealth)
	h.step()
	if got := rewardPackOf(p).slots[0].durability; got != RustySwordMaxDurability {
		t.Fatalf("a death spent %d wear on the frozen pack", RustySwordMaxDurability-got)
	}
	capture := p.Record()
	if capture.BossRewardEpoch != 0 || capture.Slots[0].Durability != once || capture.Slots[1] != (protocol.InventoryStack{}) {
		t.Fatalf("pre-publication capture = epoch %d slots %+v, want the old epoch and a worn pack without the reward",
			capture.BossRewardEpoch, capture.Slots[:2])
	}

	riseAgain(t, h, p)
	h.hurt(p, PlayerMaxHealth)
	h.step()
	if _, err := claim.Seal(); err != nil {
		t.Fatal(err)
	}
	if err := p.PublishBossReward(claim); err != nil {
		t.Fatal(err)
	}
	pack := rewardPackOf(p)
	if pack.slots[0].durability != twice {
		t.Errorf("the carried blade = %d, want %d after two deferred deaths", pack.slots[0].durability, twice)
	}
	if pack.slots[1] != (inventoryStack{item: ItemRustySword, count: 1, durability: 50, maxDurability: RustySwordMaxDurability}) {
		t.Errorf("the reward blade = %+v, worn by deaths before it arrived", pack.slots[1])
	}
	for _, slot := range []int{equipmentHead, equipmentChest, equipmentLegs} {
		if got := pack.slots[slot].durability; got != wornByDeath(wornByDeath(LeatherArmourMaxDurability)) {
			t.Errorf("equipment slot %d = %d after two deferred deaths", slot, got)
		}
	}
	if pack.epoch != 1 || pack.silver != 5 {
		t.Errorf("published epoch/silver = %d/%d, want 1/5", pack.epoch, pack.silver)
	}

	riseAgain(t, h, p)
	h.hurt(p, PlayerMaxHealth)
	h.step()
	pack = rewardPackOf(p)
	if pack.slots[0].durability != wornByDeath(twice) || pack.slots[1].durability != wornByDeath(50) {
		t.Errorf("a death after publication wore %d/%d, want it spent directly on the image", pack.slots[0].durability, pack.slots[1].durability)
	}
	if capture := p.Record(); capture.BossRewardEpoch != 1 || capture.Slots[1].Durability != wornByDeath(50) {
		t.Errorf("post-publication capture = epoch %d reward %+v", capture.BossRewardEpoch, capture.Slots[1])
	}
	if err := p.FinishBossReward(claim); err != nil {
		t.Fatal(err)
	}
	if rewardBusy(p) {
		t.Error("a finished reward left the inventory busy")
	}
}

func TestBossRewardPublicationKeepsExperienceEarnedDuringTheWrite(t *testing.T) {
	t.Parallel()
	h, p := rewardPlayer(t)
	setRewardPurse(p, 10, 100)
	claim, _ := mustReserveReward(t, p, BossRewardGrant{Silver: 4, Experience: 50})
	h.sim.mu.Lock()
	h.sim.awardExperienceLocked(p, 20)
	h.sim.mu.Unlock()
	sealed, err := claim.Seal()
	if err != nil {
		t.Fatal(err)
	}
	if err := p.PublishBossReward(claim); err != nil {
		t.Fatal(err)
	}
	pack := rewardPackOf(p)
	if pack.experience != 170 || sealed.Life.Experience != 150 {
		t.Errorf("live/image experience = %d/%d, want 170/150", pack.experience, sealed.Life.Experience)
	}
	if pack.silver != 14 || pack.epoch != 1 {
		t.Errorf("live silver/epoch = %d/%d, want 14/1", pack.silver, pack.epoch)
	}
	h.sim.mu.Lock()
	dirty := p.inventoryDirty
	h.sim.mu.Unlock()
	if !dirty {
		t.Error("publication did not owe the client an inventory state")
	}
	// A capture taken after live publication is already the published character, so a
	// detached owner can be acknowledged with it directly.
	capture := p.Record()
	if capture.BossRewardEpoch != 1 || capture.Experience != 170 {
		t.Fatalf("post-publication capture = epoch %d experience %d", capture.BossRewardEpoch, capture.Experience)
	}
	if err := claim.FinishRemembered(InstanceCharacter{p.playerID, p.characterID}, capture); err != nil {
		t.Fatalf("FinishRemembered after live publication: %v", err)
	}
}

func TestBossRewardTransitionsRefuseOutOfOrderAndForeignClaims(t *testing.T) {
	t.Parallel()
	h, p := rewardPlayer(t)
	other, _ := h.join(2, [3]float32{3.5, 64, 0.5})
	refused := func(what string, err error) {
		t.Helper()
		if !errors.Is(err, ErrBossRewardClaim) {
			t.Errorf("%s = %v, want refused", what, err)
		}
	}
	claim, _ := mustReserveReward(t, p, BossRewardGrant{Experience: 1})
	if _, _, err := reserveReward(p, p.Record(), BossRewardGrant{Experience: 1}); !errors.Is(err, ErrBossRewardBusy) {
		t.Errorf("a second reservation = %v, want busy", err)
	}
	refused("publish before seal", p.PublishBossReward(claim))
	refused("finish before publish", p.FinishBossReward(claim))
	refused("another character's abort", other.AbortBossReward(claim))
	refused("a nil claim", p.AbortBossReward(nil))
	if _, err := claim.Seal(); err != nil {
		t.Fatal(err)
	}
	refused("abort after seal", p.AbortBossReward(claim))
	refused("another character's publish", other.PublishBossReward(claim))
	if err := p.PublishBossReward(claim); err != nil {
		t.Fatal(err)
	}
	refused("publish twice", p.PublishBossReward(claim))
	refused("abort after publish", p.AbortBossReward(claim))
	if _, err := claim.PublishRemembered(InstanceCharacter{p.playerID, p.characterID}, p.Record()); !errors.Is(err, ErrBossRewardClaim) {
		t.Errorf("detached publication after live publication = %v", err)
	}
	if err := p.FinishBossReward(claim); err != nil {
		t.Fatal(err)
	}
	refused("finish twice", p.FinishBossReward(claim))
	if rewardBusy(p) {
		t.Fatal("a finished reward left the inventory busy")
	}
	if _, image := mustReserveReward(t, p, BossRewardGrant{Experience: 1}); image.Life.BossRewardEpoch != 2 {
		t.Errorf("the next reward images epoch %d, want 2", image.Life.BossRewardEpoch)
	}
}

func TestAnAbortedBossRewardSpendsTheWearItDeferredOnce(t *testing.T) {
	t.Parallel()
	h, p := rewardPlayer(t)
	claim, _ := mustReserveReward(t, p, BossRewardGrant{Experience: 1})
	h.hurt(p, PlayerMaxHealth)
	h.step()
	if err := p.AbortBossReward(claim); err != nil {
		t.Fatal(err)
	}
	once := wornByDeath(RustySwordMaxDurability)
	if got := rewardPackOf(p).slots[0].durability; got != once {
		t.Fatalf("aborted durability = %d, want %d", got, once)
	}
	riseAgain(t, h, p)
	if got := rewardPackOf(p).slots[0].durability; got != once {
		t.Fatalf("the death was charged again after abort: %d", got)
	}
}

func TestADetachedCaptureReceivesTheRewardWithTheWearItCarried(t *testing.T) {
	t.Parallel()
	h, p := rewardPlayer(t)
	setRewardPurse(p, 10, 100)
	owner := InstanceCharacter{p.playerID, p.characterID}
	blade := rewardStack(ItemRustySword, 1, 50, RustySwordMaxDurability)
	claim, _ := mustReserveReward(t, p, BossRewardGrant{Entries: []protocol.InventoryStack{blade}, Silver: 5, Experience: 30})
	h.hurt(p, PlayerMaxHealth)
	h.step()
	h.sim.Leave(p)
	capture := p.Record()
	sealed, err := claim.Seal()
	if err != nil {
		t.Fatal(err)
	}

	foreign := owner
	foreign.CharacterID++
	if _, err := claim.PublishRemembered(foreign, capture); !errors.Is(err, ErrBossRewardClaim) {
		t.Errorf("another character's detached publication = %v", err)
	}
	stale := capture
	stale.BossRewardEpoch++
	if _, err := claim.PublishRemembered(owner, stale); !errors.Is(err, ErrBossRewardClaim) {
		t.Errorf("a detached publication at the wrong epoch = %v", err)
	}
	got, err := claim.PublishRemembered(owner, capture)
	if err != nil {
		t.Fatal(err)
	}
	if got.Slots[0].Durability != wornByDeath(RustySwordMaxDurability) || got.Slots[1] != blade {
		t.Errorf("detached slots = %+v, want the worn carried blade and the unworn reward", got.Slots[:2])
	}
	if got.Silver != 15 || got.Experience != 130 || got.BossRewardEpoch != sealed.Life.BossRewardEpoch || got.rewardDeathDebt.pending() {
		t.Errorf("detached purse/experience/epoch/debt = %d/%d/%d/%v", got.Silver, got.Experience, got.BossRewardEpoch, got.rewardDeathDebt.pending())
	}
	if got.Pos != capture.Pos || got.Health != capture.Health || got.recovery != capture.recovery {
		t.Error("detached publication replaced the remembered life")
	}
	if err := p.PublishBossReward(claim); !errors.Is(err, ErrBossRewardClaim) {
		t.Errorf("live publication after a detached one = %v", err)
	}
	if err := claim.FinishRemembered(owner, capture); !errors.Is(err, ErrBossRewardClaim) {
		t.Errorf("acknowledging the unpublished capture = %v", err)
	}
	if err := claim.FinishRemembered(owner, got); err != nil {
		t.Fatal(err)
	}
	if err := claim.FinishRemembered(owner, got); !errors.Is(err, ErrBossRewardClaim) {
		t.Errorf("acknowledging twice = %v", err)
	}
}

func TestRewardDeathDebtSaturates(t *testing.T) {
	t.Parallel()
	durability := uint16(math.MaxUint16)
	for range maxRewardDeathDebt {
		durability = wornByDeath(durability)
	}
	if durability != 0 {
		t.Fatalf("%d deaths leave %d of the widest durability", maxRewardDeathDebt, durability)
	}
	var debt rewardDeathDebt
	slots := slotTable{0: stackOf(ItemRustySword, 1), 1: stackOf(ItemStone, 5), 20: stackOf(ItemRustySword, 1)}
	for range 3 * int(maxRewardDeathDebt) {
		debt.add(slots)
	}
	if debt[0] != maxRewardDeathDebt || debt[1] != 0 || debt[20] != 0 {
		t.Fatalf("debt = carried %d, wearless %d, stowed %d", debt[0], debt[1], debt[20])
	}
}

func TestInstanceManagerImagesADungeonRewardAtItsPortalReturn(t *testing.T) {
	m, open, request, join := portalHarness(t, 2)
	p := join()
	entry, reason := m.enterPortal(p, request)
	if reason != 0 {
		t.Fatal(reason)
	}
	arrival, _ := world.InstanceAnchors(entry.Session.Seed)
	if err := open.Transfer(p, entry.Session.Sim, [3]float32{float32(arrival.X) + .5, float32(arrival.Y), float32(arrival.Z) + .5}); err != nil {
		t.Fatal(err)
	}
	grant := BossRewardGrant{Experience: 1}
	character := InstanceCharacter{p.playerID, p.characterID}

	m.mu.Lock()
	delete(m.portalEntries, character)
	m.mu.Unlock()
	if claim, _, err := m.ReserveBossReward(p, p.Record(), grant); !errors.Is(err, ErrBossRewardClaim) || claim != nil {
		t.Fatalf("a dungeon character without a portal return = %v", err)
	}
	m.mu.Lock()
	m.portalEntries[character] = entry
	m.mu.Unlock()

	claim, image, err := m.ReserveBossReward(p, p.Record(), grant)
	if err != nil {
		t.Fatal(err)
	}
	for axis, value := range entry.Return {
		if image.Life.Pos[axis] != float64(value) {
			t.Fatalf("image position = %v, want the portal return %v", image.Life.Pos, entry.Return)
		}
	}
	if err := p.AbortBossReward(claim); err != nil {
		t.Fatal(err)
	}
	entry.Session.Sim.Leave(p)
	if _, _, err := m.ReserveBossReward(p, p.Record(), grant); !errors.Is(err, ErrBossRewardClaim) {
		t.Errorf("an offline character = %v, want refused", err)
	}
	if _, _, err := m.ReserveBossReward(nil, Life{}, grant); !errors.Is(err, ErrBossRewardClaim) {
		t.Errorf("a nil player = %v, want refused", err)
	}
}
