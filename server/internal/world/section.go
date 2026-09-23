package world

import (
	"errors"
	"fmt"
)

// Dungeon sections, built in code rather than drawn.
//
// **The castle is the reason this file exists.** Its drawings cost about 350,000
// characters of layer literal (schematic_keep_*.go), and the first dungeon is larger
// than the castle. A dungeon zone is mostly rooms, corridors, shafts and stairs cut
// into rock, which is a handful of operations applied to boxes — so a zone is written
// as those operations and the drawing is their result: an ordinary [Schematic], turned
// and placed by exactly the code every literal drawing uses.
//
// A section starts as void — the drawing's `.`, never written — and every operation
// works on inclusive boxes in the section's own frame. Carving is additive: a room
// wraps itself in a one-block shell only where the section is still void, so two
// rooms carved side by side, or a corridor carved into a room, open into each other
// instead of leaving a wall between them.

// Box is an inclusive box of cells in a section's own frame.
type Box struct {
	X0, Y0, Z0 int
	X1, Y1, Z1 int
}

// Section is a dungeon drawing under construction. Build turns it into a
// [Schematic]; nothing reads a Section directly.
type Section struct {
	w, h, d int
	voxels  []Block
	anchors []Anchor
	err     error
}

// NewSection returns a W×H×D section that is void everywhere.
func NewSection(w, h, d int) *Section {
	s := &Section{w: w, h: h, d: d}
	if w <= 0 || h <= 0 || d <= 0 {
		s.err = fmt.Errorf("section %d×%d×%d has no volume", w, h, d)
		return s
	}
	s.voxels = make([]Block, w*h*d)
	for i := range s.voxels {
		s.voxels[i] = keepTerrain
	}
	return s
}

// inside reports whether a cell is in the section's frame.
func (s *Section) inside(x, y, z int) bool {
	return x >= 0 && x < s.w && y >= 0 && y < s.h && z >= 0 && z < s.d
}

func (s *Section) index(x, y, z int) int { return (y*s.d+z)*s.w + x }

// check records the first box that leaves the frame or is inside out. Operations on
// a broken section do nothing, and Build reports the first failure: a zone written as
// a long list of calls reads better without an error check after every line.
func (s *Section) check(op string, b Box) bool {
	if s.err != nil {
		return false
	}
	if b.X0 > b.X1 || b.Y0 > b.Y1 || b.Z0 > b.Z1 || !s.inside(b.X0, b.Y0, b.Z0) || !s.inside(b.X1, b.Y1, b.Z1) {
		s.err = fmt.Errorf("%s box %+v is not inside the %d×%d×%d section", op, b, s.w, s.h, s.d)
		return false
	}
	return true
}

func (s *Section) each(b Box, visit func(x, y, z int)) {
	for y := b.Y0; y <= b.Y1; y++ {
		for z := b.Z0; z <= b.Z1; z++ {
			for x := b.X0; x <= b.X1; x++ {
				visit(x, y, z)
			}
		}
	}
}

// Fill writes block into every cell of the box, whatever was there.
func (s *Section) Fill(b Box, block Block) *Section {
	if s.check("fill", b) {
		s.each(b, func(x, y, z int) { s.voxels[s.index(x, y, z)] = block })
	}
	return s
}

// CarveRoom makes the box air and wraps it in a one-block shell of wall wherever the
// shell would otherwise be void. The shell may fall outside the frame, in which case
// that face is simply open to the void — so leave a margin of one around every room.
func (s *Section) CarveRoom(interior Box, wall Block) *Section {
	if !s.check("room", interior) {
		return s
	}
	shell := Box{interior.X0 - 1, interior.Y0 - 1, interior.Z0 - 1, interior.X1 + 1, interior.Y1 + 1, interior.Z1 + 1}
	s.each(shell, func(x, y, z int) {
		if s.inside(x, y, z) && s.voxels[s.index(x, y, z)] == keepTerrain {
			s.voxels[s.index(x, y, z)] = wall
		}
	})
	s.each(interior, func(x, y, z int) { s.voxels[s.index(x, y, z)] = Air })
	return s
}

// CarveCorridor cuts a passage of the given width and height (in air cells) from one
// floor-level cell to another on the same level: along X first, then along Z, so the
// passage is an L when the two ends differ on both axes. width is centred on the
// line; an even width leans towards +X / +Z.
func (s *Section) CarveCorridor(fromX, fromZ, toX, toZ, y, width, height int, wall Block) *Section {
	if s.err != nil {
		return s
	}
	if width < 1 || height < 1 {
		s.err = fmt.Errorf("corridor %d wide and %d tall", width, height)
		return s
	}
	lo, hi := -(width-1)/2, width/2
	x0, x1 := min(fromX, toX), max(fromX, toX)
	s.CarveRoom(Box{x0 + lo, y, fromZ + lo, x1 + hi, y + height - 1, fromZ + hi}, wall)
	z0, z1 := min(fromZ, toZ), max(fromZ, toZ)
	return s.CarveRoom(Box{toX + lo, y, z0 + lo, toX + hi, y + height - 1, z1 + hi}, wall)
}

