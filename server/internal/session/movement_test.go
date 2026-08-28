package session_test

import (
	"context"
	"math"
	"runtime"
	"slices"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// patience bounds every wait in this file. Generous because these tests hand work
// between three goroutines on a loaded runner, and irrelevant to a passing run because
// each assertion is reached long before it.
const patience = 10 * time.Second

// collector drains everything a session writes and remembers what it was.
//
// It has to drain: the fake connection's queue is shallow, and a test that let it fill
// would be measuring `trySend` dropping snapshots rather than the session sending them.
type collector struct {
	mu          sync.Mutex
	positions   map[uint64][3]float32
	snapshots   int
	chunks      map[world.Coord]int
	unloads     map[world.Coord]int
	updates     []protocol.BlockUpdate
	progress    []protocol.MineProgress
	inventories []protocol.InventoryState
	refusals    []protocol.ActionRefused
	chats       []protocol.ChatMessage
	invites     []protocol.PartyInvite
	partyLeader uint64
	party       []protocol.PartyMemberState
	partyRoster []protocol.PartyRosterMember
	lootStates  []protocol.LootState
	lootClosed  []uint64
	accessible  []uint64

	// explored is every MapExplored page, in arrival order and never merged: what a
	// test needs to see is how the ledger was paged, not only what it added up to.
	explored [][]world.Column

	// markerLists is every MarkerList this session received, in order and unmerged. A
	// list replaces the client's copy wholesale, so the order is the whole of the
	// meaning: what matters is what the *last* one says, and how many arrived before it.
	markerLists [][]protocol.Marker

	// drops is the newest snapshot's drop vector, replaced rather than appended:
	// a snapshot is the complete set of drops this session can see, which is exactly
	// how the client reads it.
	drops []protocol.ItemDropState
}

// collect starts draining conn until the test ends.
func collect(t *testing.T, conn *fakeConn) *collector {
	t.Helper()

	c := &collector{
		positions: make(map[uint64][3]float32),
		chunks:    make(map[world.Coord]int),
		unloads:   make(map[world.Coord]int),
	}

	stopped := make(chan struct{})
	go func() {
		defer close(stopped)
		for {
			select {
			case frame, ok := <-conn.out:
				if !ok {
					return
				}
				c.absorb(frame)
			case <-conn.done:
				return
			}
		}
	}()
	t.Cleanup(func() {
		_ = conn.Close()
		<-stopped
	})

	return c
}

func (c *collector) absorb(frame []byte) {
	env := vnet.GetRootAsEnvelope(frame, 0)

	var table flatbuffers.Table
	if !env.Payload(&table) {
		return
	}

	c.mu.Lock()
	defer c.mu.Unlock()

	switch env.PayloadType() {
	case vnet.PayloadEntitySnapshot:
		c.snapshots++
		snapshot := new(vnet.EntitySnapshot)
		snapshot.Init(table.Bytes, table.Pos)
		c.partyLeader = snapshot.PartyLeaderEntityId()
		c.party = c.party[:0]
		for i := range snapshot.PartyMembersLength() {
			var member vnet.PartyMemberState
			if !snapshot.PartyMembers(&member, i) {
				continue
			}
			pos := member.Pos(nil)
			if pos == nil {
				continue
			}
			c.party = append(c.party, protocol.PartyMemberState{
				EntityID: member.EntityId(), Pos: [3]float32{pos.X(), pos.Y(), pos.Z()},
				Health: member.Health(), MaxHealth: member.MaxHealth(), Alive: member.Alive(),
			})
		}
		c.partyRoster = c.partyRoster[:0]
		for i := range snapshot.PartyRosterLength() {
			member := new(vnet.PartyRosterMember)
			if !snapshot.PartyRoster(member, i) {
				continue
			}
			c.partyRoster = append(c.partyRoster, protocol.PartyRosterMember{
				CharacterID: member.CharacterId(), EntityID: member.EntityId(),
				Name: string(member.Name()), Online: member.Online(),
			})
		}
		c.accessible = c.accessible[:0]
		for i := range snapshot.AccessibleLootCorpsesLength() {
			c.accessible = append(c.accessible, snapshot.AccessibleLootCorpses(i))
		}
		for i := range snapshot.EntitiesLength() {
			var entity vnet.EntityState
			if !snapshot.Entities(&entity, i) {
				continue
			}
			pos := new(vnet.Vec3)
			entity.Pos(pos)
			c.positions[entity.EntityId()] = [3]float32{pos.X(), pos.Y(), pos.Z()}
		}
		c.drops = c.drops[:0]
		dropIndexes := make(map[uint64]int, snapshot.DropsLength())
		for i := range snapshot.DropsLength() {
			var drop vnet.ItemDropState
			if !snapshot.Drops(&drop, i) {
				continue
			}
			pos := drop.Pos(nil)
			if pos == nil {
				// A struct field is never absent in a well-formed snapshot; recording a
				// zero position would hide that rather than report it.
				continue
			}
			dropIndexes[drop.EntityId()] = len(c.drops)
			c.drops = append(c.drops, protocol.ItemDropState{
				EntityID: drop.EntityId(),
				Pos:      [3]float32{pos.X(), pos.Y(), pos.Z()},
				ItemID:   drop.ItemId(),
				Count:    drop.Count(),
			})
		}
		for i := range snapshot.DropDurabilitiesLength() {
			var wear vnet.ItemDropDurability
			if !snapshot.DropDurabilities(&wear, i) {
				continue
			}
			index, ok := dropIndexes[wear.EntityId()]
			if !ok {
				continue
			}
			c.drops[index].Durability = wear.Durability()
			c.drops[index].MaxDurability = wear.MaxDurability()
		}

	case vnet.PayloadChunkData:
		data := new(vnet.ChunkData)
		data.Init(table.Bytes, table.Pos)
		c.chunks[toWorldCoord(data.Coord(nil))]++

	case vnet.PayloadChunkUnload:
		unload := new(vnet.ChunkUnload)
		unload.Init(table.Bytes, table.Pos)
		c.unloads[toWorldCoord(unload.Coord(nil))]++

	case vnet.PayloadBlockUpdate:
		update := new(vnet.BlockUpdate)
		update.Init(table.Bytes, table.Pos)
		pos := update.Pos(nil)
		if pos == nil {
			// The server never sends one without a position, so recording the absence as a
			// zero would hide the bug rather than report it.
			return
		}
		c.updates = append(c.updates, protocol.BlockUpdate{
			Pos:     [3]int32{pos.X(), pos.Y(), pos.Z()},
			BlockID: update.BlockId(),
		})

	case vnet.PayloadMineProgress:
		mining := new(vnet.MineProgress)
		mining.Init(table.Bytes, table.Pos)
		pos := mining.Pos(nil)
		if pos == nil {
			return
		}
		c.progress = append(c.progress, protocol.MineProgress{
			Pos:      [3]int32{pos.X(), pos.Y(), pos.Z()},
			Progress: mining.Progress(),
		})

	case vnet.PayloadInventoryState:
		inventory := new(vnet.InventoryState)
		inventory.Init(table.Bytes, table.Pos)
		state := protocol.InventoryState{
			Stacks: make([]protocol.InventoryStack, 0, inventory.StacksLength()/2),
		}
		for index := 0; index+1 < inventory.StacksLength(); index += 2 {
			// Durability rides in two vectors parallel to the stack pairs, so slot i is
			// pairs 2i/2i+1 and durability entry i. Read here rather than ignored: a
			// projection that paired one slot's count with another's wear would decode
			// perfectly, and these tests are the only place on this side that would see it.
			slot := index / 2
			stack := protocol.InventoryStack{
				ItemID: inventory.Stacks(index),
				Count:  inventory.Stacks(index + 1),
			}
			if slot < inventory.DurabilityLength() {
				stack.Durability = inventory.Durability(slot)
			}
			if slot < inventory.MaxDurabilityLength() {
				stack.MaxDurability = inventory.MaxDurability(slot)
			}
			state.Stacks = append(state.Stacks, stack)
		}
		c.inventories = append(c.inventories, state)

	case vnet.PayloadActionRefused:
		refused := new(vnet.ActionRefused)
		refused.Init(table.Bytes, table.Pos)
		// The anchor is absent whenever the refused request named no voxel, and that is a
		// state the tests assert on: recording a zero for it would turn "no anchor" into a
		// claim about the world origin, which is the one thing the contract forbids here.
		record := protocol.ActionRefused{Action: refused.Action(), Reason: refused.Reason()}
		if anchor := refused.Anchor(nil); anchor != nil {
			record.Anchor, record.HasAnchor = [3]int32{anchor.X(), anchor.Y(), anchor.Z()}, true
		}
		c.refusals = append(c.refusals, record)

	case vnet.PayloadChatMessage:
		chat := new(vnet.ChatMessage)
		chat.Init(table.Bytes, table.Pos)
		c.chats = append(c.chats, protocol.ChatMessage{
			SenderEntityID: chat.SenderEntityId(),
			SenderName:     string(chat.SenderName()),
			Text:           string(chat.Text()),
		})

	case vnet.PayloadPartyInvite:
		invite := new(vnet.PartyInvite)
		invite.Init(table.Bytes, table.Pos)
		c.invites = append(c.invites, protocol.PartyInvite{
			FromEntityID: invite.FromEntityId(),
			FromName:     string(invite.FromName()),
			ExpiresMS:    invite.ExpiresMs(),
		})

	case vnet.PayloadLootState:
		loot := new(vnet.LootState)
		loot.Init(table.Bytes, table.Pos)
		state := protocol.LootState{CorpseID: loot.CorpseId(), Revision: loot.Revision()}
		for index := range loot.EntriesLength() {
			entry := new(vnet.LootEntry)
			if !loot.Entries(entry, index) {
				continue
			}
			state.Entries = append(state.Entries, protocol.LootEntry{
				EntryID: entry.EntryId(), ItemID: entry.ItemId(), Count: entry.Count(),
				Durability: entry.Durability(), MaxDurability: entry.MaxDurability(),
			})
		}
		c.lootStates = append(c.lootStates, state)

	case vnet.PayloadLootClosed:
		closed := new(vnet.LootClosed)
		closed.Init(table.Bytes, table.Pos)
		c.lootClosed = append(c.lootClosed, closed.CorpseId())

	case vnet.PayloadMarkerList:
		list := new(vnet.MarkerList)
		list.Init(table.Bytes, table.Pos)
		marks := make([]protocol.Marker, 0, list.MarkersLength())
		for index := range list.MarkersLength() {
			marker := new(vnet.Marker)
			if !list.Markers(marker, index) {
				continue
			}
			marks = append(marks, protocol.Marker{
				MarkerID: marker.MarkerId(), X: marker.X(), Z: marker.Z(),
				Kind: marker.Kind(), Note: string(marker.Note()),
			})
		}
		c.markerLists = append(c.markerLists, marks)

	case vnet.PayloadMapExplored:
		page := new(vnet.MapExplored)
		page.Init(table.Bytes, table.Pos)
		// One entry per message, kept whole rather than merged into a set: the ledger is
		// additive, so a union would be right about the map and blind to how it arrived
		// — and paging is exactly what these tests are about.
		columns := make([]world.Column, 0, page.ColumnsLength())
		for index := range page.ColumnsLength() {
			column := new(vnet.MapColumn)
			if !page.Columns(column, index) {
				continue
			}
			columns = append(columns, world.Column{CX: column.Cx(), CZ: column.Cz()})
		}
		c.explored = append(c.explored, columns)
	}
}

// exploredPages is every MapExplored this session received, in order and unmerged.
func (c *collector) exploredPages() [][]world.Column {
	c.mu.Lock()
	defer c.mu.Unlock()

	pages := make([][]world.Column, len(c.explored))
	for i, page := range c.explored {
		pages[i] = slices.Clone(page)
	}
	return pages
}

// exploredColumns is the union of every page, which is what a client's own ledger is.
func (c *collector) exploredColumns() map[world.Column]struct{} {
	c.mu.Lock()
	defer c.mu.Unlock()

	columns := make(map[world.Column]struct{})
	for _, page := range c.explored {
		for _, column := range page {
			columns[column] = struct{}{}
		}
	}
	return columns
}

// markerListsSeen is every MarkerList this session received, in order and unmerged.
func (c *collector) markerListsSeen() [][]protocol.Marker {
	c.mu.Lock()
	defer c.mu.Unlock()

	lists := make([][]protocol.Marker, len(c.markerLists))
	for i, list := range c.markerLists {
		lists[i] = slices.Clone(list)
	}
	return lists
}

// actionRefusals is every refusal the server has answered this session with, in order.
func (c *collector) actionRefusals() []protocol.ActionRefused {
	c.mu.Lock()
	defer c.mu.Unlock()
	return slices.Clone(c.refusals)
}

// chatMessages is every accepted world-chat line this session received, in order.
func (c *collector) chatMessages() []protocol.ChatMessage {
	c.mu.Lock()
	defer c.mu.Unlock()
	return slices.Clone(c.chats)
}

func (c *collector) partyInvites() []protocol.PartyInvite {
	c.mu.Lock()
	defer c.mu.Unlock()
	return slices.Clone(c.invites)
}

func (c *collector) partyState() (uint64, []protocol.PartyMemberState) {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.partyLeader, slices.Clone(c.party)
}

func (c *collector) rosterState() []protocol.PartyRosterMember {
	c.mu.Lock()
	defer c.mu.Unlock()
	return slices.Clone(c.partyRoster)
}

func (c *collector) lootState() ([]protocol.LootState, []uint64, []uint64) {
	c.mu.Lock()
	defer c.mu.Unlock()
	return slices.Clone(c.lootStates), slices.Clone(c.lootClosed), slices.Clone(c.accessible)
}

// blockUpdates is every voxel change the session has been told about, in order.
func (c *collector) blockUpdates() []protocol.BlockUpdate {
	c.mu.Lock()
	defer c.mu.Unlock()
	return slices.Clone(c.updates)
}

// visibleDrops is what the newest snapshot said is lying in this session's view.
func (c *collector) visibleDrops() []protocol.ItemDropState {
	c.mu.Lock()
	defer c.mu.Unlock()
	return slices.Clone(c.drops)
}

// mineProgress is every authoritative mining fraction this session received.
func (c *collector) mineProgress() []protocol.MineProgress {
	c.mu.Lock()
	defer c.mu.Unlock()
	return slices.Clone(c.progress)
}

// inventoryStates is every complete authoritative inventory this session received.
func (c *collector) inventoryStates() []protocol.InventoryState {
	c.mu.Lock()
	defer c.mu.Unlock()

	states := make([]protocol.InventoryState, len(c.inventories))
	for index, state := range c.inventories {
		states[index] = protocol.InventoryState{Stacks: slices.Clone(state.Stacks)}
	}
	return states
}

func (c *collector) inventoryCount(blockID uint16) uint16 {
	states := c.inventoryStates()
	if len(states) == 0 {
		return 0
	}
	for _, stack := range states[len(states)-1].Stacks {
		if stack.ItemID == blockID {
			return stack.Count
		}
	}
	return 0
}

// slotOf is where the newest authoritative inventory holds this item, or -1.
//
// Tests name the item they mined rather than the slot it landed in. Where a yield lands
// depends on what the player was already carrying, and every player now joins carrying
// a blade — so "the first empty slot" is no longer slot 0 and no test should assume it.
func (c *collector) slotOf(itemID uint16) int {
	states := c.inventoryStates()
	if len(states) == 0 {
		return -1
	}
	for slot, stack := range states[len(states)-1].Stacks {
		if stack.ItemID == itemID && stack.Count > 0 {
			return slot
		}
	}
	return -1
}

// emptySlot is the first slot the newest authoritative inventory holds nothing in, or
// -1. A move needs somewhere to move to, and that is no longer a fixed index either.
func (c *collector) emptySlot() int {
	states := c.inventoryStates()
	if len(states) == 0 {
		return -1
	}
	for slot, stack := range states[len(states)-1].Stacks {
		if stack.Count == 0 {
			return slot
		}
	}
	return -1
}

// carriedResources is how many stackable items the newest authoritative inventory
// holds. Equipment is deliberately excluded: every player joins carrying a blade, so a
// plain total of every count would answer "did this session pick anything up" with yes
// before the session had done anything at all. A durable slot is one with a non-zero
// maximum — the same test schemas/player.fbs uses.
func (c *collector) carriedResources() int {
	states := c.inventoryStates()
	if len(states) == 0 {
		return 0
	}
	total := 0
	for _, stack := range states[len(states)-1].Stacks {
		if stack.MaxDurability != 0 {
			continue
		}
		total += int(stack.Count)
	}
	return total
}

// chunkCount is how many times the session has been sent terrain for one chunk.
func (c *collector) chunkCount(coord world.Coord) int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.chunks[coord]
}

