package game

import (
	"errors"
	"fmt"
	"slices"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// A camp is entities, not voxels.
//
// A tent, a forge and a campfire stand in the authoritative world the way a drop and a
// draugr do: the simulation owns them, chunk visibility streams them, their identities
// come from the counter that names players. Nothing here touches chunk data, the run-length
// palette or the delta layer — which is what keeps "restore this chunk to its original
// procedural state" (the GDD's Fimbulvetr storm) a decision about *terrain* that a
// shelter cannot complicate.
//
// What a structure does read is terrain: its footprint has to rest on ground that is
// actually there. That read is non-generating and happens under the simulation's lock,
// exactly as every other terrain read on this side does, so a placement at the edge of
// loaded terrain is refused rather than waited for.

const (
	// tentHeadroom and forgeHeadroom are how many cells of air each kind needs above
	// every cell of its footprint.
	//
	// Two for a tent, because a player has to be able to stand up inside the thing they
	// respawn in — PlayerHeight is 1.8, so one cell is a tent you can only lie in and
	// two is the first that fits. One for a forge, which is furniture: an anvil and a
	// hearth are waist-high objects and nobody stands inside them. One for a campfire,
	// which is furniture on the same terms: a fire is something you sit beside, and a
	// player who could stand inside one would be standing inside the thing that is
	// supposed to be keeping the dark at arm's length.
	tentHeadroom     = 2
	forgeHeadroom    = 1
	campfireHeadroom = 1

	// Three for a runestone, because it is the first structure whose point is that you
	// can see it from somewhere else. A monolith one cell wide and three cells tall
	// stands a head above a player and is the only thing in a camp that is taller than
	// the tent — which is what makes "whose ground is this" a question you can answer by
	// looking rather than by digging and finding out.
	runestoneHeadroom = 3
)

// MaxRunestonesPerPlayer permits a camp and an outpost per identity. The cap shares
// the tent's existing "you already have one" refusal as required by the wire contract.
const MaxRunestonesPerPlayer = 2

// WardChunkRadius is a Chebyshev radius in chunk columns. One produces a 3x3,
// 96x96-block claim at every height and makes storm protection a map lookup.
const WardChunkRadius = 1

// The ground cells each kind rests on, as offsets from the anchor in the canonical North
// orientation.
//
// Ground cells, not the cells the structure *occupies*: the footprint names what has to
// be solid underneath, and the headroom above each of them is what has to be clear. That
// is the same pair of questions for both kinds, which is what lets one validator serve
// them and one collapse rule watch them.
//
// The tent's nine are symmetric, so rotating them is a no-op — deliberately, and not a
// reason to special-case it. Which way a tent faces is what its opening looks like, and
// a footprint that happens to be invariant under rotation is still computed by the same
// arithmetic as one that is not.
var (
	tentFootprint = [][2]int64{
		{-1, -1}, {0, -1}, {1, -1},
		{-1, 0}, {0, 0}, {1, 0},
		{-1, 1}, {0, 1}, {1, 1},
	}

	// The anvil on the anchor and the hearth one step along the facing. North is -Z, so
	// the canonical hearth offset is (0, -1); see rotateOffset.
	forgeFootprint = [][2]int64{{0, 0}, {0, -1}}

	// One cell, which makes facing a no-op the way the tent's symmetric nine do — and
	// which is not a reason to special-case the rotation. A fire is a single thing on a
	// single block: the radius it keeps clear is measured from the anchor and owes
	// nothing to how much ground the structure itself covers.
	campfireFootprint = [][2]int64{{0, 0}}

	// One cell, like the campfire's, and facing is a no-op on it for the same reason —
	// and still computed rather than special-cased. A monolith is one block of ground
	// with three cells of nothing above it; the ward it casts is measured from the chunk
	// column the anchor falls in and owes nothing to how much ground the stone covers.
	runestoneFootprint = [][2]int64{{0, 0}}
)

// structure is one placed tent, forge or campfire.
//
// Every field is guarded by Sim.mu.
//
// # What survives a restart, and what does not
//
// Kind, anchor, facing and owner are written to <world-dir>/structures.bin and read
// back at startup. The two fields below them are not, because both are derived: the
// chunk is a function of the anchor, and the id is re-minted from the counter that
// names every entity the simulation owns — see [Sim.RestoreStructures] for why storing
// one would be worse than re-deriving it.
//
// **The one known gap is a crash between the two flushes.** The chunk deltas and this
// file are written by separate flushes, so a kill -9 landing between them can bring the
// server back with a structure standing over ground somebody had already dug away. It
// stays standing until the next edit of one of its footprint cells, which collapses it
// through the ordinary rule. Load-time support validation would close it and is
// deliberately not done: it would generate every chunk under every camp before the
// first session is accepted. See "Known gaps" in server/AGENTS.md.
type structure struct {
	structureID uint64
	kind        vnet.StructureKind

	// anchor is the voxel this structure rests on, in world block coordinates and in
	// the int32 the wire carries. Kept at the wire's width because it arrives as one
	// and goes back out as one; the footprint arithmetic widens it to int64, which is
	// what every voxel lookup on this side takes.
	anchor [3]int32

	facing vnet.Facing

	// owner is the identity of the player who placed it. It decides removal and
	// respawn, and nothing else: any player may walk into any tent, and the crafting
	// issue reads this registry without consulting this field at all.
	//
	// **The zero identity is the world**, which is the absence of a rule rather than a
	// new one: no live player is ever the zero id, so removal already refuses a structure
	// carrying it and the tent lookup already fails to match it. See station.go.
	//
	// **An identity, not an entity id, and the difference is the whole of this issue.**
	// An entity id names one session; a camp outlives every session its owner will ever
	// open. Keyed by the entity id, a tent stopped being its owner's the moment they
	// reconnected — they came back with a new number and respawned at the world spawn
	// beside a tent they could no longer take down. The wire still carries an entity id
	// (see [Sim.structureStatesLocked]), because that is what a client can match against
	// the players it can see; the resolution from one to the other happens here, once
	// per snapshot, and answers 0 for an owner with no live session.
	owner identity.PlayerID

	// chunk is the chunk the anchor falls in, kept beside it for the reason a player's
	// and a drop's are: visibility asks for it once per viewer per tick.
	chunk world.Coord

	// doused says the rain over this structure's own column is heavy enough to have put
	// it out. It means something for StructureKindCampfire and for nothing else.
	//
	// **Derived from the weather field and never stored**, which is why it sits here and
	// not in [Structure]: [Sim.douseFiresLocked] recomputes standing fires every tick,
	// while [Player.PlaceStructure] computes a new fire before publishing it. It needs no
	// persistence, no relighting mechanic and no migration. The zero value is a burning
	// fire, which is the same direction the wire's `lit` default already fails in — a
	// fire nobody has asked about is alight.
	doused bool
}

// anchorVoxel widens the anchor to the int64 every terrain lookup takes. Exact: every
// int32 is representable in an int64.
func (s *structure) anchorVoxel() [3]int64 {
	return [3]int64{int64(s.anchor[0]), int64(s.anchor[1]), int64(s.anchor[2])}
}

// knownStructureKind resolves the item in a slot to the thing it plants.
//
// The registry decides, exactly as it decides what an item places as a voxel. An item
// with no entry here is not a structure, which is how every resource and the sword are
// refused by the same test rather than by a list of exceptions.
func knownStructureKind(item ItemID) (vnet.StructureKind, bool) {
	switch item {
	case ItemTent:
		return vnet.StructureKindTent, true
	case ItemForge:
		return vnet.StructureKindForge, true
	case ItemCampfire:
		return vnet.StructureKindCampfire, true
	case ItemRunestone:
		return vnet.StructureKindRunestone, true
	default:
		return vnet.StructureKindUnknown, false
	}
}

// structureItem is what a structure leaves behind when it stops standing.
//
// The inverse of knownStructureKind, and a separate function rather than a reversed map
// lookup because it has one caller shape — removal and collapse both need "what do I
// drop" — and because an unpaired kind should fail closed here rather than spawn a drop
// of item zero, which the wire forbids.
func structureItem(kind vnet.StructureKind) (ItemID, bool) {
	switch kind {
	case vnet.StructureKindTent:
		return ItemTent, true
	case vnet.StructureKindForge:
		return ItemForge, true
	case vnet.StructureKindCampfire:
		return ItemCampfire, true
	case vnet.StructureKindRunestone:
		return ItemRunestone, true
	default:
		return ItemNone, false
	}
}

// knownFacing reports whether a facing is one a client may send.
//
// Unknown is the absent-field case — FlatBuffers decodes a missing scalar as zero — and
// an unrecognised value is a client speaking a contract this server does not. Both are
// refused rather than defaulted to north: a structure planted facing a direction nobody
// named is a structure the player did not ask for.
func knownFacing(facing vnet.Facing) bool {
	switch facing {
	case vnet.FacingNorth, vnet.FacingEast, vnet.FacingSouth, vnet.FacingWest:
		return true
	default:
		return false
	}
}

// rotateOffset turns a canonical (North-facing) footprint offset into the one this
// facing needs.
//
// The basis is the movement integrator's: yaw 0 looks along -Z and +X is to its right,
// so North is -Z, East is +X, South is +Z and West is -X. Each case is a quarter turn
// about the vertical axis, expressed in integers — a rotation matrix in float would put
// a footprint cell on the wrong side of a boundary it is sitting exactly on.
//
// Unknown never reaches here: knownFacing refuses it at the request boundary, and the
// registry only ever holds a facing that passed. The default answers North anyway,
// because a footprint is not the place to discover a validation gap.
func rotateOffset(offset [2]int64, facing vnet.Facing) [2]int64 {
	dx, dz := offset[0], offset[1]
	switch facing {
	case vnet.FacingEast:
		return [2]int64{-dz, dx}
	case vnet.FacingSouth:
		return [2]int64{-dx, -dz}
	case vnet.FacingWest:
		return [2]int64{dz, -dx}
	default:
		return [2]int64{dx, dz}
	}
}

// footprintOf is the ground cells a structure of this kind and facing rests on, and how
// many cells of air each of them needs above it.
//
// One function for both questions because they are one description: a cell that must be
// solid and a column that must be clear are the two halves of "this thing fits here",
// and splitting them would let a later kind answer one and forget the other.
func footprintOf(kind vnet.StructureKind, facing vnet.Facing, anchor [3]int64) (cells [][3]int64, headroom int64, ok bool) {
	var (
		offsets [][2]int64
		clear   int64
	)
	switch kind {
	case vnet.StructureKindTent:
		offsets, clear = tentFootprint, tentHeadroom
	case vnet.StructureKindForge:
		offsets, clear = forgeFootprint, forgeHeadroom
	case vnet.StructureKindCampfire:
		offsets, clear = campfireFootprint, campfireHeadroom
	case vnet.StructureKindRunestone:
		offsets, clear = runestoneFootprint, runestoneHeadroom
	default:
		return nil, 0, false
	}

	cells = make([][3]int64, 0, len(offsets))
	for _, offset := range offsets {
		rotated := rotateOffset(offset, facing)
		cells = append(cells, [3]int64{anchor[0] + rotated[0], anchor[1], anchor[2] + rotated[1]})
	}
	return cells, clear, true
}

// footprintFitsLocked reports whether the world under and above a footprint will hold a
// structure, and names the first cell that will not.
//
// **A non-generating read, like every terrain read on this side.** A cell in a chunk the
// server has not composed yet is refused rather than waited for: the alternative is a
// session goroutine holding the simulation's lock across a chunk generation, which is a
// tick every connected player misses. A player standing within EditReach of the anchor
// has had the chunk it lands in for a long time, so the refusal is a boundary case
// rather than the common one.
//
// **Two answers, and neither is derived from the other.** The error is the sentence an
// operator reads in a debug line, naming the exact cell; the reason is the code the
// player is told, and it crosses the wire. Deriving one from the other would mean
// parsing prose to decide what to say — which is how a log line becomes a contract.
// A nil error always comes with RefusalReasonUnknown, because there is nothing to name.
//
// The caller holds Sim.mu.
func (s *Sim) footprintFitsLocked(cells [][3]int64, headroom int64) (vnet.RefusalReason, error) {
	for _, cell := range cells {
		block, resident := s.terrain.Block(cell[0], cell[1], cell[2])
		if !resident {
			return vnet.RefusalReasonGroundNotGenerated, fmt.Errorf("the ground at %v has not been generated yet", cell)
		}
		if block == world.Air {
			return vnet.RefusalReasonGroundIsAir, fmt.Errorf("the ground at %v is air; a structure needs something to stand on", cell)
		}

		for above := int64(1); above <= headroom; above++ {
			block, resident := s.terrain.Block(cell[0], cell[1]+above, cell[2])
			if !resident {
				return vnet.RefusalReasonSpaceNotGenerated, fmt.Errorf("the space at %v has not been generated yet", [3]int64{cell[0], cell[1] + above, cell[2]})
			}
			if block != world.Air {
				return vnet.RefusalReasonSpaceBlocked, fmt.Errorf("block %d is in the way at %v", uint16(block), [3]int64{cell[0], cell[1] + above, cell[2]})
			}
		}
	}
	return vnet.RefusalReasonUnknown, nil
}

// tentOfLocked is the standing tent this player owns, if there is one.
//
// The lowest structure id among them, which cannot matter while the one-tent rule holds
// and is what makes the answer deterministic if it ever stops holding. O(structures) per
// respawn and per placement, on the explicit trade the drops and the mobs already
// record: a spatial or per-owner index is worth building when the linear term matters
// and not one issue before.
//
// The caller holds Sim.mu.
func (s *Sim) tentOfLocked(owner identity.PlayerID) (*structure, bool) {
	var best *structure
	for _, candidate := range s.structures {
		if candidate.kind != vnet.StructureKindTent || candidate.owner != owner {
			continue
		}
		if best == nil || candidate.structureID < best.structureID {
			best = candidate
		}
	}
	return best, best != nil
}

// runestonesOfLocked counts one identity's stones in O(structures). Only placement
// enforces the cap; restore keeps every stone an older file already holds. Sim.mu is held.
func (s *Sim) runestonesOfLocked(owner identity.PlayerID) int {
	standing := 0
	for _, candidate := range s.structures {
		if candidate.kind == vnet.StructureKindRunestone && candidate.owner == owner {
			standing++
		}
	}
	return standing
}

// rebuildWardsLocked recomputes the whole ward map from the standing runestones.
//
// **Rebuilt rather than patched, and the overlap is why.** A ward is the union of squares
// that may overlap, so removing one stone cannot be expressed as "delete its columns" —
// some of those columns belong to a neighbour's stone as well, and a deletion would drop
// a claim nobody gave up. Recomputing is O(runestones) with a constant of nine columns,
// runs only when a runestone is placed, removed, collapsed or restored, and is by
// construction the same answer whichever order those happened in.
//
// **Where two wards overlap, the earlier stone wins**, and "earlier" is the lower
// structure id because ids only increase. The pass therefore walks the stones in id order
// and never overwrites a column that is already claimed. It is a rule rather than an
// accident of map iteration: without the sort, whose ground the overlap is would be
// decided by a hash seed and could change on a restart.
//
// The caller holds Sim.mu.
func (s *Sim) rebuildWardsLocked() {
	stones := make([]*structure, 0, len(s.structures))
	for _, held := range s.structures {
		if held.kind == vnet.StructureKindRunestone {
			stones = append(stones, held)
		}
	}
	if len(stones) == 0 {
		// nil rather than an empty map: wardOf reads a nil map correctly, and a world
		// nobody has claimed any ground in carries no allocation for the fact.
		s.wards = nil
		return
	}
	slices.SortFunc(stones, func(a, b *structure) int {
		return compareEntityIDs(a.structureID, b.structureID)
	})

	wards := make(map[world.Column]identity.PlayerID, len(stones)*(2*WardChunkRadius+1)*(2*WardChunkRadius+1))
	for _, stone := range stones {
		centre := stone.chunk.Column()
		for dx := int32(-WardChunkRadius); dx <= WardChunkRadius; dx++ {
			for dz := int32(-WardChunkRadius); dz <= WardChunkRadius; dz++ {
				col := world.Column{CX: centre.CX + dx, CZ: centre.CZ + dz}
				if _, claimed := wards[col]; claimed {
					continue
				}
				wards[col] = stone.owner
			}
		}
	}
	s.wards = wards
}

// wardOf is who owns the ground in one chunk column, if anybody does.
//
// A map hit and nothing else, which is the whole reason the radius is measured in chunk
// columns: this is asked on the edit path, on the mining path, on the placement path and,
// once the storm lands, once per chunk it is about to scour.
//
// The caller holds Sim.mu.
func (s *Sim) wardOf(col world.Column) (identity.PlayerID, bool) {
	owner, warded := s.wards[col]
	return owner, warded
}

// wardedAgainstLocked reports whether this voxel stands on ground somebody other than the
// actor has claimed.
//
// **The one predicate every refusal below asks**, so that "the owner is exempt" is one
// sentence in one place rather than four copies of an inequality. Unclaimed ground and
// the actor's own ground are the same answer, deliberately: neither is a refusal, and a
// caller that had to tell them apart would be a caller that could get the exemption
// wrong.
//
// The caller holds Sim.mu.
func (s *Sim) wardedAgainstLocked(voxel [3]int64, actor identity.PlayerID) bool {
	owner, warded := s.wardOf(world.ChunkOf(voxel[0], voxel[1], voxel[2]).Column())
	return warded && owner != actor
}

// wardedFootprintAgainstLocked names the first ground cell in a structure footprint
// claimed by somebody other than the actor. A structure occupies its whole footprint,
// so an unclaimed anchor cannot make a tent or forge legal across a ward boundary.
//
// The caller holds Sim.mu.
func (s *Sim) wardedFootprintAgainstLocked(cells [][3]int64, actor identity.PlayerID) ([3]int64, bool) {
	for _, cell := range cells {
		if s.wardedAgainstLocked(cell, actor) {
			return cell, true
		}
	}
	return [3]int64{}, false
}

// PlaceStructure resolves one PlaceStructureRequest and plants the structure if it is
// legal, returning the inventory the placement spent an item from and, when it refuses,
// the code that says why.
//
// The world's counterpart to Edit for things that are entities. Every refusal is still
// an ordinary error the session logs at debug, and a client must never treat its own
// request as applied — the structure exists when a snapshot says it does. What changed
// with legacy PR 205 is that the refusal is no longer *only* a log line: the reason beside the
// error is what the session puts on the wire, so a placement that does not happen is an
// answer rather than a click that vanished.
//
// **The code and the error are two outputs, not one.** The error is prose for the
// operator and names the exact cell; the code is a wire member the player is told
// about, and it is returned rather than parsed back out of the sentence. A successful
// placement answers RefusalReasonUnknown, which is the zero value and means "nothing
// was refused" — it is never sent, because nothing is sent when nothing is refused.
//
// The order of the checks is the design rather than an accident of writing:
//
//  1. Facts about the frame — an anchor at all, a facing a client may send, and a slot
//     inside the announced inventory.
//  2. What the named slot holds, read once without the simulation's lock, so a request
//     naming a stack of stone is refused before anything else is consulted.
//  3. Everything authoritative, in one critical section: liveness, reach against the
//     position **the server computed**, the footprint against terrain the server holds,
//     the one-tent rule, the item spend, and the registry insert.
//
// **Step 3 is one section on purpose.** The collapse rule below is what keeps a
// structure from floating over a hole somebody dug, and it can only see structures that
// are in the registry — so validating the ground and inserting the structure in two
// sections would leave a window in which a break passes between them and is never
// noticed. Nothing in that section blocks: the terrain read is non-generating and the
// inventory is taken with TryLock, exactly as the tick takes it.
func (p *Player) PlaceStructure(req protocol.PlaceStructureRequest) (protocol.InventoryState, vnet.RefusalReason, error) {
	if !req.HasAnchor {
		// Absent rather than zero: schemas/player.fbs refuses to read a missing anchor as
		// the origin, because the origin is a real place somebody would then have built at
		// without naming it.
		return protocol.InventoryState{}, vnet.RefusalReasonMalformedNoAnchor, errors.New("the request carries no anchor")
	}
	if !knownFacing(req.Facing) {
		return protocol.InventoryState{}, vnet.RefusalReasonMalformedFacing, fmt.Errorf("facing %s is not a direction a client may send", req.Facing)
	}
	if req.Slot >= protocol.InventorySlots {
		return protocol.InventoryState{}, vnet.RefusalReasonMalformedSlot, fmt.Errorf("inventory slot %d is outside the announced %d slots", req.Slot, protocol.InventorySlots)
	}

	// A snapshot, not the spend. It answers "is this request about a structure at all"
	// without holding the simulation's lock; the slot is re-read under the lock below
	// and the spend happens there.
	p.inventory.mu.Lock()
	stack, held := p.inventory.stackAtLocked(req.Slot)
	p.inventory.mu.Unlock()
	if !held {
		return protocol.InventoryState{}, vnet.RefusalReasonSlotEmpty, fmt.Errorf("inventory slot %d is empty", req.Slot)
	}
	kind, isStructure := knownStructureKind(stack.item)
	if !isStructure {
		// Not a malformed request, and the distinction is deliberate. A correct client
		// only offers the button while a structure is selected, but the pack can change
		// between the click and its arrival — so this is the world answering, not the
		// peer misbehaving, and a player is told about it.
		return protocol.InventoryState{}, vnet.RefusalReasonSlotUnusable, fmt.Errorf("item %d in inventory slot %d plants no structure", uint16(stack.item), req.Slot)
	}

	anchor := [3]int64{int64(req.Anchor[0]), int64(req.Anchor[1]), int64(req.Anchor[2])}
	cells, headroom, known := footprintOf(kind, req.Facing, anchor)
	if !known {
		// Unreachable: knownStructureKind and footprintOf are switches over the same
		// kinds. Stated rather than assumed, and the campfire is why it was worth stating
		// — it arrived as one entry in each switch, and a kind that had gained only the
		// first of them would otherwise have been placed on nothing.
		//
		// Coded as malformed rather than as the world refusing, because it is a defect
		// either way and the one thing it must not look like is flat ground saying no.
		return protocol.InventoryState{}, vnet.RefusalReasonMalformedKind, fmt.Errorf("structure kind %s has no footprint", kind)
	}

	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if err := p.cannotActLocked(); err != nil {
		return protocol.InventoryState{}, vnet.RefusalReasonPlayerIsDead, err
	}
	if reach, distance := p.reachLocked(), distanceToVoxel(p.pos, anchor); distance > reach {
		return protocol.InventoryState{}, vnet.RefusalReasonOutOfReach, fmt.Errorf("the anchor is %.2f blocks from the player, past the reach of %.1f", distance, reach)
	}
	// Beside the reach check and under this lock, which is where every ward check in this
	// server sits. The alternative — pushing it into the placement cache's `allow`
	// predicate, where the edit path's legality already lives — is not available: that
	// predicate must stay pure and non-blocking because every chunk being composed
	// anywhere in the server waits behind the lock it runs under, and a ward is
	// simulation state guarded by a different one.
	//
	// It names no owner, deliberately. The refusal a player is shown says the ground is
	// claimed and not by whom, because an answer that named one would let a client learn
	// who has claimed which ground by walking around poking at it.
	if cell, warded := p.sim.wardedFootprintAgainstLocked(cells, p.playerID); warded {
		return protocol.InventoryState{}, vnet.RefusalReasonWarded, fmt.Errorf("the structure footprint at %v is warded by another player", cell)
	}
	if reason, err := p.sim.footprintFitsLocked(cells, headroom); err != nil {
		return protocol.InventoryState{}, reason, err
	}
	if kind == vnet.StructureKindTent {
		// One tent to a player, because a tent is where they come back to and two answers
		// to that is a choice nobody made. Forges and campfires are unlimited: what
		// throttles a forge is eight stone and two coal, and a fire four logs and a coal,
		// which is a cost rather than a rule. **A camp may have several fires** — the
		// suppression radius is a property of each one, not a per-owner allowance, and
		// extending the tent's rule to cover them would make the second fire a refusal
		// nobody asked for.
		if existing, standing := p.sim.tentOfLocked(p.playerID); standing {
			return protocol.InventoryState{}, vnet.RefusalReasonTentAlreadyPlaced, fmt.Errorf("structure %d is already this player's tent", existing.structureID)
		}
	}
	if kind == vnet.StructureKindRunestone {
		// The tent's rule with a budget in place of a singleton, keyed by the same
		// identity for the same reason: a claim outlives every session that made it, so
		// counting by entity id would refill the allowance on every reconnect. It shares
		// the tent's refusal reason — see MaxRunestonesPerPlayer, where that is argued —
		// so the client renders one sentence for "you already have as many as you may
		// have" whichever structure asked.
		if standing := p.sim.runestonesOfLocked(p.playerID); standing >= MaxRunestonesPerPlayer {
			return protocol.InventoryState{}, vnet.RefusalReasonTentAlreadyPlaced,
				fmt.Errorf("this player already has %d runestones standing, which is the limit of %d", standing, MaxRunestonesPerPlayer)
		}
	}

	// TryLock, never Lock, and it is the same discipline the tick uses: the pair stays
	// deadlock-free by construction rather than by lock ordering. It cannot fail from
	// here — every other holder of this inventory is either this same session goroutine
	// or the tick, and the tick only ever takes it under the lock this function is
	// holding — and it is written as a refusal anyway, because "cannot fail" is a
	// property of today's callers rather than of the lock.
	if !p.inventory.mu.TryLock() {
		return protocol.InventoryState{}, vnet.RefusalReasonInventoryBusy, errors.New("the inventory is busy")
	}
	defer p.inventory.mu.Unlock()

	if !p.inventory.consumeOneLocked(req.Slot, stack.item) {
		return protocol.InventoryState{}, vnet.RefusalReasonSlotChanged, fmt.Errorf("inventory slot %d no longer holds item %d", req.Slot, uint16(stack.item))
	}

	placed := &structure{
		structureID: p.sim.mintEntityID(),
		kind:        kind,
		anchor:      req.Anchor,
		facing:      req.Facing,
		owner:       p.playerID,
		chunk:       world.ChunkOf(anchor[0], anchor[1], anchor[2]),
	}
	if kind == vnet.StructureKindCampfire {
		placed.doused = p.sim.campfireDousedLocked(p.sim.worldTick, placed)
	}
	p.sim.structures[placed.structureID] = placed
	p.sim.structuresDirty = true
	if placed.kind == vnet.StructureKindRunestone {
		// Inside the same critical section that inserted it, so no reader can see a
		// standing stone with no ward — and only for the kind that casts one, so a camp
		// full of fires costs nothing.
		p.sim.rebuildWardsLocked()
	}

	p.sim.log.Debug("structure placed",
		"structure_id", placed.structureID, "kind", placed.kind.String(),
		"anchor", placed.anchor, "facing", placed.facing.String(), "owner", placed.owner.Short())

	return p.inventory.stateLocked(), vnet.RefusalReasonUnknown, nil
}

// RemoveStructure resolves one RemoveStructureRequest and takes the structure back.
//
// The item is put on the ground rather than into the pack, for the reason a mined block
// is: what a player ends up carrying is decided by walking over it, and a full inventory
// is a reason to leave something lying there rather than a reason to destroy it.
//
// Every refusal is silence. There is deliberately no distinction on the wire between an
// id nobody has, one this player does not own and one too far away — a client that could
// tell those apart could map somebody else's camp by asking.
func (p *Player) RemoveStructure(req protocol.RemoveStructureRequest) error {
	removed, spawn, err := p.removeOwnStructure(req.StructureID)
	if err != nil {
		return err
	}

	// Outside the lock, because spawnDrop takes it. The structure is already gone from
	// the registry, so the worst a failure here could cost is the item — and spawnDrop
	// only refuses an unregistered one, which structureItem has already ruled out.
	if item, known := structureItem(removed.kind); known {
		p.sim.spawnDrop(item, 1, spawn)
	}

	p.sim.log.Debug("structure removed",
		"structure_id", removed.structureID, "kind", removed.kind.String(),
		"anchor", removed.anchor, "owner", removed.owner.Short())
	return nil
}

// removeOwnStructure takes one structure out of the registry if this player may, and
// returns what it was and the first resident air voxel above its intact anchor.
//
// Split from RemoveStructure so the whole authoritative decision, including where its
// drop can exist, is one critical section and the drop that follows is outside it.
func (p *Player) removeOwnStructure(structureID uint64) (structure, [3]int64, error) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if err := p.cannotActLocked(); err != nil {
		return structure{}, [3]int64{}, err
	}

	held, standing := p.sim.structures[structureID]
	if !standing {
		return structure{}, [3]int64{}, fmt.Errorf("no structure %d stands in this world", structureID)
	}
	if held.owner != p.playerID {
		// The owner is named by its short id and not by the identity itself, for the
		// reason this refusal reaches nobody but the log: a client is told nothing at
		// all (see RemoveStructure), and an operator reading a log line needs to tell
		// two players apart rather than to hold either one's key.
		return structure{}, [3]int64{}, fmt.Errorf("structure %d belongs to player %s", structureID, held.owner.Short())
	}
	if reach, distance := p.reachLocked(), distanceToVoxel(p.pos, held.anchorVoxel()); distance > reach {
		return structure{}, [3]int64{}, fmt.Errorf("structure %d is %.2f blocks away, past the reach of %.1f", structureID, distance, reach)
	}
	// Beside the reach check, like every other ward check. Removal is the one refused
	// action that stays *silent*, and it stays silent for the reason every other removal
	// refusal does: a client that could tell "not yours", "too far" and "warded" apart
	// could map somebody else's camp by asking. The check is still worth making — the
	// structure this refuses is the actor's own, standing inside ground a neighbour has
	// since claimed, which the owner check above would have let through.
	cells, _, known := footprintOf(held.kind, held.facing, held.anchorVoxel())
	if !known {
		return structure{}, [3]int64{}, fmt.Errorf("structure %d has unknown kind %s", structureID, held.kind)
	}
	if cell, warded := p.sim.wardedFootprintAgainstLocked(cells, p.playerID); warded {
		return structure{}, [3]int64{}, fmt.Errorf("structure %d stands on ground warded by another player at %v", structureID, cell)
	}

	spawn, clear := p.sim.firstFreeVoxelAboveLocked(held.anchorVoxel())
	if !clear {
		return structure{}, [3]int64{}, fmt.Errorf("structure %d has no resident free space above its anchor", structureID)
	}

	delete(p.sim.structures, structureID)
	p.sim.structuresDirty = true
	if held.kind == vnet.StructureKindRunestone {
		// Under the same lock the removal happened in, which is what makes "the ward
		// goes on the tick the stone does" true rather than eventual: no reader can
		// observe the world between the two statements.
		p.sim.rebuildWardsLocked()
	}
	return *held, spawn, nil
}

