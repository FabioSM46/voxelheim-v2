package world

import (
	"context"
	"errors"
	"slices"
	"sync"
	"testing"
)

// The chunk the delta tests edit, and a voxel inside it that the generator fills with
// stone — the seed puts the surface far above y=0, so the bottom of this chunk is solid.
var (
	deltaCoord = Coord{X: 0, Y: 0, Z: 0}
	deltaLocal = [3]int{5, 6, 7}
)

// allowAnything is the Apply predicate for tests about the mechanism rather than about
// legality. The legality rules live in internal/game, where the decision belongs.
func allowAnything(Block) error { return nil }

// A delta is a layer, so applying it has to change the one voxel it names and leave the
// other 32767 exactly as the generator produced them. Anything wider than that is a delta
// layer that has started rewriting terrain.
func TestADeltaChangesExactlyOneVoxel(t *testing.T) {
	t.Parallel()

	generated := Generate(11, deltaCoord)
	before := slices.Clone(generated.Blocks)

	index := Index(deltaLocal[0], deltaLocal[1], deltaLocal[2])
	want := Snow
	if before[index] == want {
		t.Fatalf("the fixture voxel already holds %d; the test would prove nothing", want)
	}

	deltas := NewDeltas()
	deltas.Record(deltaCoord, index, want)
	deltas.ApplyTo(generated)

	if got := generated.Blocks[index]; got != want {
		t.Errorf("the edited voxel holds %d, want %d", got, want)
	}
	for i := range before {
		if i == index {
			continue
		}
		if generated.Blocks[i] != before[i] {
			t.Fatalf("voxel %d changed from %d to %d; the delta touched a voxel it did not name",
				i, before[i], generated.Blocks[i])
		}
	}
}

// The determinism contract, stated as a test: Generate is a pure function of (seed, coord)
// and the edit layer does not reach into it. That is what lets the Fimbulvetr storm throw
// the deltas away and get the original world back.
func TestEditsNeverReachTheGenerator(t *testing.T) {
	t.Parallel()

	cache := NewCache(11, 1, 8)
	index := Index(deltaLocal[0], deltaLocal[1], deltaLocal[2])

	pristine := slices.Clone(Generate(11, deltaCoord).Blocks)

	if err := cache.Apply(context.Background(), 5, 6, 7, Snow, allowAnything); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	if cache.deltas.CountFor(deltaCoord) != 1 {
		t.Fatalf("the edit was not recorded in the delta layer")
	}

	regenerated := Generate(11, deltaCoord)
	if !slices.Equal(regenerated.Blocks, pristine) {
		t.Error("Generate returned different voxels after an edit; the edit reached the generator")
	}
	if regenerated.Blocks[index] == Snow {
		t.Error("the generated base carries the edit")
	}
}

// Applying the same layer twice is applying it once. The edit path relies on it: a chunk
// evicted after its delta was recorded regenerates with the edit already composed in, and
// the patch that follows writes the same value again.
func TestApplyingTheSameDeltaTwiceIsIdempotent(t *testing.T) {
	t.Parallel()

	deltas := NewDeltas()
	index := Index(1, 2, 3)
	deltas.Record(deltaCoord, index, Dirt)

	first := Generate(11, deltaCoord)
	deltas.ApplyTo(first)
	deltas.ApplyTo(first)

	second := Generate(11, deltaCoord)
	deltas.ApplyTo(second)

	if !slices.Equal(first.Blocks, second.Blocks) {
		t.Error("applying the layer twice differs from applying it once")
	}
}

func TestDeltasOnlyApplyToTheChunkTheyWereRecordedFor(t *testing.T) {
	t.Parallel()

	deltas := NewDeltas()
	index := Index(1, 2, 3)
	deltas.Record(Coord{X: 1, Y: 0, Z: 0}, index, Snow)

	other := Generate(11, Coord{X: 2, Y: 0, Z: 0})
	before := slices.Clone(other.Blocks)
	deltas.ApplyTo(other)

	if !slices.Equal(other.Blocks, before) {
		t.Error("a delta recorded for one chunk was applied to another")
	}
	if got := deltas.CountFor(Coord{X: 2, Y: 0, Z: 0}); got != 0 {
		t.Errorf("CountFor an unedited chunk = %d, want 0", got)
	}
	if got := deltas.Count(); got != 1 {
		t.Errorf("Count = %d, want 1", got)
	}
}

