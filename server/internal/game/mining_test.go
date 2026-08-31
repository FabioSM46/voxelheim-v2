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
func (w *miningWorld) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

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
	player, err := sim.Join(1, testPlayerID(1), testCharacterName, [3]float32{0.5, 200, 0.5}, testAppearance(), nil, out.deliver)
	if err != nil {
		t.Fatalf("Join: %v", err)
	}
	return sim, player, w, out
}

func activeMine(pos [3]int32, tick uint32) protocol.MineRequest {
	return protocol.MineRequest{Pos: pos, HasPos: true, Active: true, ClientTick: tick}
}

// activeMineWith is the same request naming a slot, for the tool cases.
//
// `activeMine` above leaves it zero, which is a real hotbar slot and holds nothing in
// these tests — so every existing case here goes on mining bare-handed without being
// edited, which is what makes them still the bare-hand specification.
func activeMineWith(pos [3]int32, tick uint32, slot uint8) protocol.MineRequest {
	req := activeMine(pos, tick)
	req.Slot = slot
	return req
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

// stepToHardness advances the simulation to the tick a block of this kind breaks on and
// reports the cost it paid.
//
// **Read from the simulation, never restated.** A test that stepped a hardcoded number of
// ticks is a test that has to be edited every time somebody retunes the table, and — worse —
// one that asserts the number rather than the behaviour. Four of them did, which is how
// raising the table by a factor of four broke three tests that were about races and clocks
// and had no opinion about how long a block takes.
func stepToHardness(t *testing.T, sim *Sim, block world.Block) int {
	t.Helper()

	cost, breakable := sim.hardnessTicks(block, ItemNone)
	if !breakable {
		t.Fatalf("block %d is not breakable, so there is no tick to step to", block)
	}
	for tick := 1; tick <= cost; tick++ {
		sim.Step(uint64(tick))
	}
	return cost
}

func TestHardnessTableOrdersEveryBreakableBlockByHand(t *testing.T) {
	t.Parallel()

	sim, _, _, _ := newMiningPlayer(t, nil)
	blocks := []world.Block{
		world.Leaves, world.Grass, world.Dirt, world.Snow, world.Log, world.Stone, world.CoalOre, world.IronOre,
		world.Sand, world.Gravel, world.Sandstone,
	}
	costs := make(map[world.Block]int, len(blocks))
	for _, block := range blocks {
		cost, ok := sim.hardnessTicks(block, ItemNone)
		if !ok || cost < 1 {
			t.Fatalf("block %d has cost %d, breakable %t", block, cost, ok)
		}
		costs[block] = cost
	}
	// Loose ground first, then soil, then the two compacted rocks, then ore. Gravel
	// and grass cost the same, and so do dirt and snow: the table has more blocks in
	// it than it has distinct hardnesses, and an equality is a decision here rather
	// than a gap in the ordering.
	ordered := costs[world.Leaves] < costs[world.Sand] &&
		costs[world.Sand] < costs[world.Gravel] &&
		costs[world.Gravel] == costs[world.Grass] &&
		costs[world.Grass] < costs[world.Dirt] &&
		costs[world.Dirt] == costs[world.Snow] &&
		costs[world.Snow] < costs[world.Sandstone] &&
		costs[world.Sandstone] < costs[world.Log] &&
		costs[world.Log] < costs[world.Stone] &&
		costs[world.Stone] < costs[world.CoalOre] &&
		costs[world.CoalOre] < costs[world.IronOre]
	if !ordered {
		t.Fatalf("hardness order is %+v", costs)
	}
	// 12 is the first block id nothing has been appended at yet.
	for _, block := range []world.Block{world.Air, 12, 0xffff} {
		if cost, ok := sim.hardnessTicks(block, ItemNone); ok || cost != 0 {
			t.Errorf("block %d has cost %d, breakable %t; want no mining cost", block, cost, ok)
		}
	}
}

// The table is written in seconds and converted once, so the same block takes the same
// time to break whatever rate an operator runs the server at.
//
// **This is the property a table written in ticks silently loses**, and it is worth a test
// of its own rather than an assertion inside the ordering one: a tick count that read
// correctly at 20 Hz would be two thirds of its intended duration at 30, and nothing about
// the game would look wrong — blocks would simply break faster on some servers than on
// others, for no reason anybody could see.
func TestHardnessIsTheSameDurationAtEveryTickRate(t *testing.T) {
	t.Parallel()

	for _, rate := range []uint8{10, 20, 30, 64} {
		costs := handMiningTicksFor(rate)
		for block, want := range handMiningTimes {
			cost, ok := costs[block]
			if !ok {
				t.Fatalf("block %d has no cost at %d Hz", block, rate)
			}
			got := time.Duration(cost) * time.Second / time.Duration(rate)
			if drift := got - want; drift > time.Second/time.Duration(rate) || drift < -time.Second/time.Duration(rate) {
				t.Errorf("block %d takes %v at %d Hz, want %v (drift %v, more than one tick)",
					block, got, rate, want, drift)
			}
		}
	}
}

func TestDesertPlantsCarryTheirHandTimesAndAxeClass(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		block world.Block
		want  time.Duration
	}{
		{world.PalmLog, 2400 * time.Millisecond},
		{world.PalmFronds, 400 * time.Millisecond},
		{world.DesertShrub, 400 * time.Millisecond},
	} {
		if got := handMiningTimes[tc.block]; got != tc.want {
			t.Errorf("block %d hand time = %v, want %v", tc.block, got, tc.want)
		}
		if !helpsWith(ItemAxe, tc.block) {
			t.Errorf("the axe does not help with block %d", tc.block)
		}
		if helpsWith(ItemShovel, tc.block) || helpsWith(ItemPickaxe, tc.block) {
			t.Errorf("block %d is assigned to more than the axe", tc.block)
		}
	}
}

