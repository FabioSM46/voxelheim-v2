package world

import (
	"fmt"
	"math"
)

// MaxCapitalProps bounds the one capital's immutable catalogue, fixtures included.
const MaxCapitalProps = 256

// StaticPropKind is world vocabulary, mirrored by the append-only wire catalogue.
// World generation never imports protocol or creates game entities.
type StaticPropKind uint8

const (
	PropUnknown StaticPropKind = iota
	PropBanquetTable
	PropChair
	PropBench
	PropThrone
	PropBookcase
	PropDesk
	PropCounter
	PropBarrel
	PropEquipmentRack
	PropCouncilTable
	PropRug
	PropRunner
	PropBanner
	PropShield
	PropTrophy
	PropFeastSetting
	PropWallSconce
	PropFloorCandelabrum
	PropTableCandelabrum
)

// PropBox is one root-local axis-aligned bound, in blocks. Collision uses exact
// members rather than the visual envelope, so table-leg and rack gaps remain open.
type PropBox struct{ Min, Max [3]float64 }

var propSolids = [...][]PropBox{
	PropUnknown: nil,
	PropBanquetTable: {
		{Min: [3]float64{-0.700, 0.860, -2.400}, Max: [3]float64{0.700, 1.000, 2.400}},
		{Min: [3]float64{-0.620, 0.000, -2.240}, Max: [3]float64{-0.440, 0.860, -2.060}},
		{Min: [3]float64{-0.620, 0.000, 2.060}, Max: [3]float64{-0.440, 0.860, 2.240}},
		{Min: [3]float64{0.440, 0.000, -2.240}, Max: [3]float64{0.620, 0.860, -2.060}},
		{Min: [3]float64{0.440, 0.000, 2.060}, Max: [3]float64{0.620, 0.860, 2.240}},
	},
	PropChair: {
		{Min: [3]float64{-0.350, 0.420, -0.380}, Max: [3]float64{0.350, 0.550, 0.380}},
		{Min: [3]float64{-0.350, 0.550, 0.280}, Max: [3]float64{0.350, 1.250, 0.400}},
		{Min: [3]float64{-0.305, 0.000, -0.325}, Max: [3]float64{-0.175, 0.420, -0.195}},
		{Min: [3]float64{-0.305, 0.000, 0.195}, Max: [3]float64{-0.175, 0.420, 0.325}},
		{Min: [3]float64{0.175, 0.000, -0.325}, Max: [3]float64{0.305, 0.420, -0.195}},
		{Min: [3]float64{0.175, 0.000, 0.195}, Max: [3]float64{0.305, 0.420, 0.325}},
	},
	PropBench: {
		{Min: [3]float64{-0.350, 0.430, -1.800}, Max: [3]float64{0.350, 0.560, 1.800}},
		{Min: [3]float64{-0.280, 0.000, -1.400}, Max: [3]float64{0.280, 0.430, -1.200}},
		{Min: [3]float64{-0.280, 0.000, 1.200}, Max: [3]float64{0.280, 0.430, 1.400}},
	},
	PropThrone: {
		{Min: [3]float64{-0.480, 0.000, -0.400}, Max: [3]float64{0.480, 0.500, 0.400}},
		{Min: [3]float64{-0.550, 0.500, -0.480}, Max: [3]float64{0.550, 0.650, 0.480}},
		{Min: [3]float64{-0.550, 0.650, 0.380}, Max: [3]float64{0.550, 2.100, 0.520}},
		{Min: [3]float64{-0.650, 0.650, -0.380}, Max: [3]float64{-0.500, 0.950, 0.500}},
		{Min: [3]float64{0.500, 0.650, -0.380}, Max: [3]float64{0.650, 0.950, 0.500}},
	},
	PropBookcase: {
		{Min: [3]float64{-0.900, 0.000, -0.280}, Max: [3]float64{0.900, 2.300, 0.280}},
	},
	PropDesk: {
		{Min: [3]float64{-0.900, 0.860, -0.450}, Max: [3]float64{0.900, 1.000, 0.450}},
		{Min: [3]float64{-0.800, 0.000, -0.360}, Max: [3]float64{-0.640, 0.860, -0.200}},
		{Min: [3]float64{-0.800, 0.000, 0.200}, Max: [3]float64{-0.640, 0.860, 0.360}},
		{Min: [3]float64{0.640, 0.000, -0.360}, Max: [3]float64{0.800, 0.860, -0.200}},
		{Min: [3]float64{0.640, 0.000, 0.200}, Max: [3]float64{0.800, 0.860, 0.360}},
	},
	PropCounter: {
		{Min: [3]float64{-0.650, 0.000, -1.300}, Max: [3]float64{0.650, 0.880, 1.300}},
		{Min: [3]float64{-0.700, 0.880, -1.400}, Max: [3]float64{0.700, 1.000, 1.400}},
	},
	PropBarrel: {
		{Min: [3]float64{-0.320, 0.000, -0.320}, Max: [3]float64{0.320, 0.200, 0.320}},
		{Min: [3]float64{-0.400, 0.200, -0.400}, Max: [3]float64{0.400, 0.900, 0.400}},
		{Min: [3]float64{-0.320, 0.900, -0.320}, Max: [3]float64{0.320, 1.100, 0.320}},
	},
	PropEquipmentRack: {
		{Min: [3]float64{-0.950, 0.000, -0.250}, Max: [3]float64{-0.800, 1.950, 0.250}},
		{Min: [3]float64{0.800, 0.000, -0.250}, Max: [3]float64{0.950, 1.950, 0.250}},
		{Min: [3]float64{-0.950, 0.500, -0.180}, Max: [3]float64{0.950, 0.650, 0.180}},
		{Min: [3]float64{-0.950, 1.650, -0.180}, Max: [3]float64{0.950, 1.800, 0.180}},
	},
	PropCouncilTable: {
		{Min: [3]float64{-1.200, 0.860, -1.500}, Max: [3]float64{1.200, 1.000, 1.500}},
		{Min: [3]float64{-1.060, 0.000, -1.340}, Max: [3]float64{-0.860, 0.860, -1.140}},
		{Min: [3]float64{-1.060, 0.000, 1.140}, Max: [3]float64{-0.860, 0.860, 1.340}},
		{Min: [3]float64{0.860, 0.000, -1.340}, Max: [3]float64{1.060, 0.860, -1.140}},
		{Min: [3]float64{0.860, 0.000, 1.140}, Max: [3]float64{1.060, 0.860, 1.340}},
	},
	PropRug:              {},
	PropRunner:           {},
	PropBanner:           {},
	PropShield:           {},
	PropTrophy:           {},
	PropFeastSetting:     {},
	PropWallSconce:       {},
	PropFloorCandelabrum: {},
	PropTableCandelabrum: {},
}

