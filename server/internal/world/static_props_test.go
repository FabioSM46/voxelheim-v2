package world

import (
	_ "embed"
	"fmt"
	"math"
	"reflect"
	"slices"
	"strings"
	"testing"
)

func TestStaticPropPlacementsRotateWithKeepAndPreserveSlots(t *testing.T) {
	poses := []StaticPropPose{{Slot: 7, Kind: PropChair, X: 20, Y: 7, Z: 20, Facing: 1}, {Slot: 256, Kind: PropBanquetTable, X: 41, Y: 0, Z: 21, Facing: 2}}
	for facing := Facing(0); facing < 4; facing++ {
		building := Building{Kind: BuildingKeep, OriginX: -100, OriginY: 10, OriginZ: 200, Facing: facing}
		got, err := placeStaticProps(123, building, poses)
		if err != nil {
			t.Fatal(err)
		}
		reversed, err := placeStaticProps(123, building, []StaticPropPose{poses[1], poses[0]})
		if err != nil {
			t.Fatal(err)
		}
		if !reflect.DeepEqual(got[0], reversed[1]) || !reflect.DeepEqual(got[1], reversed[0]) {
			t.Fatal("insertion order renames or moves props")
		}
		for i, p := range got {
			if p.ID == 0 || p.ID&511 != uint64(poses[i].Slot) {
				t.Fatal("slot namespace lost")
			}
			if p.Facing != 1+(poses[i].Facing-1+uint8(facing))%4 {
				t.Fatal("prop does not rotate with keep")
			}
		}
	}
}

func TestStaticPropCardinalBoundsMatchAsymmetricShape(t *testing.T) {
	local := PropBox{Min: [3]float64{-1, 0, -2}, Max: [3]float64{3, 4, 5}}
	expected := []PropBox{
		{Min: [3]float64{9.5, 20, 28.5}, Max: [3]float64{13.5, 24, 35.5}},
		{Min: [3]float64{5.5, 20, 29.5}, Max: [3]float64{12.5, 24, 33.5}},
		{Min: [3]float64{7.5, 20, 25.5}, Max: [3]float64{11.5, 24, 32.5}},
		{Min: [3]float64{8.5, 20, 27.5}, Max: [3]float64{15.5, 24, 31.5}},
	}
	for i, want := range expected {
		p := PlacedStaticProp{Origin: [3]int64{10, 20, 30}, Facing: uint8(i + 1)}
		if got := p.Bounds(local); got != want {
			t.Fatalf("facing%d: got%v want%v", i+1, got, want)
		}
	}
}

func TestStaticPropCatalogueSeparatesSolidFurnitureAndDressing(t *testing.T) {
	count := 0
	for kind := PropBanquetTable; kind <= PropTableCandelabrum; kind++ {
		bounds := PropCollisionBounds(kind)
		if (len(bounds) > 0) != (kind <= PropCouncilTable) {
			t.Fatalf("kind%d solidity policy", kind)
		}
		count += len(bounds)
		visual := PropVisualBounds(kind)
		for _, b := range bounds {
			for axis := range 3 {
				if math.IsNaN(b.Min[axis]) || b.Min[axis] >= b.Max[axis] || b.Min[axis] < visual.Min[axis] || b.Max[axis] > visual.Max[axis] {
					t.Fatalf("kind%d invalid bound", kind)
				}
			}
		}
	}
	if count != 39 {
		t.Fatalf("collision catalogue changed without parity fixture update: %d", count)
	}
}

func TestStaticPropLayoutRejectsMalformedOrDuplicateSlots(t *testing.T) {
	building := Building{Kind: BuildingKeep}
	valid := StaticPropPose{Slot: 1, Kind: PropChair, X: 20, Y: 0, Z: 20, Facing: 1}
	for _, bad := range []StaticPropPose{
		{Slot: 0, Kind: PropChair, Facing: 1}, {Slot: 257, Kind: PropChair, Facing: 1},
		{Slot: 1, Kind: PropUnknown, Facing: 1}, {Slot: 1, Kind: PropChair, Facing: 0},
		{Slot: 1, Kind: PropChair, Facing: 1, Variant: 4}, {Slot: 1, Kind: PropChair, Facing: 1, X: -1},
	} {
		if _, err := placeStaticProps(1, building, []StaticPropPose{bad}); err == nil {
			t.Fatalf("accepted%+v", bad)
		}
	}
	if _, err := placeStaticProps(1, building, []StaticPropPose{valid, valid}); err == nil {
		t.Fatal("accepted duplicate slot")
	}
	if _, err := placeStaticProps(1, building, make([]StaticPropPose, 257)); err == nil {
		t.Fatal("accepted excess roots")
	}
}

