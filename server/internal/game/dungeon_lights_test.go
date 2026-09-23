package game

import (
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// A fresh instance indexes every sconce the dungeon places, at every rotation, and a
// sconce is streamed once the chunk holding it has been in somebody's view: a viewer
// in that chunk sees it. None of them is solid to a body.
func TestAnInstanceStreamsTheDungeonsSconces(t *testing.T) {
	for seed := int64(0); seed < 4; seed++ {
		manager := instanceTestManager(t, 20, 1)
		manager.mu.Lock()
		raw, err := manager.newSessionLocked(100, seed, InstanceRuin{}, nil, DungeonRoute{})
		manager.mu.Unlock()
		if err != nil {
			t.Fatal(err)
		}
		s := raw.snapshot().Sim
		placed, err := world.InstanceStaticProps(seed)
		if err != nil {
			t.Fatal(err)
		}
		if s.staticProps == nil || len(s.staticProps.props) != len(placed) || len(placed) == 0 {
			t.Fatalf("seed %d: the instance indexed %v of %d sconces", seed, s.staticProps, len(placed))
		}
		cached, ok := s.terrain.(*CacheTerrain)
		if !ok || cached.staticProps != s.staticProps {
			t.Fatalf("seed %d: the terrain does not share the sim's index", seed)
		}
		for _, p := range placed {
			coord := world.ChunkOf(p.Origin[0], p.Origin[1], p.Origin[2])
			s.mu.Lock()
			s.materialiseSettlementsLocked(coord)
			s.mu.Unlock()
			found := false
			for _, state := range s.staticProps.visible(coord, 1, nil) {
				if state.PropID == p.ID {
					found = true
				}
			}
			if !found {
				t.Fatalf("seed %d: sconce %d is not streamed to a viewer in its chunk", seed, p.ID)
			}
			centre := [3]float64{float64(p.Origin[0]) + .5, float64(p.Origin[1]) + .5, float64(p.Origin[2]) + .5}
			if s.staticProps.overlaps(box{min: centre, max: centre}) {
				t.Fatalf("seed %d: sconce %d is solid", seed, p.ID)
			}
		}
	}
}
