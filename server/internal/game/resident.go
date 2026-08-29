package game

import (
	"fmt"
	"math"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The people the seed put there.
//
// A resident is the third entity class in this package, beside the mob and the
// structure, and the reason it is a class rather than a species row is that the two
// things a mob *is* are both wrong for it. The director's model — appear in a ring
// around a player, be counted against a ceiling, be taken away once nobody is watching
// (spawn.go) — cannot hold somebody who stands in a doorway because a village was drawn
// with a doorway. And [mobRegistry] is a table of health, damage, reach, telegraph
// timings, aggro radius and a loot roll: a row for a villager would be six zeroes and a
// lie, which is the argument species_test.go already makes for exempting
// `MobKind.Villager` from the registry.
//
// **They are not in [Sim.mobs], and that is the whole of what makes them safe.**
// `swingTargetLocked` scans `mobs`; a projectile scans `mobs`; the director counts,
// spawns and despawns `mobs`; a corpse is made from a `*mob`. A resident is invulnerable,
// unlootable, un-aggroable and un-despawnable because it is not in the collection any of
// those read — by construction rather than by a branch in each of them, and a branch in
// each of them is exactly what a fourth reader would forget to add.
//
// **What they share with a world-owned station is everything about where they come
// from**: a settlement anchor for a position, a hash of the seed and that column for an
// id, materialisation on the chunk-enters-view hook, and not one byte written down. See
// station.go, which owns that hook and argues the shape.

const (
	// residentBit marks an id as a resident's.
	//
	// Bit 62 for [worldOwnedStructureBit]'s reason and with its arithmetic: every minted
	// id comes from session.Registry.NextID, a counter from zero, so nothing minted will
	// reach 2^62 and the two derived spaces cannot reach each other's. Disjointness is
	// load-bearing rather than tidy — a resident travels in the same `MobState` vector as
	// the mobs and the corpses, schemas/player.fbs requires one id to name one entity in
	// one snapshot, and a client that meets a collision closes the connection. It also
	// makes the id non-zero, which the same contract requires.
	//
	// A hash whose own bit 63 happens to be set therefore produces an id with both top
	// bits set, which is inside the *structure* space. That is a 2^-62 coincidence away
	// from a real collision — the two hashes would have to agree on 62 further bits — and
	// it is the same residual [worldStructureID] carries and states.
	residentBit uint64 = 1 << 62

	// The three offsets from the world seed, in the style internal/world gives every
	// decision one and for [worldStructureSeedOffset]'s reason: who somebody is, what
	// they are called and what they look like must not be functions of one another, or
	// re-rolling a name would move a face.
	residentSeedOffset           int64 = 0x6F1B7A53
	residentNameSeedOffset       int64 = 0x2C9E4D17
	residentAppearanceSeedOffset int64 = 0x51A3F80B

	// residentHealth is the number the health bar over a resident carries.
	//
	// **A wire requirement rather than a simulation quantity.** `MobState.health` and
	// `MobState.max_health` are not optional, and nothing in this world can take a point
	// off a resident, so there is no state here that changes — the field is a constant
	// equal to its own maximum, which is what a client draws as a full bar or as no bar
	// at all. The player's hundred rather than an invented number, so a reader who wonders
	// what scale it is on has an answer.
	residentHealth uint16 = PlayerMaxHealth
)

// residentBody is the box a resident occupies.
//
// The player's dimensions, because a resident is a person — but written out rather than
// aliased to [playerBody], which is the argument species.go makes for stating the
// draugr's box in full: narrowing a corridor for players must not silently narrow
// somebody standing in one. It is used for exactly two distances, the notice radius and
// the interaction reach, and for no collision at all: nothing collides with a resident,
// because a resident is not in any collection the integrator reads.
var residentBody = body{width: PlayerWidth, height: PlayerHeight}

// resident is one person standing where the settlement drawing put them.
//
// Every field is guarded by Sim.mu. Nothing is persisted and nothing is ever removed: a
// resident is a fact about the world rather than a moment in a simulation, so a restart
// re-derives the same person with the same id and the same name the first time anybody
// looks at that chunk again.
type resident struct {
	entityID uint64

	// role is what this person does, and it is the only thing about them that crosses
	// the wire besides a name and a face. It comes from the anchor the schematic drew,
	// so the smith is the one standing at the smithy's slot.
	role vnet.ResidentRole

	name       string
	appearance protocol.Appearance

	// pos is the standing position of the resident's box — its minimum in y, its centre
	// in x and z, exactly as a player's and a mob's are. It never changes: a resident
	// does not walk, and there is no integrator that could move one.
	pos [3]float64

	// yaw is the only thing about a resident that a tick may change. See
	// [Sim.advanceResidentsLocked].
	yaw float64

	// home is the centre of the settlement this person belongs to, and restYaw is the
	// bearing derived from it: the way the anchor faces, which is the way they look when
	// nobody is near. Both are kept because they answer different questions — the centre
	// is the settlement's identity, the bearing is a number the turn arithmetic uses
	// twenty times a second.
	home    [2]int64
	restYaw float64
	chunk   world.Coord
}

// residentID is the identity of the person standing on one column.
//
// Derived, never minted, for [worldStructureID]'s reason: two servers running one seed
// have to name the same smith and nothing tells either about the other. The column is
// enough to be unique — no two anchors of one settlement share a column, and no two
// settlements share ground. A collision is answered the way a station's is:
// [Sim.materialiseResidentLocked]'s existence check makes it a person quietly never
// created rather than a duplicate id in a snapshot.
func residentID(seed, x, z int64) uint64 {
	return world.HashLattice(seed+residentSeedOffset, x, z) | residentBit
}

// residentRole is the trade a settlement anchor asks for, and whether it asks for one.
//
// [stationKind] read from the other side: an anchor this file has nothing to put in is
// passed over rather than defaulted, so the forge and the campfire slots fall through
// here exactly as the six resident slots fall through there.
func residentRole(anchor world.AnchorKind) (vnet.ResidentRole, bool) {
	switch anchor {
	case world.AnchorSmith:
		return vnet.ResidentRoleSmith, true
	case world.AnchorCarpenter:
		return vnet.ResidentRoleCarpenter, true
	case world.AnchorCook:
		return vnet.ResidentRoleCook, true
	case world.AnchorTrader:
		return vnet.ResidentRoleTrader, true
	case world.AnchorVillager:
		return vnet.ResidentRoleVillager, true
	case world.AnchorGuard:
		return vnet.ResidentRoleGuard, true
	default:
		return vnet.ResidentRoleUnknown, false
	}
}

// residentNames is the sixty-four names this world hands out.
//
// **Sixty-four because the index is six bits of a hash**, which is what makes the choice
// a pure function of the column with no arithmetic that could bias it. A table whose
// length is not a power of two would need a modulo, and a modulo over a hash is the one
// step in this file where "deterministic" and "uniform" stop being the same claim.
//
// **ASCII, deliberately, and it is not a stylistic preference.** These are drawn over a
// resident's head by a client whose font has ninety-five glyphs, and since #481 the
// client's own source scan fails its build on a non-ASCII literal in the production
// crate. A name spelled with the letters Old Norse actually used would be a name the
// game cannot draw, so they are spelled the way an English text spells them.
var residentNames = [64]string{
	"Bjorn", "Sigrun", "Ivar", "Astrid", "Ragnar", "Hilda", "Leif", "Gudrun",
	"Ulf", "Thora", "Halfdan", "Ingrid", "Sigurd", "Freydis", "Torstein", "Solveig",
	"Eirik", "Ragnhild", "Knut", "Aslaug", "Hakon", "Gunnhild", "Egil", "Bergthora",
	"Ketil", "Helga", "Orm", "Yrsa", "Steinar", "Signy", "Vidar", "Runa",
	"Arnvid", "Dagny", "Trygve", "Brynhild", "Hrolf", "Alfhild", "Ozur", "Groa",
	"Sten", "Idunn", "Toke", "Saga", "Gorm", "Hervor", "Rurik", "Thyra",
	"Vali", "Embla", "Skarde", "Sunniva", "Frode", "Liv", "Njal", "Vigdis",
	"Ottar", "Katla", "Bard", "Jorunn", "Hallvard", "Ranveig", "Sindri", "Aud",
}

// residentName is what the person on one column is called.
func residentName(seed, x, z int64) string {
	return residentNames[world.HashLattice(seed+residentNameSeedOffset, x, z)%uint64(len(residentNames))]
}

// The palettes a resident's face and clothes are drawn from.
//
// **Palettes rather than twenty-four random bits per channel, and the difference is the
// whole design.** A colour taken from a hash is uniform over sixteen million values, and
// almost none of them are a person: a village generated that way is a crowd with green
// skin and magenta hair. Choosing *from a list somebody wrote down* keeps every resident
// plausible while leaving the choice exactly as deterministic — and it costs sixteen bits
// of one hash rather than a hundred and twenty, which is the second reason it fits.
//
// Each is 0x00RRGGBB with the top byte reserved and zero, which is the one colour
// encoding on this wire; protocol.Appearance.Validate is what refuses anything else, and
// [residentAppearance] is checked against it by the tests.
var (
	residentSkinTones = [8]uint32{
		0x00F2D4B8, 0x00E8C39E, 0x00D9AE86, 0x00C69A72,
		0x00A97A55, 0x008C6142, 0x006F4A31, 0x00543724,
	}
	residentHairColors = [8]uint32{
		0x00E8D9A0, 0x00D8B65C, 0x00B98A3C, 0x008A5A2B,
		0x005C3A1E, 0x00332018, 0x00B04A28, 0x00CFCFC8,
	}
	residentShirtColors = [8]uint32{
		0x006B4A2F, 0x008C6239, 0x004F5B45, 0x003C4A5C,
		0x00713A32, 0x005A4A6B, 0x00A08A5E, 0x00404040,
	}
	residentTrouserColors = [4]uint32{0x003A2E22, 0x004A3F30, 0x002C3340, 0x00514334}
	residentShoeColors    = [4]uint32{0x00241A12, 0x00352518, 0x00443020, 0x001A1A1A}
	residentHairModels    = [5]vnet.HairModel{
		vnet.HairModelShaved, vnet.HairModelCropped, vnet.HairModelBraided,
		vnet.HairModelLoose, vnet.HairModelTopknot,
	}
)

// residentAppearance is what the person on one column looks like.
//
// Six choices out of one hash, taken from disjoint bit fields rather than from six
// hashes: the finalizer in world.HashLattice already mixes every input bit into every
// output bit, so neighbouring fields of one value are as independent as separate calls
// and are one multiplication instead of six. Sixteen bits are spent and forty-eight are
// left, which is room for whatever a later issue wants to vary.
//
// The hair model is drawn from [residentHairModels] rather than from the generated enum's
// range, because `HairModel.Unknown` is the absent-field zero and not a haircut —
// protocol.Appearance.Validate refuses it, and a list of the five real members is what
// keeps this side of that promise instead of an arithmetic that happens to avoid zero.
func residentAppearance(seed, x, z int64) protocol.Appearance {
	h := world.HashLattice(seed+residentAppearanceSeedOffset, x, z)
	return protocol.Appearance{
		SkinColor:     residentSkinTones[h&0x7],
		HairColor:     residentHairColors[(h>>3)&0x7],
		ShirtColor:    residentShirtColors[(h>>6)&0x7],
		TrousersColor: residentTrouserColors[(h>>9)&0x3],
		ShoesColor:    residentShoeColors[(h>>11)&0x3],
		HairModel:     residentHairModels[(h>>13)%uint64(len(residentHairModels))],
	}
}

// yawOfFacing is the heading a [vnet.Facing] points along.
//
// The compass is the movement integrator's, exactly as [facingTowards] reads it: North is
// -Z, East is +X, South is +Z, West is -X, and yaw 0 looks along -Z with +X to its right.
// The four answers are [mob.faceToward]'s formula evaluated at the four unit vectors, and
// the tests check them against it rather than against these literals.
func yawOfFacing(facing vnet.Facing) float64 {
	switch facing {
	case vnet.FacingEast:
		return -math.Pi / 2
	case vnet.FacingSouth:
		return math.Pi
	case vnet.FacingWest:
		return math.Pi / 2
	default:
		return 0
	}
}

// materialiseResidentLocked puts one person in one anchor slot, if they are not standing
// already.
//
// **Idempotent, because the id is the answer** — [Sim.materialiseSettlementsLocked]'s
// rule, and the same second pass derives the same number, finds it in the map and does
// nothing. That is what makes the hook safe to run again on every chunk that re-enters a
// view, and what a restart rests on.
//
// **The floor, not the ground under it**, which is the one place this differs from a
// station. A world.PlacedAnchor names the cell the slot occupies, and that cell is a
// building's floor level and therefore air; a station is a voxel and takes the block
// *below* so it rests on something, while a person stands *in* that air with their feet
// at its bottom face. So the position is the slot itself, and the chunk this resident
// belongs to is the slot's own — which is the chunk a viewer's cube is measured against,
// so materialisation and visibility keep answering about one chunk.
//
// The caller holds Sim.mu.
func (s *Sim) materialiseResidentLocked(coord world.Coord, settled world.Settlement, slot world.PlacedAnchor, role vnet.ResidentRole) {
	x, y, z := slot.X, slot.Y, slot.Z
	if world.ChunkOf(x, y, z) != coord {
		return
	}
	if x < -worldLimit || x >= worldLimit || z < -worldLimit || z >= worldLimit {
		// Unreachable from a streamed chunk, because a player cannot stand outside the
		// world. Refused rather than narrowed, for the reason station.go refuses it: the
		// arithmetic below stops being meaningful out there.
		return
	}

	id := residentID(s.worldSeed, x, z)
	if _, standing := s.residents[id]; standing {
		return
	}

	restYaw := yawOfFacing(facingTowards(x, z, settled.CentreX, settled.CentreZ))
	s.residents[id] = &resident{
		entityID: id,
		role:     role,
		name:     residentName(s.worldSeed, x, z),
		// The centre of the cell in x and z and its floor in y, which is where a body
		// stands: a position at the cell's corner would put half of a person through the
		// wall the drawing put beside them.
		pos:        [3]float64{float64(x) + 0.5, float64(y), float64(z) + 0.5},
		yaw:        restYaw,
		restYaw:    restYaw,
		home:       [2]int64{settled.CentreX, settled.CentreZ},
		chunk:      coord,
		appearance: residentAppearance(s.worldSeed, x, z),
	}

	s.log.Debug("resident materialised", "entity_id", id, "role", role.String(),
		"name", s.residents[id].name, "pos", s.residents[id].pos)
}

// advanceResidentsLocked turns whoever has somebody near them.
//
// **The entire behaviour of this entity class, and it is one number per tick.** A
// resident with a live player inside [ResidentNoticeRadius] turns toward the nearest of
// them at [ResidentTurnRate]; with nobody near, it turns back to the bearing its anchor
// faces at the same rate. Turning back rather than snapping, because a body that changed
// heading in one frame reads as a glitch rather than as somebody losing interest — and
// the same arithmetic serves both, so there is one rate to reason about instead of two.
//
// **Order-independent on purpose**, which is why it iterates the map rather than a sorted
// slice: each resident reads only its own state and the players, writes only its own yaw,
// and draws no random number. Nothing here can depend on which person is stepped first,
// so the determinism the rest of this package buys with a sort is already free.
//
// The caller holds Sim.mu.
func (s *Sim) advanceResidentsLocked(players []*Player) {
	if len(s.residents) == 0 {
		return
	}
	step := ResidentTurnRate * s.dt

	for _, r := range s.residents {
		want := r.restYaw
		if noticed := r.nearestNoticedLocked(players); noticed != nil {
			want = wrapAngle(math.Atan2(r.pos[0]-noticed.pos[0], r.pos[2]-noticed.pos[2]))
		}
		r.yaw = turnToward(r.yaw, want, step)
	}
}

// nearestNoticedLocked is the closest live player this resident can see, or nil.
//
// Body to body and in three dimensions, which is [mob]'s aggro measurement rather than a
// second one: a person is noticed at the distance between the two boxes, so standing on
// somebody's doorstep is inside the radius whatever the two positions round to.
//
// **A dead player is not noticed**, and that is the one condition worth stating: a corpse
// on the ground is not somebody walking past, and a resident staring at it until the
// respawn timer runs out is the wrong picture. It is also the only place this class reads
// a player's life state at all.
//
// The caller holds Sim.mu.
func (r *resident) nearestNoticedLocked(players []*Player) *Player {
	var nearest *Player
	best := math.Inf(1)
	for _, p := range players {
		if !p.alive() {
			continue
		}
		distance := boxDistance(residentBody.boxAt(r.pos), playerBox(p.pos))
		if distance > ResidentNoticeRadius || distance >= best {
			continue
		}
		best, nearest = distance, p
	}
	return nearest
}

// turnToward rotates `from` toward `to` by at most `step` radians, the short way round.
//
// The wrap is what makes "the short way" true: a resident at 3.1 radians asked to face
// -3.1 turns a twenty-fifth of a turn rather than very nearly a whole one. Landing inside
// one step of the target snaps to it exactly, so a heading that is being held cannot
// oscillate by a step forever.
func turnToward(from, to, step float64) float64 {
	delta := wrapAngle(to - from)
	if math.Abs(delta) <= step {
		return wrapAngle(to)
	}
	if delta > 0 {
		return wrapAngle(from + step)
	}
	return wrapAngle(from - step)
}

// state is the wire form of one resident.
//
// **Idle, full health and no target, and every one of those is a constant rather than a
// field**: a resident has no action state machine, cannot be damaged and cannot choose
// somebody, so a struct field for any of them would be a value nothing writes. The kind
// is `MobKind.Villager` for every role — the role travels in [protocol.ResidentAppearance]
// instead — because `MobState.kind` is what sizes the body a client draws and every
// resident is the same body.
func (r *resident) state() protocol.MobState {
	return protocol.MobState{
		EntityID:       r.entityID,
		Kind:           vnet.MobKindVillager,
		Pos:            toWire(r.pos),
		Yaw:            float32(r.yaw),
		Health:         residentHealth,
		MaxHealth:      residentHealth,
		Action:         vnet.MobActionIdle,
		TargetEntityID: 0,
	}
}

// appearanceFrame is the once-per-viewer description of one resident.
//
// The counterpart of the PlayerAppearance the snapshot loop builds beside it, and it is
// built for the same reason at the same moment: a client needs a name and a face for an
// entity a snapshot is about to draw, and neither belongs in a frame sent twenty times a
// second. Unlike a player's, nothing about it can change while the session lasts — a
// resident does not level up, change clothes or rename itself — so the encoding is worth
// caching for the whole tick and would be worth caching for longer.
func (r *resident) appearanceFrame() []byte {
	return protocol.EncodeResidentAppearance(protocol.ResidentAppearance{
		EntityID:      r.entityID,
		Name:          r.name,
		HasName:       true,
		Role:          r.role,
		Appearance:    r.appearance,
		HasAppearance: true,
	})
}

// vendorRole reports whether this role is one that could keep a stall.
//
// **Nothing acts on a true today, and that is deliberate rather than unfinished.** #459
// is what teaches the server what a vendor role opens; until it lands, every interaction
// is refused and this predicate exists so that the *shape* of the answer is already
// right — the trades are named in one place, and the issue that adds a price list changes
// what happens on a true rather than having to discover which roles it applies to.
func vendorRole(role vnet.ResidentRole) bool {
	switch role {
	case vnet.ResidentRoleSmith, vnet.ResidentRoleCarpenter,
		vnet.ResidentRoleCook, vnet.ResidentRoleTrader:
		return true
	default:
		return false
	}
}

// InteractNPC answers one NpcInteractRequest, and today it answers every one of them no.
//
// **`NotAVendor` for all four refusals, and the uniformity is the fail-closed default
// rather than a placeholder.** An entity id nobody has, an id that names something other
// than a resident, a resident too far away and a resident who keeps no stall all produce
// the same frame, so nothing a client can send tells it anything about the world it could
// not already see. A smith is refused with it too: until #459 the server has no price
// list to open, and answering "yes" to a request it cannot fulfil would be the client
// deciding an outcome.
//
// The error is separate from the reason for the reason every other refusal here keeps the
// two apart: the code is for the player and the sentence is for the operator, and the
// sentence is the only thing that says *which* of the four it was. It is non-nil on every
// path, because there is no success to report yet — #459 is what gives this a second
// return shape.
func (p *Player) InteractNPC(request protocol.NpcInteractRequest) (vnet.RefusalReason, error) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	r, standing := p.sim.residents[request.EntityID]
	if !standing {
		return vnet.RefusalReasonNotAVendor, fmt.Errorf("entity %d is not a resident of this world", request.EntityID)
	}
	if distance := boxDistance(playerBox(p.pos), residentBody.boxAt(r.pos)); distance > EditReach {
		return vnet.RefusalReasonNotAVendor, fmt.Errorf("%s is %.2f blocks away, past the reach of %.1f", r.name, distance, EditReach)
	}
	if !vendorRole(r.role) {
		return vnet.RefusalReasonNotAVendor, fmt.Errorf("%s is a %s and keeps no stall", r.name, r.role.String())
	}
	return vnet.RefusalReasonNotAVendor, fmt.Errorf("%s is a %s, and no stall opens until this server knows what a vendor sells", r.name, r.role.String())
}
