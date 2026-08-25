package game

import (
	"errors"
	"fmt"
	"math"
	"math/rand/v2"
	"slices"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Normal-mob loot belongs to the corpse, never to the ground. The roll happens once,
// when Dying completes, and is stored with stable owner and entry identities. Opening
// only projects that settled state and therefore consumes no random numbers.

const mobLootStream = 0x766F78656C6C6F74 // "voxellot"

func newLootRNG(worldSeed int64) *rand.Rand {
	return rand.New(rand.NewPCG(uint64(worldSeed), mobLootStream))
}

type lootRoll struct {
	item     ItemID
	min, max uint16
}

type corpseEntry struct {
	entryID uint64
	stack   inventoryStack
}

// corpseOwner is exactly the stable account-plus-character boundary. Display names
// and connection entity ids may change without changing an earned container right.
type corpseOwner struct {
	playerID    identity.PlayerID
	characterID uint64
}

func corpseOwnerFromPartyKey(key partyMemberKey) corpseOwner {
	return corpseOwner{playerID: key.playerID, characterID: key.characterID}
}

func (p *Player) corpseOwner() corpseOwner {
	if p == nil {
		return corpseOwner{}
	}
	return corpseOwner{playerID: p.playerID, characterID: p.characterID}
}

// corpse is one server-owned normal-mob container. It is deliberately not a mob:
// nothing steps it and spawn ceilings only count Sim.mobs. Its wire projection keeps
// the killed entity id, species, yaw and final resting position continuous.
type corpse struct {
	entityID uint64
	kind     vnet.MobKind
	pos      [3]float64
	yaw      float64
	chunk    world.Coord

	owner       corpseOwner
	entries     []corpseEntry
	revision    uint32
	expiresTick uint64
}

func (c *corpse) ownedBy(p *Player) bool {
	return c != nil && p != nil && c.owner == p.corpseOwner()
}

func (c *corpse) state() protocol.MobState {
	def := mobRegistry[c.kind]
	return protocol.MobState{
		EntityID:  c.entityID,
		Kind:      c.kind,
		Pos:       toWire(c.pos),
		Yaw:       float32(c.yaw),
		Health:    0,
		MaxHealth: def.maxHealth,
		Action:    vnet.MobActionCorpse,
	}
}

type mobSnapshot struct {
	state  protocol.MobState
	chunk  world.Coord
	corpse *corpse
}

// mobSnapshotsLocked merges living/dying mobs and corpses in entity-id order. The
// collection boundary stays internal: a receiver sees one continuous MobState stream.
func (s *Sim) mobSnapshotsLocked(mobs []*mob) []mobSnapshot {
	shown := make([]mobSnapshot, 0, len(mobs)+len(s.corpses))
	states := mobStates(mobs)
	for index, m := range mobs {
		shown = append(shown, mobSnapshot{state: states[index], chunk: m.chunk})
	}
	for _, c := range s.corpses {
		shown = append(shown, mobSnapshot{state: c.state(), chunk: c.chunk, corpse: c})
	}
	slices.SortFunc(shown, func(a, b mobSnapshot) int {
		return compareEntityIDs(a.state.EntityID, b.state.EntityID)
	})
	return shown
}

// makeCorpseLocked is the only killed-mob transition. The caller holds Sim.mu.
func (s *Sim) makeCorpseLocked(m *mob) *corpse {
	owner := corpseOwner{}
	if m.firstHit != nil {
		owner = s.corpseOwnerLocked(m.firstHit, m.pos)
	}
	c := &corpse{
		entityID:    m.entityID,
		kind:        m.kind,
		pos:         m.pos,
		yaw:         m.yaw,
		chunk:       m.chunk,
		owner:       owner,
		entries:     s.rollLootLocked(m),
		revision:    1,
		expiresTick: s.currentTick + s.corpseLifetimeTicks,
	}
	s.corpses[c.entityID] = c
	return c
}

// corpseOwnerLocked resolves normal-party round robin at the death location. Offline
// members are never eligible; living state is deliberately irrelevant. If nobody in
// the retained roster is online and near, the first-tap character keeps the corpse.
func (s *Sim) corpseOwnerLocked(tap *mobTap, pos [3]float64) corpseOwner {
	tapped := partyMemberKey{
		playerID:    tap.playerID,
		characterID: tap.characterID,
		foldedName:  foldPlayerName(tap.characterName),
	}
	partyID := s.partyMemberships[tapped]
	held := s.parties[partyID]
	if partyID == 0 || held == nil || len(held.members) == 0 {
		return corpseOwnerFromPartyKey(tapped)
	}

	start := 0
	for index := range held.members {
		if held.members[index].key == held.lootCursor {
			start = index
			break
		}
	}
	for offset := range len(held.members) {
		index := (start + offset) % len(held.members)
		member := &held.members[index]
		if member.player == nil || !s.onlineLocked(member.player) {
			continue
		}
		distanceSquared := standingDistanceSquared(member.player.pos, pos)
		if math.IsNaN(distanceSquared) ||
			distanceSquared > PartyShareRadius*PartyShareRadius {
			continue
		}
		held.lootCursor = held.members[(index+1)%len(held.members)].key
		return corpseOwnerFromPartyKey(member.key)
	}

	// The fallback is still an assignment. Advance after the tap owner's roster slot
	// when it remains present, so unopened corpses never stall round robin.
	for index := range held.members {
		if held.members[index].key == tapped {
			held.lootCursor = held.members[(index+1)%len(held.members)].key
			break
		}
	}
	return corpseOwnerFromPartyKey(tapped)
}

func standingDistanceSquared(a, b [3]float64) float64 {
	var distance float64
	for axis := range 3 {
		delta := a[axis] - b[axis]
		distance += delta * delta
	}
	return distance
}

// rollLootLocked rolls the species table exactly once, at the Corpse transition.
func (s *Sim) rollLootLocked(m *mob) []corpseEntry {
	table := m.species().loot
	entries := make([]corpseEntry, 0, len(table))
	for _, roll := range table {
		count := roll.min
		if roll.max > roll.min {
			count += uint16(s.loot.IntN(int(roll.max-roll.min) + 1))
		}
		if count == 0 {
			continue
		}
		entries = append(entries, corpseEntry{
			entryID: uint64(len(entries) + 1),
			stack:   stackOf(roll.item, count),
		})
	}
	return entries
}

// expireCorpsesLocked removes bodies at the exact authoritative tick and schedules an
// explicit close for any session that had the container open.
func (s *Sim) expireCorpsesLocked(tick uint64) {
	for id, c := range s.corpses {
		if tick >= c.expiresTick {
			s.removeCorpseLocked(id)
		}
	}
}

func (s *Sim) removeCorpseLocked(id uint64) {
	if _, exists := s.corpses[id]; !exists {
		return
	}
	delete(s.corpses, id)
	for _, player := range s.players {
		if player.openLootID == id {
			player.openLootID = 0
			player.lootDirty = false
			player.queueLootClosedLocked(id)
		}
	}
}

func (p *Player) queueLootClosedLocked(id uint64) {
	if id == 0 || slices.Contains(p.lootClosures, id) {
		return
	}
	p.lootClosures = append(p.lootClosures, id)
}

// OpenLoot validates one open intent and schedules a complete LootState. The caller is
// the session goroutine; no frame is lost to its queue because the tick retries it.
func (p *Player) OpenLoot(req protocol.LootOpenRequest) (vnet.RefusalReason, error) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if err := p.cannotActLocked(); err != nil {
		return vnet.RefusalReasonPlayerIsDead, err
	}
	if p.haveLootOpenTick && !newerTick(req.ClientTick, p.lastLootOpenTick) {
		return vnet.RefusalReasonUnknown, fmt.Errorf("stale loot-open client tick %d; newest is %d", req.ClientTick, p.lastLootOpenTick)
	}
	p.haveLootOpenTick, p.lastLootOpenTick = true, req.ClientTick

	c, reason, err := p.accessibleCorpseLocked(req.CorpseID)
	if err != nil {
		return reason, err
	}
	if p.openLootID != 0 && p.openLootID != c.entityID {
		p.queueLootClosedLocked(p.openLootID)
	}
	p.openLootID = c.entityID
	p.lootDirty = true
	return vnet.RefusalReasonUnknown, nil
}

