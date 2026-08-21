package game

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"sync"
	"testing"
	"time"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

type miningWorld struct {
	mu       sync.Mutex
	blocks   map[[3]int64]world.Block
	resident bool
}

func newMiningWorld(blocks map[[3]int64]world.Block) *miningWorld {
	if blocks == nil {
		blocks = make(map[[3]int64]world.Block)
	}
	return &miningWorld{blocks: blocks, resident: true}
}

func (w *miningWorld) Block(x, y, z int64) (world.Block, bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if !w.resident {
		return world.Air, false
	}
	return w.blocks[[3]int64{x, y, z}], true
}

func (w *miningWorld) Solid(x, y, z int64) bool {
	// A flat floor holds test players still. Explicit fixture blocks participate in
	// collision too, though every target is kept outside the body.
	if y <= 199 {
		return true
	}
	block, resident := w.Block(x, y, z)
	return !resident || block != world.Air
}

func (w *miningWorld) ApplyGuarded(_ context.Context, x, y, z int64, block world.Block, guard func() error, allow func(world.Block) error) error {
	if guard != nil {
		if err := guard(); err != nil {
			return err
		}
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	key := [3]int64{x, y, z}
	if allow != nil {
		if err := allow(w.blocks[key]); err != nil {
			return err
		}
	}
	w.blocks[key] = block
	return nil
}

func (w *miningWorld) set(pos [3]int32, block world.Block) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.blocks[mineTarget(pos)] = block
}

type miningSink struct {
	mu      sync.Mutex
	frames  [][]byte
	refuses bool
}

func (s *miningSink) deliver(frame []byte) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.refuses {
		return false
	}
	s.frames = append(s.frames, frame)
	return true
}

func (s *miningSink) setRefuses(refuses bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.refuses = refuses
}

func (s *miningSink) progress(t *testing.T) []protocol.MineProgress {
	t.Helper()
	s.mu.Lock()
	defer s.mu.Unlock()

	var progress []protocol.MineProgress
	for _, frame := range s.frames {
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		if envelope.PayloadType() != vnet.PayloadMineProgress {
			continue
		}
		var payload flatbuffers.Table
		if !envelope.Payload(&payload) {
			t.Fatal("MineProgress envelope has no payload")
		}
		var table vnet.MineProgress
		table.Init(payload.Bytes, payload.Pos)
		pos := table.Pos(nil)
		if pos == nil {
			t.Fatal("MineProgress has no position")
		}
		progress = append(progress, protocol.MineProgress{
			Pos:      [3]int32{pos.X(), pos.Y(), pos.Z()},
			Progress: table.Progress(),
		})
	}
	return progress
}