func TestPlainsFoliageCarriesItsHandTimesAndAxeClass(t *testing.T) {
	t.Parallel()

	for _, block := range []world.Block{world.BroadLeaves, world.Bush} {
		if got := handMiningTimes[block]; got != 400*time.Millisecond {
			t.Errorf("block %d hand time = %v, want 400ms", block, got)
		}
		if !helpsWith(ItemAxe, block) {
			t.Errorf("the axe does not help with block %d", block)
		}
		if helpsWith(ItemShovel, block) || helpsWith(ItemPickaxe, block) {
			t.Errorf("block %d is assigned to more than the axe", block)
		}
	}
}

// Nothing is breakable by hand in under a quarter of a second, which is the complaint this
// table was retuned to answer: dirt used to go in three tenths and a log in six.
//
// A floor rather than the seven exact numbers, because the numbers are a first guess from
// ratios and will move once somebody has dug for an hour — where the property that they are
// *slow enough to want a tool* is the one this issue exists to establish, and the one #185
// is defined against.
func TestNothingBreaksInstantlyByHand(t *testing.T) {
	t.Parallel()

	const floor = 250 * time.Millisecond
	for block, cost := range handMiningTimes {
		if cost < floor {
			t.Errorf("block %d breaks by hand in %v, under the %v floor", block, cost, floor)
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

	// Read from the simulation rather than restated: the cost is a tuning number and a
	// test that hardcoded it would have to be edited every time somebody retuned it — and
	// would be asserting the number rather than the behaviour.
	cost, breakable := sim.hardnessTicks(world.Leaves, ItemNone)
	if !breakable || cost < 2 {
		t.Fatalf("leaves cost %d ticks, breakable %t; this test needs at least two", cost, breakable)
	}

	sim.Step(1)
	if block, _ := terrain.Block(3, 200, 0); block != world.Leaves {
		t.Fatalf("target broke on tick 1, and leaves cost %d ticks", cost)
	}
	progress := out.progress(t)
	if len(progress) != 1 || progress[0].Progress == 0 {
		t.Fatalf("tick 1 progress = %+v, want one positive frame", progress)
	}

	for tick := 2; tick <= cost; tick++ {
		sim.Step(uint64(tick))
	}
	completion := awaitCompletion(t, player)
	result, err := player.CompleteMining(context.Background(), completion)
	if err != nil {
		t.Fatalf("CompleteMining: %v", err)
	}
	if result.Block != world.Air {
		t.Fatalf("completion reports block %d, want Air", result.Block)
	}
	if block, _ := terrain.Block(3, 200, 0); block != world.Air {
		t.Fatalf("target holds block %d after hardness tick %d, want Air", block, cost)
	}
	if got := len(out.progress(t)); got != cost-1 {
		t.Fatalf("completion emitted progress: got %d frames, want the %d before the break", got, cost-1)
	}
}

func TestOnlyRewardedBlocksAwardExperienceAfterACompletedBreak(t *testing.T) {
	t.Parallel()

	for name, tc := range map[string]struct {
		block world.Block
		want  uint32
	}{
		"a log": {block: world.Log, want: 2},
		"dirt":  {block: world.Dirt, want: 0},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			target := [3]int32{3, 200, 0}
			sim, player, _, _ := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(target): tc.block})
			if err := player.Mine(activeMine(target, 1), true); err != nil {
				t.Fatalf("Mine: %v", err)
			}
			cost, breakable := sim.hardnessTicks(tc.block, ItemNone)
			if !breakable {
				t.Fatalf("block %d is not breakable", tc.block)
			}
			for tick := 1; tick <= cost; tick++ {
				if tick > 1 {
					if err := player.Mine(activeMine(target, uint32(tick)), true); err != nil {
						t.Fatalf("mining refresh %d: %v", tick, err)
					}
				}
				sim.Step(uint64(tick))
			}
			completion := awaitCompletion(t, player)
			if _, err := player.CompleteMining(context.Background(), completion); err != nil {
				t.Fatalf("CompleteMining: %v", err)
			}
			if got := experienceOf(player); got != tc.want {
				t.Errorf("breaking %d awarded %d experience, want %d", tc.block, got, tc.want)
			}
		})
	}
}

