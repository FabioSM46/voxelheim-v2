package game

import (
	"slices"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The village forge belongs to nobody.
//
// A world-owned station is an ordinary [structure] whose owner is the zero
// [identity.PlayerID], and almost nothing needed a new rule for it: removal refuses one
// because no live player is that identity, [Sim.tentOfLocked] never matches one because a
// station is not a tent, and [Sim.stationWithinLocked] accepts one because crafting never
// consulted the owner at all. The wire is untouched.
//
// **Nothing about one is written down.** structures.bin is for what players did; the seed
// is for what the world is. A station's position is a settlement anchor and its id is a
// hash of the seed and the column it stands on, both pure functions of the seed — so a
// restart re-derives the same forge with the same id the first time somebody looks at that
// chunk again, and no file exists that could disagree with the world.

const (
	// worldOwnedStructureBit marks an id as derived rather than minted.
	//
	// **The high bit is free, and provably so**: every other entity takes its id from
	// session.Registry.NextID, a counter from zero, so reaching 2^63 would take nine
	// quintillion mints. The disjointness is load-bearing rather than tidy —
	// schemas/player.fbs requires a structure id to be unique against every player, drop,
	// mob and structure in one snapshot, and a client that meets a collision closes the
	// connection rather than dropping the frame. It also makes the id non-zero, which the
	// same contract requires.
	worldOwnedStructureBit uint64 = 1 << 63

	// worldStructureSeedOffset is this feature's own offset from the world seed, in the
	// style internal/world gives every decision one: an id must not be a function of
	// anything else the seed decides, or moving one system would move the other.
	worldStructureSeedOffset int64 = 0x3B1C8C2A
)

// worldOwned reports whether nobody placed this structure.
//
// The zero identity is the digest of nothing and names no player — [Sim.Join] refuses one,
// [Sim.RestoreStructures] refuses a stored structure carrying one — so it is free to mean
// "the world put this here" with no live player ever matching it.
func (s *structure) worldOwned() bool { return s.owner == identity.PlayerID{} }

// worldStructureID is the identity of the station standing on one column.
//
// Derived, never minted: two servers running one seed have to name the same forge and
// nothing tells either about the other. The column is enough to be unique — a building has
// at most one station slot and no two buildings of a settlement share a footprint. A
// 63-bit hash can still collide, and if one ever did,
// [Sim.materialiseSettlementsLocked]'s existence check makes it a station quietly never
// created rather than a duplicate id in a snapshot, which is the safe direction.
func worldStructureID(seed, x, z int64) uint64 {
	return world.HashLattice(seed+worldStructureSeedOffset, x, z) | worldOwnedStructureBit
}

// stationKind is the structure a settlement anchor asks for, and whether it asks for one.
//
// internal/world offers slots for residents and guards too, and they are not structures: an
// anchor this build has nothing to put in is passed over rather than defaulted, which is
// [knownStructureKind]'s rule read from the other side.
func stationKind(anchor world.AnchorKind) (vnet.StructureKind, bool) {
	switch anchor {
	case world.AnchorForge:
		return vnet.StructureKindForge, true
	case world.AnchorCampfire:
		return vnet.StructureKindCampfire, true
	default:
		return vnet.StructureKindUnknown, false
	}
}

// facingTowards is the compass member that points from a column at a settlement's centre.
//
// The compass is the movement integrator's, as [rotateOffset] reads it: North is -Z, East
// is +X, South is +Z, West is -X. The larger axis wins, so a station on a diagonal faces
// the cardinal nearest the middle — world.facingTowardsCentre's rule for the building
// around it. A station *at* the centre has no direction to face and keeps North.
func facingTowards(x, z, centreX, centreZ int64) vnet.Facing {
	dx, dz := centreX-x, centreZ-z
	if absInt64(dx) >= absInt64(dz) {
		switch {
		case dx > 0:
			return vnet.FacingEast
		case dx < 0:
			return vnet.FacingWest
		default:
			return vnet.FacingNorth
		}
	}
	if dz > 0 {
		return vnet.FacingSouth
	}
	return vnet.FacingNorth
}

// absInt64 is |v|, which internal/game had no need of until a bearing had to be chosen.
func absInt64(v int64) int64 {
	if v < 0 {
		return -v
	}
	return v
}

// MaterialiseSettlements creates whatever world-owned stations stand in one chunk.
//
// A method on [Player] for [Player.WakeStreaming]'s reason: the caller is that session's
// streamer, reporting that a chunk entered *this* player's view. The simulation decides.
func (p *Player) MaterialiseSettlements(coord world.Coord) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	p.sim.materialiseSettlementsLocked(coord)
}