// A player who joins after an edit must be sent the modified chunk rather than the
// generated one, which means the cached payload cannot outlive the voxels it describes.
func TestTheEncodedPayloadChangesWithTheChunk(t *testing.T) {
	t.Parallel()

	cache := NewCache(11, 1, 8)
	ctx := context.Background()

	_, before, err := cache.Get(ctx, deltaCoord)
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	beforeCopy := slices.Clone(before)

	if err := cache.Apply(ctx, 5, 6, 7, Snow, allowAnything); err != nil {
		t.Fatalf("Apply: %v", err)
	}

	chunk, after, err := cache.Get(ctx, deltaCoord)
	if err != nil {
		t.Fatalf("Get after the edit: %v", err)
	}
	if slices.Equal(beforeCopy, after) {
		t.Fatal("the encoded payload is unchanged after an edit; a later joiner would be sent the generated chunk")
	}

	blocks, err := Decode(after)
	if err != nil {
		t.Fatalf("the payload after the edit does not decode: %v", err)
	}
	if !slices.Equal(blocks, chunk.Blocks) {
		t.Error("the payload after the edit does not describe the chunk after the edit")
	}
}

// The immutability rule, which is the whole reason collision can read a voxel without
// taking a lock: an edit publishes a *new* chunk and leaves the old one exactly as it was.
func TestAnEditPublishesANewChunkRatherThanWritingToTheOldOne(t *testing.T) {
	t.Parallel()

	cache := NewCache(11, 1, 8)
	ctx := context.Background()
	index := Index(deltaLocal[0], deltaLocal[1], deltaLocal[2])

	before, _, err := cache.Get(ctx, deltaCoord)
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	wasThere := before.Blocks[index]

	if err := cache.Apply(ctx, 5, 6, 7, Snow, allowAnything); err != nil {
		t.Fatalf("Apply: %v", err)
	}

	after, err := cache.Peek(deltaCoord)
	if err != nil {
		t.Fatalf("Peek: %v", err)
	}
	if after == before {
		t.Fatal("the edit reused the published chunk; every lock-free reader of it is now a data race")
	}
	if got := before.Blocks[index]; got != wasThere {
		t.Errorf("the already-published chunk was written to: voxel holds %d, want the original %d", got, wasThere)
	}
	if got := after.Blocks[index]; got != Snow {
		t.Errorf("the republished chunk holds %d at the edited voxel, want Snow", got)
	}
}

// Eviction throws chunks away; the edit layer is what makes that safe now that a chunk is
// not simply a function of the seed. A chunk that comes back without its edits is a world
// that undoes a player's digging when they walk far enough away.
func TestAnEditSurvivesEvictionAndRegeneration(t *testing.T) {
	t.Parallel()

	// Capacity 1, so touching any other chunk evicts this one.
	cache := NewCache(11, 1, 1)
	ctx := context.Background()
	index := Index(deltaLocal[0], deltaLocal[1], deltaLocal[2])

	if err := cache.Apply(ctx, 5, 6, 7, Snow, allowAnything); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	if _, _, err := cache.Get(ctx, Coord{X: 99, Y: 0, Z: 0}); err != nil {
		t.Fatalf("Get(other): %v", err)
	}
	if _, err := cache.Peek(deltaCoord); !errors.Is(err, ErrNotResident) {
		t.Fatalf("the edited chunk was not evicted (err = %v)", err)
	}

	chunk, runs, err := cache.Get(ctx, deltaCoord)
	if err != nil {
		t.Fatalf("Get after eviction: %v", err)
	}
	if got := chunk.Blocks[index]; got != Snow {
		t.Errorf("the regenerated chunk holds %d at the edited voxel, want Snow", got)
	}

	blocks, err := Decode(runs)
	if err != nil {
		t.Fatalf("the regenerated payload does not decode: %v", err)
	}
	if got := blocks[index]; got != Snow {
		t.Errorf("the regenerated payload holds %d at the edited voxel, want Snow", got)
	}
}

// The legality test travels into the write. A rule evaluated before the call would leave a
// window in which two edits to the same voxel are both told they succeeded, and the delta
// layer keeps only one of the answers they broadcast.
func TestApplyRefusesWhenTheCallersRuleRefuses(t *testing.T) {
	t.Parallel()

	cache := NewCache(11, 1, 8)
	ctx := context.Background()
	index := Index(deltaLocal[0], deltaLocal[1], deltaLocal[2])

	chunk, _, err := cache.Get(ctx, deltaCoord)
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	wasThere := chunk.Blocks[index]

	refused := errors.New("not allowed")
	var saw Block
	err = cache.Apply(ctx, 5, 6, 7, Snow, func(current Block) error {
		saw = current
		return refused
	})
	if !errors.Is(err, refused) {
		t.Fatalf("Apply returned %v, want the rule's own error unwrapped", err)
	}
	if saw != wasThere {
		t.Errorf("the rule was shown block %d, want the %d that is actually there", saw, wasThere)
	}

	if got, err := cache.BlockAt(ctx, 5, 6, 7); err != nil || got != wasThere {
		t.Errorf("the refused edit changed the voxel to %d (err = %v)", got, err)
	}
	if got := cache.deltas.Count(); got != 0 {
		t.Errorf("a refused edit recorded %d deltas", got)
	}
	if got := cache.Revision(); got != 0 {
		t.Errorf("a refused edit advanced the revision to %d", got)
	}
}

