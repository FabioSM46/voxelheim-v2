package main

import (
	"fmt"
	"slices"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Where the synthetic speakers stand, and why the geometry is decided here rather than
// left to whatever the spawn does.
//
// **Everybody joins at one point.** The server places every session at world.SpawnAt and
// the client never states a position, so a thousand sessions that did nothing would be one
// thousand-strong audible set — which is a run, but it is not a run whose fan-out anybody
// chose. The clusters are made with the gated `/teleport` development command, which is
// the only way a session may name a position and which is why the server this command
// starts is started with -dev-commands.
//
// Two properties the layout has to have, and both are checked by options.validate rather
// than assumed here:
//
//   - every member of a cluster is inside every other member's voice range, so the audible
//     set is exactly the cluster and the expected delivery count is arithmetic;
//   - no member of one cluster is inside any member of another's, remembering that the set
//     is hysteretic and reaches game.VoiceExitFactor past the range.

// placement is one session's part in the run.
type placement struct {
	// cluster is the index of the huddle this session belongs to.
	cluster int

	// speaker says whether this session sends frames. A listener still joins, still
	// heartbeats and still receives, because a listener is what a relay costs.
	speaker bool

	// at is the whole-block position `/teleport` is given. Whole blocks because that
	// command takes integers — game.Player.teleportCommandLocked parses them with
	// strconv.ParseInt — and a plan that could not be executed is not a plan.
	at [3]int
}

// planPlacements lays every session out, in join order.
//
// Deterministic in the options alone: the same flags produce the same positions, which is
// what lets a number in the ADR be re-measured rather than re-discovered.
func planPlacements(o options) []placement {
	origin := o.origin()
	lattice := latticeDisc(o.clusterRadius)

	places := make([]placement, 0, o.sessions)
	for cluster := range o.clusters {
		size := clusterSize(o.sessions, o.clusters, cluster)
		speakers := speakersPerCluster(size, o.speaking)
		centre := origin[0] + cluster*o.clusterSpacing

		for member := range size {
			// Members wrap around the lattice when a cluster holds more sessions than the
			// disc holds blocks, which is the ordinary case at a thousand in one huddle: a
			// crowd stands closer together than one per block. Sharing a block is legal —
			// players do not collide with each other — and it keeps the radius honest
			// instead of quietly growing it to fit.
			offset := lattice[member%len(lattice)]
			places = append(places, placement{
				cluster: cluster,
				speaker: member < speakers,
				at:      [3]int{centre + offset[0], origin[1], origin[2] + offset[1]},
			})
		}
	}
	return places
}

// clusterSize is how many sessions the given cluster holds.
//
// The remainder is spread over the first clusters rather than dumped on the last, so ten
// clusters of a hundred and four sessions are 11,11,11,11,10,10,10,10,10,10 and not
// 10×10 plus a fourteen-strong outlier that would carry a different fan-out from every
// other cluster in the same run.
func clusterSize(sessions, clusters, cluster int) int {
	size := sessions / clusters
	if cluster < sessions%clusters {
		size++
	}
	return size
}

// latticeDisc is every whole-block offset inside a disc of the given radius, nearest
// first.
//
// Nearest first so a cluster that does not fill its disc is a huddle rather than a ring,
// and so the wrap in planPlacements piles the overflow where people would actually stand.
// Ordered by squared distance and then by coordinate, which is a total order and therefore
// reproducible; ties broken any other way would make the layout depend on map iteration.
func latticeDisc(radius int) [][2]int {
	offsets := make([][2]int, 0, (2*radius+1)*(2*radius+1))
	for x := -radius; x <= radius; x++ {
		for z := -radius; z <= radius; z++ {
			if x*x+z*z <= radius*radius {
				offsets = append(offsets, [2]int{x, z})
			}
		}
	}
	slices.SortFunc(offsets, func(a, b [2]int) int {
		if d := (a[0]*a[0] + a[1]*a[1]) - (b[0]*b[0] + b[1]*b[1]); d != 0 {
			return d
		}
		if d := a[0] - b[0]; d != 0 {
			return d
		}
		return a[1] - b[1]
	})
	return offsets
}

// checkInsideTheWorld refuses a layout any of whose positions falls off the world.
//
// `/teleport` refuses a coordinate outside `|c| <= world.BlockLimit` on **every** axis and
// answers the session privately, so a run laid out past an edge would connect, teleport
// nobody, and report a huddle that never formed as one that produced no drops. Checked here
// because flag validation is where a refusal an operator can act on belongs — the pattern
// cmd/voxelheim-auth states — rather than minutes into a run.
//
// **It walks the plan rather than a bounding box derived from the flags**, and that is the
// correction #930's review asked for taken one step further. A box computed from the origin,
// the spacing and the radius is a second description of the layout, and the first version of
// this check was exactly such a description with three of its four horizontal edges missing:
// it tested the rightmost x and neither z edge nor the leftmost x. Rewriting the box
// correctly would fix today's bug and leave the drift — a placement rule that moves and a box
// that does not. [planPlacements] is the layout, so asking it is the one form of this check
// that cannot be wrong about a layout it did not predict. It costs one pass over at most
// session.MaxConcurrentSessions positions, at flag-validation time, once.
//
// **Only the +x edge is reachable today, and that is a fact about two other checks rather
// than a reason to test one edge.** The spawn is within a few hundred blocks of the origin,
// [maxClusterRadius] bounds how far a disc reaches from its centre, and a negative
// -cluster-spacing is already refused for making two clusters one — so nothing marches
// towards −x, and neither z edge nor the y one can be approached at all. Every one of those
// three is a check somebody could move; this one does not depend on any of them.
func checkInsideTheWorld(o options) error {
	for _, place := range planPlacements(o) {
		for axis, coordinate := range place.at {
			if coordinate < -world.BlockLimit || coordinate > world.BlockLimit {
				return fmt.Errorf(
					"%d cluster(s) of radius %d spaced %d blocks apart from the spawn put a session at %s=%d, "+
						"outside world.BlockLimit (%d); /teleport would refuse it",
					o.clusters, o.clusterRadius, o.clusterSpacing, axisName(axis), coordinate, world.BlockLimit)
			}
		}
	}
	return nil
}

// axisName is what a refusal calls the axis it is about, so an operator reads "x=…" rather
// than an index. The same three letters game.axisName uses, which is the vocabulary the
// `/teleport` refusal this check exists to pre-empt is written in.
func axisName(axis int) string {
	return [...]string{"x", "y", "z"}[axis]
}