// firstFreeVoxelAboveLocked finds where a manually removed structure's drop can begin
// without intersecting its intact support or terrain added over it since placement.
// Unknown terrain is not free, and the upper bound keeps the drop's whole box inside the
// arithmetic world. The caller holds Sim.mu, as every non-generating terrain decision does.
func (s *Sim) firstFreeVoxelAboveLocked(anchor [3]int64) ([3]int64, bool) {
	if anchor[0] < -worldLimit || anchor[0] >= worldLimit || anchor[2] < -worldLimit || anchor[2] >= worldLimit {
		return [3]int64{}, false
	}
	for y := anchor[1] + 1; y < worldLimit; y++ {
		candidate := [3]int64{anchor[0], y, anchor[2]}
		block, resident := s.terrain.Block(candidate[0], candidate[1], candidate[2])
		if !resident {
			return [3]int64{}, false
		}
		if block == world.Air {
			return candidate, true
		}
	}
	return [3]int64{}, false
}

// collapseStructuresAt removes every structure the broken voxel was holding up and
// returns them, so the caller can put their items on the ground outside the lock.
//
// **The rule is "a supporting ground block stopped being solid".** A structure is not a
// voxel and does not collide with one, so nothing else about the world can knock one
// down — but a tent over a pit and an anvil in mid-air are worlds the server would be
// asserting, and one break is enough to make either.
//
// **A world-owned station does not come down, and the reason is duplication.** Nothing
// removes one and the seed puts it back the next time its chunk enters a view, so a
// collapse that dropped its item would leave a forge on the ground *and* a forge still in
// the village — one crafted station per break, for as long as somebody keeps digging.
// Digging under a village forge therefore leaves it standing on nothing, which is the
// honest consequence of "never despawned" rather than an oversight.
//
// O(structures × footprint) per completed break, on the explicit trade the drops and the
// mobs already record. The footprint is recomputed rather than stored, because it is
// derived from two fields that never change and a second copy is a second thing to keep
// in step.
func (s *Sim) collapseStructuresAt(voxel [3]int64) []structure {
	s.mu.Lock()
	defer s.mu.Unlock()

	var (
		collapsed []structure
		claimed   bool
	)
	for id, held := range s.structures {
		if held.worldOwned() {
			continue
		}
		cells, _, known := footprintOf(held.kind, held.facing, held.anchorVoxel())
		if !known || !slices.Contains(cells, voxel) {
			continue
		}
		delete(s.structures, id)
		s.structuresDirty = true
		if held.kind == vnet.StructureKindRunestone {
			claimed = true
		}
		collapsed = append(collapsed, *held)
	}
	if claimed {
		// After the loop rather than inside it: a single break can bring down more than
		// one stone, and rebuilding per removal would compute an intermediate ward map
		// nothing is allowed to see anyway.
		s.rebuildWardsLocked()
	}

	// Ordered by identity, so a break that brings down two structures spawns their drops
	// in the same order every run. Map iteration would leave that to a hash seed.
	slices.SortFunc(collapsed, func(a, b structure) int { return compareEntityIDs(a.structureID, b.structureID) })
	return collapsed
}

