package world

// The buildings a settlement is made of, written down as pictures.
//
// **A schematic is a literal, not a generator.** Every other feature in this package
// is a field sampled at a coordinate, because terrain is continuous and a field is
// the only thing that stays seamless across a chunk border. A hut is not continuous:
// it is a specific arrangement of a hundred and seventy voxels, and the honest way to
// write one down is to draw it. So this file holds four drawings, one `[]string` per
// y layer, and the only arithmetic anywhere near them is the quarter turn that points
// a door at the middle of the village.
//
// **The four legend runes are the whole of the vocabulary**, and two of them are not
// blocks: `.` is "leave whatever is there alone" and `_` is air. They are distinct on
// purpose even though the builder's clip makes them behave identically today — it only
// ever fills air, so writing air into air changes nothing — because they
// say different things about the drawing. A `.` is outside the building; a `_` is a
// room. A later issue that wants a cellar dug into the plateau will need exactly that
// difference, and re-deriving it from a picture that never made it is not possible.

// AnchorKind names what a slot in a building is for.
//
// **Anchors are the only thing this package says about entities, and it says it
// without knowing what one is.** internal/world places no forge, no villager and no
// stall — generate.go's standing note about entity helpers still holds — but the
// stations issue and the residents issue both need to know *where* one goes, and the
// answer has to be a pure function of the seed like everything else here or two
// servers would furnish the same keep differently. So a schematic carries coordinates
// with a word attached, and the packages that own entities read them.
type AnchorKind uint8

// The slots a building offers. Server-side only — no anchor ever crosses the wire —
// so this list is free to grow without a protocol bump.
const (
	// AnchorNone is the zero value and names no slot. It exists for the reason
	// [SurfaceUnknown] does: a zero that means "a forge goes here" would furnish a
	// building from an uninitialised struct.
	AnchorNone AnchorKind = iota

	// The two world-owned stations. A forge stands in a smithy and a campfire in a
	// hall; both are entities somebody else creates from these coordinates.
	AnchorForge
	AnchorCampfire

	// The residents. Four trades and two roles: a settlement's smith, carpenter,
	// cook and trader, the villagers who live in the huts, and the guards at a
	// keep's gate.
	AnchorSmith
	AnchorCarpenter
	AnchorCook
	AnchorTrader
	AnchorVillager
	AnchorGuard
)

// String names an anchor for test failures and diagnostics.
func (a AnchorKind) String() string {
	switch a {
	case AnchorForge:
		return "forge"
	case AnchorCampfire:
		return "campfire"
	case AnchorSmith:
		return "smith"
	case AnchorCarpenter:
		return "carpenter"
	case AnchorCook:
		return "cook"
	case AnchorTrader:
		return "trader"
	case AnchorVillager:
		return "villager"
	case AnchorGuard:
		return "guard"
	default:
		return "no anchor"
	}
}

// Anchor is one slot in a schematic's own frame: a coordinate inside the drawing,
// before any rotation or placement.
type Anchor struct {
	X, Y, Z int
	Kind    AnchorKind
}

// Schematic is one building, as voxels.
//
// Voxels is W×H×D and is indexed by [Schematic.At] — x fastest, then z, then y, which
// is [Index]'s order for the same reason a reader benefits from one order rather than
// two. A cell holding [keepTerrain] is not a block and is never written; see the
// legend above.
type Schematic struct {
	W, H, D int
	Voxels  []Block
	Anchors []Anchor
}

// keepTerrain is the `.` of a layer literal: a cell the builder passes over.
//
// Deliberately outside the palette rather than a second field beside Voxels. Every id
// in chunk.go is a small number that could plausibly be appended to one day, so a
// sentinel drawn from the same space would collide the moment somebody added a
// seventeenth block; the top of the uint16 range cannot, because [Block] is the wire
// type and 65535 ids is a limit this game will not reach. Nothing writes it: both
// [Schematic.At]'s callers test for it first.
const keepTerrain Block = 1<<16 - 1

