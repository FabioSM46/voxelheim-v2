package main

import (
	"math"
	"testing"
	"time"

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
	got := humanEstimate(20*time.Minute, 5*time.Minute, 6*time.Minute)
	want := 9*time.Minute + time.Duration(1618.65*float64(time.Second))
	if d := got - want; d < -time.Millisecond || d > time.Millisecond {
		t.Fatalf("estimate %v, want %v", got, want)
	}
}

func TestFlagsRefuseARunWithNoServer(t *testing.T) {
	if _, err := parseFlags("bot", nil); err == nil {
		t.Fatal("a run with no -server was accepted")
	}
	if _, err := parseFlags("bot", []string{"-server", "voxelheimd", "-max-deaths", "0"}); err == nil {
		t.Fatal("-max-deaths 0 was accepted")
	}
	o, err := parseFlags("bot", []string{"-server", "voxelheimd"})
	if err != nil || o.maxDeaths != 3 || o.viewDistance != 4 {
		t.Fatalf("defaults %+v, %v", o, err)
	}
}
