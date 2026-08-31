package world

import "slices"

// keepSchematic is 63×63×68: a curtain wall with a gate and a walk, two masses over a
// bridged inner court, four spires and four corner towers.
//
// **It is the largest drawing in this repository by an order of magnitude, and the size
// is the point.** A capital's keep is the first building a session sees — #519 puts the
// spawn on its gate square — and a 21-across keep read as a gatehouse standing on its
// own. The envelope here is taken 1:1 from the reference #682 names, *Creepy Blackstone
// Castle* by NevasBuildings: 62×61 blocks across and 68 courses tall, rounded up to an
// odd 63 because [centredBuilding] centres exactly and
// TestEverySchematicIsTheSizeItsIssueAsksFor requires an odd footprint.
//
// **The envelope is the reference's; not one block of it is.** No third-party file was
// imported, converted or read by a program. The build was looked at — elevations, a roof
// plan and floor plans rendered from a local extraction — and the massing, the plan and
// every course were composed here, against this palette, from our own geometry. That is
// what keeps the licence question out of a public repository entirely, and it is the
// decision #682 recorded before any of this was drawn.
//
// **How these literals were produced, said plainly because it matters.** They were
// composed by a drafting pass over our own plan rather than typed course by course:
// 4,284 rows of ASCII is not a thing a person edits, and pretending otherwise would
// invite somebody to try. What that pass does not do is read the reference. Edit this
// drawing the way you would edit any other picture — by hand, one course at a time — and
// let the tests below say whether the result still stands up.
//
// **The courses live in four files, and the reason is worth knowing before you tidy it
// away.** GitHub's Files API omits a file's `patch` once its diff passes a per-file size,
// and `.github/scripts/deepseek_review.py` refuses to review a diff it could not read —
// correctly, because a file returned with changes and no patch is a withheld patch and
// not a binary. Whole, this drawing measured 4,470 added lines and no patch at all, and
// the review job went red eighteen seconds in. Split four ways it is readable again.
// Each part is a storey: see schematic_keep_ground.go, _middle.go, _roofs.go and
// _spires.go.
//
// **Every course is checked for something a drawing can get wrong and a comment cannot
// say.** The doorway is a centred run on the +Z face; nothing standable and roofed is
// unreachable from it; every anchor can be walked to. The last of those shaped the plan
// more than taste did: the wall walk is cut through all four corner towers because a ring
// severed by them is three dead ends, and the four spired towers are solid shafts because
// a tower room nobody can enter is a room the drawing promises and never gives.
//
// The runes are [schematicLegend]: `b` the basalt footing, `K` and `k` the dressed wall
// and its weathered course, `S` smooth trim, `R` the slate of every roof and spire, `D`
// floorboards, `W` pale timber, `G` a window, `_` a room and `.` the terrain left alone.
// Rows run from the back (z=0) to the front (z=62, where the gate is), and each row runs
// left to right along +X.
var keepSchematic = mustSchematic(
	[]Anchor{
		// The gate's two guards stand inside the passage, one to each jamb.
		{X: 29, Y: 0, Z: 59, Kind: AnchorGuard},
		{X: 33, Y: 0, Z: 59, Kind: AnchorGuard},
		// And the carpenter works the west wing's ground floor.
		{X: 16, Y: 0, Z: 22, Kind: AnchorCarpenter},
	},
	slices.Concat(keepCoursesGround, keepCoursesMiddle, keepCoursesRoofs, keepCoursesSpires)...,
)