// dropCollapsed puts the item each collapsed structure left behind at its anchor.
//
// Called with no lock held, because spawnDrop takes the simulation's own.
func (s *Sim) dropCollapsed(collapsed []structure) {
	for _, fallen := range collapsed {
		item, known := structureItem(fallen.kind)
		if !known {
			continue
		}
		s.spawnDrop(item, 1, fallen.anchorVoxel())
		s.log.Debug("structure collapsed",
			"structure_id", fallen.structureID, "kind", fallen.kind.String(),
			"anchor", fallen.anchor, "owner", fallen.owner.Short())
	}
}

// sortedStructuresLocked is every structure in identity order.
//
// Stable order for the reason the mobs and the drops have one: the tick is reproducible
// and a snapshot's structure vector is the same bytes from the same state.
func (s *Sim) sortedStructuresLocked() []*structure {
	structures := make([]*structure, 0, len(s.structures))
	for _, held := range s.structures {
		structures = append(structures, held)
	}
	slices.SortFunc(structures, func(a, b *structure) int {
		return compareEntityIDs(a.structureID, b.structureID)
	})
	return structures
}

// StructureCount is how many structures stand in the world.
func (s *Sim) StructureCount() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.structures)
}

// structureStatesLocked is the wire form of every structure, in the order it was given.
//
// **The owner crosses the seam here, and only here.** The registry keys ownership by
// identity, because a camp outlives a session; the wire carries an entity id, because
// that is the handle a client can match against the players it can see. The resolution
// between them is this lookup: the owner's current entity id while they have a live
// session, and 0 otherwise.
//
// Zero is the contract's way of saying "the owner is offline" (schemas/player.fbs, V5),
// not "unowned" and not "owned by entity 0" — no entity is ever numbered 0, so an
// offline owner matches nobody and a client cannot mistake somebody else's camp for its
// own. It is also why the identity never goes out: an identity is what a player record
// is keyed by, and putting one on the wire would hand every client a key to every camp.
//
// The lookup is a map hit per structure rather than a scan of the players, which is
// what the identity index exists for — the alternative is O(structures × players) once
// per tick.
//
// The caller holds Sim.mu.
func (s *Sim) structureStatesLocked(structures []*structure) []protocol.StructureState {
	states := make([]protocol.StructureState, len(structures))
	for i, held := range structures {
		var ownerEntityID uint64
		if live, online := s.byIdentity[held.owner]; online {
			ownerEntityID = live.entityID
		}
		states[i] = protocol.StructureState{
			StructureID:   held.structureID,
			Kind:          held.kind,
			Anchor:        held.anchor,
			Facing:        held.facing,
			OwnerEntityID: ownerEntityID,
			Doused:        held.doused,
		}
	}
	return states
}

