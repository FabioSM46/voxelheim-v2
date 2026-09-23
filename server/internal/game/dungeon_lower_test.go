package game

import (
	"math"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The two lever puzzles below the chasm are races against a timer, and the layout is
// what decides whether each race is winnable. The timers are the mechanisms issue's
// numbers (#1293): the grille stays open 12 seconds after its lever is pulled, and each
// of the sand hall's twin levers stays on 10 seconds.
const (
	grilleWindowSeconds    = 12.0
	twinLeverWindowSeconds = 10.0
)

// standableCell is the walking body's cell: two clear courses over something solid.
func standableCell(terrain *descentTerrain, x, y, z int64) bool {
	feet, _ := terrain.Block(x, y, z)
	return !world.Solid(feet) && !world.IsWater(feet) && !terrain.Solid(x, y+1, z) && terrain.Solid(x, y-1, z)
}

// walkingSteps is the length, in blocks, of the shortest walk between two standing
// cells over whole blocks — a step along X or Z, up one course with room to jump or
// down one. A body at WalkSpeed covers it in no more than steps/WalkSpeed, since a
// real route may also cut corners diagonally. -1 means the cell is never reached.
func walkingSteps(terrain *descentTerrain, from, to [3]int64) int {
	dist := map[[3]int64]int{from: 0}
	queue := [][3]int64{from}
	for len(queue) > 0 {
		c := queue[0]
		queue = queue[1:]
		if c == to {
			return dist[c]
		}
		for _, step := range [][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
			for _, dy := range []int64{1, 0, -1} {
				next := [3]int64{c[0] + step[0], c[1] + dy, c[2] + step[1]}
				if _, seen := dist[next]; seen || !standableCell(terrain, next[0], next[1], next[2]) {
					continue
				}
				if dy == 1 && terrain.Solid(c[0], c[1]+2, c[2]) {
					continue
				}
				dist[next] = dist[c] + 1
				queue = append(queue, next)
			}
		}
	}
	return -1
}

// leverStand is the one standing cell a lever is pulled from, a course below it.
func leverStand(t *testing.T, terrain *descentTerrain, lever world.PlacedAnchor) [3]int64 {
	t.Helper()
	var found [][3]int64
	for _, d := range [][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
		if standableCell(terrain, lever.X+d[0], lever.Y-1, lever.Z+d[1]) {
			found = append(found, [3]int64{lever.X + d[0], lever.Y - 1, lever.Z + d[1]})
		}
	}
	if len(found) != 1 {
		t.Fatalf("lever %+v has %d standing cells, want one", lever, len(found))
	}
	return found[0]
}

// One runner pulls both of the sand hall's levers inside their window: the walk from
// one to the other, whatever dune it crosses, takes less than ten seconds at walking
// speed, at every rotation. And the grille is a real race: the walk from its lever to
// the checkpoint behind it fits in the twelve seconds, but even the straight line
// between the two — shorter than any walk the cave allows — takes more than half of
// them, so crossing uses most of the window.
func TestTheLeverPuzzlesAreRacesTheRunnerCanWin(t *testing.T) {
	for seed := int64(0); seed < 4; seed++ {
		terrain := newDescentTerrain(t, seed)
		var grilleLever world.PlacedAnchor
		var twins []world.PlacedAnchor
		var pastGrille world.PlacedAnchor
		for _, a := range world.InstanceDungeonAnchors(seed) {
			switch {
			case a.Kind == world.AnchorInstanceMechanism && a.Index == world.GrillePuzzle:
				grilleLever = a
			case a.Kind == world.AnchorInstanceMechanism && a.Index == world.TwinLeverPuzzle:
				twins = append(twins, a)
			case a.Kind == world.AnchorInstanceCheckpoint && a.Index == 1:
				pastGrille = a
			}
		}
		if len(twins) != 2 {
			t.Fatalf("seed %d: %d twin levers", seed, len(twins))
		}

		a, b := leverStand(t, terrain, twins[0]), leverStand(t, terrain, twins[1])
		twinSteps := walkingSteps(terrain, a, b)
		if twinSteps < 0 || float64(twinSteps)/WalkSpeed >= twinLeverWindowSeconds {
			t.Fatalf("seed %d: the twin levers are %d blocks apart on foot, %.1f s at walking speed", seed, twinSteps, float64(twinSteps)/WalkSpeed)
		}

		from, to := leverStand(t, terrain, grilleLever), [3]int64{pastGrille.X, pastGrille.Y, pastGrille.Z}
		steps := walkingSteps(terrain, from, to)
		if steps < 0 || float64(steps)/WalkSpeed > grilleWindowSeconds {
			t.Fatalf("seed %d: the grille is %d blocks from its lever on foot, %.1f s at walking speed", seed, steps, float64(steps)/WalkSpeed)
		}
		straight := math.Hypot(float64(to[0]-from[0]), float64(to[2]-from[2]))
		t.Logf("seed %d: twin levers %d blocks on foot; grille %d blocks on foot, %.1f straight", seed, twinSteps, steps, straight)
		if straight/WalkSpeed < grilleWindowSeconds/2 {
			t.Fatalf("seed %d: the grille is %.1f blocks from its lever as the crow flies, %.1f s of a %.0f s window", seed, straight, straight/WalkSpeed, grilleWindowSeconds)
		}
	}
}
