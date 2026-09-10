package game

import (
	"errors"
	"math"
	"sync"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

var (
	// ErrBossRewardBusy refuses an inventory or silver mutation while a boss reward
	// owns this character's pack.
	ErrBossRewardBusy = errors.New("game: the inventory has a pending boss reward")
	// ErrBossRewardClaim refuses a reservation or transition the claim does not allow.
	ErrBossRewardClaim = errors.New("game: invalid boss reward reservation")
)

// maxBossRewardEntries is the width of the persisted taken-entry mask.
const maxBossRewardEntries = 64

// BossRewardGrant contains already-rolled entitlements, never RNG instructions.
// Partial preserves TakeAllLoot's entry-order fit policy; a purse that cannot hold
// the silver refuses the whole grant, as TakeAllLoot does. No gameplay producer
// calls the reservation API until the durable coordinator lands.
type BossRewardGrant struct {
	Entries            []protocol.InventoryStack
	Silver, Experience uint32
	Partial            bool
}

// BossRewardImage is the immutable character postimage a claim publishes, and which
// parts of the grant it contains: Entries is a mask over Grant.Entries.
type BossRewardImage struct {
	Life               Life
	Entries            uint64
	Silver, Experience bool
}

type bossRewardPhase uint8

const (
	bossRewardReserved bossRewardPhase = iota
	bossRewardSealed
	bossRewardPublished
	bossRewardFinished
	bossRewardAborted
)

// BossRewardClaim owns only bounded values, never a simulation, cache or player
// pointer, so the coordinator may keep it across a detach. Its mutex never spans I/O
// and never acquires a gameplay lock; live methods take Sim -> inventory -> claim.
//
// The live lifecycle is Reserve -> Seal -> Publish -> Finish, or Reserve -> Abort.
// Between Reserve and Finish every inventory and silver mutation of the owner is
// refused, and death wear before publication is deferred onto the published image.
type BossRewardClaim struct {
	mu             sync.Mutex
	owner          InstanceCharacter
	epoch          uint64
	phase          bossRewardPhase
	image          BossRewardImage
	experience     uint32
	baseExperience uint32
}

// Seal marks the point before durable intent I/O. Abort is forbidden afterwards,
// including after an uncertain write. The returned image is the one to persist.
func (c *BossRewardClaim) Seal() (BossRewardImage, error) {
	if c == nil {
		return BossRewardImage{}, ErrBossRewardClaim
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.phase != bossRewardReserved {
		return BossRewardImage{}, ErrBossRewardClaim
	}
	c.phase = bossRewardSealed
	return c.image, nil
}

// ReserveBossReward is the manager boundary for a connected character. The future
// coordinator first establishes the Store barrier, then calls here after all Store
// I/O has returned. durable is that barrier's baseline: it supplies the epoch and
// any already-durable offline experience, while the live pack remains authority.
// A character inside an instance is imaged at its portal return, as Records does.
func (m *InstanceManager) ReserveBossReward(p *Player, durable Life, grant BossRewardGrant) (*BossRewardClaim, BossRewardImage, error) {
	if p == nil {
		return nil, BossRewardImage{}, ErrBossRewardClaim
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	if m.closed || !p.sim.onlineLocked(p) {
		return nil, BossRewardImage{}, ErrBossRewardClaim
	}
	var returnPos *[3]float32
	if p.sim.dungeon != nil {
		entry, ok := m.portalEntries[InstanceCharacter{p.playerID, p.characterID}]
		if !ok || entry.Session.Sim != p.sim {
			return nil, BossRewardImage{}, ErrBossRewardClaim
		}
		pos := entry.Return
		returnPos = &pos
	}
	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	return p.reserveBossRewardLocked(durable, grant, returnPos)
}

// reserveBossRewardLocked validates everything before changing anything, so every
// refusal leaves the player exactly as it found them. The caller holds sim.mu and
// the inventory lock.
func (p *Player) reserveBossRewardLocked(durable Life, grant BossRewardGrant, returnPos *[3]float32) (*BossRewardClaim, BossRewardImage, error) {
	if err := p.cannotActLocked(); err != nil {
		return nil, BossRewardImage{}, err
	}
	if p.rewardInventoryBusyLocked() {
		return nil, BossRewardImage{}, ErrBossRewardBusy
	}
	if durable.BossRewardEpoch != p.bossRewardEpoch || p.bossRewardEpoch == math.MaxUint64 || len(grant.Entries) > maxBossRewardEntries {
		return nil, BossRewardImage{}, ErrBossRewardClaim
	}
	if err := durable.Validate(); err != nil {
		return nil, BossRewardImage{}, err
	}
	if grant.Silver > math.MaxUint32-p.inventory.silver {
		return nil, BossRewardImage{}, ErrBossRewardClaim
	}
	next := p.inventory.slots
	image := BossRewardImage{}
	for i, entry := range grant.Entries {
		stack := inventoryStack{item: ItemID(entry.ItemID), count: entry.Count, durability: entry.Durability, maxDurability: entry.MaxDurability}
		definition, ok := itemByID(stack.item)
		if !ok || stack.item == ItemNone || stack.count == 0 || !validDropWear(stack, definition) {
			return nil, BossRewardImage{}, ErrBossRewardClaim
		}
		// Whole entries only, on a copy: the insertion rule TakeAllLoot uses.
		trial := next
		if trial.insertStack(stack) != 0 {
			if grant.Partial {
				continue
			}
			return nil, BossRewardImage{}, ErrBossRewardClaim
		}
		next = trial
		image.Entries |= uint64(1) << i
	}
	image.Silver = grant.Silver != 0
	image.Experience = grant.Experience != 0
	if image.Entries == 0 && !image.Silver && !image.Experience {
		return nil, BossRewardImage{}, ErrBossRewardClaim
	}

	// Durable experience joins the live total before the capture, so the baseline the
	// grant is added to includes it. Experience earned later stays live; publication
	// adds only this grant rather than assigning an obsolete total.
	if durable.Experience > p.experience {
		p.sim.awardExperienceLocked(p, durable.Experience-p.experience)
	}
	image.Life = p.recordLocked()
	if returnPos != nil {
		for axis, value := range returnPos {
			image.Life.Pos[axis] = float64(value)
		}
	}
	image.Life.Slots = next.stored()
	image.Life.Silver += grant.Silver
	baseExperience := image.Life.Experience
	image.Life.Experience = experienceAfter(baseExperience, grant.Experience)
	image.Life.BossRewardEpoch++

	claim := &BossRewardClaim{
		owner: InstanceCharacter{p.playerID, p.characterID}, epoch: p.bossRewardEpoch,
		phase: bossRewardReserved, image: image,
		experience: grant.Experience, baseExperience: baseExperience,
	}
	p.rewardClaim = claim
	return claim, image, nil
}

func (p *Player) ownsBossRewardLocked(c *BossRewardClaim) bool {
	return c != nil && p.rewardClaim == c && c.owner == (InstanceCharacter{p.playerID, p.characterID})
}

// rewardInventoryBusyLocked is the guard every inventory and silver mutation asks
// before its side effects. The caller holds sim.mu.
func (p *Player) rewardInventoryBusyLocked() bool {
	return p.rewardClaim != nil
}

// rewardWearDeferredLocked reports whether death wear must be remembered rather than
// spent: until publication the live pack is the reserved image's baseline, and the
// image is where that wear belongs. After publication the pack is the image, so wear
// is spent on it directly. The caller holds sim.mu and the inventory lock.
func (p *Player) rewardWearDeferredLocked() bool {
	c := p.rewardClaim
	if c == nil {
		return false
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.phase < bossRewardPublished
}

// PublishBossReward follows a successful durable character write. It publishes the
// image's pack, purse and epoch, spends wear deferred since the reservation, and adds
// the grant to the live experience so experience earned during the I/O is kept.
// Position, life and hunger stay live. Inventory stays unavailable until Finish.
func (p *Player) PublishBossReward(c *BossRewardClaim) error {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	if !p.ownsBossRewardLocked(c) {
		return ErrBossRewardClaim
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.phase != bossRewardSealed || p.bossRewardEpoch != c.epoch {
		return ErrBossRewardClaim
	}
	slots := restoredSlots(c.image.Life.Slots)
	p.rewardDeathDebt.apply(&slots)
	p.rewardDeathDebt = rewardDeathDebt{}
	p.inventory.slots = slots
	p.inventory.silver = c.image.Life.Silver
	p.bossRewardEpoch = c.image.Life.BossRewardEpoch
	p.sim.awardExperienceLocked(p, c.experience)
	p.inventoryDirty = true
	p.refreshWornLocked()
	c.phase = bossRewardPublished
	return nil
}

// FinishBossReward follows durable journal acknowledgement, while the Store barrier
// is still owned; the coordinator releases that barrier only after this succeeds.
func (p *Player) FinishBossReward(c *BossRewardClaim) error {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	if !p.ownsBossRewardLocked(c) {
		return ErrBossRewardClaim
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.phase != bossRewardPublished {
		return ErrBossRewardClaim
	}
	p.rewardClaim = nil
	c.phase = bossRewardFinished
	return nil
}

// AbortBossReward is only for a failure before sealing. The pack was never replaced,
// so the wear deferred since the reservation is spent on it here.
func (p *Player) AbortBossReward(c *BossRewardClaim) error {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	if !p.ownsBossRewardLocked(c) {
		return ErrBossRewardClaim
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.phase != bossRewardReserved {
		return ErrBossRewardClaim
	}
	if p.rewardDeathDebt.pending() {
		p.rewardDeathDebt.apply(&p.inventory.slots)
		p.rewardDeathDebt = rewardDeathDebt{}
		p.inventoryDirty = true
		p.refreshWornLocked()
	}
	p.rewardClaim = nil
	c.phase = bossRewardAborted
	return nil
}

// PublishRemembered applies the sealed reward to a life captured before live
// publication, after its character detached. The capture's own pack is discarded for
// the image, and the wear it carried is spent on the image instead. Part 5 must own
// the token exclusively and replace the remembered life before any resume; no
// runtime caller exists yet. Disk still receives Seal's image, not this value.
func (c *BossRewardClaim) PublishRemembered(owner InstanceCharacter, current Life) (Life, error) {
	if c == nil {
		return Life{}, ErrBossRewardClaim
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if owner != c.owner || c.phase != bossRewardSealed || current.BossRewardEpoch != c.epoch {
		return Life{}, ErrBossRewardClaim
	}
	slots := restoredSlots(c.image.Life.Slots)
	current.rewardDeathDebt.apply(&slots)
	current.rewardDeathDebt = rewardDeathDebt{}
	current.Slots = slots.stored()
	current.Silver = c.image.Life.Silver
	current.Experience = experienceAfter(max(current.Experience, c.baseExperience), c.experience)
	current.BossRewardEpoch = c.image.Life.BossRewardEpoch
	c.phase = bossRewardPublished
	return current, nil
}

// FinishRemembered is the acknowledgement boundary for a detached owner. It accepts
// only a life that already carries the published epoch, whichever publication put it
// there.
func (c *BossRewardClaim) FinishRemembered(owner InstanceCharacter, current Life) error {
	if c == nil {
		return ErrBossRewardClaim
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if owner != c.owner || c.phase != bossRewardPublished || current.BossRewardEpoch != c.image.Life.BossRewardEpoch {
		return ErrBossRewardClaim
	}
	c.phase = bossRewardFinished
	return nil
}

// maxRewardDeathDebt is where deferred deaths stop mattering: 46 applications of
// wornByDeath take every uint16 durability to zero, which TestRewardDeathDebtSaturates
// pins. The pack is frozen between reservation and publication, so bounded per-slot
// counts are exactly equivalent to replaying every death, and never reach an item the
// image inserted after the deaths they represent.
const maxRewardDeathDebt uint8 = 46

type rewardDeathDebt [protocol.InventorySlots]uint8

func (d rewardDeathDebt) pending() bool {
	return d != rewardDeathDebt{}
}

// add records one death against the slots applyDeathPenaltyLocked would have worn.
func (d *rewardDeathDebt) add(slots slotTable) {
	for i, s := range slots {
		if carriedOnPerson(i) && s.durable() {
			d[i] = min(d[i]+1, maxRewardDeathDebt)
		}
	}
}

func (d rewardDeathDebt) apply(slots *slotTable) {
	for i, n := range d {
		if !slots[i].durable() {
			continue
		}
		for range n {
			slots[i].durability = wornByDeath(slots[i].durability)
		}
	}
}

// stored is the slot table in the shape a Life and the wire carry.
func (t slotTable) stored() [protocol.InventorySlots]protocol.InventoryStack {
	var out [protocol.InventorySlots]protocol.InventoryStack
	for i, stack := range t {
		out[i] = protocol.InventoryStack{ItemID: uint16(stack.item), Count: stack.count, Durability: stack.durability, MaxDurability: stack.maxDurability}
	}
	return out
}
