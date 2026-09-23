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
	// The terrain's copy is what collision and sight lines read; a terrain that cannot
	// hold one would leave the two views of the same props disagreeing, so it is an
	// error rather than a skipped step.
	cached, ok := s.terrain.(*CacheTerrain)
	if !ok {
		return fmt.Errorf("game: dungeon lights: terrain %T cannot hold a prop index", s.terrain)
	}
	s.staticProps = index
	cached.staticProps = index
	return nil
}
