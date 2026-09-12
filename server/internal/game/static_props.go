package game

import (
	"fmt"
	"math"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

type indexedStaticProp struct {
	state              protocol.StaticPropState
	visual             box
	solids             []box
	minChunk, maxChunk world.Coord
	materialised       bool
}

// Bounds and chunk associations are immutable after construction, outside sim.mu.
// Materialisation changes only snapshot availability, never collision authority.
type staticPropIndex struct {
	props  []indexedStaticProp
	chunks map[world.Coord][]uint16
	extent box
}

func newStaticPropIndex(poses []world.PlacedStaticProp) (*staticPropIndex, error) {
	if len(poses) == 0 {
		return nil, nil
	}
	if len(poses) > protocol.MaxStaticProps {
		return nil, fmt.Errorf("game: excess static props")
	}
	index := &staticPropIndex{chunks: make(map[world.Coord][]uint16)}
	ids := make(map[uint64]bool, len(poses))
	for _, pose := range poses {
		if pose.ID == 0 || ids[pose.ID] || pose.Kind < world.PropBanquetTable || pose.Kind > world.PropTableCandelabrum || pose.Facing < 1 || pose.Facing > 4 || pose.Variant > 3 {
			return nil, fmt.Errorf("game: malformed static prop")
		}
		ids[pose.ID] = true
		visual := pose.Bounds(world.PropVisualBounds(pose.Kind))
		for axis := range 3 {
			if visual.Min[axis] < -float64(world.BlockLimit) || visual.Max[axis] >= float64(world.BlockLimit) {
				return nil, fmt.Errorf("game: static prop outside world")
			}
		}
		prop := indexedStaticProp{state: protocol.StaticPropState{PropID: pose.ID, Kind: vnet.StaticPropKind(pose.Kind), Facing: vnet.Facing(pose.Facing), Variant: pose.Variant}, visual: box{min: visual.Min, max: visual.Max}}
		for axis := range 3 {
			prop.state.Origin[axis] = int32(pose.Origin[axis])
		}
		for _, local := range world.PropCollisionBounds(pose.Kind) {
			b := pose.Bounds(local)
			prop.solids = append(prop.solids, box{min: b.Min, max: b.Max})
		}
		prop.minChunk = propChunk(visual.Min)
		prop.maxChunk = propChunk(visual.Max)
		if chunkVolume(prop.minChunk, prop.maxChunk) > 8 {
			return nil, fmt.Errorf("game: static prop spans excessive chunks")
		}
		slot := uint16(len(index.props))
		index.props = append(index.props, prop)
		if slot == 0 {
			index.extent = prop.visual
		} else {
			for axis := range 3 {
				index.extent.min[axis] = min(index.extent.min[axis], visual.Min[axis])
				index.extent.max[axis] = max(index.extent.max[axis], visual.Max[axis])
			}
		}
		for x := prop.minChunk.X; x <= prop.maxChunk.X; x++ {
			for y := prop.minChunk.Y; y <= prop.maxChunk.Y; y++ {
				for z := prop.minChunk.Z; z <= prop.maxChunk.Z; z++ {
					c := world.Coord{X: x, Y: y, Z: z}
					index.chunks[c] = append(index.chunks[c], slot)
				}
			}
		}
	}
	return index, nil
}

func propChunk(point [3]float64) world.Coord {
	return world.Coord{X: int32(math.Floor(point[0] / world.ChunkSize)), Y: int32(math.Floor(point[1] / world.ChunkSize)), Z: int32(math.Floor(point[2] / world.ChunkSize))}
}
func chunkVolume(lo, hi world.Coord) int64 {
	return (int64(hi.X) - int64(lo.X) + 1) * (int64(hi.Y) - int64(lo.Y) + 1) * (int64(hi.Z) - int64(lo.Z) + 1)
}
func propBoxesTouch(a, b box) bool {
	for axis := range 3 {
		if a.max[axis] < b.min[axis] || a.min[axis] > b.max[axis] {
			return false
		}
	}
	return true
}

// Tiny local probes use chunk candidates. Long rays scan at most 256 roots instead
// of traversing an unbounded empty chunk volume. Neither branch allocates.
func (index *staticPropIndex) visit(query box, fn func(*indexedStaticProp) bool) bool {
	if index == nil || !propBoxesTouch(query, index.extent) {
		return false
	}
	for axis := range 3 {
		if math.IsNaN(query.min[axis]) || math.IsNaN(query.max[axis]) {
			return true
		}
		query.min[axis] = max(query.min[axis], index.extent.min[axis])
		query.max[axis] = min(query.max[axis], index.extent.max[axis])
	}
	lo, hi := propChunk(query.min), propChunk(query.max)
	if chunkVolume(lo, hi) > 64 {
		for i := range index.props {
			p := &index.props[i]
			if propBoxesTouch(query, p.visual) && fn(p) {
				return true
			}
		}
		return false
	}
	var seen [protocol.MaxStaticProps]bool
	for x := lo.X; x <= hi.X; x++ {
		for y := lo.Y; y <= hi.Y; y++ {
			for z := lo.Z; z <= hi.Z; z++ {
				for _, slot := range index.chunks[world.Coord{X: x, Y: y, Z: z}] {
					if seen[slot] {
						continue
					}
					seen[slot] = true
					p := &index.props[slot]
					if propBoxesTouch(query, p.visual) && fn(p) {
						return true
					}
				}
			}
		}
	}
	return false
}
func (index *staticPropIndex) overlaps(body box) bool {
	return index.visit(body, func(p *indexedStaticProp) bool {
		for _, solid := range p.solids {
			overlap := true
			for axis := range 3 {
				if body.max[axis] <= solid.min[axis] || body.min[axis] >= solid.max[axis] {
					overlap = false
					break
				}
			}
			if overlap {
				return true
			}
		}
		return false
	})
}
func (index *staticPropIndex) blocksRay(from, to [3]float64) bool {
	if index != nil && (!finiteVec(from) || !finiteVec(to)) {
		return true
	}
	query := box{}
	for axis := range 3 {
		query.min[axis] = min(from[axis], to[axis])
		query.max[axis] = max(from[axis], to[axis])
	}
	return index.visit(query, func(p *indexedStaticProp) bool {
		for _, solid := range p.solids {
			if _, hit := segmentBoxIntersection(from, to, solid); hit {
				return true
			}
		}
		return false
	})
}
func (index *staticPropIndex) materialise(coord world.Coord) {
	if index == nil {
		return
	}
	for _, slot := range index.chunks[coord] {
		index.props[slot].materialised = true
	}
}
func (index *staticPropIndex) visible(viewer world.Coord, distance int32, dst []protocol.StaticPropState) []protocol.StaticPropState {
	if index == nil {
		return dst
	}
	for _, p := range index.props {
		if p.materialised && p.minChunk.X <= viewer.X+distance && p.maxChunk.X >= viewer.X-distance && p.minChunk.Y <= viewer.Y+distance && p.maxChunk.Y >= viewer.Y-distance && p.minChunk.Z <= viewer.Z+distance && p.maxChunk.Z >= viewer.Z-distance {
			dst = append(dst, p.state)
		}
	}
	return dst
}

func (t *CacheTerrain) staticPropOverlap(b box) bool { return t.staticProps.overlaps(b) }
func (t *CacheTerrain) staticPropRay(from, to [3]float64) bool {
	return t.staticProps.blocksRay(from, to)
}

// Only authored furniture is added here. Existing voxel targeting policy is unchanged.
func staticPropsBlockRay(t Terrain, from, to [3]float64) bool {
	provider, ok := t.(interface {
		staticPropRay([3]float64, [3]float64) bool
	})
	return ok && provider.staticPropRay(from, to)
}

// Residents and players share the standing eye offset. A table is not a wall:
// the eye-to-eye line keeps a visible over-counter stall available.
func (p *Player) residentVisiblePastProps(r *resident) bool {
	target := r.pos
	target[1] += ProjectileEyeHeight
	return !staticPropsBlockRay(p.sim.terrain, projectileOriginLocked(p), target)
}

// SafeStaticPropArrival preserves saved positions unless newly authored furniture
// occupies the standing body. The existing world spawn is the only fallback; this
// does not generate terrain or invent a second spawn search. Immutable solids can
// be read before the session publishes its Welcome, outside the simulation lock.
func (s *Sim) SafeStaticPropArrival(pos, fallback [3]float64) ([3]float64, error) {
	if !s.staticProps.overlaps(playerBox(pos)) {
		return pos, nil
	}
	if s.staticProps.overlaps(playerBox(fallback)) {
		return pos, fmt.Errorf("game: arrival and fallback overlap static furniture")
	}
	return fallback, nil
}