// ---------------------------------------------------------------------------
// What a camp is, once it has to survive the process
// ---------------------------------------------------------------------------

// Structure is one placed tent, forge or campfire, as it is written down.
//
// The four fields that outlive a process. Everything else a live structure carries is
// derived from them: the chunk from the anchor, the id from the counter that names
// every entity the simulation owns.
//
// Declared here and again as persist.StructureRecord, exactly as [Life] is declared
// again as persist.Record and for the same reason: game and persist do not import each
// other, so a store never decides what a camp may say and the simulation never decides
// how one is written down. cmd/voxelheimd is the one place that maps between them, as
// session is for a life.
type Structure struct {
	Kind   vnet.StructureKind
	Anchor [3]int32
	Facing vnet.Facing
	Owner  identity.PlayerID
}

// Structures is every structure standing, in the order [sortedStructuresLocked] gives.
//
// The capture half of the capture-and-write split this server keeps everywhere it
// touches a disk: the lock is taken, the values are copied, the lock is released, and
// the caller writes with nothing held. Deterministic order, so two saves of the same
// world are byte-identical and a test can compare files rather than sets.
func (s *Sim) Structures() []Structure {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.structuresLocked()
}

// structuresLocked is [Sim.Structures] with the lock already held.
func (s *Sim) structuresLocked() []Structure {
	standing := s.sortedStructuresLocked()
	out := make([]Structure, len(standing))
	for i, held := range standing {
		out[i] = Structure{
			Kind:   held.kind,
			Anchor: held.anchor,
			Facing: held.facing,
			Owner:  held.owner,
		}
	}
	return out
}

