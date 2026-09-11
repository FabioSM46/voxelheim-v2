package game

import (
	"fmt"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// everyPortalSheet is each arch this server places, in every orientation it can take:
// both ruin drawings at all four quarter turns, and the instance exit under the four
// seeds that select each of its turns.
func everyPortalSheet(t *testing.T) map[string]portalSheet {
	t.Helper()
	sheets := make(map[string]portalSheet)
	for variant := range uint8(world.RuinVariantCount) {
		for facing := world.Facing(0); facing < 4; facing++ {
			b := world.Building{Kind: world.BuildingRuin, Variant: variant, OriginX: 4093, OriginY: 51, OriginZ: -870, Facing: facing}
			sheets[fmt.Sprintf("ruin%d/facing%d", variant, facing)] = newPortalSheet(world.Ruin{Building: b}.Threshold())
		}
	}
	for seed := range int64(4) {
		sheets[fmt.Sprintf("instance/seed%d", seed)] = newPortalSheet(world.InstanceExitThreshold(seed))
	}
	return sheets
}

// at places a standing body relative to a sheet: lateral blocks across the opening from
// the heart column's centre, normal blocks in front of (negative) or behind the veil's
// plane, and up blocks above the lowest veil course.
func (s portalSheet) at(lateral, normal, up float64) [3]float64 {
	floor := s.heart[1]
	for _, c := range s.cells {
		floor = min(floor, c[1])
	}
	var pos [3]float64
	pos[s.normal] = s.plane + normal
	pos[2-s.normal] = float64(s.heart[2-s.normal]) + .5 + lateral
	pos[1] = float64(floor) + up
	return pos
}

func TestAPortalSheetIsTouchedOnlyInsideTheOpening(t *testing.T) {
	for name, s := range everyPortalSheet(t) {
		t.Run(name, func(t *testing.T) {
			for _, tc := range []struct {
				name                string
				lateral, normal, up float64
				want                bool
			}{
				{"standing in the middle of the veil", 0, 0, 0, true},
				{"a toe across the plane", 0, -PlayerWidth/2 + .01, 0, true},
				{"a hundredth in front of the plane", 0, -PlayerWidth/2 - .01, 0, false},
				{"just in front", 0, -.5, 0, false},
				{"just behind", 0, .5, 0, false},
				{"edge of the opening by a hundredth", 2.79, 0, 0, true},
				{"a hundredth clear of the opening", 2.81, 0, 0, false},
				{"inside a jamb's column", 3, 0, 0, false},
				{"beside the arch", 4.5, 0, 0, false},
				{"under a shoulder", 2, 0, 2, false},
				{"under the crown, in the opening", 1, 0, 1, true},
				{"above the crown", 0, 0, 4, false},
				{"standing on the crown", 0, 0, 3, false},
			} {
				got := s.touches(playerBox(s.at(tc.lateral, tc.normal, tc.up)))
				if got != tc.want {
					t.Errorf("%s: touches = %v, want %v", tc.name, got, tc.want)
				}
			}
		})
	}
}

func TestAPortalSheetIsCrossedBetweenTicksAndNeverByACornerCut(t *testing.T) {
	for name, s := range everyPortalSheet(t) {
		t.Run(name, func(t *testing.T) {
			for _, tc := range []struct {
				name     string
				from, to [3]float64
				want     bool
			}{
				// Terminal velocity is three blocks a tick: neither endpoint is near the
				// plane, and a sampled trigger would never see this body.
				{"straight through in one tick", s.at(0, -1.6, 0), s.at(0, 1.6, 0), true},
				{"through the opening's edge column", s.at(2.5, -1.6, 0), s.at(2.5, 1.6, 0), true},
				{"backwards through it", s.at(0, 1.6, 0), s.at(-.5, -1.6, 0), true},
				{"stationary inside", s.at(0, 0, 0), s.at(0, 0, 0), true},
				{"stationary in front", s.at(0, -1, 0), s.at(0, -1, 0), false},
				{"walking up to the veil and stopping short", s.at(0, -2, 0), s.at(0, -.31, 0), false},
				{"walking into the veil", s.at(0, -2, 0), s.at(0, -.29, 0), true},
				{"parallel to the veil in front of it", s.at(-5, -1, 0), s.at(5, -1, 0), false},
				{"through the plane beside the arch", s.at(4.5, -1.6, 0), s.at(4.5, 1.6, 0), false},
				// The body crosses the plane 3.75 blocks from the heart's centre, outside the
				// jamb. Its swept bounding box covers half the opening; the body never does.
				{"cutting past the corner of the arch", s.at(0, -1.5, 0), s.at(7.5, 1.5, 0), false},
				// And the same cut, steep enough that it is still inside the opening when it
				// reaches the plane.
				{"diagonally through the opening", s.at(0, -1.5, 0), s.at(3, 1.5, 0), true},
				{"falling past it above the crown", s.at(0, 0, 6), s.at(0, 0, 3.1), false},
				{"falling into it from above", s.at(0, 0, 6), s.at(0, 0, 2.9), true},
			} {
				got := s.crossedBy(playerBox(tc.from), playerBox(tc.to))
				if got != tc.want {
					t.Errorf("%s: crossedBy = %v, want %v", tc.name, got, tc.want)
				}
			}
		})
	}
}

// The two ways a sweep can end exactly on the boundary of the veil, built from bounds a
// float64 holds exactly. The closed intervals crossedBy narrows meet at that one instant,
// and only asking the half-open face test there tells a body that reaches the veil from
// one that stops flush against it.
func TestAPortalSheetSweepThatEndsFlushDoesNotTouch(t *testing.T) {
	for name, s := range everyPortalSheet(t) {
		lateral := 2 - s.normal
		exact := func(normalMin, lateralMin float64) box {
			var b box
			b.min[s.normal], b.max[s.normal] = normalMin, normalMin+.625
			b.min[lateral], b.max[lateral] = lateralMin, lateralMin+.625
			floor := s.at(0, 0, 0)[1]
			b.min[1], b.max[1] = floor, floor+1.75
			return b
		}
		heart := float64(s.heart[lateral])
		if s.crossedBy(exact(s.plane-2.5, heart), exact(s.plane-.625, heart)) {
			t.Errorf("%s: a body whose face arrives exactly on the plane touched the veil", name)
		}
		if s.crossedBy(exact(s.plane-.25, heart+6), exact(s.plane-.25, heart+3)) {
			t.Errorf("%s: a body sliding along the veil to stop flush with the opening touched it", name)
		}
		if !s.crossedBy(exact(s.plane-.25, heart+6), exact(s.plane-.25, heart+2.875)) {
			t.Errorf("%s: a body sliding an eighth into the opening did not touch it", name)
		}
	}
}

// A mounted body is wider and taller, and the sheet measures whichever box it is handed.
// Admission still refuses the rider; contact is geometry and does not.
func TestAPortalSheetMeasuresTheBodyItIsGiven(t *testing.T) {
	for name, s := range everyPortalSheet(t) {
		foot, mounted := playerBox(s.at(2.9, 0, 0)), mountedBody.boxAt(s.at(2.9, 0, 0))
		if s.touches(foot) || !s.touches(mounted) {
			t.Errorf("%s: a body's width is not what decides its contact", name)
		}
	}
}
