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

// Normal-mob loot belongs to the corpse, never to the ground. The roll happens once, on
// the tick the killing blow lands, and is stored with stable owner and entry identities.
// Opening only projects that settled state and therefore consumes no random numbers.

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

type corpseContainer struct {
	entries  []corpseEntry
	revision uint32
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

	owner     corpseOwner
	container corpseContainer
	// personal is non-nil only for boss corpses. Each stable character owns an
	// independent entry slice and revision; normal mobs keep one container above.
	personal    map[corpseOwner]*corpseContainer
	expiresTick uint64
}

func (c *corpse) ownedBy(p *Player) bool {
	_, ok := c.containerFor(p)
	return ok
}

func (c *corpse) containerFor(p *Player) (*corpseContainer, bool) {
	if c == nil || p == nil {
		return nil, false
	}
	owner := p.corpseOwner()
	if c.personal != nil {
		container, ok := c.personal[owner]
		return container, ok
	}
	if c.owner != owner {
		return nil, false
	}
	return &c.container, true
}

func (c *corpse) hasLoot() bool {
	if c == nil {
		return false
	}
	if c.personal == nil {
		return len(c.container.entries) > 0
	}
	for _, container := range c.personal {
		if len(container.entries) > 0 {
			return true
		}
	}
	return false
}

func (c *corpse) entryCount() int {
	if c == nil {
		return 0
	}
	if c.personal == nil {
		return len(c.container.entries)
	}
	count := 0
	for _, container := range c.personal {
		count += len(container.entries)
	}
	return count
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

	// resident is set when this row is a settlement's person rather than a creature.
	// The snapshot loop needs it for one thing only — the once-per-view description that
	// carries the name and the role — because everything else about a resident is already
	// in `state`. Nil for every mob and every corpse, exactly as `corpse` is nil here.
	resident *resident
}

// mobSnapshotsLocked merges living mobs, corpses and residents in entity-id order. The
// collection boundary stays internal: a receiver sees one continuous MobState stream.
//
// **Three collections rather than two, and the merge is the reason they can be three.**
// A resident is kept out of Sim.mobs so that combat, the projectiles and the director
// cannot reach one (resident.go argues it); the wire has no third vector to put one in
// and needs none, because `MobState` already says everything a client draws a standing
// body from. So the split that makes them safe costs exactly this: one more loop here.
func (s *Sim) mobSnapshotsLocked(mobs []*mob) []mobSnapshot {
	shown := make([]mobSnapshot, 0, len(mobs)+len(s.corpses)+len(s.residents))
	states := mobStates(mobs)
	for index, m := range mobs {
		shown = append(shown, mobSnapshot{state: states[index], chunk: m.chunk})
	}
	for _, c := range s.corpses {
		shown = append(shown, mobSnapshot{state: c.state(), chunk: c.chunk, corpse: c})
	}
	for _, r := range s.residents {
		shown = append(shown, mobSnapshot{state: r.state(), chunk: r.chunk, resident: r})
	}
	slices.SortFunc(shown, func(a, b mobSnapshot) int {
		return compareEntityIDs(a.state.EntityID, b.state.EntityID)
	})
	return shown
}

