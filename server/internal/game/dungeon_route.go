package game

import (
	"math"
	"slices"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// How far a party has come down the first dungeon, and what of it survives a restart.
//
// # Checkpoints
//
// The route has three checkpoints (world.InstanceDungeonAnchors): 0 the shore under the
// chasm, 1 past the grille, 2 past the sand hall's door. **Reached is a party fact and it
// only grows**: the furthest checkpoint any live member has reached is where every dead
// member comes back, whoever died and wherever. That is what makes a 20-minute dungeon
// something one mistake does not restart from the top.
//
//   - **The shore is reached by descending, not by standing on it.** A live player whose
//     feet are below the chasm's bottom — anywhere in the pool chamber, the cave, the sand
//     hall or the king's arena — has come down, and nothing below the chasm is reachable
//     any other way. Tying it to the fall rather than to a pad on the shore is what makes
//     "never above the chasm once the party has descended" exact: the moment one member is
//     under it, a death anywhere respawns under it too.
//   - **The other two are reached by passing them.** Each stands in a passage three blocks
//     wide just past its door, and a live player whose feet come within
//     [checkpointReach] of the checkpoint's cell centre on both horizontal axes, and within
//     a course of its floor, has gone through. The passages are the only way on, so a
//     party cannot reach the sand hall's checkpoint without walking past the grille's.
//
// Before anybody has descended no checkpoint is reached and a death recovers in place,
// which is what every dungeon death did before #1294.
//
// **Nothing here moves a live player.** A member still above the chasm when the others
// descend can still drop — the trapdoor the guardian opened stays open — and one entering
// later arrives at the arrival court like anybody else: the entry path places them, and it
// never asks this file. Only a respawn, which already moves a body, reads the checkpoint.
//
// # What a restart keeps
//
// [DungeonRoute] is the route's durable half: the checkpoints reached, the puzzles solved
// for good and the minor-spawn groups cleared, all indices over the seed's own layout.
// A restore rebuilds the rest — open doors and empty halls — from them, before the session
// is published, exactly as a defeated boss's absence is rebuilt from its species.

// checkpointReach is how close to a checkpoint's cell centre, in blocks on each
// horizontal axis, a live player's feet must come to have passed it: the half-width of
// the three-block passage it stands in, so no body can walk the passage beside it.
const checkpointReach = 1.5

// DungeonRoute is how far one run has come, as it crosses to persistence. Every list is
// sorted, so a run whose progress has not changed is written identically.
type DungeonRoute struct {
	// Checkpoints is how many of the route's checkpoints the party has reached, in route
	// order: zero is none, and n means checkpoint n-1 is the furthest.
	Checkpoints uint8
	// SolvedPuzzles is every puzzle whose door is open for good: the rune hall and the
	// sand hall's twin levers. The grille is not among them — it shuts behind every
	// party — and its progress is the checkpoint past it.
	SolvedPuzzles []uint8
	// ClearedGroups is every minor-spawn group whose creatures are all dead; for the
	// cave's burrows, once all three spider waves have come out and died.
	ClearedGroups []uint8
}

// permanentPuzzles are the puzzles whose door stays open once solved, which are the only
// ones a route records.
var permanentPuzzles = [...]int{world.RunePuzzle, world.TwinLeverPuzzle}

// clone is an independent copy of a route, so a snapshot never aliases a caller's lists.
func (r DungeonRoute) clone() DungeonRoute {
	r.SolvedPuzzles = slices.Clone(r.SolvedPuzzles)
	r.ClearedGroups = slices.Clone(r.ClearedGroups)
	return r
}

// solved reports whether the route records a puzzle as solved.
func (r DungeonRoute) solved(puzzle int) bool {
	return puzzle >= 0 && puzzle <= 255 && slices.Contains(r.SolvedPuzzles, uint8(puzzle))
}

// cleared reports whether the route records a minor-spawn group as cleared.
func (r DungeonRoute) cleared(group int) bool {
	return group >= 0 && group <= 255 && slices.Contains(r.ClearedGroups, uint8(group))
}

// dungeonCheckpoints is the route's checkpoints in one instance.
type dungeonCheckpoints struct {
	// anchors is every checkpoint slot, indexed by its route order.
	anchors []world.PlacedAnchor
	// reached is how many of anchors the party has reached; see [DungeonRoute.Checkpoints].
	reached int
}

// placeDungeonCheckpointsLocked reads the checkpoint slots and restores how many were
// reached. A stored count past the layout's checkpoints is clamped to all of them: it can
// only have come from a layout with more, and the furthest this one has is the answer
// closest to what the party earned.
func (s *Sim) placeDungeonCheckpointsLocked(seed int64, d *dungeonEncounters) {
	var anchors []world.PlacedAnchor
	for _, a := range world.InstanceDungeonAnchors(seed) {
		if a.Kind == world.AnchorInstanceCheckpoint {
			anchors = append(anchors, a)
		}
	}
	slices.SortFunc(anchors, func(a, b world.PlacedAnchor) int { return a.Index - b.Index })
	d.checkpoints = dungeonCheckpoints{anchors: anchors, reached: min(int(d.progress.route.Checkpoints), len(anchors))}
}

// advanceDungeonCheckpointsLocked records every checkpoint a live player has reached this
// tick. Only the next one can be reached — the route is a single path — but a party that
// somehow skipped one is still credited with the furthest it stands at.
//
// The caller holds Sim.mu.
func (s *Sim) advanceDungeonCheckpointsLocked(players []*Player) {
	c := &s.dungeon.checkpoints
	for _, p := range players {
		if !p.alive() {
			continue
		}
		for k := len(c.anchors) - 1; k >= c.reached; k-- {
			if c.passed(k, p.pos) {
				c.reached = k + 1
				break
			}
		}
	}
}

// passed reports whether a standing position has reached checkpoint k.
func (c *dungeonCheckpoints) passed(k int, pos [3]float64) bool {
	a := c.anchors[k]
	if k == 0 {
		// Below the chasm. Its clear column ends three courses above the shore's
		// standing level, where it breaks through the pool chamber's ceiling.
		return pos[1] < float64(a.Y)+3
	}
	centre := anchorStanding(a)
	return math.Abs(pos[0]-centre[0]) <= checkpointReach && math.Abs(pos[2]-centre[2]) <= checkpointReach &&
		pos[1] >= centre[1]-.5 && pos[1] <= centre[1]+1.5
}

// dungeonCheckpointLocked is where a dead member of this party comes back: standing on
// the furthest checkpoint reached, or no answer when nobody has descended yet.
//
// The caller holds Sim.mu.
func (s *Sim) dungeonCheckpointLocked() ([3]float64, bool) {
	if s.dungeon == nil || s.dungeon.checkpoints.reached == 0 {
		return [3]float64{}, false
	}
	c := s.dungeon.checkpoints
	return anchorStanding(c.anchors[c.reached-1]), true
}

// DungeonRoute is this instance's progress as a restart would keep it. Zero for a
// simulation that is not a dungeon.
func (s *Sim) DungeonRoute() DungeonRoute {
	s.mu.Lock()
	defer s.mu.Unlock()
	d := s.dungeon
	if d == nil {
		return DungeonRoute{}
	}
	route := DungeonRoute{Checkpoints: uint8(d.checkpoints.reached)}
	for _, puzzle := range permanentPuzzles {
		if d.gate.DoorOpen(puzzle) {
			route.SolvedPuzzles = append(route.SolvedPuzzles, uint8(puzzle))
		}
	}
	for group := range d.descent.zones {
		if s.dungeonGroupClearedLocked(group) {
			route.ClearedGroups = append(route.ClearedGroups, uint8(group))
		}
	}
	return route
}