// CarveShaft cuts a vertical opening through every level from bottom to top
// inclusive, shelled like a room. A shaft is how two floors meet without stairs: a
// chasm, a well, a drop into water.
func (s *Section) CarveShaft(x0, z0, x1, z1, bottom, top int, wall Block) *Section {
	return s.CarveRoom(Box{x0, bottom, z0, x1, top, z1}, wall)
}

// FillFloor writes block across one level of a rectangle — a sand or water floor, or
// a bridge across a shaft — replacing whatever was there.
func (s *Section) FillFloor(x0, z0, x1, z1, y int, block Block) *Section {
	return s.Fill(Box{x0, y, z0, x1, y, z1}, block)
}

// PlaceStairs lays a flight that climbs one level per step towards facing, starting
// with its first step on level y at (x, z). width runs to the right of the direction
// of travel. Each step is a slate stair whose high half points up the flight, the
// cell under every step but the first is filled with support, and three cells of
// headroom above each step are cleared. Carve the space the flight stands in first:
// stairs furnish a room, they do not shell one.
func (s *Section) PlaceStairs(x, y, z int, facing Facing, steps, width int, support Block) *Section {
	if s.err != nil {
		return s
	}
	if steps < 1 || width < 1 || facing > FacingPlusX {
		s.err = fmt.Errorf("stairs of %d steps, %d wide, facing %d", steps, width, facing)
		return s
	}
	// Direction of travel and the step's high half. Facing names where a door points,
	// so FacingPlusZ climbs towards +Z, whose high half is the south one.
	var dx, dz int
	var high ShapeFacing
	switch facing {
	case FacingPlusZ:
		dz, high = 1, ShapeSouth
	case FacingMinusX:
		dx, high = -1, ShapeWest
	case FacingMinusZ:
		dz, high = -1, ShapeNorth
	case FacingPlusX:
		dx, high = 1, ShapeEast
	}
	rx, rz := -dz, dx // to the right of travel: +Z travel widens towards -X
	stair := SlateStairNorthBottom + Block(high)
	for i := range steps {
		for j := range width {
			cx, cy, cz := x+dx*i+rx*j, y+i, z+dz*i+rz*j
			if !s.inside(cx, cy, cz) || !s.inside(cx, cy+3, cz) || (i > 0 && !s.inside(cx, cy-1, cz)) {
				s.err = fmt.Errorf("stair step %d,%d,%d or its headroom leaves the section", cx, cy, cz)
				return s
			}
			s.voxels[s.index(cx, cy, cz)] = stair
			for up := 1; up <= 3; up++ {
				s.voxels[s.index(cx, cy+up, cz)] = Air
			}
			if i > 0 {
				s.voxels[s.index(cx, cy-1, cz)] = support
			}
		}
	}
	return s
}

// Anchor records a slot at one cell. The rules for where each kind may stand are the
// ones every drawing obeys, and Build checks them.
func (s *Section) Anchor(kind AnchorKind, x, y, z, index int) *Section {
	s.anchors = append(s.anchors, Anchor{X: x, Y: y, Z: z, Kind: kind, Index: index})
	return s
}

// errEmptySection is Build's answer for a section nothing was ever carved into.
var errEmptySection = errors.New("section holds nothing but void")

// Build returns the finished drawing, or the first thing wrong with it.
func (s *Section) Build() (*Schematic, error) {
	if s.err != nil {
		return nil, s.err
	}
	drawn := false
	for _, b := range s.voxels {
		if b != keepTerrain {
			drawn = true
			break
		}
	}
	if !drawn {
		return nil, errEmptySection
	}
	out := &Schematic{
		W: s.w, H: s.h, D: s.d,
		Voxels:  append([]Block(nil), s.voxels...),
		Anchors: append([]Anchor(nil), s.anchors...),
	}
	if msg := anchorProblem(out); msg != "" {
		return nil, errors.New(msg)
	}
	return out, nil
}

// MustBuild is Build for a zone defined at package initialisation, where a broken
// section is a programming error — the same bargain [mustSchematic] makes.
func (s *Section) MustBuild() *Schematic {
	out, err := s.Build()
	if err != nil {
		panic("section: " + err.Error())
	}
	return out
}