func newMiningPlayer(t *testing.T, blocks map[[3]int64]world.Block) (*Sim, *Player, *miningWorld, *miningSink) {
	t.Helper()
	w := newMiningWorld(blocks)
	sim, err := NewSim(DefaultTickRate, 1, testWorldSeed, w, w, testEntityIDs(), slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	out := &miningSink{}
	player, err := sim.Join(1, testPlayerID(1), [3]float32{0.5, 200, 0.5}, nil, out.deliver)
	if err != nil {
		t.Fatalf("Join: %v", err)
	}
	return sim, player, w, out
}

func activeMine(pos [3]int32, tick uint32) protocol.MineRequest {
	return protocol.MineRequest{Pos: pos, HasPos: true, Active: true, ClientTick: tick}
}

func awaitCompletion(t *testing.T, player *Player) MiningCompletion {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	completion, err := player.NextMining(ctx)
	if err != nil {
		t.Fatalf("NextMining: %v", err)
	}
	return completion
}

func TestHardnessTableOrdersEveryBreakableBlockByHand(t *testing.T) {
	t.Parallel()

	blocks := []world.Block{world.Leaves, world.Grass, world.Dirt, world.Snow, world.Log, world.Stone, world.CoalOre, world.IronOre}
	costs := make(map[world.Block]int, len(blocks))
	for _, block := range blocks {
		cost, ok := hardnessTicks(block)
		if !ok || cost < 1 {
			t.Fatalf("block %d has cost %d, breakable %t", block, cost, ok)
		}
		costs[block] = cost
	}
	ordered := costs[world.Leaves] < costs[world.Grass] &&
		costs[world.Grass] < costs[world.Dirt] &&
		costs[world.Dirt] == costs[world.Snow] &&
		costs[world.Snow] < costs[world.Stone] &&
		costs[world.Stone] < costs[world.CoalOre] &&
		costs[world.CoalOre] < costs[world.IronOre]
	if !ordered {
		t.Fatalf("hardness order is %+v", costs)
	}
	for _, block := range []world.Block{world.Air, 9, 0xffff} {
		if cost, ok := hardnessTicks(block); ok || cost != 0 {
			t.Errorf("block %d has cost %d, breakable %t; want no mining cost", block, cost, ok)
		}
	}
}

func TestMiningBreaksOnItsHardnessTickAndSendsNoCompletionProgress(t *testing.T) {
	t.Parallel()

	target := [3]int32{3, 200, 0}
	sim, player, terrain, out := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(target): world.Leaves})
	if err := player.Mine(activeMine(target, 1), true); err != nil {
		t.Fatalf("Mine: %v", err)
	}

	sim.Step(1)
	if block, _ := terrain.Block(3, 200, 0); block != world.Leaves {
		t.Fatalf("target broke on tick 1, hardness is 2")
	}
	progress := out.progress(t)
	if len(progress) != 1 || progress[0].Progress == 0 {
		t.Fatalf("tick 1 progress = %+v, want one positive frame", progress)
	}

	sim.Step(2)
	completion := awaitCompletion(t, player)
	result, err := player.CompleteMining(context.Background(), completion)
	if err != nil {
		t.Fatalf("CompleteMining: %v", err)
	}
	if result.Block != world.Air {
		t.Fatalf("completion reports block %d, want Air", result.Block)
	}
	if block, _ := terrain.Block(3, 200, 0); block != world.Air {
		t.Fatalf("target holds block %d after hardness tick 2, want Air", block)
	}
	if got := len(out.progress(t)); got != 1 {
		t.Fatalf("completion emitted progress: got %d frames, want the tick-1 frame only", got)
	}
}

func TestAChangedBlockAfterHardnessPaymentSendsExactlyOneReset(t *testing.T) {
	t.Parallel()

	target := [3]int32{3, 200, 0}
	sim, player, terrain, out := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(target): world.Leaves})
	if err := player.Mine(activeMine(target, 1), true); err != nil {
		t.Fatalf("Mine: %v", err)
	}
	sim.Step(1)
	sim.Step(2)
	completion := awaitCompletion(t, player)
	terrain.set(target, world.Dirt)

	if _, err := player.CompleteMining(context.Background(), completion); !errors.Is(err, ErrMiningTargetChanged) {
		t.Fatalf("CompleteMining returned %v, want ErrMiningTargetChanged", err)
	}
	progress := out.progress(t)
	if len(progress) != 2 || progress[0].Progress == 0 || progress[1].Progress != 0 {
		t.Fatalf("completion race progress = %+v, want positive then one zero", progress)
	}
	if _, err := player.CompleteMining(context.Background(), completion); err == nil {
		t.Fatal("the same completion was accepted twice")
	}
	if got := len(out.progress(t)); got != 2 {
		t.Fatalf("replayed completion emitted a second reset: got %d frames", got)
	}
}

type miningClock struct{ now time.Time }

func (c *miningClock) Now() time.Time { return c.now }
func (c *miningClock) SleepUntil(ctx context.Context, deadline time.Time) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	c.now = deadline
	return nil
}