// At reads one voxel of a schematic in its own frame.
//
// Unchecked, and panics on a coordinate outside the drawing — [Index] and this
// package's other index helpers are the same, and the reason is the same: every caller
// is inside this package and is already iterating a range derived from W, H and D. A
// bounds check here would be dead code that reads as a promise to callers who do not
// exist. [mustSchematic] is where a coordinate from outside that loop — an anchor — is
// checked, once, at init.
func (s *Schematic) At(x, y, z int) Block {
	return s.Voxels[(y*s.D+z)*s.W+x]
}

// BuildingKind names one of the four drawings below.
type BuildingKind uint8

// The buildings. A hut is where somebody lives, a smithy is where the forge is, a
// hall is where the fire and the food are, and a keep is what a capital has and a
// village does not.
const (
	BuildingHut BuildingKind = iota
	BuildingSmithy
	BuildingHall
	BuildingKeep
)

// String names a building for test failures and diagnostics.
func (k BuildingKind) String() string {
	switch k {
	case BuildingHut:
		return "hut"
	case BuildingSmithy:
		return "smithy"
	case BuildingHall:
		return "hall"
	case BuildingKeep:
		return "keep"
	default:
		return "unknown building"
	}
}

// Facing is which way a placed building's door points, expressed as the quarter turns
// its drawing is rotated by.
//
// **Every schematic below draws its door on the +Z face**, which is what makes one
// number enough: a building faces the middle of its settlement, so the placement code
// picks the turn that sends +Z at the centre and nothing else has to agree on a
// convention.
type Facing uint8

// The four turns, named by the world direction the door ends up pointing in.
const (
	FacingPlusZ Facing = iota
	FacingMinusX
	FacingMinusZ
	FacingPlusX
)

// SchematicFor returns the drawing for a building kind.
//
// The returned pointer addresses package state and must be treated as read-only: the
// four schematics are built once at init and are shared by every settlement in every
// world this process serves.
func SchematicFor(kind BuildingKind) *Schematic {
	switch kind {
	case BuildingSmithy:
		return smithySchematic
	case BuildingHall:
		return hallSchematic
	case BuildingKeep:
		return keepSchematic
	default:
		return hutSchematic
	}
}

// schematicLegend maps a layer literal's runes to what the builder writes.
//
// Five entries and no more. A rune absent from this map is a typo in a drawing, and
// [mustSchematic] refuses to build rather than guessing — which is what makes the
// literals below safe to edit by hand.
var schematicLegend = map[rune]Block{
	'.': keepTerrain,
	'_': Air,
	'#': Cobblestone,
	'P': Planks,
	'T': Thatch,
}

// mustSchematic turns layer literals into a [Schematic], and panics on a drawing that
// is not a box.
//
// **A panic at package initialisation rather than an error**, because the input is a
// literal in this file: there is no runtime condition under which a ragged drawing is
// recoverable, and a schematic that silently truncated would put a building into the
// world with one wall missing. TestEverySchematicIsRectangularAndLegible asserts the
// same thing against every drawing, so the failure arrives as a test rather than as a
// crashed server.
func mustSchematic(anchors []Anchor, layers ...[]string) *Schematic {
	if len(layers) == 0 {
		panic("schematic with no layers")
	}
	h := len(layers)
	d := len(layers[0])
	if d == 0 {
		panic("schematic layer with no rows")
	}
	w := len([]rune(layers[0][0]))

	voxels := make([]Block, w*h*d)
	for y, layer := range layers {
		if len(layer) != d {
			panic("schematic layer with a different depth from the first")
		}
		for z, row := range layer {
			runes := []rune(row)
			if len(runes) != w {
				panic("schematic row with a different width from the first")
			}
			for x, r := range runes {
				block, known := schematicLegend[r]
				if !known {
					panic("schematic rune outside the legend")
				}
				voxels[(y*d+z)*w+x] = block
			}
		}
	}

	s := &Schematic{W: w, H: h, D: d, Voxels: voxels, Anchors: anchors}
	for _, a := range anchors {
		if a.X < 0 || a.X >= w || a.Y < 0 || a.Y >= h || a.Z < 0 || a.Z >= d {
			panic("schematic anchor outside the drawing")
		}
		if s.At(a.X, a.Y, a.Z) != Air {
			panic("schematic anchor in a cell that is not air")
		}
	}
	return s
}

