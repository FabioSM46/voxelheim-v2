package game

import (
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
	"testing"
)

func castleFixtureBox(p indexedStaticProp) box {
	local := world.PropBox{Min: [3]float64{-.4, 0, -.24}, Max: [3]float64{.4, 1.95, .24}}
	switch p.state.Kind {
	case vnet.StaticPropKindWallSconce:
		local = world.PropBox{Min: [3]float64{-.11, .1, -.07}, Max: [3]float64{.11, .85, .5}}
	case vnet.StaticPropKindTableCandelabrum:
		local.Max[1] = .95
	}
	pose := world.PlacedStaticProp{Origin: [3]int64{int64(p.state.Origin[0]), int64(p.state.Origin[1]), int64(p.state.Origin[2])}, Facing: uint8(p.state.Facing)}
	b := pose.Bounds(local)
	return box{min: b.Min, max: b.Max}
}

func TestCastleCandleFixturesAttachWithoutClipping(t *testing.T) {
	for turn := range 4 {
		c := castleTerrain{turn: turn}
		index := castleFurnitureIndex(t, c)
		for _, p := range index.props {
			if p.state.Kind < vnet.StaticPropKindWallSconce {
				continue
			}
			body := castleFixtureBox(p)
			if overlaps(c, body) || index.overlaps(body) {
				t.Errorf("turn %d fixture slot %d intersects architecture/furniture", turn, p.state.PropID&511)
			}
			if p.state.Kind == vnet.StaticPropKindWallSconce {
				axis, sign := 2, 1.0
				switch p.state.Facing {
				case vnet.FacingEast:
					axis, sign = 0, -1
				case vnet.FacingSouth:
					sign = -1
				case vnet.FacingWest:
					axis = 0
				}
				if !overlaps(c, body.translate(axis, sign*.01)) {
					t.Errorf("turn %d wall fixture %d has no mount", turn, p.state.PropID&511)
				}
			} else {
				support := body
				support.max[1] = support.min[1] + .01
				support = support.translate(1, -.01)
				if !overlaps(c, support) && !index.overlaps(support) {
					t.Errorf("fixture %d floats", p.state.PropID&511)
				}
			}
		}
	}
}