// Revision is what a consumer holding a chunk pointer watches. It has to move for an
// accepted edit and stand still for a refused one, or collision either never notices a
// change or re-reads the cache for nothing.
func TestRevisionCountsAcceptedEditsOnly(t *testing.T) {
	t.Parallel()

	cache := NewCache(11, 1, 8)
	ctx := context.Background()

	if got := cache.Revision(); got != 0 {
		t.Fatalf("a new cache is at revision %d", got)
	}
	if err := cache.Apply(ctx, 5, 6, 7, Snow, allowAnything); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	if got := cache.Revision(); got != 1 {
		t.Errorf("Revision = %d after one edit, want 1", got)
	}
	if err := cache.Apply(ctx, 5, 6, 7, Dirt, func(Block) error { return errors.New("no") }); err == nil {
		t.Fatal("the refusing rule was ignored")
	}
	if got := cache.Revision(); got != 1 {
		t.Errorf("Revision = %d after a refused edit, want it to stand still at 1", got)
	}
}

func TestBlockAtReadsThroughTheEditLayer(t *testing.T) {
	t.Parallel()

	cache := NewCache(11, 1, 8)
	ctx := context.Background()

	// A voxel far above the surface, so the generator leaves it as air.
	if got, err := cache.BlockAt(ctx, 5, 1000, 7); err != nil || got != Air {
		t.Fatalf("BlockAt before the edit = %d (err = %v), want Air", got, err)
	}
	if err := cache.Apply(ctx, 5, 1000, 7, Stone, allowAnything); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	if got, err := cache.BlockAt(ctx, 5, 1000, 7); err != nil || got != Stone {
		t.Errorf("BlockAt after the edit = %d (err = %v), want Stone", got, err)
	}
}

// Negative coordinates go through floor division and a positive modulo on the way to a
// chunk and an index. Getting either wrong puts the edit in the neighbouring chunk, or 31
// blocks away inside the right one.
func TestApplyAddressesNegativeCoordinatesCorrectly(t *testing.T) {
	t.Parallel()

	cache := NewCache(11, 1, 8)
	ctx := context.Background()

	if err := cache.Apply(ctx, -1, 1000, -1, Stone, allowAnything); err != nil {
		t.Fatalf("Apply: %v", err)
	}

	// World -1 is local 31 of chunk -1, not local -1 of chunk 0.
	coord := Coord{X: -1, Y: 1000 / ChunkSize, Z: -1}
	if got := cache.deltas.CountFor(coord); got != 1 {
		t.Fatalf("the edit was recorded in %d voxels of chunk %+v, want 1", got, coord)
	}
	chunk, err := cache.Peek(coord)
	if err != nil {
		t.Fatalf("Peek: %v", err)
	}
	if got := chunk.At(ChunkSize-1, Local(1000), ChunkSize-1); got != Stone {
		t.Errorf("the voxel at local (31, %d, 31) holds %d, want Stone", Local(1000), got)
	}
	if got, err := cache.BlockAt(ctx, -1, 1000, -1); err != nil || got != Stone {
		t.Errorf("BlockAt(-1, 1000, -1) = %d (err = %v), want Stone", got, err)
	}
}