// rotatedFootprint is a schematic's extent after a quarter turn: the two horizontal
// axes swap on an odd number of turns and the height never moves.
func rotatedFootprint(s *Schematic, facing Facing) (w, d int) {
	if facing == FacingMinusX || facing == FacingPlusX {
		return s.D, s.W
	}
	return s.W, s.D
}

// rotateCell maps a cell of a schematic's own frame onto the rotated footprint.
//
// The four cases are one rotation each, and the pair that matters is what happens to
// the +Z direction the doors are drawn on: no turn leaves it at +Z, one sends it to
// −X, two to −Z and three to +X — which is exactly the order [Facing]'s constants are
// declared in, so the enum value is the number of turns.
func rotateCell(x, z, w, d int, facing Facing) (int, int) {
	switch facing {
	case FacingMinusX:
		return d - 1 - z, x
	case FacingMinusZ:
		return w - 1 - x, d - 1 - z
	case FacingPlusX:
		return z, w - 1 - x
	default:
		return x, z
	}
}

// PlacedAnchor is one of a building's slots in world coordinates.
//
// **The reason this package computes anchors at all.** internal/world creates no
// entities and must not start; the stations and residents issues do, and what they need
// from terrain is a coordinate that is the same on every server for a seed. So the
// drawing carries the slot, the placement rotates and translates it, and whoever owns
// forges reads the answer.
type PlacedAnchor struct {
	X, Y, Z int64
	Kind    AnchorKind
}

// Building is one placed schematic.
//
// Origin is the world coordinate of the *rotated* footprint's minimum corner, and
// OriginY is the first air voxel above the ground, so a building stands on what it is
// built on rather than in it.
type Building struct {
	Kind                      BuildingKind
	OriginX, OriginY, OriginZ int64
	Facing                    Facing
	Anchors                   []PlacedAnchor
}

// centredBuilding places a drawing on a column, facing a way, with its floor at floorY.
//
// The centring is exact because every footprint here is an odd number of blocks across —
// TestEverySchematicIsTheSizeItsIssueAsksFor is what keeps that true. The slots are
// turned with the walls, which is the whole reason a caller never rotates an anchor
// itself.
func centredBuilding(kind BuildingKind, plotX, plotZ, floorY int64, facing Facing) Building {
	schematic := SchematicFor(kind)
	w, d := rotatedFootprint(schematic, facing)

	b := Building{
		Kind:    kind,
		OriginX: plotX - int64(w/2),
		OriginY: floorY,
		OriginZ: plotZ - int64(d/2),
		Facing:  facing,
	}
	for _, a := range schematic.Anchors {
		rx, rz := rotateCell(a.X, a.Z, schematic.W, schematic.D, facing)
		b.Anchors = append(b.Anchors, PlacedAnchor{
			X:    b.OriginX + int64(rx),
			Y:    b.OriginY + int64(a.Y),
			Z:    b.OriginZ + int64(rz),
			Kind: a.Kind,
		})
	}
	return b
}

// visitSchematic yields every voxel of a placed building in world coordinates.
//
// The counterpart of [visitTree], and the same contract: it reads nothing but its
// arguments, so a chunk completes a building that overhangs its border by visiting it
// again rather than by consulting the neighbour that also holds part of it. Cells the
// drawing marks with `.` are not yielded at all.
//
// **A schematic knows nothing about settlements, and that is the seam.** What is drawn
// and how it is turned is this file's business; where it stands is settlement.go's.
func visitSchematic(b Building, visit func(x, y, z int64, block Block)) {
	s := SchematicFor(b.Kind)
	for y := range s.H {
		for z := range s.D {
			for x := range s.W {
				block := s.At(x, y, z)
				if block == keepTerrain {
					continue
				}
				rx, rz := rotateCell(x, z, s.W, s.D, b.Facing)
				visit(b.OriginX+int64(rx), b.OriginY+int64(y), b.OriginZ+int64(rz), block)
			}
		}
	}
}

// The four drawings. Rows run from the back of the building (z = 0) to its front
// (z = D−1, where every door is), and each row runs left to right along +X.