func TestFakeClockLoopCompletesMiningOnHardnessTick(t *testing.T) {
	t.Parallel()

	target := [3]int32{3, 200, 0}
	sim, player, terrain, _ := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(target): world.Leaves})
	if err := player.Mine(activeMine(target, 1), true); err != nil {
		t.Fatalf("Mine: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	clock := &miningClock{}
	loop, err := NewLoop(DefaultTickRate, clock, slog.New(slog.NewTextHandler(io.Discard, nil)), func(tick uint64) {
		if tick == 2 {
			if block, _ := terrain.Block(3, 200, 0); block != world.Leaves {
				t.Errorf("target changed before hardness tick: block %d", block)
			}
		}
		sim.Step(tick)
		if tick == 2 {
			cancel()
		}
	})
	if err != nil {
		t.Fatalf("NewLoop: %v", err)
	}
	if err := loop.Run(ctx); !errors.Is(err, context.Canceled) {
		t.Fatalf("Run: %v", err)
	}
	completion := awaitCompletion(t, player)
	if _, err := player.CompleteMining(context.Background(), completion); err != nil {
		t.Fatalf("CompleteMining: %v", err)
	}
	if block, _ := terrain.Block(3, 200, 0); block != world.Air {
		t.Errorf("fake-clock tick 2 left block %d, want Air", block)
	}
}

func TestRequestSpamAdvancesOnlyOncePerServerTick(t *testing.T) {
	t.Parallel()

	target := [3]int32{3, 200, 0}
	sim, player, _, out := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(target): world.Stone})
	for tick := uint32(1); tick <= 20; tick++ {
		if err := player.Mine(activeMine(target, tick), true); err != nil {
			t.Fatalf("request %d: %v", tick, err)
		}
	}
	sim.Step(1)

	progress := out.progress(t)
	if len(progress) != 1 {
		t.Fatalf("twenty requests before one tick emitted %d progress frames, want 1", len(progress))
	}
	want := uint8(255 / 20)
	if progress[0].Progress != want {
		t.Errorf("one server tick reported %d, want %d", progress[0].Progress, want)
	}
}

func TestClientCausedMiningResetsAreSilent(t *testing.T) {
	t.Parallel()

	first := [3]int32{2, 200, 0}
	second := [3]int32{3, 200, 0}
	sim, player, _, out := newMiningPlayer(t, map[[3]int64]world.Block{
		mineTarget(first): world.Stone, mineTarget(second): world.Stone,
	})
	if err := player.Mine(activeMine(first, 1), true); err != nil {
		t.Fatalf("start: %v", err)
	}
	sim.Step(1)
	if err := player.Mine(activeMine(second, 2), true); err != nil {
		t.Fatalf("change target: %v", err)
	}
	sim.Step(2)
	if err := player.Mine(protocol.MineRequest{Pos: second, HasPos: true, Active: false, ClientTick: 3}, true); err != nil {
		t.Fatalf("cancel: %v", err)
	}
	sim.Step(3)

	progress := out.progress(t)
	if len(progress) != 2 {
		t.Fatalf("target change plus cancellation produced %+v, want one positive frame per active target", progress)
	}
	for _, frame := range progress {
		if frame.Progress == 0 {
			t.Errorf("client-caused reset emitted zero progress: %+v", progress)
		}
	}
	if progress[0].Progress != progress[1].Progress {
		t.Errorf("target change retained paid ticks: progress = %+v", progress)
	}
}

func TestRefusedMiningTargetsAccumulateNothing(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		target  [3]int32
		prepare func(*miningWorld)
	}{
		{name: "air", target: [3]int32{3, 200, 0}},
		{name: "out of reach", target: [3]int32{100, 200, 0}, prepare: func(w *miningWorld) {
			w.set([3]int32{100, 200, 0}, world.Stone)
		}},
		{name: "not resident", target: [3]int32{3, 200, 0}, prepare: func(w *miningWorld) {
			w.set([3]int32{3, 200, 0}, world.Stone)
			w.mu.Lock()
			w.resident = false
			w.mu.Unlock()
		}},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			sim, player, terrain, out := newMiningPlayer(t, nil)
			if test.prepare != nil {
				test.prepare(terrain)
			}
			if err := player.Mine(activeMine(test.target, 1), true); err == nil {
				t.Fatal("Mine accepted an ineligible target")
			}
			for tick := uint64(1); tick <= 5; tick++ {
				sim.Step(tick)
			}
			if got := out.progress(t); len(got) != 0 {
				t.Errorf("refused target produced progress %+v", got)
			}
			sim.mu.Lock()
			active := player.mining != nil || player.mineCompleting || player.mineReset != nil
			sim.mu.Unlock()
			if active {
				t.Error("refused target retained mining state")
			}
		})
	}
}

func TestCacheTerrainMiningReadDoesNotTreatAnEvictedMemoAsResident(t *testing.T) {
	t.Parallel()

	cache := world.NewCache(17, 1, 1)
	first := world.Coord{}
	if _, _, err := cache.Get(context.Background(), first); err != nil {
		t.Fatalf("generate first chunk: %v", err)
	}
	terrain := NewCacheTerrain(cache)
	if _, resident := terrain.Block(0, 0, 0); !resident {
		t.Fatal("freshly generated chunk is not resident")
	}
	second := world.Coord{X: 1}
	if _, _, err := cache.Get(context.Background(), second); err != nil {
		t.Fatalf("generate evicting chunk: %v", err)
	}
	if _, resident := terrain.Block(0, 0, 0); resident {
		t.Fatal("mining read treated an evicted immutable chunk pointer as resident")
	}
}