// Edits arriving on many goroutines while others read the same chunk. The assertion is
// partly the absence of a race report — this is the test `go test -race` is here for — and
// partly that every accepted edit is still there at the end: a lost update would mean the
// composition and the delta layer had disagreed.
func TestConcurrentEditsAndReadsAgree(t *testing.T) {
	t.Parallel()

	cache := NewCache(11, 4, 64)
	ctx := context.Background()

	// Resident before the readers start, so a Peek miss does not make the read half of
	// this test vacuous.
	if _, _, err := cache.Get(ctx, Coord{X: 0, Y: 31, Z: 0}); err != nil {
		t.Fatalf("Get: %v", err)
	}

	const (
		editors = 8
		perEdit = 16
	)
	stop := make(chan struct{})

	var readers sync.WaitGroup
	for range 4 {
		readers.Add(1)
		go func() {
			defer readers.Done()
			for {
				select {
				case <-stop:
					return
				default:
				}
				chunk, err := cache.Peek(Coord{X: 0, Y: 31, Z: 0})
				if err != nil {
					continue
				}
				// Read every voxel the editors are writing to, through the same accessor
				// collision uses.
				for i := range editors * perEdit {
					_ = chunk.At(i%ChunkSize, 0, i/ChunkSize)
				}
				_ = cache.Revision()
			}
		}()
	}

	var writers sync.WaitGroup
	for e := range editors {
		writers.Add(1)
		go func() {
			defer writers.Done()
			for i := range perEdit {
				index := e*perEdit + i
				// y = 992 is local 0 of chunk y=31, and 992 is far above the surface, so every
				// target starts as air.
				if err := cache.Apply(ctx, int64(index%ChunkSize), 992, int64(index/ChunkSize), Stone, allowAnything); err != nil {
					t.Errorf("Apply(%d): %v", index, err)
					return
				}
			}
		}()
	}
	writers.Wait()
	close(stop)
	readers.Wait()

	if got := cache.Revision(); got != editors*perEdit {
		t.Errorf("Revision = %d, want %d", got, editors*perEdit)
	}
	for index := range editors * perEdit {
		x, z := int64(index%ChunkSize), int64(index/ChunkSize)
		if got, err := cache.BlockAt(ctx, x, 992, z); err != nil || got != Stone {
			t.Fatalf("voxel (%d, 992, %d) holds %d (err = %v), want Stone: an edit was lost", x, z, got, err)
		}
	}
}

func TestPlaceableIncludesBuildingBlocksAndExcludesAirAndOre(t *testing.T) {
	t.Parallel()

	for _, block := range []Block{Stone, Dirt, Grass, Snow, Log, Leaves, Sand, Sandstone, Gravel} {
		if !Placeable(block) {
			t.Errorf("building block %d is not placeable", block)
		}
	}
	// A break is the server's own placement of Air; letting a client ask for it would be a
	// second, unchecked route to breaking.
	if Placeable(Air) {
		t.Error("Air is placeable; a client can break a block by placing air into it")
	}
	// 12 is the first id nothing has been appended at; 99 and 0xFFFF are ids a
	// newer contract might one day issue and this build must refuse today.
	for _, block := range []Block{CoalOre, IronOre, 12, 99, 0xFFFF} {
		if Placeable(block) {
			t.Errorf("ore or unknown block %d is placeable", block)
		}
	}
}

// The half of a restoration that lives in memory. Known is what persistence reads before
// it decides whether a file is worth opening, so a chunk this layer still claims to know
// is a chunk whose stored edits are never read again — which after a storm would mean the
// edits come back at the next restart and not before.
func TestForgetLeavesTheChunkUnknownToTheEditLayer(t *testing.T) {
	t.Parallel()

	deltas := NewDeltas()
	kept := Coord{X: 1}
	forgotten := Coord{X: 2}
	deltas.Record(kept, 0, Snow)
	deltas.Record(forgotten, 0, Snow)
	deltas.Record(forgotten, 1, Dirt)

	deltas.Forget(forgotten)

	if deltas.Known(forgotten) {
		t.Error("the edit layer still knows a chunk it was told to forget")
	}
	if got := deltas.CountFor(forgotten); got != 0 {
		t.Errorf("the forgotten chunk holds %d edits, want none", got)
	}
	if deltas.Snapshot(forgotten) != nil {
		t.Error("a forgotten chunk still snapshots edits for the saver to write")
	}
	if !deltas.Known(kept) || deltas.CountFor(kept) != 1 {
		t.Error("forgetting one chunk disturbed another")
	}
	if got := deltas.Count(); got != 1 {
		t.Errorf("the world holds %d edits after one chunk was forgotten, want 1", got)
	}

	// And composition stops carrying them: a chunk generated after the Forget is the
	// base, unchanged.
	want := Generate(11, forgotten).Blocks[0]
	if want == Snow {
		t.Fatal("the generator already produces Snow at the edited voxel; the test would prove nothing")
	}
	composed := Generate(11, forgotten)
	deltas.ApplyTo(composed)
	if composed.Blocks[0] != want {
		t.Errorf("the composed chunk holds %d at the forgotten edit, want the generated %d", composed.Blocks[0], want)
	}
}

// Forgetting a chunk nobody has edited is not an error and not a special case: the storm
// visits chunks by coordinate and most of them have never been touched.
func TestForgettingAnUneditedChunkIsANoOp(t *testing.T) {
	t.Parallel()

	deltas := NewDeltas()
	deltas.Forget(Coord{X: 7})
	if got := deltas.Count(); got != 0 {
		t.Errorf("the edit layer holds %d edits after forgetting nothing, want 0", got)
	}
}