func (c *collector) position(entityID uint64) ([3]float32, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	pos, ok := c.positions[entityID]
	return pos, ok
}

func (c *collector) snapshotCount() int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.snapshots
}

// chunkCoords is every coordinate the session has sent terrain for.
func (c *collector) chunkCoords() []world.Coord {
	c.mu.Lock()
	defer c.mu.Unlock()

	coords := make([]world.Coord, 0, len(c.chunks))
	for coord := range c.chunks {
		coords = append(coords, coord)
	}
	return coords
}

// waitUntil polls a condition until it holds, or fails the test at patience.
func waitUntil(t *testing.T, what string, done func() bool) {
	t.Helper()

	deadline := time.Now().Add(patience)
	for time.Now().Before(deadline) {
		if done() {
			return
		}
		time.Sleep(time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s", what)
}

// tickUntil steps the simulation until done() holds, and reports whether it did
// before patience ran out.
//
// The stepping is the point of it, not a way to pass the time. What this test waits
// for is produced on other goroutines and none of it is retried: a snapshot that finds
// the outbound queue full is *dropped* rather than queued (game.Sim.Step), so the
// newest position reaches the collector only on a later tick that finds room. A wait
// that merely slept would be waiting for a frame nobody was going to send again.
//
// Spinning here is safe for as long as patience allows because the caller has stopped
// sending input: after idleLimit ticks of silence the simulation decays the intent and
// the player is stationary. A wait cannot walk it anywhere.
func tickUntil(step func(), done func() bool) bool {
	deadline := time.Now().Add(patience)
	for time.Now().Before(deadline) {
		if done() {
			return true
		}
		step()
		// Hand the processor to the goroutines being waited on — they are what makes
		// done() change, so spinning against them would only delay it.
		runtime.Gosched()
	}
	// One last look, so a condition that came true during the final step is not
	// reported as a timeout.
	return done()
}

// generateAround pre-generates the chunks a player at pos can collide with, the way
// streaming eventually would. Without it the terrain reads as solid and the player
// stands still — which is a correct answer and not the one these tests are about.
func generateAround(t *testing.T, chunks *world.Cache, pos [3]float32, radius int32) {
	t.Helper()

	center := world.ContainingChunk(pos[0], pos[1], pos[2])
	for y := center.Y - radius; y <= center.Y+radius; y++ {
		for z := center.Z - radius; z <= center.Z+radius; z++ {
			for x := center.X - radius; x <= center.X+radius; x++ {
				if _, _, err := chunks.Get(context.Background(), world.Coord{X: x, Y: y, Z: z}); err != nil {
					t.Fatalf("generate chunk %d,%d,%d: %v", x, y, z, err)
				}
			}
		}
	}
}

// The seam this issue exists for, end to end through one session: intent arrives on a
// socket, the tick loop integrates it, the answer leaves as a snapshot, and the chunks
// that follow are the ones the *server's* position walked into.
//
// Nothing in the exchange carries a position the client chose, because there is no
// such field on the wire — which is why this test has no rejection path to cover.
func TestSessionWalksThePlayerAndStreamsWhereItWalks(t *testing.T) {
	t.Parallel()

	const entityID = 3

	cfg := testConfig()
	cfg.ViewDistance = 0 // one chunk, so a border crossing is unambiguous
	cfg.Spawn = world.SpawnAt(cfg.WorldSeed)

	chunks := world.NewCache(cfg.WorldSeed, 4, 512)
	peers := session.NewRegistry()
	sim, err := game.NewSim(cfg.TickRate, cfg.ViewDistance, cfg.WorldSeed, game.NewCacheTerrain(chunks), chunks, peers.NextID, discard())
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	// Two chunks either side, so the player has ground under it for the whole walk.
	generateAround(t, chunks, cfg.Spawn, 2)

	conn := newFakeConn()
	frames := collect(t, conn)

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	served := make(chan error, 1)
	go func() {
		served <- session.Serve(ctx, conn, cfg, noTimeouts(), chunks, sim, peers, ephemeralIdentities(), entityID, discard())
	}()

	conn.in <- hello(1)
	createCharacter(conn, "Eivor")
	waitUntil(t, "the player to join the simulation", func() bool { return sim.Count() == 1 })

	// Settle onto the ground before measuring, so the walk is a walk and not a fall.
	var tick atomic.Uint64
	step := func() { sim.Step(tick.Add(1)) }
	for range 60 {
		step()
	}
	waitUntil(t, "the first snapshot", func() bool { _, ok := frames.position(entityID); return ok })

	start, _ := frames.position(entityID)
	spawnChunk := world.ContainingChunk(cfg.Spawn[0], cfg.Spawn[1], cfg.Spawn[2])

	// Walk north — yaw 0, forward — one input per tick, exactly as the client does.
	// The spawn column is z = 0.5, so four blocks is comfortably past the chunk border
	// at z = 0 and long enough that the distance is a measurement rather than a step.
	const walkBlocks = 4.0

	// The walk is bounded in *ticks*, and the bound is derived from the simulation
	// rather than from the clock: one tick of full intent is WalkSpeed/TickRate blocks,
	// so twice the ticks the distance needs is headroom for the terrain and nothing
	// more. Deliberately not a wall-clock deadline, which is what this loop used to
	// have and could not be: it steered by a position that arrives over the wire and
	// therefore lags, so a drain goroutine that lost the processor left it ticking on.
	// Measured with the collector stalled for 60 ms, it ran ~2,700 ticks and put the
	// player at z = -38 — a whole chunk past the one the streaming assertion names —
	// while the snapshot it was reading still said four blocks. game.chunkFeed keeps
	// only the newest coordinate, so a streamer waking after that point would correctly
	// never send the chunk this test asks about.
	walkTicks := 2 * int(math.Ceil(walkBlocks/(game.WalkSpeed/float64(cfg.TickRate))))

	clientTick := uint32(0)
	for range walkTicks {
		clientTick++
		conn.in <- protocol.EncodePlayerInput(protocol.PlayerInput{ClientTick: clientTick, MoveZ: 1})
		step()
	}

	// Input stops here, and so does the player: the simulation decays an intent its
	// client has stopped refreshing. Everything below is therefore asserted about a
	// player parked inside one chunk, which is what turns the waits into waits rather
	// than into races with the walk.
	if !tickUntil(step, func() bool {
		pos, ok := frames.position(entityID)
		return ok && float64(start[2]-pos[2]) >= walkBlocks
	}) {
		pos, _ := frames.position(entityID)
		t.Fatalf("after %d ticks of walking north the session had reported only %v blocks (z = %v)",
			walkTicks, start[2]-pos[2], pos[2])
	}

	moved, ok := frames.position(entityID)
	if !ok {
		t.Fatal("the session never reported a position")
	}

	// North is -Z. The distance is what the server computed; the client only ever said
	// which way it was trying to go.
	if moved[2] >= start[2] {
		t.Fatalf("the player did not walk north: z went from %v to %v", start[2], moved[2])
	}
	if travelled := float64(start[2] - moved[2]); travelled < walkBlocks {
		t.Fatalf("the player only walked %v blocks north in %d ticks", travelled, walkTicks)
	}
	if crossed := world.ContainingChunk(moved[0], moved[1], moved[2]); crossed.Z >= spawnChunk.Z {
		t.Fatalf("the player is still in chunk %+v after walking north out of %+v", crossed, spawnChunk)
	}
	if moved[0] != start[0] {
		t.Errorf("walking north moved the player sideways, from x = %v to %v", start[0], moved[0])
	}
	// Enough to be a stream rather than an accident. Not an exact count: `trySend`
	// legitimately drops a snapshot when this test's drain goroutine falls behind, and
	// the once-per-tick cadence is pinned where the tick is, in the package that owns
	// Step.
	if got := frames.snapshotCount(); got < 20 {
		t.Errorf("only %d snapshots were sent over the whole walk", got)
	}

	// And the streaming follows. The view is one chunk wide, so a player parked north
	// of the border has to be sent the chunk it is parked in — driven by the position
	// the server computed, because that is the only position there is.
	//
	// **Waited for, not read once.** The tick loop only rings a doorbell
	// (game.chunkFeed); the chunk itself is read, encoded and enqueued by the session's
	// streaming goroutine, precisely so that generating terrain never costs every
	// connected player a tick. Reading the set in the same breath as the snapshot that
	// reported the crossing asserted an ordering the server deliberately does not
	// provide, and that was this test's flake: on a loaded machine, 2 failures in 800
	// runs at GOMAXPROCS=1 and 3 in 400 at GOMAXPROCS=2, every one of them this
	// assertion and no other — rising to a third of runs once the window between the
	// crossing and the assertion was widened. The wait is not a grace period — the
	// player is stationary, so "the streamer has not run yet" is the only thing that
	// can still be true, and it stops being true the moment it is scheduled.
	wanted := world.Coord{X: spawnChunk.X, Y: spawnChunk.Y, Z: spawnChunk.Z - 1}
	if !tickUntil(step, func() bool { return slices.Contains(frames.chunkCoords(), wanted) }) {
		t.Errorf("chunk %+v was never streamed; the session sent %+v", wanted, frames.chunkCoords())
	}

	cancel()
	_ = conn.Close()
	select {
	case <-served:
	case <-time.After(patience):
		t.Fatal("Serve did not return")
	}
}

// Input before the handshake is refused by the handshake itself, which is stricter than
// dropping it: the contract says the first message on a connection is a ClientHello, so
// a session that starts with input never becomes a session at all — and the simulation
// never hears about it.
func TestInputBeforeTheHandshakeNeverReachesTheSimulation(t *testing.T) {
	t.Parallel()

	chunks, sim, peers := serveDeps(t)
	conn := newFakeConn()

	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, serveConfig(), noTimeouts(), chunks, sim, peers, ephemeralIdentities(), 3, discard())
	}()

	conn.in <- encodePlayerInput(1, 1)

	env := vnet.GetRootAsEnvelope(nextFrame(t, conn), 0)
	if env.PayloadType() != vnet.PayloadServerReject {
		t.Fatalf("reply is %s, want %s", env.PayloadType(), vnet.PayloadServerReject)
	}
	if got := rejectFrom(t, env).Reason(); got != vnet.RejectReasonBAD_REQUEST {
		t.Errorf("Reason = %s, want %s", got, vnet.RejectReasonBAD_REQUEST)
	}

	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Serve returned %v, want nil for a refused handshake", err)
		}
	case <-time.After(patience):
		t.Fatal("Serve did not return after refusing the handshake")
	}

	if got := sim.Count(); got != 0 {
		t.Errorf("the simulation holds %d players after a refused handshake", got)
	}

	// And a tick after the refusal must be harmless: there is nothing to step and
	// nothing to deliver to.
	sim.Step(1)
}

