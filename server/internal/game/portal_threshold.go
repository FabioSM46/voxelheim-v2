package game

import (
	"math"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// portalSheet is a runic arch's veil as a body can touch it: the plane through the
// middle of the arch's one-block course, bounded by the faces of the drawn veil cells.
//
// **Contact is with the opening, not with the arch and not with a radius.** A body
// standing beside the arch, pressed against a jamb or ducking under a shoulder is not
// in any veil cell's face and does not touch it; a body whose box reaches the plane
// inside the opening does, even by a hundredth of a block. It makes a crossing begin, and
// [Player.portalReachLocked] asks it again at admission: a body touching the veil is at
// the portal even when a sandstorm has shortened its reach to the heart.
type portalSheet struct {
	heart  [3]int64
	normal int
	plane  float64
	cells  [][3]int64
}

func newPortalSheet(t world.PortalThreshold) portalSheet {
	return portalSheet{heart: t.Heart, normal: t.Normal, plane: float64(t.Heart[t.Normal]) + .5, cells: t.Cells}
}

// touches reports whether b intersects the veil where it stands.
//
// The plane test is half-open like every box here: a body whose far face sits exactly
// on the plane does not reach it. Laterally the overlap must have width, so a body
// flush against the edge of the opening — which is the face of a jamb, a shoulder or
// the crown — touches the frame and not the veil.
func (s portalSheet) touches(b box) bool {
	if b.min[s.normal] > s.plane || b.max[s.normal] <= s.plane {
		return false
	}
	lateral := 2 - s.normal
	for _, c := range s.cells {
		if b.min[1] < float64(c[1]+1) && b.max[1] > float64(c[1]) &&
			b.min[lateral] < float64(c[lateral]+1) && b.max[lateral] > float64(c[lateral]) {
			return true
		}
	}
	return false
}

// crossedBy reports whether a body moving from one box to another touches the veil at
// any point along the way, the two endpoints included.
//
// **A tick is not a sample.** At a sprint, and certainly at terminal velocity, a body
// can stand in front of the veil on one tick and behind it on the next without either
// box reaching the plane, and a trigger that only asked [portalSheet.touches] would let
// it walk straight through. So the motion is treated as the straight line it is within
// one integration and solved exactly: for each veil cell, every face condition is
// linear in time, each narrows the interval of times at which it holds, and a non-empty
// intersection is contact.
//
// The same arithmetic is why this is not a swept bounding box. A body cutting past the
// corner of the arch sweeps a box that covers the opening while the body itself never
// enters it; solving per cell, per axis, over a shared time is what tells those apart.
//
// Bounds are narrowed as closed intervals and the one remaining boundary case — an
// intersection that is a single instant — is settled by asking [portalSheet.touches]
// at the midpoint, which is that instant. A positive-length intersection of intervals
// always contains its midpoint, so that question is exact in both cases.
func (s portalSheet) crossedBy(from, to box) bool {
	lo, hi := 0.0, 1.0
	// Straddling the plane: min at or before it and max beyond it.
	if !narrowTime(&lo, &hi, from.min[s.normal], to.min[s.normal], math.Inf(-1), s.plane) ||
		!narrowTime(&lo, &hi, from.max[s.normal], to.max[s.normal], s.plane, math.Inf(1)) {
		return false
	}
	lateral := 2 - s.normal
	for _, c := range s.cells {
		cLo, cHi := lo, hi
		if !narrowTime(&cLo, &cHi, from.min[1], to.min[1], math.Inf(-1), float64(c[1]+1)) ||
			!narrowTime(&cLo, &cHi, from.max[1], to.max[1], float64(c[1]), math.Inf(1)) ||
			!narrowTime(&cLo, &cHi, from.min[lateral], to.min[lateral], math.Inf(-1), float64(c[lateral]+1)) ||
			!narrowTime(&cLo, &cHi, from.max[lateral], to.max[lateral], float64(c[lateral]), math.Inf(1)) {
			continue
		}
		if s.touches(lerpBox(from, to, (cLo+cHi)/2)) {
			return true
		}
	}
	return false
}

// narrowTime narrows [lo, hi] to the times t at which v0 + (v1-v0)·t lies in [a, b], and
// reports whether any remain. Infinite bounds are one-sided conditions.
func narrowTime(lo, hi *float64, v0, v1, a, b float64) bool {
	d := v1 - v0
	if d == 0 {
		return v0 >= a && v0 <= b && *lo <= *hi
	}
	t1, t2 := (a-v0)/d, (b-v0)/d
	if t1 > t2 {
		t1, t2 = t2, t1
	}
	*lo, *hi = max(*lo, t1), min(*hi, t2)
	return *lo <= *hi
}

func lerpBox(from, to box, t float64) box {
	var b box
	for axis := range 3 {
		b.min[axis] = from.min[axis] + (to.min[axis]-from.min[axis])*t
		b.max[axis] = from.max[axis] + (to.max[axis]-from.max[axis])*t
	}
	return b
}
