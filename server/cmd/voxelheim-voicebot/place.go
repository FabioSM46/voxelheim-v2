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

// checkInsideTheWorld refuses a layout whose far edge falls off the world.
//
// `/teleport` refuses a coordinate past world.BlockLimit and answers the session privately,
// so a run laid out past the edge would connect, teleport nobody, and report a huddle that
// never formed as one that produced no drops. Checked here because it is the first place
// both the spacing and the spawn are known.
func checkInsideTheWorld(o options) error {
	origin := o.origin()
	far := origin[0] + (o.clusters-1)*o.clusterSpacing + o.clusterRadius
	if far > world.BlockLimit {
		return fmt.Errorf(
			"%d clusters spaced %d blocks apart from the spawn at x=%d reach x=%d, past world.BlockLimit (%d); "+
				"/teleport would refuse the far clusters", o.clusters, o.clusterSpacing, origin[0], far, world.BlockLimit)
	}
	return nil
}