// Input the simulation refuses is dropped, not fatal. The frame was well formed, the
// stream is still trustworthy, and only a value was wrong — so a client with one bad
// frame keeps its session and keeps moving on the intent that was accepted.
func TestRefusedInputDoesNotEndTheSession(t *testing.T) {
	t.Parallel()

	chunks, sim, peers := serveDeps(t)
	conn := newFakeConn()
	frames := collect(t, conn)

	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, serveConfig(), noTimeouts(), chunks, sim, peers, ephemeralIdentities(), 3, discard())
	}()

	conn.in <- hello(1)
	createCharacter(conn, "Eivor")
	waitUntil(t, "the player to join the simulation", func() bool { return sim.Count() == 1 })

	// A NaN axis, an infinite yaw, and a client tick that goes backwards: every refusal
	// the simulation has, in the frames a client actually sends.
	for _, bad := range []protocol.PlayerInput{
		{ClientTick: 5, MoveZ: float32(math.NaN())},
		{ClientTick: 6, Yaw: float32(math.Inf(1))},
		{ClientTick: 1, MoveZ: 1},
	} {
		conn.in <- protocol.EncodePlayerInput(bad)
	}
	conn.in <- protocol.EncodePlayerInput(protocol.PlayerInput{ClientTick: 100, MoveZ: 1})

	sim.Step(1)
	waitUntil(t, "a snapshot", func() bool { return frames.snapshotCount() > 0 })

	// Still alive, and its position is still a number.
	pos, ok := frames.position(3)
	if !ok {
		t.Fatal("no snapshot named the player")
	}
	for axis, value := range pos {
		if v := float64(value); math.IsNaN(v) || math.IsInf(v, 0) {
			t.Errorf("position axis %d is %v after refused input", axis, value)
		}
	}

	select {
	case err := <-done:
		t.Fatalf("Serve ended over refused input: %v", err)
	case <-time.After(50 * time.Millisecond):
	}

	if err := conn.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Serve returned %v, want nil for a clean disconnect", err)
		}
	case <-time.After(patience):
		t.Fatal("Serve did not return after the connection closed")
	}
}