func TestKeepFurnitureEnvelopeIsInsidePermanentCapitalWard(t *testing.T) {
	// Four blocks enclose every current prop visual envelope beyond an origin cell.
	// Pin the whole keep, so future room placements cannot escape this authority rule.
	for _, seed := range []int64{0, 1, -42, 123, 8675309} {
		capital := CapitalAt(seed)
		for _, building := range capital.Buildings {
			if building.Kind != BuildingKeep {
				continue
			}
			schematic := SchematicFor(BuildingKeep)
			for facing := Facing(0); facing < 4; facing++ {
				width, depth := schematic.W, schematic.D
				if facing == 1 || facing == 3 {
					width, depth = depth, width
				}
				lo := ChunkOf(building.OriginX-4, building.OriginY, building.OriginZ-4)
				hi := ChunkOf(building.OriginX+int64(width)+4, building.OriginY, building.OriginZ+int64(depth)+4)
				for x := lo.X; x <= hi.X; x++ {
					for z := lo.Z; z <= hi.Z; z++ {
						ward, ok := SettlementWarding(seed, Column{CX: x, CZ: z})
						if !ok || ward.Kind != SettlementCapital {
							t.Fatalf("seed%d facing%d keep column%d,%d escapes capital ward", seed, facing, x, z)
						}
					}
				}
			}
		}
	}
}

//go:embed testdata/static_prop_bounds.tsv
var staticPropBoundsFixture string

func TestStaticPropBoundsMatchSharedRendererFixture(t *testing.T) {
	rows := map[StaticPropKind][]PropBox{}
	names := map[string]StaticPropKind{"BanquetTable": PropBanquetTable, "Chair": PropChair, "Bench": PropBench, "Throne": PropThrone, "Bookcase": PropBookcase, "Desk": PropDesk, "Counter": PropCounter, "Barrel": PropBarrel, "EquipmentRack": PropEquipmentRack, "CouncilTable": PropCouncilTable}
	for _, line := range strings.Split(staticPropBoundsFixture, "\n") {
		if strings.HasPrefix(line, "#") || strings.TrimSpace(line) == "" {
			continue
		}
		var name string
		var b PropBox
		if n, err := fmt.Sscanf(line, "%s %f %f %f %f %f %f", &name, &b.Min[0], &b.Min[1], &b.Min[2], &b.Max[0], &b.Max[1], &b.Max[2]); err != nil || n != 7 {
			t.Fatalf("invalid shared bounds fixture: %v", err)
		}
		kind, ok := names[name]
		if !ok {
			t.Fatalf("unknown fixture kind %q", name)
		}
		rows[kind] = append(rows[kind], b)
	}
	for kind := PropBanquetTable; kind <= PropTableCandelabrum; kind++ {
		if !slices.Equal(rows[kind], PropCollisionBounds(kind)) {
			t.Fatalf("kind%d server bounds differ from renderer fixture", kind)
		}
	}
}

func TestStaticPropCornersComposeKeepAndAuthoredTurns(t *testing.T) {
	schematic := SchematicFor(BuildingKeep)
	// Independent continuous affine transforms of the whole keep, including the
	// rectangular footprint translation. These do not call rotateCell or Bounds.
	turns := [4][6]float64{
		{1, 0, 0, 1, 0, 0}, {0, -1, 1, 0, float64(schematic.D), 0},
		{-1, 0, 0, -1, float64(schematic.W), float64(schematic.D)},
		{0, 1, -1, 0, 0, float64(schematic.W)},
	}
	local := PropCollisionBounds(PropThrone)[2] // asymmetric high back, away from origin
	for keepTurn, turn := range turns {
		for authored := uint8(1); authored <= 4; authored++ {
			pose := StaticPropPose{Slot: 1, Kind: PropThrone, X: 20, Y: 7, Z: 11, Facing: authored}
			building := Building{Kind: BuildingKeep, OriginX: -100, OriginY: 20, OriginZ: 200, Facing: Facing(keepTurn)}
			placed, err := placeStaticProps(123, building, []StaticPropPose{pose})
			if err != nil {
				t.Fatal(err)
			}
			want := PropBox{Min: [3]float64{math.Inf(1), math.Inf(1), math.Inf(1)}, Max: [3]float64{math.Inf(-1), math.Inf(-1), math.Inf(-1)}}
			sin, cos := math.Sincos(-float64(authored-1) * math.Pi / 2)
			for _, x := range [2]float64{local.Min[0], local.Max[0]} {
				for _, y := range [2]float64{local.Min[1], local.Max[1]} {
					for _, z := range [2]float64{local.Min[2], local.Max[2]} {
						// First turn the prop about its own cell centre, then the whole keep.
						u := float64(pose.X) + .5 + x*cos + z*sin
						v := float64(pose.Z) + .5 - x*sin + z*cos
						corner := [3]float64{float64(building.OriginX) + turn[0]*u + turn[1]*v + turn[4], float64(building.OriginY) + float64(pose.Y) + y, float64(building.OriginZ) + turn[2]*u + turn[3]*v + turn[5]}
						for axis := range 3 {
							want.Min[axis] = min(want.Min[axis], corner[axis])
							want.Max[axis] = max(want.Max[axis], corner[axis])
						}
					}
				}
			}
			got := placed[0].Bounds(local)
			for axis := range 3 {
				if math.Abs(got.Min[axis]-want.Min[axis]) > 1e-10 || math.Abs(got.Max[axis]-want.Max[axis]) > 1e-10 {
					t.Fatalf("keepturn%d authored%d: got%v want%v", keepTurn, authored, got, want)
				}
			}
		}
	}
}