// makeCorpseLocked is the only killed-mob transition. The caller holds Sim.mu.
func (s *Sim) makeCorpseLocked(m *mob) *corpse {
	c := &corpse{
		entityID:    m.entityID,
		kind:        m.kind,
		pos:         m.pos,
		yaw:         m.yaw,
		chunk:       m.chunk,
		expiresTick: s.currentTick + s.corpseLifetimeTicks,
	}
	if m.species().isBoss() {
		var roster []corpseOwner
		if m.encounter != nil {
			roster = m.encounter.roster
		} else if m.firstHit != nil {
			// Defensive fallback for a future damage caller that forgets to start the
			// encounter. It may preserve the tap, but it may never route a boss through
			// normal-party round robin or consult mutable membership at death.
			roster = []corpseOwner{{playerID: m.firstHit.playerID, characterID: m.firstHit.characterID}}
		}
		c.personal = make(map[corpseOwner]*corpseContainer, len(roster))
		// Roster order is RNG order. The map is only the lookup after every roll
		// has settled, so opening order can never influence the sequence.
		for _, owner := range roster {
			c.personal[owner] = &corpseContainer{entries: s.rollLootLocked(m), revision: 1}
		}
	} else {
		if m.firstHit != nil {
			c.owner = s.corpseOwnerLocked(m.firstHit, m.pos)
		}
		c.container = corpseContainer{entries: s.rollLootLocked(m), revision: 1}
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

// rollLootLocked rolls the species table exactly once, at the Corpse transition — which is
// the tick of the killing blow, and the only tick on which it is ever called.
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

	c, _, reason, err := p.accessibleCorpseLocked(req.CorpseID)
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

	c, container, reason, err := p.openContainerLocked(req.CorpseID, req.Revision)
	if err != nil {
		return reason, err
	}
	entryIndex := -1
	for index := range container.entries {
		if container.entries[index].entryID == req.EntryID {
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
	if !p.inventory.insertWholeStackLocked(container.entries[entryIndex].stack) {
		return vnet.RefusalReasonInventoryFull, errors.New("the whole loot entry does not fit")
	}

	container.entries = append(container.entries[:entryIndex], container.entries[entryIndex+1:]...)
	container.revision++
	p.inventoryDirty = true
	p.lootDirty = true
	if !c.hasLoot() {
		p.sim.removeCorpseLocked(c.entityID)
	}
	return vnet.RefusalReasonUnknown, nil
}

// TakeAllLoot empties into the pack every entry of one known revision that fits, in
// entry order, and leaves the rest where they are.
//
// It is TakeLoot's preconditions and a different loop: an entry that does not fit is
// skipped rather than aborted on, so a bone behind a blade still comes home. The whole
// walk runs inside the one TryLock window, which is what makes "what fits" a question
// about a pack no other request can be halfway through changing.
//
// Partial success is reported as both things it is: the entries that moved are
// committed and dirty the two states, and the remainder answers InventoryFull so the
// player is told why the window is still open. Nothing moving is the same shape with
// no revision spent.
func (p *Player) TakeAllLoot(req protocol.LootTakeAllRequest) (vnet.RefusalReason, error) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if err := p.cannotActLocked(); err != nil {
		return vnet.RefusalReasonPlayerIsDead, err
	}
	if p.haveLootTakeAllTick && !newerTick(req.ClientTick, p.lastLootTakeAllTick) {
		return vnet.RefusalReasonUnknown, fmt.Errorf("stale loot-take-all client tick %d; newest is %d", req.ClientTick, p.lastLootTakeAllTick)
	}
	p.haveLootTakeAllTick, p.lastLootTakeAllTick = true, req.ClientTick

	c, container, reason, err := p.openContainerLocked(req.CorpseID, req.Revision)
	if err != nil {
		return reason, err
	}
	if !p.inventory.mu.TryLock() {
		return vnet.RefusalReasonInventoryBusy, errors.New("the inventory is busy")
	}
	defer p.inventory.mu.Unlock()

	// Filtered in place: the write index never overtakes the read index, so an entry
	// is always read before its slot can be reused. Nothing is committed to the
	// container until the walk has finished counting what moved.
	kept, moved := container.entries[:0], 0
	for _, entry := range container.entries {
		if p.inventory.insertWholeStackLocked(entry.stack) {
			moved++
			continue
		}
		kept = append(kept, entry)
	}
	if moved > 0 {
		container.entries = kept
		container.revision++
		p.inventoryDirty = true
		p.lootDirty = true
		if !c.hasLoot() {
			p.sim.removeCorpseLocked(c.entityID)
			return vnet.RefusalReasonUnknown, nil
		}
	}
	if len(container.entries) > 0 {
		return vnet.RefusalReasonInventoryFull, fmt.Errorf("%d loot entries do not fit", len(container.entries))
	}
	return vnet.RefusalReasonUnknown, nil
}

// openContainerLocked names the one container a take may act on: reachable, owned,
// non-empty, currently open on this session, and at the revision the request carries.
// Both take paths share it so that "which container, and is the client's view of it
// current" has exactly one answer.
func (p *Player) openContainerLocked(corpseID uint64, revision uint32) (*corpse, *corpseContainer, vnet.RefusalReason, error) {
	c, container, reason, err := p.accessibleCorpseLocked(corpseID)
	if err != nil {
		return nil, nil, reason, err
	}
	if p.openLootID != c.entityID {
		return nil, nil, vnet.RefusalReasonCorpseUnavailable, errors.New("the corpse container is not open")
	}
	if revision != container.revision {
		return nil, nil, vnet.RefusalReasonStaleRevision, fmt.Errorf("loot revision %d is not current revision %d", revision, container.revision)
	}
	return c, container, vnet.RefusalReasonUnknown, nil
}

func (p *Player) accessibleCorpseLocked(id uint64) (*corpse, *corpseContainer, vnet.RefusalReason, error) {
	c := p.sim.corpses[id]
	if c == nil || !withinView(p.chunk, c.chunk, p.sim.viewDistance) {
		return nil, nil, vnet.RefusalReasonCorpseUnavailable, errors.New("the corpse is unavailable")
	}
	container, owned := c.containerFor(p)
	if !owned {
		return nil, nil, vnet.RefusalReasonLootNotOwned, errors.New("the corpse belongs to another character")
	}
	if len(container.entries) == 0 {
		return nil, nil, vnet.RefusalReasonCorpseUnavailable, errors.New("the character's corpse container is empty")
	}
	if distance := boxDistance(playerBox(p.pos), mobRegistry[c.kind].body.boxAt(c.pos)); math.IsNaN(distance) || distance > EditReach {
		return nil, nil, vnet.RefusalReasonOutOfReach, fmt.Errorf("the corpse is %.2f blocks away, past the reach of %.1f", distance, EditReach)
	}
	return c, container, vnet.RefusalReasonUnknown, nil
}

// canOpenCorpseLocked is the snapshot-side form of the same access rule. It carries
// no reason because a snapshot advertises capabilities rather than refusals.
func (p *Player) canOpenCorpseLocked(c *corpse) bool {
	container, owned := c.containerFor(p)
	if c == nil || !owned || len(container.entries) == 0 || p.cannotActLocked() != nil ||
		!withinView(p.chunk, c.chunk, p.sim.viewDistance) {
		return false
	}
	return boxDistance(playerBox(p.pos), mobRegistry[c.kind].body.boxAt(c.pos)) <= EditReach
}

func (c *corpse) lootState(container *corpseContainer) protocol.LootState {
	entries := make([]protocol.LootEntry, len(container.entries))
	for index, entry := range container.entries {
		entries[index] = protocol.LootEntry{
			EntryID:       entry.entryID,
			ItemID:        uint16(entry.stack.item),
			Count:         entry.stack.count,
			Durability:    entry.stack.durability,
			MaxDurability: entry.stack.maxDurability,
		}
	}
	return protocol.LootState{CorpseID: c.entityID, Revision: container.revision, Entries: entries}
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
	container, owned := c.containerFor(p)
	if c == nil || !owned || len(container.entries) == 0 {
		p.queueLootClosedLocked(p.openLootID)
		p.openLootID = 0
		p.lootDirty = false
		return
	}
	if p.deliver(protocol.EncodeLootState(c.lootState(container))) {
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
