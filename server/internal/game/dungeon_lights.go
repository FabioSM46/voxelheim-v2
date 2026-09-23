package game

import (
	"fmt"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// indexDungeonLights gives an instance's simulation the dungeon's wall sconces,
// the one light the cave has. They travel as the capital's furniture does — static
// props, materialised with the chunk that holds them and streamed to whoever can see
// that chunk — so a client lights the dungeon from what the server placed and never
// decides where a light stands. A sconce has no solid member, so indexing them changes
// no collision.
//
// NewSim indexes the capital's furniture only for the open world's endless cache; an
// instance's cache is finite, so its session does this instead, once, before the
// simulation is filed.
func (s *Sim) indexDungeonLights(seed int64) error {
	props, err := world.InstanceStaticProps(seed)
	if err != nil {
		return fmt.Errorf("game: dungeon lights: %w", err)
	}
	index, err := newStaticPropIndex(props)
	if err != nil {
		return err
	}
	s.staticProps = index
	if cached, ok := s.terrain.(*CacheTerrain); ok {
		cached.staticProps = index
	}
	return nil
}