// hutSchematic is 7×5×7: a cobble footing, plank walls, a thatched roof and one
// person's worth of floor.
var hutSchematic = mustSchematic(
	[]Anchor{{X: 3, Y: 0, Z: 3, Kind: AnchorVillager}},
	[]string{ // y=0 — the footing course, with the doorway open
		"#######",
		"#_____#",
		"#_____#",
		"#_____#",
		"#_____#",
		"#_____#",
		"###_###",
	},
	[]string{ // y=1 — timber, doorway still open
		"PPPPPPP",
		"P_____P",
		"P_____P",
		"P_____P",
		"P_____P",
		"P_____P",
		"PPP_PPP",
	},
	[]string{ // y=2 — the lintel closes the doorway
		"PPPPPPP",
		"P_____P",
		"P_____P",
		"P_____P",
		"P_____P",
		"P_____P",
		"PPPPPPP",
	},
	[]string{ // y=3 — the eaves
		"TTTTTTT",
		"TTTTTTT",
		"TTTTTTT",
		"TTTTTTT",
		"TTTTTTT",
		"TTTTTTT",
		"TTTTTTT",
	},
	[]string{ // y=4 — the cap
		"_______",
		"_TTTTT_",
		"_TTTTT_",
		"_TTTTT_",
		"_TTTTT_",
		"_TTTTT_",
		"_______",
	},
)

// smithySchematic is 9×6×9: the same construction as a hut, one course taller and
// wide enough that the forge is not standing in the doorway.
var smithySchematic = mustSchematic(
	[]Anchor{
		{X: 2, Y: 0, Z: 2, Kind: AnchorForge},
		{X: 6, Y: 0, Z: 6, Kind: AnchorSmith},
	},
	[]string{ // y=0
		"#########",
		"#_______#",
		"#_______#",
		"#_______#",
		"#_______#",
		"#_______#",
		"#_______#",
		"#_______#",
		"####_####",
	},
	[]string{ // y=1
		"PPPPPPPPP",
		"P_______P",
		"P_______P",
		"P_______P",
		"P_______P",
		"P_______P",
		"P_______P",
		"P_______P",
		"PPPP_PPPP",
	},
	[]string{ // y=2 — windows on the two long sides
		"PPPPPPPPP",
		"P_______P",
		"_________",
		"P_______P",
		"P_______P",
		"P_______P",
		"_________",
		"P_______P",
		"PPPPPPPPP",
	},
	[]string{ // y=3
		"PPPPPPPPP",
		"P_______P",
		"P_______P",
		"P_______P",
		"P_______P",
		"P_______P",
		"P_______P",
		"P_______P",
		"PPPPPPPPP",
	},
	[]string{ // y=4 — the eaves
		"TTTTTTTTT",
		"TTTTTTTTT",
		"TTTTTTTTT",
		"TTTTTTTTT",
		"TTTTTTTTT",
		"TTTTTTTTT",
		"TTTTTTTTT",
		"TTTTTTTTT",
		"TTTTTTTTT",
	},
	[]string{ // y=5 — the cap
		"_________",
		"_TTTTTTT_",
		"_TTTTTTT_",
		"_TTTTTTT_",
		"_TTTTTTT_",
		"_TTTTTTT_",
		"_TTTTTTT_",
		"_TTTTTTT_",
		"_________",
	},
)