// TakeLoot transfers one whole entry atomically. Busy and full inventories leave both
// authoritative values untouched; an accepted transfer dirties inventory and loot
// independently until each complete state has reached the session.
func (p *Player) TakeLoot(req protocol.LootTakeRequest) (vnet.RefusalReason, error) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if err := p.cannotActLocked(); err != nil {
		return vnet.RefusalReasonPlayerIsDead, err
	}
	if p.haveLootTakeTick && !newerTick(req.ClientTick, p.lastLootTakeTick) {
		return vnet.RefusalReasonUnknown, fmt.Errorf("stale loot-take client tick %d; newest is %d", req.ClientTick, p.lastLootTakeTick)
	}
	p.haveLootTakeTick, p.lastLootTakeTick = true, req.ClientTick

	c, reason, err := p.accessibleCorpseLocked(req.CorpseID)
	if err != nil {
		return reason, err
	}
	if p.openLootID != c.entityID {
		return vnet.RefusalReasonCorpseUnavailable, errors.New("the corpse container is not open")
	}
	if req.Revision != c.revision {
		return vnet.RefusalReasonStaleRevision, fmt.Errorf("loot revision %d is not current revision %d", req.Revision, c.revision)
	}
	entryIndex := -1
	for index := range c.entries {
		if c.entries[index].entryID == req.EntryID {
			entryIndex = index
			break
		}
	}
	if entryIndex < 0 {
		return vnet.RefusalReasonCorpseUnavailable, fmt.Errorf("loot entry %d is unavailable", req.EntryID)
	}
	if !p.inventory.mu.TryLock() {
		return vnet.RefusalReasonInventoryBusy, errors.New("the inventory is busy")
	}
	defer p.inventory.mu.Unlock()
	if !p.inventory.insertWholeStackLocked(c.entries[entryIndex].stack) {
		return vnet.RefusalReasonInventoryFull, errors.New("the whole loot entry does not fit")
	}

	c.entries = append(c.entries[:entryIndex], c.entries[entryIndex+1:]...)
	c.revision++
	p.inventoryDirty = true
	p.lootDirty = true
	if len(c.entries) == 0 {
		p.sim.removeCorpseLocked(c.entityID)
	}
	return vnet.RefusalReasonUnknown, nil
}

