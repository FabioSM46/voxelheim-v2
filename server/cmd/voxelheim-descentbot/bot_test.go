package main

import (
	"math"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// flatView is one delivered chunk: a stone floor at y = 0 and air above it.
func flatView() *blockView {
	v := newBlockView()
	blocks := make([]world.Block, world.ChunkSize*world.ChunkSize*world.ChunkSize)
	for z := range world.ChunkSize {
		for x := range world.ChunkSize {
			blocks[world.Index(x, 0, z)] = world.Stone
		}
	}
	v.chunks[world.Coord{}] = blocks
	return v
}

func TestThePathWalksAroundAWallAndClimbsOneCourse(t *testing.T) {
	v := flatView()
	for z := int64(0); z < 10; z++ {
		v.set(5, 1, z, world.Stone)
		v.set(5, 2, z, world.Stone)
	}
	route := v.path(cell{2, 1, 2}, exactly(cell{8, 1, 2}))
	if route == nil || route[len(route)-1] != (cell{8, 1, 2}) {
		t.Fatalf("no route around the wall: %v", route)
	}
	for _, c := range route {
		if c[0] == 5 && c[2] < 10 {
			t.Fatalf("the route walks through the wall at %v", c)
		}
	}
	v.set(3, 1, 2, world.Stone) // a one-course step
	up := v.path(cell{2, 1, 2}, exactly(cell{3, 2, 2}))
	if len(up) != 1 {
		t.Fatalf("one step up takes %v", up)
	}
}

func TestUnknownTerrainAndPortalsAreNotWalkable(t *testing.T) {
	v := flatView()
	if v.path(cell{2, 1, 2}, exactly(cell{40, 1, 2})) != nil {
		t.Fatal("a route crossed a chunk that was never delivered")
	}
	for z := int64(0); z < world.ChunkSize; z++ {
		v.set(6, 1, z, world.PortalVeil)
		v.set(6, 2, z, world.PortalVeil)
	}
	if v.path(cell{2, 1, 2}, exactly(cell{9, 1, 2})) != nil {
		t.Fatal("a route walked through a portal's veil")
	}
}

func TestYawTowardMatchesTheServersBasis(t *testing.T) {
	// yaw 0 looks along -Z and forward is (-sin yaw, -cos yaw).
	for _, d := range [][2]float64{{0, -1}, {1, 0}, {0, 1}, {-1, 0}, {3, 4}} {
		yaw := yawToward(d[0], d[1])
		length := math.Hypot(d[0], d[1])
		if fx, fz := -math.Sin(yaw), -math.Cos(yaw); math.Abs(fx-d[0]/length) > 1e-9 || math.Abs(fz-d[1]/length) > 1e-9 {
			t.Fatalf("yaw %.3f walks along (%.3f, %.3f), not %v", yaw, fx, fz, d)
		}
	}
}

func TestTheSoloSiegeIsTwelvePacksOfThree(t *testing.T) {
	for members, want := range map[int]int{1: 36, 2: 36, 3: 36, 4: 48, 5: 60} {
		if got := expectedSpiders(members); got != want {
			t.Fatalf("a party of %d faces %d spiders, want %d", members, got, want)
		}
	}
}

func TestTheHumanEstimateReplacesOnlyTheBossFights(t *testing.T) {
	got, ok := humanEstimate(3, 20*time.Minute, 5*time.Minute, 6*time.Minute)
	want := 9*time.Minute + time.Duration((206.65+324.70)*float64(time.Second))
	if d := got - want; !ok || d < -time.Millisecond || d > time.Millisecond {
		t.Fatalf("estimate %v (%t), want %v", got, ok, want)
	}
	if _, ok := humanEstimate(2, 20*time.Minute, 0, 0); ok {
		t.Fatal("a pair has an estimate, but no reader was ever measured at two")
	}
}

func TestTheBarrierReleasesOnlyWhenEveryMemberHasArrived(t *testing.T) {
	b := newBarrier(3)
	first, second := b.arrive(), b.arrive()
	select {
	case <-first:
		t.Fatal("released with two of three members")
	default:
	}
	third := b.arrive()
	for i, ch := range []<-chan struct{}{first, second, third} {
		select {
		case <-ch:
		default:
			t.Fatalf("arrival %d was not released once all three had arrived", i)
		}
	}
	// The next part of the route starts a fresh count.
	select {
	case <-b.arrive():
		t.Fatal("the barrier's next round released at once")
	default:
	}
}

func TestFlagsRefuseARunWithNoServer(t *testing.T) {
	if _, err := parseFlags("bot", nil); err == nil {
		t.Fatal("a run with no -server was accepted")
	}
	if _, err := parseFlags("bot", []string{"-server", "voxelheimd", "-max-wipes", "0"}); err == nil {
		t.Fatal("-max-wipes 0 was accepted")
	}
	for _, n := range []string{"0", "6"} {
		if _, err := parseFlags("bot", []string{"-server", "voxelheimd", "-party", n}); err == nil {
			t.Fatalf("-party %s was accepted", n)
		}
	}
	o, err := parseFlags("bot", []string{"-server", "voxelheimd"})
	if err != nil || o.maxWipes != 3 || o.viewDistance != 4 || o.members != 3 {
		t.Fatalf("defaults %+v, %v", o, err)
	}
}

func TestTheReaderSeesTheRegionsTheServerStrikes(t *testing.T) {
	disc := region{shape: vnet.HazardShapeDisc, origin: [3]float64{0, 10, 0}, radius: 3, height: 4}
	lane := region{shape: vnet.HazardShapeLine, origin: [3]float64{0, 10, 0}, direction: [3]float64{1, 0, 0},
		radius: 8, height: 4, halfWidth: 1}
	for _, c := range []struct {
		what    string
		regions []region
		pos     [3]float64
		want    bool
	}{
		{"inside the disc", []region{disc}, [3]float64{1, 10, 1}, true},
		{"a margin past its rim", []region{disc}, [3]float64{3.5, 10, 0}, true},
		{"well clear of it", []region{disc}, [3]float64{5, 10, 0}, false},
		{"a floor above it", []region{disc}, [3]float64{0, 13, 0}, false},
		{"on the lane", []region{lane}, [3]float64{6, 10, 0.5}, true},
		{"behind the lane's start", []region{lane}, [3]float64{-2, 10, 0}, false},
		{"beside the lane", []region{lane}, [3]float64{4, 10, 2.5}, false},
		{"nothing announced", nil, [3]float64{0, 10, 0}, false},
	} {
		if got := touches(c.regions, c.pos); got != c.want {
			t.Errorf("%s: touches %t, want %t", c.what, got, c.want)
		}
	}
}

func TestAWalkAlongTheWorldBecomesTheControlsForTheFacing(t *testing.T) {
	yaw := yawToward(1, 0) // facing +X
	if x, z := relative([2]float64{1, 0}, yaw); math.Abs(x) > 1e-9 || math.Abs(z-1) > 1e-9 {
		t.Fatalf("walking the way the body faces is (%.3f, %.3f), want forward", x, z)
	}
	if x, z := relative([2]float64{0, 1}, yaw); math.Abs(math.Abs(x)-1) > 1e-9 || math.Abs(z) > 1e-9 {
		t.Fatalf("walking across the facing is (%.3f, %.3f), want a pure strafe", x, z)
	}
}