// hallSchematic is 13×8×13: the long house, with a fire in the middle of the floor
// and a double doorway wide enough to carry something through.
var hallSchematic = mustSchematic(
	[]Anchor{
		{X: 6, Y: 0, Z: 6, Kind: AnchorCampfire},
		{X: 9, Y: 0, Z: 4, Kind: AnchorCook},
		{X: 3, Y: 0, Z: 9, Kind: AnchorTrader},
	},
	[]string{ // y=0
		"#############",
		"#___________#",
		"#___________#",
		"#___________#",
		"#___________#",
		"#___________#",
		"#___________#",
		"#___________#",
		"#___________#",
		"#___________#",
		"#___________#",
		"#___________#",
		"#####___#####",
	},
	[]string{ // y=1
		"PPPPPPPPPPPPP",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"PPPPP___PPPPP",
	},
	[]string{ // y=2 — windows on the two long sides
		"PPPPPPPPPPPPP",
		"P___________P",
		"P___________P",
		"_____________",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"_____________",
		"P___________P",
		"P___________P",
		"PPPPP___PPPPP",
	},
	[]string{ // y=3
		"PPPPPPPPPPPPP",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"PPPPPPPPPPPPP",
	},
	[]string{ // y=4
		"PPPPPPPPPPPPP",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"P___________P",
		"PPPPPPPPPPPPP",
	},
	[]string{ // y=5 — the eaves
		"TTTTTTTTTTTTT",
		"TTTTTTTTTTTTT",
		"TTTTTTTTTTTTT",
		"TTTTTTTTTTTTT",
		"TTTTTTTTTTTTT",
		"TTTTTTTTTTTTT",
		"TTTTTTTTTTTTT",
		"TTTTTTTTTTTTT",
		"TTTTTTTTTTTTT",
		"TTTTTTTTTTTTT",
		"TTTTTTTTTTTTT",
		"TTTTTTTTTTTTT",
		"TTTTTTTTTTTTT",
	},
	[]string{ // y=6
		"_____________",
		"_TTTTTTTTTTT_",
		"_TTTTTTTTTTT_",
		"_TTTTTTTTTTT_",
		"_TTTTTTTTTTT_",
		"_TTTTTTTTTTT_",
		"_TTTTTTTTTTT_",
		"_TTTTTTTTTTT_",
		"_TTTTTTTTTTT_",
		"_TTTTTTTTTTT_",
		"_TTTTTTTTTTT_",
		"_TTTTTTTTTTT_",
		"_____________",
	},
	[]string{ // y=7 — the ridge
		"_____________",
		"_____________",
		"__TTTTTTTTT__",
		"__TTTTTTTTT__",
		"__TTTTTTTTT__",
		"__TTTTTTTTT__",
		"__TTTTTTTTT__",
		"__TTTTTTTTT__",
		"__TTTTTTTTT__",
		"__TTTTTTTTT__",
		"__TTTTTTTTT__",
		"_____________",
		"_____________",
	},
)

