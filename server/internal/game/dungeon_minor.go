package game

import (
	"math"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// The descent's lesser creatures: the two things they do that nothing on the surface does.
//
// **A leash.** Every creature an instance places belongs to a zone — the cave, the sand
// hall — and hunts only inside it. A player who walks back up past a checkpoint or out
// through a door has left the zone, and the creature that was chasing them stops at the
// boundary and forgets them rather than following. A closed door is also a wall, and
// the collision already stops a body at a wall; the leash is what keeps the hunt from
// resuming the moment the door opens, and what stops one at a boundary that is not a
// wall at all, such as the top of a stair to a checkpoint.
//
// **Burial.** A species whose row carries an emerge range may be placed buried: lying one
// block under where it would stand, in the sand, until a live player comes close enough
// to bring it up. While buried it is nobody's target and nothing else's business — no
// swing or arrow can reach it, no damage lands on it, the spawn separation does not see
// it, and it has no body above the sand for anything to meet. It still travels in the
// snapshot, so a client can draw the disturbance.
//
// # What the wire says about a buried creature
//
// The contract has no member for burial and none was added: `MobAction` has no free
// value that means it, and a new field would be a contract change this issue does not
// own. So a buried creature is sent as what it is — **action `Idle`, and a standing
// position one block below the surface it will rise to**, which puts its whole body
// inside the sand. No live creature above ground is ever inside a solid block, because
// the collision will not put one there; a receiver that finds a body inside solid
// terrain is looking at a buried one. Rising moves it up onto the surface and puts it in
// `Recovery` for the species' emergence span — Recovery's own meaning, "cannot attack
// until this expires", is exactly what a creature still shaking off the sand is, and a
// client sees the jump from inside the sand to on top of it as the emergence.
//
// # Where they come from
//
// Not here. [Sim.placeMinorMobLocked] is the seam the dungeon's placement uses; which
// anchors, how many and when belong to that placement, and the open-world director never
// offers a dungeon-only species at all — see [spawnableSpecies].

// burialDepth is how far below its risen standing position a buried creature lies, in
// blocks: one whole block, so a body shorter than a block is entirely inside the sand
// cell under the surface and nothing of it stands above.
const burialDepth = 1.0

// mobLeash is the zone one creature hunts in: a box its standing position never leaves
// and outside which no player is prey.
//
// Closed on every face, unlike a collision box: a leash asks "is this position inside
// the zone", and a creature stopped exactly on the boundary is still inside it.
type mobLeash struct {
	zone box
}

// holds reports whether a standing position lies inside the zone. A nil leash — every
// creature in the open world — holds everywhere, which is what lets the state machine
// ask this of every creature without a branch.
func (l *mobLeash) holds(pos [3]float64) bool {
	if l == nil {
		return true
	}
	for axis := range 3 {
		if pos[axis] < l.zone.min[axis] || pos[axis] > l.zone.max[axis] {
			return false
		}
	}
	return true
}

// clamp trims a horizontal step so the standing position does not leave the zone, and
// stops the velocity on any axis it trimmed.
//
// **Before the collision rather than after it**, because what the collision returns is a
// move it has approved as a whole: reverting one axis of that answer afterwards would
// produce a position nothing checked. Trimming the request instead leaves the collision
// the last word, as it is for every other step.
//
// Only x and z. The vertical is the terrain's: a creature falls where it falls, and a
// zone's height bounds who is prey, not where gravity may take the hunter.
//
// A creature already outside on an axis may step back inward and is never pushed; it
// just cannot step further out.
func (l *mobLeash) clamp(pos [3]float64, delta, vel *[3]float64) {
	if l == nil {
		return
	}
	for _, axis := range [...]int{0, 2} {
		next := pos[axis] + delta[axis]
		switch {
		case delta[axis] < 0 && next < l.zone.min[axis]:
			delta[axis] = min(0, l.zone.min[axis]-pos[axis])
			vel[axis] = 0
		case delta[axis] > 0 && next > l.zone.max[axis]:
			delta[axis] = max(0, l.zone.max[axis]-pos[axis])
			vel[axis] = 0
		}
	}
}

// placeMinorMobLocked places one of an instance's lesser creatures, standing at pos,
// hunting only inside zone. With buried, it lies [burialDepth] under pos until a player
// brings it up, and rises to stand exactly at pos.
//
// Refused — nothing is created — for a kind the registry does not hold, for a boss (the
// two bosses have their own placement, see dungeon.go), for burial of a species that
// never lies buried or is taller than [burialDepth], and for a standing position outside its own zone, which would be a
// creature that could never hunt anybody.
//
// The caller holds Sim.mu.
func (s *Sim) placeMinorMobLocked(kind vnet.MobKind, pos [3]float64, zone box, buried bool) (uint64, bool) {
	def, registered := mobByKind(kind)
	if !registered || def.isBoss() {
		return 0, false
	}
	// A body taller than the burial depth would stand partly above the sand, breaking
	// the one signal a client reads burial from: a body wholly inside solid terrain.
	if buried && (def.emergeRange <= 0 || def.body.height > burialDepth) {
		return 0, false
	}
	leash := &mobLeash{zone: zone}
	if !leash.holds(pos) {
		return 0, false
	}
	id, made := s.spawnMobLocked(kind, pos)
	if !made {
		return 0, false
	}
	m := s.mobs[id]
	m.leash = leash
	if buried {
		m.buried = true
		m.pos[1] -= burialDepth
		m.chunk = chunkAt(m.pos)
	}
	return id, true
}

// stepBuried is one tick under the sand: nothing, until a live player inside the zone
// comes within the species' emerge range of where it would stand.
//
// **Measured from the risen body, not the buried one.** The distance a player reads is to
// the patch of sand they can see, and a body a block down would put the threshold a
// block further off for somebody standing on top of it than for somebody level with it.
//
// Rising needs room: if something now stands solid where the body would rise to, it stays
// down. A creature forced up into a block would be stuck inside it, which is the one state
// the collision exists to prevent. The ties between players resolve by identity, as every
// other choice in this file does, so the creature faces the same waker on every run.
//
// The caller holds Sim.mu.
func (m *mob) stepBuried(s *Sim, players []*Player) {
	m.vel = [3]float64{}
	m.action = vnet.MobActionIdle
	m.actionTicks = 0
	m.target = 0

	def := m.species()
	risen := m.pos
	risen[1] += burialDepth
	surface := def.body.boxAt(risen)

	var waker *Player
	nearest := math.Inf(1)
	for _, p := range players {
		if !p.alive() || !m.leash.holds(p.pos) {
			continue
		}
		distance := boxDistance(surface, p.box())
		if distance > def.emergeRange {
			continue
		}
		if distance < nearest || (distance == nearest && p.entityID < waker.entityID) {
			waker, nearest = p, distance
		}
	}
	if waker == nil || overlaps(s.terrain, surface) {
		return
	}

	m.buried = false
	m.pos = risen
	m.chunk = chunkAt(m.pos)
	m.faceToward(waker)
	m.action = vnet.MobActionRecovery
	m.actionTicks = s.mobTimings[m.kind].emergence
}