// materialiseSettlementsLocked puts a forge in every smithy, a fire in every hall and a
// person in every resident slot and a horse at every paddock anchor this chunk holds, for
// whatever is not there already.
//
// **One hook for all three resident classes, deliberately.** A station, a person and a
// paddock horse are different things — one is a structure a client draws from a footprint,
// the others are separately safe entities in the mob stream — but they are the same *fact about the world*: something the
// seed put in a settlement that nothing wrote down. Asking the question twice would mean
// two passes over world.SettlementsNear per chunk for one answer.
//
// **Idempotent, because the id is the answer**: a second pass derives the same number,
// finds it in the registry and does nothing. That is what makes the hook safe on a failed
// send's retry, and what a restart rests on.
//
// **Nothing here blocks and nothing here generates terrain.** world.SettlementsNear is
// arithmetic over hashes and noise — no chunk, no cache, no disk — which is what makes it
// legal under the lock a whole tick is under. It is not free, at roughly a microsecond per
// settled cell, and it runs once per chunk per view rather than per tick, so a join's
// several hundred chunks each pay it once in a critical section of their own.
//
// One lattice cell out is far more than enough: a settlement reaches 56 blocks from its
// centre at most, and a cell is 2048 across.
//
// The caller holds Sim.mu.
func (s *Sim) materialiseSettlementsLocked(coord world.Coord) {
	originX, _, originZ := coord.Origin()
	near := world.SettlementsNear(s.worldSeed, originX+world.ChunkSize/2, originZ+world.ChunkSize/2, 1)

	for _, settled := range near {
		var paddock []world.PlacedAnchor
		for _, slot := range settled.Anchors() {
			// The two answers a slot can have, asked in the order the vocabulary was
			// written: a forge or a fire goes to station.go's half, a smith or a guard to
			// resident.go's, and a paddock slot to paddock_horse.go's. Anything none of
			// them claims is passed over rather than defaulted.
			if kind, station := stationKind(slot.Kind); station {
				s.materialiseStationLocked(coord, settled, slot, kind)
				continue
			}
			if role, lives := residentRole(slot.Kind); lives {
				s.materialiseResidentLocked(coord, settled, slot, role)
				continue
			}
			if slot.Kind == world.AnchorPaddock {
				paddock = append(paddock, slot)
			}
		}

		// Rotation changes which local paddock slot is west or north, so the trio is
		// ordered in world coordinates and two things are read off that order: the colour
		// variants, one of each, and the route — the middle anchor is the oval's centre
		// and first-to-third its long axis. Every chunk-enter call sees all three anchors
		// and therefore derives the same variants and the same oval even when only one
		// horse belongs to the chunk being materialised. A settlement without a stable
		// offers no paddock slot and gets no horse.
		if len(paddock) == paddockHorseVariants {
			slices.SortFunc(paddock, paddockAnchorOrder)
			route := paddockRouteOf([paddockHorseVariants]world.PlacedAnchor(paddock))
			for variant, slot := range paddock {
				s.materialisePaddockHorseLocked(coord, slot, route, uint8(variant))
			}
		}
	}
}

// materialiseStationLocked puts one world-owned station in one anchor slot, if it is not
// standing already.
//
// **The ground, not the floor.** A world.PlacedAnchor names the cell a station occupies,
// which is a building's floor and therefore air; a [structure]'s anchor is the voxel it
// *rests on*. Taking the one below is what makes a village forge draw where a placed one
// would, and what puts it in the chunk of the ground it stands on — placement's own rule.
// [Sim.materialiseResidentLocked] is where the other half of that distinction is argued:
// a person stands *in* the air the drawing left them.
//
// The caller holds Sim.mu.
func (s *Sim) materialiseStationLocked(coord world.Coord, settled world.Settlement, slot world.PlacedAnchor, kind vnet.StructureKind) {
	// The block under the slot, and the chunk *that* block falls in — which is not always
	// the slot's own, and asking about the ground is what keeps materialisation and
	// visibility answering about one chunk.
	x, y, z := slot.X, slot.Y-1, slot.Z
	if world.ChunkOf(x, y, z) != coord {
		return
	}
	if x < -worldLimit || x >= worldLimit || z < -worldLimit || z >= worldLimit {
		// Unreachable from a streamed chunk, because a player cannot stand outside the
		// world. Refused rather than narrowed: the anchor is an int32 on the wire, and a
		// wrapped one is a station somewhere else entirely.
		return
	}

	id := worldStructureID(s.worldSeed, x, z)
	if _, standing := s.structures[id]; standing {
		return
	}

	s.structures[id] = &structure{
		structureID: id,
		kind:        kind,
		anchor:      [3]int32{int32(x), int32(y), int32(z)},
		facing:      facingTowards(x, z, settled.CentreX, settled.CentreZ),
		owner:       identity.PlayerID{},
		chunk:       coord,
	}

	// Deliberately not marked dirty: nothing about the camp changed. A station is
	// re-derived from the seed and filtered out on the way to disk, so setting the
	// flag would make walking into a village rewrite structures.bin for nothing.
	s.log.Debug("world-owned station materialised", "structure_id", id,
		"kind", kind.String(), "anchor", s.structures[id].anchor)
}