// PropCollisionBounds returns immutable local members; variants never alter them.
// Callers must not modify the returned slice. Unknown kinds have no physical shape.
func PropCollisionBounds(kind StaticPropKind) []PropBox {
	if int(kind) >= len(propSolids) {
		return nil
	}
	return propSolids[kind]
}

// PropVisualBounds conservatively encloses decoration as well as the major solids.
// It chooses streaming relevance, never gameplay solidity.
func PropVisualBounds(kind StaticPropKind) PropBox {
	members := PropCollisionBounds(kind)
	if len(members) > 0 {
		bounds := members[0]
		for _, member := range members[1:] {
			for axis := range 3 {
				bounds.Min[axis] = min(bounds.Min[axis], member.Min[axis])
				bounds.Max[axis] = max(bounds.Max[axis], member.Max[axis])
			}
		}
		bounds.Min[0] -= .15
		bounds.Min[2] -= .15
		bounds.Max[0] += .15
		bounds.Max[2] += .15
		bounds.Max[1] += .6
		return bounds
	}
	switch kind {
	case PropRug:
		return PropBox{[3]float64{-1.6, 0, -2.1}, [3]float64{1.6, .1, 2.1}}
	case PropRunner:
		return PropBox{[3]float64{-.8, 0, -2.6}, [3]float64{.8, .1, 2.6}}
	case PropBanner:
		return PropBox{[3]float64{-.8, 0, -.2}, [3]float64{.8, 2.5, .2}}
	case PropShield, PropTrophy:
		return PropBox{[3]float64{-.8, 0, -.6}, [3]float64{.8, 1.8, .6}}
	case PropFeastSetting:
		return PropBox{[3]float64{-.6, 0, -.6}, [3]float64{.6, .7, .6}}
	case PropWallSconce, PropFloorCandelabrum, PropTableCandelabrum:
		return PropBox{[3]float64{-.8, -.2, -.8}, [3]float64{.8, 2.5, .8}}
	default:
		return PropBox{}
	}
}

// StaticPropPose is an authored slot in the keep's unrotated coordinate frame.
// Slot is explicit and unique in 1..256, so inserting a row never renames old props.
// Facing is 1 North (-Z), 2 East (+X), 3 South (+Z), 4 West (-X), not world.Facing.
type StaticPropPose struct {
	Slot            uint16
	Kind            StaticPropKind
	X, Y, Z         int
	Facing, Variant uint8
}