// keepSchematic is 21×28×21: the capital's castle — a curtain wall two courses thick
// with a gate and a walk along its top, a courtyard, and a keep of three occupiable
// floors joined by two flights of stairs.
//
// **The wall ring is part of the building rather than a feature of the settlement**,
// which keeps the whole of a capital's centre one pure function of one lattice cell:
// there is no second pass that draws a perimeter, and a chunk holding only a corner of
// the wall reaches it by visiting this drawing like any other. At 21×28×21 it straddles
// up to eight chunks at once, and [visitSchematic]'s contract — read nothing but the
// arguments, let the chunk clip — is what makes that a non-event.
//
// **The section, because the layers below are hard to read it off.** A body's feet are
// at the level *above* the block it stands on, so a slab at y=6 is walked at y=7. Ground
// floor y=0..5; second slab at y=6, walked at 7; third at y=12, walked at 13; the eaves
// at y=17 close the top storey; roof at 18, cap at 19. The curtain is solid to y=5, its
// walk is the inner ring at y=6..7, and the outer ring carries on to 7 as a parapet.
// Four corner towers rise from there through y=27. Their front pair share a plank bridge
// at y=20, walked at y=21; a stair from the third floor reaches its open south rail.
//
// **A stair is a diagonal of single blocks, which is why almost no two layers here are
// alike.** Nothing in this world auto-steps — the jump impulse in internal/game clears
// one block and not two — so a flight rises exactly one block per cell. Two are inside
// the keep, each leaving a hole in the slab it arrives through so there is headroom on
// the way up; a third, in the courtyard, reaches the wall walk. A fourth climbs from the
// third floor to the bridge one block per cell, turning outside the keep to clear its
// eaves. The front tower rooms open directly onto that bridge; the rear shafts stay solid,
// so the drawing introduces no sealed tower room. Each shaft ends in a plank course that
// oversails inward, an inset cobble course and a tapering thatch finial — the four capitals
// break the silhouette without widening the footprint the settlement guards were built for.
var keepSchematic = mustSchematic(
	[]Anchor{
		{X: 8, Y: 0, Z: 18, Kind: AnchorGuard},
		{X: 12, Y: 0, Z: 18, Kind: AnchorGuard},
		{X: 10, Y: 0, Z: 13, Kind: AnchorCarpenter},
	},
	[]string{ // y=0
		"#####################",
		"#####################",
		"##_________________##",
		"##_________________##",
		"##_________________##",
		"##___###########___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"###__##________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___####___####___##",
		"##_________________##",
		"##_________________##",
		"##_________________##",
		"#########___#########",
		"#########___#########",
	},
	[]string{ // y=1
		"#####################",
		"#####################",
		"##_________________##",
		"##_________________##",
		"##_________________##",
		"##___###########___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"###__##________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___####___####___##",
		"##_________________##",
		"##_________________##",
		"##_________________##",
		"#########___#########",
		"#########___#########",
	},
	[]string{ // y=2
		"#####################",
		"#####################",
		"##_________________##",
		"##_________________##",
		"##_________________##",
		"##___###########___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"###__##________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___####___####___##",
		"##_________________##",
		"##_________________##",
		"##_________________##",
		"#########___#########",
		"#########___#########",
	},
	[]string{ // y=3
		"#####################",
		"#####################",
		"##_________________##",
		"##_________________##",
		"##_________________##",
		"##___##_#####_##___##",
		"##___#_________#___##",
		"##_________________##",
		"##___#_________#___##",
		"###__##________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##_________________##",
		"##___#_________#___##",
		"##___##_#####_##___##",
		"##_________________##",
		"##_________________##",
		"##_________________##",
		"#####################",
		"#####################",
	},
	[]string{ // y=4
		"#####################",
		"#####################",
		"##_________________##",
		"##_________________##",
		"##_________________##",
		"##___###########___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"###__##________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___###########___##",
		"##_________________##",
		"##_________________##",
		"##_________________##",
		"#####################",
		"#####################",
	},
	[]string{ // y=5
		"#####################",
		"#####################",
		"##_________________##",
		"##_________________##",
		"##_________________##",
		"##___###########___##",
		"##___#_________#___##",
		"###__##________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___#_________#___##",
		"##___###########___##",
		"##_________________##",
		"##_________________##",
		"##_________________##",
		"#####################",
		"#####################",
	},
	[]string{ // y=6
		"#####################",
		"#___________________#",
		"#___________________#",
		"#___________________#",
		"#___________________#",
		"#____###########____#",
		"#____###########____#",
		"#____#_#########____#",
		"#____#_#########____#",
		"#____#_#########____#",
		"#____#_#########____#",
		"#____#_#########____#",
		"#____#_#########____#",
		"#____###########____#",
		"#____###########____#",
		"#____###########____#",
		"#___________________#",
		"#___________________#",
		"#___________________#",
		"#___________________#",
		"#####################",
	},
	[]string{ // y=7
		"#####################",
		"#___________________#",
		"#___________________#",
		"#___________________#",
		"#___________________#",
		"#____###########____#",
		"#____#_________#____#",
		"#____#_________#____#",
		"#____#_________#____#",
		"#____#_________#____#",
		"#____#_________#____#",
		"#____#_________#____#",
		"#____#________##____#",
		"#____#_________#____#",
		"#____#_________#____#",
		"#____###########____#",
		"#___________________#",
		"#___________________#",
		"#___________________#",
		"#___________________#",
		"#####################",
	},
	[]string{ // y=8
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"_____###########_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#________##_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____###########_____",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
	},
	[]string{ // y=9
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"_____##_#####_##_____",
		"_____#_________#_____",
		"_____________________",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#________##_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____________________",
		"_____#_________#_____",
		"_____##_#####_##_____",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
	},
	[]string{ // y=10
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"_____###########_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#________##_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____###########_____",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
	},
	[]string{ // y=11
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"_____###########_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#________##_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____###########_____",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
	},
	[]string{ // y=12
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"_____###########_____",
		"_____###########_____",
		"_____###########_____",
		"_____#########_#_____",
		"_____#########_#_____",
		"_____#########_#_____",
		"_____#########_#_____",
		"_____#########_#_____",
		"_____###########_____",
		"_____###########_____",
		"_____###########_____",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
	},
	[]string{ // y=13
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"_____###########_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#____#____#_____",
		"_____###########_____",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
	},
	[]string{ // y=14
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"_____###########_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____###########_____",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
	},
	[]string{ // y=15
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"_____##_#####_##_____",
		"_____#_________#_____",
		"_____________________",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____#_________#_____",
		"_____________________",
		"_____#_________#_____",
		"_____##_##_##_##_____",
		"#####_____#_____#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
	},
	[]string{ // y=16
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"_____PPPPPPPPPPP_____",
		"_____P_________P_____",
		"_____P_________P_____",
		"_____P_________P_____",
		"_____P_________P_____",
		"_____P_________P_____",
		"_____P_________P_____",
		"_____P_________P_____",
		"_____P_________P_____",
		"_____P_________P_____",
		"_____PPPPP_PPPPP_____",
		"#####____#______#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
	},
	[]string{ // y=17
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####PPPPPPPPPPP#####",
		"____PPPPPPPPPPPPP____",
		"____PPPPPPPPPPPPP____",
		"____PPPPPPPPPPPPP____",
		"____PPPPPPPPPPPPP____",
		"____PPPPPPPPPPPPP____",
		"____PPPPPPPPPPPPP____",
		"____PPPPPPPPPPPPP____",
		"____PPPPPPPPPPPPP____",
		"____PPPPPPPPPPPPP____",
		"____PPPPPPPPPPPPP____",
		"____PPPPPP_PPPPPP____",
		"#####PPP#__PPPPP#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
	},
	[]string{ // y=18
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"_____PPPPPPPPPPP_____",
		"_____PPPPPPPPPPP_____",
		"_____PPPPPPPPPPP_____",
		"_____PPPPPPPPPPP_____",
		"_____PPPPPPPPPPP_____",
		"_____PPPPPPPPPPP_____",
		"_____PPPPPPPPPPP_____",
		"_____PPPPPPPPPPP_____",
		"_____PPPPPPPPPPP_____",
		"_____PPPPPPPPPPP_____",
		"_____PPPPPPPPPPP_____",
		"#####__#________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
	},
	[]string{ // y=19
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"_____________________",
		"_____________________",
		"_______TTTTTTT_______",
		"_______TTTTTTT_______",
		"_______TTTTTTT_______",
		"_______TTTTTTT_______",
		"_______TTTTTTT_______",
		"_______TTTTTTT_______",
		"_______TTTTTTT_______",
		"_____________________",
		"_____________________",
		"#####_#_________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
	},
	[]string{ // y=20 — tower platforms and the elevated bridge deck
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"#####___________#####",
		"####PPPPPPPPPPPPP####",
		"####PPPPPPPPPPPPP####",
		"####PPPPPPPPPPPPP####",
		"#####___________#####",
	},
	[]string{ // y=21 — front tower rooms, bridge rails and stair landing
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"#####___________#####",
		"#___#P_PPPPPPPPP#___#",
		"#___________________#",
		"#___#PPPPPPPPPPP#___#",
		"#####___________#####",
	},
	[]string{ // y=22 — tower shafts
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"#####___________#####",
		"#___#___________#___#",
		"#___________________#",
		"#___#___________#___#",
		"#####___________#####",
	},
	[]string{ // y=23 — tower shafts under the capitals
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"#####___________#####",
		"#___#___________#___#",
		"#___________________#",
		"#___#___________#___#",
		"#####___________#####",
	},
	[]string{ // y=24 — plank corbels oversail each shaft inward
		"PPPPPP_________PPPPPP",
		"PPPPPP_________PPPPPP",
		"PPPPPP_________PPPPPP",
		"PPPPPP_________PPPPPP",
		"PPPPPP_________PPPPPP",
		"PPPPPP_________PPPPPP",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"PPPPPP_________PPPPPP",
		"PPPPPP_________PPPPPP",
		"PPPPPP_________PPPPPP",
		"PPPPPP_________PPPPPP",
		"PPPPPP_________PPPPPP",
		"PPPPPP_________PPPPPP",
	},
	[]string{ // y=25 — inset cobble capitals
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
		"#####___________#####",
	},
	[]string{ // y=26 — thatch finials
		"_____________________",
		"_TTT_____________TTT_",
		"_TTT_____________TTT_",
		"_TTT_____________TTT_",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_TTT_____________TTT_",
		"_TTT_____________TTT_",
		"_TTT_____________TTT_",
		"_____________________",
	},
	[]string{ // y=27 — tower caps
		"_____________________",
		"_____________________",
		"__T_______________T__",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"_____________________",
		"__T_______________T__",
		"_____________________",
		"_____________________",
	},
)