func TestAChangedBlockAfterHardnessPaymentSendsExactlyOneReset(t *testing.T) {
	t.Parallel()

	target := [3]int32{3, 200, 0}
	sim, player, terrain, out := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(target): world.Leaves})
	if err := player.Mine(activeMine(target, 1), true); err != nil {
		t.Fatalf("Mine: %v", err)
	}
	cost := stepToHardness(t, sim, world.Leaves)
	completion := awaitCompletion(t, player)
	terrain.set(target, world.Dirt)

	if _, err := player.CompleteMining(context.Background(), completion); !errors.Is(err, ErrMiningTargetChanged) {
		t.Fatalf("CompleteMining returned %v, want ErrMiningTargetChanged", err)
	}
	// One positive frame per tick paid, and then the single zero the reset carries.
	progress := out.progress(t)
	if len(progress) != cost || progress[0].Progress == 0 || progress[len(progress)-1].Progress != 0 {
		t.Fatalf("completion race progress = %+v, want positive then one zero", progress)
	}
	if _, err := player.CompleteMining(context.Background(), completion); err == nil {
		t.Fatal("the same completion was accepted twice")
	}
	if got := len(out.progress(t)); got != cost {
		t.Fatalf("replayed completion emitted a second reset: got %d frames, want %d", got, cost)
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

	cost, breakable := sim.hardnessTicks(world.Leaves, ItemNone)
	if !breakable {
		t.Fatal("leaves are not breakable, so there is no hardness tick to complete on")
	}

	ctx, cancel := context.WithCancel(context.Background())
	clock := &miningClock{}
	loop, err := NewLoop(DefaultTickRate, clock, slog.New(slog.NewTextHandler(io.Discard, nil)), func(tick uint64) {
		if tick == uint64(cost) {
			if block, _ := terrain.Block(3, 200, 0); block != world.Leaves {
				t.Errorf("target changed before hardness tick: block %d", block)
			}
		}
		sim.Step(tick)
		if tick == uint64(cost) {
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
	cost, breakable := sim.hardnessTicks(world.Stone, ItemNone)
	if !breakable {
		t.Fatal("stone is not breakable, so there is no progress fraction to spam against")
	}
	const requests = 20
	for tick := uint32(1); tick <= requests; tick++ {
		if err := player.Mine(activeMine(target, tick), true); err != nil {
			t.Fatalf("request %d: %v", tick, err)
		}
	}
	sim.Step(1)

	progress := out.progress(t)
	if len(progress) != 1 {
		t.Fatalf("%d requests before one tick emitted %d progress frames, want 1", requests, len(progress))
	}
	// One tick of a cost read from the simulation, not a fraction restated here.
	want := uint8(255 / cost)
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
	first, err := sim.Join(1, testPlayerID(1), testCharacterName, [3]float32{0.5, 200, 0.5}, testAppearance(), nil, firstOut.deliver)
	if err != nil {
		t.Fatalf("Join first: %v", err)
	}
	second, err := sim.Join(2, testPlayerID(2), testCharacterName, [3]float32{0.5, 200, 0.5}, testAppearance(), nil, secondOut.deliver)
	if err != nil {
		t.Fatalf("Join second: %v", err)
	}
	late, err := sim.Join(3, testPlayerID(3), testCharacterName, [3]float32{0.5, 200, 0.5}, testAppearance(), nil, lateOut.deliver)
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

func TestBeginLeavingClearsMiningAndProducesNoLaterProgress(t *testing.T) {
	t.Parallel()

	target := [3]int32{3, 200, 0}
	sim, player, _, out := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(target): world.Stone})
	if err := player.Mine(activeMine(target, 1), true); err != nil {
		t.Fatalf("Mine: %v", err)
	}
	sim.Step(1)
	before := len(out.progress(t))
	player.BeginLeaving()
	if err := player.Mine(activeMine(target, 2), true); err == nil {
		t.Fatal("a leaving player started another mine")
	}
	sim.Step(2)
	sim.Step(3)
	sim.mu.Lock()
	active := player.mining != nil || player.mineCompleting || player.mineReset != nil
	sim.mu.Unlock()
	if active {
		t.Fatal("BeginLeaving retained per-session mining state")
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
	first, err := sim.Join(1, testPlayerID(1), testCharacterName, [3]float32{0.5, 200, 0.5}, testAppearance(), nil, firstOut.deliver)
	if err != nil {
		t.Fatalf("Join first: %v", err)
	}
	second, err := sim.Join(2, testPlayerID(2), testCharacterName, [3]float32{0.5, 200, 0.5}, testAppearance(), nil, secondOut.deliver)
	if err != nil {
		t.Fatalf("Join second: %v", err)
	}

	cost, breakable := sim.hardnessTicks(world.Leaves, ItemNone)
	if !breakable {
		t.Fatal("leaves are not breakable, so neither player can pay for one")
	}
	if err := first.Mine(activeMine(target, 1), true); err != nil {
		t.Fatalf("first Mine: %v", err)
	}
	sim.Step(1)
	if err := second.Mine(activeMine(target, 1), true); err != nil {
		t.Fatalf("second Mine: %v", err)
	}
	// The first player keeps paying to the last tick; the second started late.
	for tick := 2; tick <= cost; tick++ {
		if err := first.Mine(activeMine(target, uint32(tick)), true); err != nil {
			t.Fatalf("first refresh at tick %d: %v", tick, err)
		}
		sim.Step(uint64(tick))
	}

	completion := awaitCompletion(t, first)
	if _, err := first.CompleteMining(context.Background(), completion); err != nil {
		t.Fatalf("first completion: %v", err)
	}
	// The tick after the last one paid. It was 3 while leaves cost 2 ticks, and stayed 3
	// when the table moved — a tick number already in the past, which `Sim.Step` does not
	// refuse and which would have gone into the reset frame it produces.
	//
	// **And it is load-bearing rather than tidy**: removing it leaves the second player
	// with seven positive frames and no reset at all, because `CompleteMining` does not
	// emit the loser's reset synchronously — the following tick does.
	sim.Step(uint64(cost + 1))

	// One frame per tick paid except the last: the completion tick emits none, which is
	// the property TestMiningBreaksOnItsHardnessTickAndSendsNoCompletionProgress owns.
	if got := firstOut.progress(t); len(got) != cost-1 || got[0].Progress == 0 {
		t.Fatalf("first player's progress = %+v; want %d frames and no completion frame", got, cost-1)
	}
	got := secondOut.progress(t)
	if len(got) < 2 || got[0].Progress == 0 || got[len(got)-1].Progress != 0 {
		t.Fatalf("second player's progress = %+v, want its own positive ticks then a reset", got)
	}
	for i, frame := range got[:len(got)-1] {
		if frame.Progress == 0 {
			t.Fatalf("second player's frame %d is a reset before the last: %+v", i, got)
		}
	}
}

func TestConcurrentWorldEditAndMiningCompletionChooseOneOutcome(t *testing.T) {
	t.Parallel()

	target := [3]int32{3, 200, 0}
	sim, player, terrain, _ := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(target): world.Leaves})
	if err := player.Mine(activeMine(target, 1), true); err != nil {
		t.Fatalf("Mine: %v", err)
	}
	stepToHardness(t, sim, world.Leaves)
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

// TestTheRightToolIsFourTimesFasterAndTheWrongOneIsABareHand is the tool half of the
// decision #178 opened.
//
// That issue raised every bare-hand time by four, on the argument that the old table had
// been tuned for a player holding the right implement and was attached to the wrong hand.
// This asserts the other half: the right tool divides the cost by [ToolSpeedFactor], and
// **the wrong one is exactly a bare hand** — not a penalty, not a smaller bonus.
//
// Read off `hardnessTicks` rather than restated as tick counts, for the reason
// `stepToHardness` above exists: a test that spelled the numbers would have to be edited
// every time somebody retuned the table, and would be asserting the number instead of the
// behaviour.
func TestTheRightToolIsFourTimesFasterAndTheWrongOneIsABareHand(t *testing.T) {
	t.Parallel()

	sim, _, _, _ := newMiningPlayer(t, nil)
	tools := []ItemID{ItemShovel, ItemPickaxe, ItemAxe}

	for block := range handMiningTimes {
		byHand, breakable := sim.hardnessTicks(block, ItemNone)
		if !breakable {
			t.Fatalf("block %d is in the hand table and is not breakable", block)
		}

		suited := 0
		for _, tool := range tools {
			cost, ok := sim.hardnessTicks(block, tool)
			if !ok {
				t.Fatalf("block %d stopped being breakable while holding %d", block, tool)
			}
			if !helpsWith(tool, block) {
				// The whole of the wrong-tool rule: the same number a bare hand pays.
				if cost != byHand {
					t.Errorf("block %d with the wrong tool %d costs %d, a bare hand costs %d",
						block, tool, cost, byHand)
				}
				continue
			}
			suited++
			want := max((byHand+ToolSpeedFactor-1)/ToolSpeedFactor, 1)
			if cost != want {
				t.Errorf("block %d with tool %d costs %d, want %d (%d by hand, divided by %d)",
					block, tool, cost, want, byHand, ToolSpeedFactor)
			}
			if cost >= byHand {
				t.Errorf("block %d with its own tool costs %d, no better than the hand's %d",
					block, cost, byHand)
			}
			if cost < 1 {
				t.Errorf("block %d with tool %d became free", block, tool)
			}
		}

		// Every block a hand can break has exactly one implement for it. The zero case is
		// the one that would go unnoticed: a block added to handMiningTimes and forgotten
		// in toolFamilies is merely unhelped, and this is what says so out loud.
		if suited != 1 {
			t.Errorf("block %d is suited by %d of the three implements, want exactly 1", block, suited)
		}
	}
}

// TestAnImplementIsNotAWeaponAndCarriesNoDamage pins the zero that says so.
//
// `meleeDamage` made "is this a weapon" a registry question rather than a list of item ids
// in the combat code. A pickaxe is not a bad sword — it is not a sword — and the row's zero
// is the whole of that statement.
func TestAnImplementIsNotAWeaponAndCarriesNoDamage(t *testing.T) {
	t.Parallel()

	for _, tool := range []ItemID{ItemShovel, ItemPickaxe, ItemAxe} {
		definition, registered := itemByID(tool)
		if !registered {
			t.Fatalf("item %d is not in the registry", tool)
		}
		if definition.meleeDamage != 0 {
			t.Errorf("item %d does %d melee damage; an implement is not a weapon", tool, definition.meleeDamage)
		}
		if definition.repairRestore != 0 {
			t.Errorf("item %d restores %d durability; an implement is not a repair kit", tool, definition.repairRestore)
		}
		// It wears out, like the blades do — and tools do not wear from *use*, so what this
		// buys is that dying costs something. See #199.
		if definition.maxDurability != ToolMaxDurability {
			t.Errorf("item %d has %d durability, want %d", tool, definition.maxDurability, ToolMaxDurability)
		}
		if definition.maxStack != 1 {
			t.Errorf("item %d stacks to %d; two implements are two objects with two amounts of wear left",
				tool, definition.maxStack)
		}
	}
}

// TestMiningWithAPickaxeCostsAQuarterOfTheHandOnTheRealPath drives the whole path the
// server actually takes, rather than calling hardnessTicks directly.
//
// **This is the test that would have caught the slot never arriving.** The multiplier
// itself is pinned above; what this asserts is that the number the *request* produces is
// the tool's — the request names a slot, the server reads its own inventory at that slot,
// and the cost it sets is the quickened one. Break any link in that chain and the target
// is set at hand speed with every other test still green.
func TestMiningWithAPickaxeCostsAQuarterOfTheHandOnTheRealPath(t *testing.T) {
	t.Parallel()

	const toolSlot = 3
	target := [3]int32{1, 199, 0}
	sim, player, _, _ := newMiningPlayer(t, map[[3]int64]world.Block{
		{1, 199, 0}: world.Stone,
	})

	byHand, ok := sim.hardnessTicks(world.Stone, ItemNone)
	if !ok {
		t.Fatal("stone is not breakable")
	}

	// Bare hands first: the same request, naming a slot that holds nothing.
	if err := player.Mine(activeMineWith(target, 1, toolSlot), true); err != nil {
		t.Fatalf("Mine with an empty slot: %v", err)
	}
	if got := player.mining.cost; got != byHand {
		t.Errorf("an empty hand set a cost of %d, want the hand's %d", got, byHand)
	}

	// Now put a pickaxe in that slot and ask again.
	player.inventory.mu.Lock()
	player.inventory.slots[toolSlot] = stackOf(ItemPickaxe, 1)
	player.inventory.mu.Unlock()

	// A different target, because the same one is a refresh rather than a new judgement.
	other := [3]int32{2, 199, 0}
	sim.terrain.(*miningWorld).set(other, world.Stone)
	if err := player.Mine(activeMineWith(other, 2, toolSlot), true); err != nil {
		t.Fatalf("Mine with a pickaxe: %v", err)
	}
	want := max((byHand+ToolSpeedFactor-1)/ToolSpeedFactor, 1)
	if got := player.mining.cost; got != want {
		t.Errorf("a pickaxe set a cost of %d, want %d (%d by hand)", got, want, byHand)
	}

	// And the wrong implement is the hand again, from the same slot.
	player.inventory.mu.Lock()
	player.inventory.slots[toolSlot] = stackOf(ItemAxe, 1)
	player.inventory.mu.Unlock()

	third := [3]int32{3, 199, 0}
	sim.terrain.(*miningWorld).set(third, world.Stone)
	if err := player.Mine(activeMineWith(third, 3, toolSlot), true); err != nil {
		t.Fatalf("Mine with an axe: %v", err)
	}
	if got := player.mining.cost; got != byHand {
		t.Errorf("an axe on stone set a cost of %d, want the hand's %d", got, byHand)
	}
}

// TestSwitchingToTheRightToolMidBlockAppliesImmediately is the review of #185's finding.
//
// The cost used to be set only when a *new* target was judged, so a player who started on
// stone bare-handed and then selected the pickaxe without releasing the button went on
// paying hand price until they re-targeted — while the client sent the new slot on every
// tick. It is now re-read on every refresh.
//
// Progress is asserted alongside, because keeping it is what makes the change safe in both
// directions rather than a refund.
func TestSwitchingToTheRightToolMidBlockAppliesImmediately(t *testing.T) {
	t.Parallel()

	const toolSlot = 4
	target := [3]int32{1, 199, 0}
	sim, player, _, _ := newMiningPlayer(t, map[[3]int64]world.Block{
		{1, 199, 0}: world.Stone,
	})

	byHand, ok := sim.hardnessTicks(world.Stone, ItemNone)
	if !ok {
		t.Fatal("stone is not breakable")
	}

	// Start bare-handed and pay a few ticks.
	if err := player.Mine(activeMineWith(target, 1, toolSlot), true); err != nil {
		t.Fatalf("Mine bare-handed: %v", err)
	}
	if player.mining.cost != byHand {
		t.Fatalf("started at cost %d, want the hand's %d", player.mining.cost, byHand)
	}
	for tick := uint64(1); tick <= 3; tick++ {
		sim.Step(tick)
	}
	paid := player.mining.progress
	if paid == 0 {
		t.Fatal("three ticks bought no progress, so there is nothing to carry across")
	}

	// Select the pickaxe and refresh the *same* target, which is what a client does on
	// every tick while the button is held.
	player.inventory.mu.Lock()
	player.inventory.slots[toolSlot] = stackOf(ItemPickaxe, 1)
	player.inventory.mu.Unlock()

	if err := player.Mine(activeMineWith(target, 2, toolSlot), true); err != nil {
		t.Fatalf("Mine after switching: %v", err)
	}

	want := max((byHand+ToolSpeedFactor-1)/ToolSpeedFactor, 1)
	if got := player.mining.cost; got != want {
		t.Errorf("after switching to the pickaxe the cost is %d, want %d", got, want)
	}
	if got := player.mining.progress; got != paid {
		t.Errorf("switching tools changed progress from %d to %d; it must be kept", paid, got)
	}

	// And the other direction: putting the pickaxe away costs hand price again, still
	// without a refund.
	player.inventory.mu.Lock()
	player.inventory.slots[toolSlot] = inventoryStack{}
	player.inventory.mu.Unlock()

	if err := player.Mine(activeMineWith(target, 3, toolSlot), true); err != nil {
		t.Fatalf("Mine after putting it away: %v", err)
	}
	if got := player.mining.cost; got != byHand {
		t.Errorf("after putting the tool away the cost is %d, want the hand's %d", got, byHand)
	}
	if got := player.mining.progress; got != paid {
		t.Errorf("putting the tool away changed progress from %d to %d", paid, got)
	}
}

// **Breaking a flower leaves nothing in the hand and nothing on the ground**, and it
// costs what the rest of the plant matter costs. Checked together because each half
// fails silently alone: a drop row would put an unusable item in a pack, an
// experience row would make picking flowers a progression exploit, and a voxel that
// did not become air would leave the flower standing.
func TestBreakingAFlowerLeavesNothingBehind(t *testing.T) {
	t.Parallel()

	// The registry rows are per id, so all three are read; the break runs once,
	// because nothing on that path names a flower by id.
	for _, block := range []world.Block{world.FlowerRed, world.FlowerYellow, world.FlowerBlue} {
		if got := handMiningTimes[block]; got != 300*time.Millisecond {
			t.Errorf("block %d hand time = %v, want 300ms", block, got)
		}
		if !helpsWith(ItemAxe, block) {
			t.Errorf("the axe does not help with block %d", block)
		}
		if helpsWith(ItemShovel, block) || helpsWith(ItemPickaxe, block) {
			t.Errorf("block %d is assigned to more than the axe", block)
		}
		if got := itemDroppedBy(block); got != ItemNone {
			t.Errorf("block %d drops item %d, want nothing", block, got)
		}
	}

	const block = world.FlowerRed
	target := [3]int32{3, 200, 0}
	sim, player, terrain, _ := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(target): block})
	cost, breakable := sim.hardnessTicks(block, ItemNone)
	if !breakable {
		t.Fatalf("block %d is not breakable by hand", block)
	}
	for tick := 1; tick <= cost; tick++ {
		if err := player.Mine(activeMine(target, uint32(tick)), true); err != nil {
			t.Fatalf("mining refresh %d: %v", tick, err)
		}
		sim.Step(uint64(tick))
	}
	completion := awaitCompletion(t, player)
	result, err := player.CompleteMining(context.Background(), completion)
	if err != nil {
		t.Fatalf("CompleteMining: %v", err)
	}
	if result.Block != world.Air {
		t.Errorf("breaking the flower reports block %d, want Air", result.Block)
	}
	if got, _ := terrain.Block(3, 200, 0); got != world.Air {
		t.Errorf("the voxel holds block %d after the flower broke, want Air", got)
	}
	if got := experienceOf(player); got != 0 {
		t.Errorf("breaking the flower awarded %d experience, want 0", got)
	}
	sim.mu.Lock()
	drops := len(sim.drops)
	sim.mu.Unlock()
	if drops != 0 {
		t.Errorf("breaking the flower created %d drop entities, want none", drops)
	}
	if result.Inventory != nil {
		t.Error("breaking the flower reported an inventory change")
	}
}