// The teardown ordering in Serve, as a race rather than as a comment.
//
// Sim.Step sends into the session's outbound channel and the teardown closes it. A send
// on a closed channel is a panic in a goroutine, which takes the whole process down —
// so the simulation has to have stopped delivering *before* the close. Ticking hard
// while sessions come and go is the only way to observe that; if Sim.Leave moved after
// close(out) in the defer, this test would crash the test binary rather than fail.
func TestSnapshotsStopBeforeTheOutboundQueueIsClosed(t *testing.T) {
	t.Parallel()

	chunks, sim, peers := serveDeps(t)

	var tick atomic.Uint64
	stop := make(chan struct{})
	var ticking sync.WaitGroup
	ticking.Add(1)
	go func() {
		defer ticking.Done()
		for {
			select {
			case <-stop:
				return
			default:
				sim.Step(tick.Add(1))
			}
		}
	}()
	defer func() {
		close(stop)
		ticking.Wait()
	}()

	// Sessions arriving and leaving under a running tick loop, which is what a server
	// does all day. Each needs its own identity: Join refuses a duplicate.
	for round := range 40 {
		conn := newFakeConn()
		done := make(chan error, 1)
		go func() {
			done <- session.Serve(context.Background(), conn, serveConfig(), noTimeouts(), chunks, sim, peers, ephemeralIdentities(), uint64(round+1), discard())
		}()

		conn.in <- hello(1)
		createCharacter(conn, "Eivor")
		waitUntil(t, "the player to join the simulation", func() bool { return sim.Count() > 0 })

		if err := conn.Close(); err != nil {
			t.Fatalf("round %d: Close: %v", round, err)
		}
		select {
		case <-done:
		case <-time.After(patience):
			t.Fatalf("round %d: Serve did not return", round)
		}
		waitUntil(t, "the player to leave the simulation", func() bool { return sim.Count() == 0 })
	}
}
