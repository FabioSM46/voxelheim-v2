package game

import (
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
	"math"
	"testing"
)

// Reframe the actual authoritative catalogue into the same offset/rotation as
// the production movement walkthrough, rather than repeating the authored rows.
func castleFurnitureIndex(t *testing.T, c castleTerrain) *staticPropIndex {
	t.Helper()
	const seed = 0x5eed
	poses, err := world.CapitalStaticProps(seed)
	if err != nil {
		t.Fatal(err)
	}
	var keep world.Building
	for _, b := range world.CapitalAt(seed).Buildings {
		if b.Kind == world.BuildingKeep {
			keep = b
		}
	}
	if keep.Kind != world.BuildingKeep || keep.Facing != 0 {
		t.Fatal("capture frame requires the actual north-facing keep")
	}
	for i := range poses {
		p := &poses[i]
		point := c.point([3]float64{float64(p.Origin[0]-keep.OriginX) + .5, float64(p.Origin[1] - keep.OriginY), float64(p.Origin[2]-keep.OriginZ) + .5})
		p.Origin = [3]int64{int64(math.Floor(point[0])), int64(point[1]), int64(math.Floor(point[2]))}
		p.Facing = 1 + (p.Facing-1+uint8(c.turn))%4
	}
	index, err := newStaticPropIndex(poses)
	if err != nil {
		t.Fatal(err)
	}
	if index == nil {
		t.Fatal("the furnished catalogue must be active")
	}
	return index
}

func TestCastleFurnitureFitsArchitectureAndRestsOnFloors(t *testing.T) {
	for turn := range 4 {
		c := castleTerrain{turn: turn}
		index := castleFurnitureIndex(t, c)
		for _, prop := range index.props {
			for _, solid := range prop.solids {
				if overlaps(c, solid) {
					t.Errorf("turn %d slot %d physical member intersects architecture: %v", turn, prop.state.PropID&511, solid)
				}
				if solid.min[1] == float64(prop.state.Origin[1]) && !overlaps(c, solid.translate(1, -.01)) {
					t.Errorf("turn %d slot %d foot has no support", turn, prop.state.PropID&511)
				}
			}
		}

		for _, prop := range index.props {
			if prop.state.Kind != vnet.StaticPropKindRug && prop.state.Kind != vnet.StaticPropKindRunner {
				continue
			}
			// Remove the declared 0.1 envelope margin, then verify every covered
			// voxel patch is wholly supported. A sparse sample could miss a hole.
			y := float64(prop.state.Origin[1])
			mesh := box{min: prop.visual.min, max: prop.visual.max}
			for _, axis := range []int{0, 2} {
				mesh.min[axis] = math.Round((mesh.min[axis]+.1)*1e8) / 1e8
				mesh.max[axis] = math.Round((mesh.max[axis]-.1)*1e8) / 1e8
			}
			mesh.min[1], mesh.max[1] = y+.014, y+.03
			if overlaps(c, mesh) {
				t.Fatalf("turn %d rug slot %d clips architecture", turn, prop.state.PropID&511)
			}
			for x := int64(math.Floor(mesh.min[0])); x < int64(math.Ceil(mesh.max[0])); x++ {
				for z := int64(math.Floor(mesh.min[2])); z < int64(math.Ceil(mesh.max[2])); z++ {
					block, _ := c.Block(x, int64(y)-1, z)
					bounds, n := world.CollisionBounds(block)
					supported := false
					for _, b := range bounds[:n] {
						if b.Max[1] == 1 && float64(x)+b.Min[0] <= math.Max(mesh.min[0], float64(x)) && float64(x)+b.Max[0] >= math.Min(mesh.max[0], float64(x+1)) && float64(z)+b.Min[2] <= math.Max(mesh.min[2], float64(z)) && float64(z)+b.Max[2] >= math.Min(mesh.max[2], float64(z+1)) {
							supported = true
						}
					}
					if !world.Solid(block) || !supported {
						t.Fatalf("turn %d rug slot %d lacks full floor support at %d,%d", turn, prop.state.PropID&511, x, z)
					}
				}
			}
		}
		for _, prop := range index.props {
			if prop.state.Kind < vnet.StaticPropKindBanner || prop.state.Kind > vnet.StaticPropKindTrophy {
				continue
			}
			if overlaps(c, prop.visual) {
				t.Errorf("wall detail slot %d clips architecture", prop.state.PropID&511)
			}
			axis, sign := 2, 1.0
			switch prop.state.Facing {
			case vnet.FacingEast:
				axis, sign = 0, -1
			case vnet.FacingSouth:
				sign = -1
			case vnet.FacingWest:
				axis = 0
			}
			if !overlaps(c, prop.visual.translate(axis, sign*.01)) {
				t.Errorf("wall detail slot %d lacks attachment", prop.state.PropID&511)
			}
		}
		for i, a := range index.props {
			for _, b := range index.props[i+1:] {
				for _, aa := range a.solids {
					for _, bb := range b.solids {
						if boxesOverlap(aa, bb) {
							t.Errorf("turn %d props %d and %d intersect", turn, a.state.PropID&511, b.state.PropID&511)
						}
					}
				}
			}
		}
	}
}
