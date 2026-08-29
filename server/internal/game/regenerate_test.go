package game

import (
	"io"
	"log/slog"
	"sync"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

type regenerationFixture struct {
	mu           sync.Mutex
	regenerated  map[world.Coord]bool
	becomesSolid map[world.Coord]map[[3]int64]bool
	calls        []world.Coord
}

func newRegenerationFixture() *regenerationFixture {
	return &regenerationFixture{
		regenerated:  make(map[world.Coord]bool),
		becomesSolid: make(map[world.Coord]map[[3]int64]bool),
	}
}

func (w *regenerationFixture) Regenerate(coord world.Coord) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.calls = append(w.calls, coord)
	w.regenerated[coord] = true
	return nil
}

func (w *regenerationFixture) Block(x, y, z int64) (world.Block, bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	coord := world.ChunkOf(x, y, z)
	if w.regenerated[coord] && w.becomesSolid[coord][[3]int64{x, y, z}] {
		return world.Stone, true
	}
	return world.Air, true
}

func (w *regenerationFixture) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || world.Solid(block)
}

func (w *regenerationFixture) Fluid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return resident && world.Fluid(block)
}

func (w *regenerationFixture) callCount() int {
	w.mu.Lock()
	defer w.mu.Unlock()
	return len(w.calls)
}

func newRegenerationSim(t *testing.T, fixture *regenerationFixture, resend func(world.Coord) int) *Sim {
	t.Helper()
	sim, err := NewSim(DefaultTickRate, 2, testWorldSeed, fixture, refusedEdits{}, testEntityIDs(),
		slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	if err := sim.ConfigureChunkRegeneration(fixture, resend); err != nil {
		t.Fatalf("ConfigureChunkRegeneration: %v", err)
	}
	return sim
}

func TestRegenerationClearsPlayerStateKeepsTheWorldAndLiftsAnEnclosedPlayer(t *testing.T) {
	t.Parallel()

	fixture := newRegenerationFixture()
	target := world.ChunkOf(0, 64, 0)
	kept := world.Coord{X: target.X + 1, Y: target.Y, Z: target.Z}
	other := world.Coord{X: target.X + 2, Y: target.Y, Z: target.Z}
	fixture.becomesSolid[target] = map[[3]int64]bool{{0, 64, 0}: true}

	var resent []world.Coord
	sim := newRegenerationSim(t, fixture, func(coord world.Coord) int {
		resent = append(resent, coord)
		return 1
	})
	player, err := sim.Join(1, testPlayerID(1), testCharacterName,
		[3]float32{0.5, 64, 0.5}, testAppearance(), nil, func([]byte) bool { return true })
	if err != nil {
		t.Fatalf("Join: %v", err)
	}

	playerOwnedID := identity.PlayerID{1}
	sim.mu.Lock()
	sim.structures[10] = &structure{structureID: 10, kind: vnet.StructureKindTent, owner: playerOwnedID, chunk: target}
	sim.structures[11] = &structure{structureID: 11, kind: vnet.StructureKindForge, chunk: target}
	sim.structures[12] = &structure{structureID: 12, kind: vnet.StructureKindTent, owner: playerOwnedID, chunk: kept}
	sim.structures[13] = &structure{structureID: 13, kind: vnet.StructureKindTent, owner: playerOwnedID, chunk: other}

	sim.drops[20] = &itemDrop{entityID: 20, chunk: target}
	sim.drops[21] = &itemDrop{entityID: 21, chunk: kept}
	sim.corpses[30] = &corpse{entityID: 30, chunk: target}
	sim.corpses[31] = &corpse{entityID: 31, chunk: kept}
	sim.mobs[40] = &mob{entityID: 40, chunk: target}
	sim.mobs[41] = &mob{entityID: 41, chunk: kept}
	player.openLootID = 30
	player.lootDirty = true
	player.vel[1] = -12
	player.onGround = true

	sim.RegenerateChunksLocked([]world.Coord{target, kept}, func(column world.Column) bool {
		return column == kept.Column()
	})
	sim.advanceChunkRegenerationLocked()

	_, playerStructure := sim.structures[10]
	_, worldStructure := sim.structures[11]
	_, keptStructure := sim.structures[12]
	_, otherStructure := sim.structures[13]
	_, targetDrop := sim.drops[20]
	_, keptDrop := sim.drops[21]
	_, targetCorpse := sim.corpses[30]
	_, keptCorpse := sim.corpses[31]
	_, targetMob := sim.mobs[40]
	_, keptMob := sim.mobs[41]
	wantY := float64(world.GeneratedColumnTop(testWorldSeed, 0, 0) + world.SpawnClearance)
	gotY, gotVelY, gotGround := player.pos[1], player.vel[1], player.onGround
	openLoot, structuresDirty := player.openLootID, sim.structuresDirty
	closureQueued := len(player.lootClosures) == 1 && player.lootClosures[0] == 30
	sim.mu.Unlock()

	if playerStructure || !worldStructure || !keptStructure || !otherStructure {
		t.Errorf("structures after regeneration: player=%v world=%v kept=%v other=%v; want false,true,true,true",
			playerStructure, worldStructure, keptStructure, otherStructure)
	}
	if targetDrop || !keptDrop || targetCorpse || !keptCorpse || targetMob || !keptMob {
		t.Errorf("entities after regeneration: drop=%v/%v corpse=%v/%v mob=%v/%v; want false/true for each pair",
			targetDrop, keptDrop, targetCorpse, keptCorpse, targetMob, keptMob)
	}
	if !structuresDirty {
		t.Error("destroying the player structure did not mark the persisted camp dirty")
	}
	if openLoot != 0 || !closureQueued {
		t.Errorf("removed corpse left open_loot=%d closure_queued=%v", openLoot, closureQueued)
	}
	if gotY != wantY || gotVelY != 0 || gotGround {
		t.Errorf("enclosed player ended at y=%v vel_y=%v on_ground=%v; want y=%v, 0, false", gotY, gotVelY, gotGround, wantY)
	}
	if got := fixture.callCount(); got != 1 {
		t.Errorf("world regenerated %d chunks, want only the rejected target", got)
	}
	if len(resent) != 1 || resent[0] != target {
		t.Errorf("session repairs = %+v, want only %+v", resent, target)
	}
}

func TestRegenerationExaminesAtMostSixtyFourChunksPerTick(t *testing.T) {
	t.Parallel()

	fixture := newRegenerationFixture()
	resent := 0
	sim := newRegenerationSim(t, fixture, func(world.Coord) int { resent++; return 0 })
	coords := make([]world.Coord, RegenerateChunksPerTick+1)
	for i := range coords {
		coords[i] = world.Coord{X: int32(i)}
	}

	sim.mu.Lock()
	sim.RegenerateChunksLocked(coords, func(world.Column) bool { return false })
	sim.mu.Unlock()
	sim.Step(1)
	if got := fixture.callCount(); got != RegenerateChunksPerTick {
		t.Fatalf("first tick regenerated %d chunks, want %d", got, RegenerateChunksPerTick)
	}
	if resent != RegenerateChunksPerTick {
		t.Fatalf("first tick scheduled %d repairs, want %d", resent, RegenerateChunksPerTick)
	}

	sim.Step(2)
	if got := fixture.callCount(); got != len(coords) {
		t.Errorf("second tick left the pass at %d chunks, want all %d", got, len(coords))
	}
	if resent != len(coords) {
		t.Errorf("second tick left repairs at %d, want %d", resent, len(coords))
	}
}
