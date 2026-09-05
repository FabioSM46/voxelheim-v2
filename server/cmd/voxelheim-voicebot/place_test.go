package main

import (
	"flag"
	"strings"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func TestTheLatticeIsEveryWholeBlockInsideTheDiscNearestFirst(t *testing.T) {
	t.Parallel()

	for _, radius := range []int{0, 1, 5, 10} {
		disc := latticeDisc(radius)
		if len(disc) == 0 {
			t.Fatalf("radius %d produced no positions at all", radius)
		}
		if disc[0] != [2]int{0, 0} {
			t.Errorf("radius %d starts at %v, not the centre", radius, disc[0])
		}

		seen := map[[2]int]bool{}
		previous := -1
		for _, offset := range disc {
			square := offset[0]*offset[0] + offset[1]*offset[1]
			if square > radius*radius {
				t.Errorf("radius %d holds %v, which is outside the disc", radius, offset)
			}
			if square < previous {
				t.Errorf("radius %d is not nearest-first: %v follows something further out", radius, offset)
			}
			if seen[offset] {
				t.Errorf("radius %d holds %v twice", radius, offset)
			}
			seen[offset], previous = true, square
		}

		// Nothing inside the disc may be missing, or a crowd would stand deeper than it
		// needs to and the radius would stop describing the huddle.
		for x := -radius; x <= radius; x++ {
			for z := -radius; z <= radius; z++ {
				if x*x+z*z <= radius*radius && !seen[[2]int{x, z}] {
					t.Errorf("radius %d is missing %v", radius, [2]int{x, z})
				}
			}
		}
	}
}

func TestTheRemainderIsSpreadOverTheFirstClustersRatherThanDumpedOnTheLast(t *testing.T) {
	t.Parallel()

	const sessions, clusters = 104, 10
	total, largest, smallest := 0, 0, sessions
	for cluster := range clusters {
		size := clusterSize(sessions, clusters, cluster)
		total += size
		largest, smallest = max(largest, size), min(smallest, size)
	}
	if total != sessions {
		t.Errorf("the clusters hold %d sessions, want %d", total, sessions)
	}
	if largest-smallest > 1 {
		t.Errorf("cluster sizes range %d..%d; they may differ by at most one, or two clusters carry different fan-outs in one run",
			smallest, largest)
	}
}

func TestEveryPlannedClusterCanHearItselfAndNoOtherCluster(t *testing.T) {
	t.Parallel()

	var opts options
	registerPlanFlags(flag.NewFlagSet("test", flag.ContinueOnError), &opts)
	opts.sessions, opts.clusters, opts.speaking = 100, 10, 0.3
	if err := opts.validate(); err != nil {
		t.Fatalf("the plan does not validate: %v", err)
	}

	places := planPlacements(opts)
	if len(places) != opts.sessions {
		t.Fatalf("the plan holds %d placements, want %d", len(places), opts.sessions)
	}

	enter := opts.voiceRange * opts.voiceRange
	// The set is hysteretic, so what separates two clusters is the wider limit.
	exit := (opts.voiceRange * game.VoiceExitFactor) * (opts.voiceRange * game.VoiceExitFactor)
	speakers := make([]int, opts.clusters)
	for i, a := range places {
		if a.speaker {
			speakers[a.cluster]++
		}
		for _, b := range places[i+1:] {
			square := float64(0)
			for axis := range 3 {
				d := float64(a.at[axis] - b.at[axis])
				square += d * d
			}
			if a.cluster == b.cluster && square > enter {
				t.Fatalf("two members of cluster %d are %v apart, past the %v this run carries", a.cluster, square, enter)
			}
			if a.cluster != b.cluster && square <= exit {
				t.Fatalf("a member of cluster %d and one of cluster %d are within hearing of each other", a.cluster, b.cluster)
			}
		}
	}
	for cluster, count := range speakers {
		if want := speakersPerCluster(clusterSize(opts.sessions, opts.clusters, cluster), opts.speaking); count != want {
			t.Errorf("cluster %d has %d speakers, want %d", cluster, count, want)
		}
	}
}

// **The crowd is the case the lattice has to survive**, and the property that matters is
// not that everybody has a block of their own — a thousand people in a twenty-block circle
// cannot — but that nobody is placed outside the radius the run was told to use.
func TestAThousandInOneHuddleWrapTheLatticeWithoutLeavingIt(t *testing.T) {
	t.Parallel()

	var opts options
	registerPlanFlags(flag.NewFlagSet("test", flag.ContinueOnError), &opts)
	opts.sessions, opts.clusters, opts.clusterRadius, opts.speaking = 1000, 1, 10, 0.1
	if err := opts.validate(); err != nil {
		t.Fatalf("the plan does not validate: %v", err)
	}

	origin := opts.origin()
	places := planPlacements(opts)
	occupied := map[[3]int]int{}
	for _, place := range places {
		dx, dz := place.at[0]-origin[0], place.at[2]-origin[2]
		if dx*dx+dz*dz > opts.clusterRadius*opts.clusterRadius {
			t.Fatalf("a session was placed at %v, outside the %d-block radius", place.at, opts.clusterRadius)
		}
		occupied[place.at]++
	}
	if len(occupied) != len(latticeDisc(opts.clusterRadius)) {
		t.Errorf("the crowd occupies %d blocks of the %d in the disc; the wrap should fill it",
			len(occupied), len(latticeDisc(opts.clusterRadius)))
	}
}

// **The check is on every axis of every planned position, not on the one edge the first
// version happened to test.** #930's review found it validating the rightmost x alone. Only
// that edge is reachable today — [maxClusterRadius] and the spacing rule are why, and both
// are checked here too — so this pins the reachable case and the two bounds that make the
// others unreachable.
func TestALayoutThatFallsOffTheWorldIsRefusedBeforeAnythingConnects(t *testing.T) {
	t.Parallel()

	var opts options
	registerPlanFlags(flag.NewFlagSet("test", flag.ContinueOnError), &opts)
	opts.sessions, opts.clusters, opts.clusterSpacing = 1000, 1000, world.BlockLimit
	if err := opts.validate(); err != nil {
		t.Fatalf("the plan does not validate: %v", err)
	}
	if err := checkInsideTheWorld(opts); err == nil {
		t.Error("a layout reaching past world.BlockLimit was accepted; /teleport would have refused the far clusters")
	}

	// And the layout the defaults describe is inside the world on every axis.
	var fine options
	registerPlanFlags(flag.NewFlagSet("test", flag.ContinueOnError), &fine)
	fine.sessions, fine.clusters = 100, 10
	if err := checkInsideTheWorld(fine); err != nil {
		t.Errorf("the default layout was refused: %v", err)
	}
}

// A radius past the bound is refused before anything tries to lay it out, because laying it
// out is what would fail: the disc is materialised and a radius costs its square.
func TestARadiusNoLatticeCanHoldIsRefused(t *testing.T) {
	t.Parallel()

	var opts options
	registerPlanFlags(flag.NewFlagSet("test", flag.ContinueOnError), &opts)
	opts.clusterRadius = maxClusterRadius + 1
	opts.voiceRange = 4 * float64(opts.clusterRadius)
	opts.clusterSpacing = 8 * opts.clusterRadius
	err := opts.validate()
	if err == nil {
		t.Fatal("a cluster radius past the bound was accepted")
	}
	if !strings.Contains(err.Error(), "cluster radius must be at most") {
		t.Errorf("it was refused with %q, which does not name the radius", err)
	}

	// The bound itself is a legal radius, given a voice that carries far enough.
	opts.clusterRadius = maxClusterRadius
	if err := opts.validate(); err != nil {
		t.Errorf("a radius of exactly %d was refused: %v", maxClusterRadius, err)
	}
}