func TestServerCausedMiningResetsSendExactlyOneZero(t *testing.T) {
	t.Parallel()

	t.Run("block changed", func(t *testing.T) {
		target := [3]int32{3, 200, 0}
		sim, player, terrain, out := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(target): world.Stone})
		if err := player.Mine(activeMine(target, 1), true); err != nil {
			t.Fatalf("Mine: %v", err)
		}
		sim.Step(1)
		terrain.set(target, world.Dirt)
		sim.Step(2)
		sim.Step(3)
		progress := out.progress(t)
		if len(progress) != 2 || progress[0].Progress == 0 || progress[1].Progress != 0 {
			t.Fatalf("block-change progress = %+v, want positive then one zero", progress)
		}
	})

	t.Run("out of reach", func(t *testing.T) {
		target := [3]int32{3, 200, 0}
		sim, player, _, out := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(target): world.Stone})
		if err := player.Mine(activeMine(target, 1), true); err != nil {
			t.Fatalf("Mine: %v", err)
		}
		sim.Step(1)
		sim.mu.Lock()
		player.pos[0] = 100
		sim.mu.Unlock()
		sim.Step(2)
		sim.Step(3)
		progress := out.progress(t)
		if len(progress) != 2 || progress[0].Progress == 0 || progress[1].Progress != 0 {
			t.Fatalf("out-of-reach progress = %+v, want positive then one zero", progress)
		}
	})
}

func TestServerResetSurvivesBackpressureAndPrecedesNewProgress(t *testing.T) {
	t.Parallel()

	target := [3]int32{3, 200, 0}
	sim, player, _, out := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(target): world.Stone})
	if err := player.Mine(activeMine(target, 1), true); err != nil {
		t.Fatalf("Mine: %v", err)
	}
	sim.Step(1)

	// Make the session queue refuse the server-caused reset. It must become durable
	// state rather than disappear like an obsolete positive fraction.
	out.setRefuses(true)
	sim.mu.Lock()
	player.pos[0] = 100
	sim.mu.Unlock()
	sim.Step(2)
	sim.Step(3)
	if progress := out.progress(t); len(progress) != 1 || progress[0].Progress == 0 {
		t.Fatalf("full queue changed delivered progress to %+v, want only tick 1", progress)
	}
	if err := player.Mine(activeMine(target, 2), true); err == nil {
		t.Fatal("a new target started before the pending reset reached the queue")
	}

	// Once room exists, the next tick offers exactly one zero and clears the guard.
	out.setRefuses(false)
	sim.Step(4)
	sim.Step(5)
	progress := out.progress(t)
	if len(progress) != 2 || progress[0].Progress == 0 || progress[1].Progress != 0 {
		t.Fatalf("retried reset progress = %+v, want one positive then one zero", progress)
	}

	sim.mu.Lock()
	player.pos[0] = 0.5
	sim.mu.Unlock()
	if err := player.Mine(activeMine(target, 3), true); err != nil {
		t.Fatalf("new Mine after reset delivery: %v", err)
	}
	sim.Step(6)
	progress = out.progress(t)
	if len(progress) != 3 || progress[2].Progress == 0 {
		t.Fatalf("new progress did not follow the zero in order: %+v", progress)
	}
}