func (p *Player) accessibleCorpseLocked(id uint64) (*corpse, vnet.RefusalReason, error) {
	c := p.sim.corpses[id]
	if c == nil || len(c.entries) == 0 || !withinView(p.chunk, c.chunk, p.sim.viewDistance) {
		return nil, vnet.RefusalReasonCorpseUnavailable, errors.New("the corpse is unavailable")
	}
	if !c.ownedBy(p) {
		return nil, vnet.RefusalReasonLootNotOwned, errors.New("the corpse belongs to another character")
	}
	if distance := boxDistance(playerBox(p.pos), mobRegistry[c.kind].body.boxAt(c.pos)); math.IsNaN(distance) || distance > EditReach {
		return nil, vnet.RefusalReasonOutOfReach, fmt.Errorf("the corpse is %.2f blocks away, past the reach of %.1f", distance, EditReach)
	}
	return c, vnet.RefusalReasonUnknown, nil
}

// canOpenCorpseLocked is the snapshot-side form of the same access rule. It carries
// no reason because a snapshot advertises capabilities rather than refusals.
func (p *Player) canOpenCorpseLocked(c *corpse) bool {
	if c == nil || len(c.entries) == 0 || p.cannotActLocked() != nil ||
		!withinView(p.chunk, c.chunk, p.sim.viewDistance) || !c.ownedBy(p) {
		return false
	}
	return boxDistance(playerBox(p.pos), mobRegistry[c.kind].body.boxAt(c.pos)) <= EditReach
}

func (c *corpse) lootState() protocol.LootState {
	entries := make([]protocol.LootEntry, len(c.entries))
	for index, entry := range c.entries {
		entries[index] = protocol.LootEntry{
			EntryID:       entry.entryID,
			ItemID:        uint16(entry.stack.item),
			Count:         entry.stack.count,
			Durability:    entry.stack.durability,
			MaxDurability: entry.stack.maxDurability,
		}
	}
	return protocol.LootState{CorpseID: c.entityID, Revision: c.revision, Entries: entries}
}

// offerLootLocked retries explicit closures before the currently open full state. A
// successful send clears only the fact that frame satisfied.
func (p *Player) offerLootLocked() {
	for len(p.lootClosures) > 0 {
		id := p.lootClosures[0]
		if !p.deliver(protocol.EncodeLootClosed(protocol.LootClosed{CorpseID: id})) {
			return
		}
		p.lootClosures = p.lootClosures[1:]
	}
	if !p.lootDirty || p.openLootID == 0 {
		return
	}
	c := p.sim.corpses[p.openLootID]
	if c == nil || !c.ownedBy(p) || len(c.entries) == 0 {
		p.queueLootClosedLocked(p.openLootID)
		p.openLootID = 0
		p.lootDirty = false
		return
	}
	if p.deliver(protocol.EncodeLootState(c.lootState())) {
		p.lootDirty = false
	}
}

// voxelAt remains the shared conversion used by world-produced drop tests and callers.
func voxelAt(pos [3]float64) [3]int64 {
	return [3]int64{
		int64(math.Floor(pos[0])),
		int64(math.Floor(pos[1])),
		int64(math.Floor(pos[2])),
	}
}