// TakeDirtyStructures is the camp to write, and whether there is anything to write.
//
// The chunk cache's takeDirty, in the shape a single whole-file store needs: it clears
// the flag and hands back the snapshot, so a save that finds nothing dirty costs one
// lock and no I/O. **A caller whose write fails must call [Sim.MarkStructuresDirty]**,
// which is the same contract world.Cache.Flush keeps by re-marking the chunk it could
// not save — without it a failed write would drop the change for good, because the
// registry it came from has already forgotten it was new.
func (s *Sim) TakeDirtyStructures() ([]Structure, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if !s.structuresDirty {
		return nil, false
	}
	s.structuresDirty = false
	return s.structuresLocked(), true
}

// MarkStructuresDirty puts the camp back in the queue to be written.
//
// For a caller whose write failed. See [Sim.TakeDirtyStructures].
func (s *Sim) MarkStructuresDirty() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.structuresDirty = true
}

// RestoreStructures puts a stored camp back in the world, or refuses the whole of it.
//
// **Refused whole or restored whole**, which is [Life.Validate]'s rule applied to a
// file that describes many things instead of one. A camp half believed is a world in
// which some of what a player built came back — and there is no way for them to tell
// which half is missing because it was never there and which because a byte flipped.
// The caller's answer to an error is to start with no structures and to keep the file.
//
// Ids are minted here rather than read, which is what makes it safe to leave them off
// the disk: they come from the same counter that names players, drops and mobs, so
// "one id names one thing" holds across a restart without the counter itself ever being
// serialised. A stored id would have to be either re-used — colliding with whatever the
// counter hands out next — or restored along with the counter, which puts a second
// thing on disk that has to stay in step with the first.
//
// # What is checked, and what deliberately is not
//
// Every rule here is one placement already enforces, so the set of files this accepts is
// exactly the set this server can write: a known kind, a known facing, an owner that
// names somebody, and one tent to a player.
//
// **Overlapping footprints are not refused, and that is not an oversight.** Placement
// validates a footprint against *terrain* and not against the other structures standing
// in it, so a forge inside its owner's tent is legal today and is a thing a player can
// have built. Refusing it here would mean a camp that this server wrote, accepted and
// drew for weeks became an unloadable file the first time it was restarted — the exact
// shape of data loss this whole issue exists to prevent. If structures ever stop being
// allowed to overlap, the rule belongs in PlaceStructure first and here second.
//
// Support is not validated either: reading the terrain under every footprint would
// generate every chunk under every camp before the first session is accepted. The gap
// that leaves is documented on [structure].
func (s *Sim) RestoreStructures(stored []Structure) error {
	restored := make(map[uint64]*structure, len(stored))
	tents := make(map[identity.PlayerID]struct{}, len(stored))

	for i, entry := range stored {
		if _, _, known := footprintOf(entry.Kind, entry.Facing, [3]int64{}); !known {
			return fmt.Errorf("game: stored structure %d is of kind %s, which this build cannot place", i, entry.Kind)
		}
		if !knownFacing(entry.Facing) {
			return fmt.Errorf("game: stored structure %d faces %s, which is not a direction this build knows", i, entry.Facing)
		}
		if entry.Owner == (identity.PlayerID{}) {
			// The zero id is the digest of nothing and names nobody, exactly as it does
			// in Join. A structure owned by it could never be taken down and would be
			// nobody's respawn — it would stand for the life of the world.
			return fmt.Errorf("game: stored structure %d has no owner", i)
		}
		if entry.Kind == vnet.StructureKindTent {
			if _, second := tents[entry.Owner]; second {
				return fmt.Errorf("game: stored structure %d is a second tent for player %s", i, entry.Owner.Short())
			}
			tents[entry.Owner] = struct{}{}
		}
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	if len(s.structures) != 0 {
		// Startup only, before the first session is accepted. Stated rather than
		// assumed, because restoring into a world that already has a camp in it would
		// silently drop whatever was standing.
		return fmt.Errorf("game: %d structures already stand; a stored camp is restored into an empty world", len(s.structures))
	}

	// **Every refusal is above this line, and that is what makes "a refused file spends
	// nothing" true.** Minting is the one step with a side effect outside this map — the
	// counter that names players, drops and mobs moves — so it runs only once the answer
	// is known to be yes. Under the lock, which is where PlaceStructure mints too:
	// mintEntityID takes the registry's own lock and nothing else.
	for _, entry := range stored {
		anchor := [3]int64{int64(entry.Anchor[0]), int64(entry.Anchor[1]), int64(entry.Anchor[2])}
		held := &structure{
			structureID: s.mintEntityID(),
			kind:        entry.Kind,
			anchor:      entry.Anchor,
			facing:      entry.Facing,
			owner:       entry.Owner,
			chunk:       world.ChunkOf(anchor[0], anchor[1], anchor[2]),
		}
		restored[held.structureID] = held
	}
	s.structures = restored
	// The wards a stored camp brings back with it. Derived rather than stored, for the
	// reason the ids are: a ward is a function of where the stones are, and a second copy
	// on disk is a second thing that has to stay in step with the first.
	s.rebuildWardsLocked()

	// Deliberately not marked dirty. What was just loaded is what the file already
	// holds, and marking it would make every restart rewrite a byte-identical file —
	// and, worse, would turn a start that failed for some other reason into one that
	// had already overwritten the camp it could not use.
	return nil
}