func TestMiningIndexInvalidatesOnlyPlayersWhoTargetedTheEditedVoxel(t *testing.T) {
	t.Parallel()

	firstTarget := [3]int32{2, 200, 0}
	secondTarget := [3]int32{3, 200, 0}
	terrain := newMiningWorld(map[[3]int64]world.Block{
		mineTarget(firstTarget):  world.Stone,
		mineTarget(secondTarget): world.Stone,
	})
	sim, err := NewSim(DefaultTickRate, 1, testWorldSeed, terrain, terrain, testEntityIDs(), slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	firstOut, secondOut, lateOut := &miningSink{}, &miningSink{}, &miningSink{}
	first, err := sim.Join(1, testPlayerID(1), [3]float32{0.5, 200, 0.5}, nil, firstOut.deliver)
	if err != nil {
		t.Fatalf("Join first: %v", err)
	}
	second, err := sim.Join(2, testPlayerID(2), [3]float32{0.5, 200, 0.5}, nil, secondOut.deliver)
	if err != nil {
		t.Fatalf("Join second: %v", err)
	}
	late, err := sim.Join(3, testPlayerID(3), [3]float32{0.5, 200, 0.5}, nil, lateOut.deliver)
	if err != nil {
		t.Fatalf("Join late: %v", err)
	}
	if err := first.Mine(activeMine(firstTarget, 1), true); err != nil {
		t.Fatalf("first Mine: %v", err)
	}
	if err := second.Mine(activeMine(secondTarget, 1), true); err != nil {
		t.Fatalf("second Mine: %v", err)
	}

	// The edit marks the miners present at that moment. A miner that starts on the
	// same block afterwards sampled the edited state and must not inherit the old
	// invalidation; this is the distinction a queued-position scan would lose.
	terrain.set(firstTarget, world.Dirt)
	sim.invalidateMining(firstTarget)
	if err := late.Mine(activeMine(firstTarget, 1), true); err != nil {
		t.Fatalf("late Mine: %v", err)
	}
	sim.Step(1)

	if got := firstOut.progress(t); len(got) != 1 || got[0].Progress != 0 {
		t.Fatalf("miner at edited target got %+v, want one reset", got)
	}
	for name, out := range map[string]*miningSink{"other target": secondOut, "late same target": lateOut} {
		if got := out.progress(t); len(got) != 1 || got[0].Progress == 0 {
			t.Errorf("%s got %+v, want one positive progress frame", name, got)
		}
	}

	sim.mu.Lock()
	firstMiners := len(sim.minersByPos[firstTarget])
	secondMiners := len(sim.minersByPos[secondTarget])
	sim.mu.Unlock()
	if firstMiners != 1 || secondMiners != 1 {
		t.Fatalf("reverse index counts are first=%d second=%d, want 1 and 1", firstMiners, secondMiners)
	}
	if err := second.Mine(protocol.MineRequest{Pos: secondTarget, HasPos: true, Active: false, ClientTick: 2}, true); err != nil {
		t.Fatalf("cancel second: %v", err)
	}
	sim.mu.Lock()
	_, secondStillIndexed := sim.minersByPos[secondTarget]
	sim.mu.Unlock()
	if secondStillIndexed {
		t.Fatal("cancelled miner remained in the reverse index")
	}
}

func TestMiningSilenceExpiresAndMissingTerrainHoldsPaidProgress(t *testing.T) {
	t.Parallel()

	target := [3]int32{3, 200, 0}
	sim, player, terrain, out := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(target): world.Stone})
	if err := player.Mine(activeMine(target, 1), true); err != nil {
		t.Fatalf("Mine: %v", err)
	}
	sim.Step(1)

	terrain.mu.Lock()
	terrain.resident = false
	terrain.mu.Unlock()
	for tick := uint64(2); tick <= 4; tick++ {
		if err := player.Mine(activeMine(target, uint32(tick)), true); err != nil {
			t.Fatalf("refresh while missing: %v", err)
		}
		sim.Step(tick)
	}
	if got := len(out.progress(t)); got != 1 {
		t.Fatalf("missing terrain advanced or reset progress: got %d frames, want 1", got)
	}

	terrain.mu.Lock()
	terrain.resident = true
	terrain.mu.Unlock()
	if err := player.Mine(activeMine(target, 5), true); err != nil {
		t.Fatalf("refresh after resident: %v", err)
	}
	sim.Step(5)
	progress := out.progress(t)
	if len(progress) != 2 || progress[1].Progress <= progress[0].Progress {
		t.Fatalf("restored terrain did not resume held progress: %+v", progress)
	}

	// No more refreshes. The same half-second idle boundary movement uses clears the
	// state silently, before Stone can reach its 20-tick cost.
	for tick := uint64(6); tick <= 6+uint64(sim.idleLimit)+5; tick++ {
		sim.Step(tick)
	}
	progress = out.progress(t)
	for _, frame := range progress {
		if frame.Progress == 0 {
			t.Fatalf("idle expiry emitted a zero frame: %+v", progress)
		}
	}
	if len(progress) >= 20 {
		t.Fatalf("silent client kept mining through Stone hardness: %d progress frames", len(progress))
	}
}