// The castle's eight materials: a stated hand time each, and exactly one implement.
//
// **The invariant this pins is the one [toolFamilies] names — every block a hand can
// break has exactly one implement for it — and eight blocks added at once is precisely
// when it drifts.** A block put in handMiningTimes and forgotten in toolFamilies is
// merely unhelped, which is the fail-closed direction and is why nothing else would have
// caught it; a block put in two families is four times faster with either, which is the
// direction that is not fail-closed at all.
//
// The times are written out rather than compared as an ordering, because "the wall is
// harder than the roof" is true of numbers that are all wrong together.
func TestTheCastleMaterialsCarryTheirHandTimesAndOneImplement(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		block world.Block
		want  time.Duration
		tool  ItemID
	}{
		{world.BlackBrick, 3500 * time.Millisecond, ItemPickaxe},
		{world.SmoothBlackStone, 3 * time.Second, ItemPickaxe},
		{world.Basalt, 3 * time.Second, ItemPickaxe},
		{world.BlackBrickWorn, 2500 * time.Millisecond, ItemPickaxe},
		{world.SlateTile, 1500 * time.Millisecond, ItemPickaxe},
		{world.DarkGlass, 400 * time.Millisecond, ItemPickaxe},
		{world.DarkTimber, 800 * time.Millisecond, ItemAxe},
		{world.PaleTimber, 800 * time.Millisecond, ItemAxe},
	} {
		if got := handMiningTimes[tc.block]; got != tc.want {
			t.Errorf("block %d hand time = %v, want %v", tc.block, got, tc.want)
		}
		for _, tool := range []ItemID{ItemShovel, ItemPickaxe, ItemAxe} {
			helps := helpsWith(tool, tc.block)
			if tool == tc.tool && !helps {
				t.Errorf("item %d does not help with block %d", tool, tc.block)
			}
			if tool != tc.tool && helps {
				t.Errorf("item %d also helps with block %d", tool, tc.block)
			}
		}
	}

	// The wall is the hardest thing in the game to break by hand, and a wall that has
	// already lost its polish is not. Both are relations the numbers above are chosen
	// for, so a retune that keeps the numbers plausible and loses the point fails here.
	if handMiningTimes[world.BlackBrick] <= handMiningTimes[world.Cobblestone] {
		t.Error("dressed castle brick is no harder than a hut's cobblestone")
	}
	if handMiningTimes[world.BlackBrickWorn] >= handMiningTimes[world.BlackBrick] {
		t.Error("a weathered wall is not softer than an intact one")
	}
	if handMiningTimes[world.SlateTile] <= handMiningTimes[world.Thatch] {
		t.Error("a slate roof comes off no slower than a thatched one")
	}
}