// PlacedStaticProp is immutable world data, not a session entity or a saved structure.
// Its origin is (X+.5,Y,Z+.5), with no supporting-voxel Y adjustment.
type PlacedStaticProp struct {
	ID              uint64
	Kind            StaticPropKind
	Origin          [3]int64
	Facing, Variant uint8
}

// Bounds places one local bound with the exact quarter turn used by the renderer.
func (p PlacedStaticProp) Bounds(local PropBox) PropBox {
	bounds := PropBox{Min: [3]float64{math.Inf(1), local.Min[1], math.Inf(1)}, Max: [3]float64{math.Inf(-1), local.Max[1], math.Inf(-1)}}
	for _, x := range [2]float64{local.Min[0], local.Max[0]} {
		for _, z := range [2]float64{local.Min[2], local.Max[2]} {
			rx, rz := x, z
			switch p.Facing {
			case 2:
				rx, rz = -z, x
			case 3:
				rx, rz = -x, -z
			case 4:
				rx, rz = z, -x
			}
			bounds.Min[0] = min(bounds.Min[0], rx)
			bounds.Min[2] = min(bounds.Min[2], rz)
			bounds.Max[0] = max(bounds.Max[0], rx)
			bounds.Max[2] = max(bounds.Max[2], rz)
		}
	}
	for axis, offset := range [3]float64{float64(p.Origin[0]) + .5, float64(p.Origin[1]), float64(p.Origin[2]) + .5} {
		bounds.Min[axis] += offset
		bounds.Max[axis] += offset
	}
	return bounds
}

// CapitalStaticProps is called once when the overworld simulation is constructed,
// never while stepping a body or streaming a recipient's snapshot. Authored layout
// errors fail startup rather than quietly dropping a solid the player could hit.
func CapitalStaticProps(seed int64) []PlacedStaticProp {
	if len(keepStaticProps) == 0 {
		return nil
	}
	for _, building := range CapitalAt(seed).Buildings {
		if building.Kind == BuildingKeep {
			props, err := placeStaticProps(seed, building, keepStaticProps)
			if err != nil {
				panic(err)
			}
			return props
		}
	}
	panic("world: capital has no keep for static props")
}

func placeStaticProps(seed int64, building Building, poses []StaticPropPose) ([]PlacedStaticProp, error) {
	if building.Kind != BuildingKeep {
		return nil, fmt.Errorf("world: static props belong only to the capital keep")
	}
	if len(poses) > MaxCapitalProps {
		return nil, fmt.Errorf("world: capital exceeds %d static props", MaxCapitalProps)
	}
	s := SchematicFor(building.Kind)
	result := make([]PlacedStaticProp, 0, len(poses))
	var slots [MaxCapitalProps + 1]bool
	// The high55bits name this world's placed building. The low9bits are the explicit
	// slot: hashes can agree between worlds, but two slots in this capital never alias.
	prefix := (HashLattice(seed+0x7F592183+building.OriginY, building.OriginX, building.OriginZ) & ((1 << 55) - 1)) << 9
	for _, pose := range poses {
		if pose.Slot == 0 || int(pose.Slot) > MaxCapitalProps || slots[pose.Slot] {
			return nil, fmt.Errorf("world: duplicate or invalid static prop slot")
		}
		slots[pose.Slot] = true
		if pose.Kind < PropBanquetTable || pose.Kind > PropTableCandelabrum || pose.Facing < 1 || pose.Facing > 4 || pose.Variant > 3 {
			return nil, fmt.Errorf("world: malformed static prop descriptor")
		}
		if pose.X < 0 || pose.X >= s.W || pose.Y < 0 || pose.Y >= s.H || pose.Z < 0 || pose.Z >= s.D {
			return nil, fmt.Errorf("world: static prop origin outside keep")
		}
		x, z := rotateCell(pose.X, pose.Z, s.W, s.D, building.Facing)
		p := PlacedStaticProp{ID: prefix | uint64(pose.Slot), Kind: pose.Kind, Origin: [3]int64{building.OriginX + int64(x), building.OriginY + int64(pose.Y), building.OriginZ + int64(z)}, Facing: 1 + (pose.Facing-1+uint8(building.Facing))%4, Variant: pose.Variant}
		visual := p.Bounds(PropVisualBounds(p.Kind))
		for axis := range 3 {
			if visual.Min[axis] < -float64(BlockLimit) || visual.Max[axis] >= float64(BlockLimit) {
				return nil, fmt.Errorf("world: static prop extent outside world")
			}
		}
		result = append(result, p)
	}
	return result, nil
}