func TestLeavingClearsMiningAndProducesNoLaterProgress(t *testing.T) {
	t.Parallel()

	target := [3]int32{3, 200, 0}
	sim, player, _, out := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(target): world.Stone})
	if err := player.Mine(activeMine(target, 1), true); err != nil {
		t.Fatalf("Mine: %v", err)
	}
	sim.Step(1)
	before := len(out.progress(t))
	sim.Leave(player)
	sim.Step(2)
	sim.Step(3)
	sim.mu.Lock()
	active := player.mining != nil || player.mineCompleting || player.mineReset != nil
	sim.mu.Unlock()
	if active {
		t.Fatal("Leave retained per-session mining state")
	}
	if got := len(out.progress(t)); got != before {
		t.Errorf("session received %d progress frames after Leave, want 0", got-before)
	}
}

func TestTwoPlayersHoldIndependentProgressAndTheFirstCompletionWins(t *testing.T) {
	t.Parallel()

	target := [3]int32{3, 200, 0}
	terrain := newMiningWorld(map[[3]int64]world.Block{mineTarget(target): world.Leaves})
	sim, err := NewSim(DefaultTickRate, 1, testWorldSeed, terrain, terrain, testEntityIDs(), slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	firstOut, secondOut := &miningSink{}, &miningSink{}
	first, err := sim.Join(1, testPlayerID(1), [3]float32{0.5, 200, 0.5}, nil, firstOut.deliver)
	if err != nil {
		t.Fatalf("Join first: %v", err)
	}
	second, err := sim.Join(2, testPlayerID(2), [3]float32{0.5, 200, 0.5}, nil, secondOut.deliver)
	if err != nil {
		t.Fatalf("Join second: %v", err)
	}

	if err := first.Mine(activeMine(target, 1), true); err != nil {
		t.Fatalf("first Mine: %v", err)
	}
	sim.Step(1)
	if err := first.Mine(activeMine(target, 2), true); err != nil {
		t.Fatalf("first refresh: %v", err)
	}
	if err := second.Mine(activeMine(target, 1), true); err != nil {
		t.Fatalf("second Mine: %v", err)
	}
	sim.Step(2)

	completion := awaitCompletion(t, first)
	if _, err := first.CompleteMining(context.Background(), completion); err != nil {
		t.Fatalf("first completion: %v", err)
	}
	sim.Step(3)

	if got := firstOut.progress(t); len(got) != 1 || got[0].Progress == 0 {
		t.Fatalf("first player's progress = %+v; completion must add no frame", got)
	}
	if got := secondOut.progress(t); len(got) != 2 || got[0].Progress == 0 || got[1].Progress != 0 {
		t.Fatalf("second player's progress = %+v, want its own positive tick then a reset", got)
	}
}

func TestConcurrentWorldEditAndMiningCompletionChooseOneOutcome(t *testing.T) {
	t.Parallel()

	target := [3]int32{3, 200, 0}
	sim, player, terrain, _ := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(target): world.Leaves})
	if err := player.Mine(activeMine(target, 1), true); err != nil {
		t.Fatalf("Mine: %v", err)
	}
	sim.Step(1)
	sim.Step(2)
	completion := awaitCompletion(t, player)

	start := make(chan struct{})
	results := make(chan error, 2)
	go func() {
		<-start
		_, err := player.CompleteMining(context.Background(), completion)
		results <- err
	}()
	go func() {
		<-start
		err := terrain.ApplyGuarded(context.Background(), 3, 200, 0, world.Dirt, nil, func(current world.Block) error {
			if current != world.Leaves {
				return errors.New("mining won")
			}
			return nil
		})
		results <- err
	}()
	close(start)
	firstErr, secondErr := <-results, <-results

	block, _ := terrain.Block(3, 200, 0)
	if block != world.Air && block != world.Dirt {
		t.Fatalf("concurrent outcome is block %d, want Air or Dirt", block)
	}
	if firstErr == nil && secondErr == nil {
		t.Fatal("both competing writes reported success")
	}
	if firstErr != nil && secondErr != nil {
		t.Fatalf("both competing writes failed: %v and %v", firstErr, secondErr)
	}
}
