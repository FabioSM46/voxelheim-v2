package world

import (
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"io/fs"
	"math"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"
)

const storeSeed = 11

// storeCoord and the voxels below are inside the chunk the delta tests already use: the
// surface for this seed is far above y = 0, so the bottom of the chunk is solid stone and
// an edit to Air is visibly not terrain.
var storeCoord = Coord{X: 0, Y: 0, Z: 0}

func testStore(t *testing.T, dir string, seed int64) *Store {
	t.Helper()

	store, err := OpenStore(dir, seed)
	if err != nil {
		t.Fatalf("OpenStore(%s, %d): %v", dir, seed, err)
	}
	return store
}

// requireEditable fails the test if the generator already produces want at pos, which
// would make every later assertion pass without the store doing anything.
func requireEditable(t *testing.T, seed int64, pos [3]int64, want Block) {
	t.Helper()

	coord := ChunkOf(pos[0], pos[1], pos[2])
	index := Index(Local(pos[0]), Local(pos[1]), Local(pos[2]))
	if got := Generate(seed, coord).Blocks[index]; got == want {
		t.Fatalf("the generated world already holds %d at %v; the test would prove nothing", want, pos)
	}
}

func chunkFiles(t *testing.T, store *Store) []string {
	t.Helper()

	entries, err := os.ReadDir(store.chunkDir)
	if err != nil {
		t.Fatalf("reading %s: %v", store.chunkDir, err)
	}
	names := make([]string, 0, len(entries))
	for _, entry := range entries {
		names = append(names, entry.Name())
	}
	slices.Sort(names)
	return names
}

// The headline promise of the issue: an evening of digging is still there tomorrow.
//
// Two processes, one directory. The first edits and shuts down; the second knows nothing
// but the seed and the path, and has to serve the world the first one left.
func TestARestartServesTheEditedWorld(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	ctx := context.Background()
	edits := map[[3]int64]Block{
		{5, 6, 7}:      Snow,
		{0, 0, 0}:      Air,
		{31, 6, 31}:    Dirt,
		{-1, 1000, -1}: Stone, // negative coordinates, and a chunk high above the surface
	}
	for pos, want := range edits {
		requireEditable(t, storeSeed, pos, want)
	}

	// First run.
	first := NewPersistentCache(testStore(t, dir, storeSeed), 2, 16)
	for pos, want := range edits {
		if err := first.Apply(ctx, pos[0], pos[1], pos[2], want, allowAnything); err != nil {
			t.Fatalf("Apply%v: %v", pos, err)
		}
	}
	if err := first.Flush(); err != nil {
		t.Fatalf("Flush: %v", err)
	}

	// Second run: a new store, a new cache, an empty delta layer.
	second := NewPersistentCache(testStore(t, dir, storeSeed), 2, 16)
	for pos, want := range edits {
		got, err := second.BlockAt(ctx, pos[0], pos[1], pos[2])
		if err != nil {
			t.Fatalf("BlockAt%v after a restart: %v", pos, err)
		}
		if got != want {
			t.Errorf("voxel %v holds %d after a restart, want %d: the edit did not survive", pos, got, want)
		}
	}

	// And the encoded payload a joining client would receive carries them too — a restart
	// that only fixed the collision view would still show every player the old world.
	chunk, encoded, err := second.Get(ctx, storeCoord)
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	decoded, err := Decode(encoded)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if !slices.Equal(decoded, chunk.Blocks) {
		t.Error("the payload a client would be sent disagrees with the reloaded chunk")
	}
	if got := chunk.At(5, 6, 7); got != Snow {
		t.Errorf("the reloaded chunk holds %d at (5,6,7), want Snow", got)
	}
}

// The constraint the Fimbulvetr storm depends on: what is on disk is the *edits*, not the
// world. A stored base would be indistinguishable from terrain, and restoring an
// unprotected chunk would stop being "throw the deltas away".
func TestOnlyTheDeltasAreStoredAndTheBaseStaysGenerated(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	ctx := context.Background()
	pristine := slices.Clone(Generate(storeSeed, storeCoord).Blocks)

	first := NewPersistentCache(testStore(t, dir, storeSeed), 1, 8)
	if err := first.Apply(ctx, 5, 6, 7, Snow, allowAnything); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	if err := first.Flush(); err != nil {
		t.Fatalf("Flush: %v", err)
	}

	store := testStore(t, dir, storeSeed)
	files := chunkFiles(t, store)
	if len(files) != 1 {
		t.Fatalf("the world directory holds %v, want exactly one chunk file", files)
	}

	// One edited voxel costs a few dozen bytes. A stored base would cost at least the
	// 65536 bytes a chunk's voxels occupy, so this bound is the difference between the
	// two designs rather than a tidiness check.
	info, err := os.Stat(filepath.Join(store.chunkDir, files[0]))
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if info.Size() >= ChunkVolume {
		t.Errorf("one edit occupies %d bytes on disk; the generated base is being stored", info.Size())
	}

	// Reloading restores exactly one voxel into the delta layer, and nothing into the
	// generator: drop the layer and the original world is back.
	second := NewPersistentCache(store, 1, 8)
	if _, _, err := second.Get(ctx, storeCoord); err != nil {
		t.Fatalf("Get: %v", err)
	}
	if got := second.deltas.Count(); got != 1 {
		t.Errorf("the reloaded world holds %d edits, want 1: the base came back as deltas", got)
	}
	if !slices.Equal(Generate(storeSeed, storeCoord).Blocks, pristine) {
		t.Error("Generate returns different voxels after a reload; persistence reached the generator")
	}
}

// A chunk nobody touched costs zero bytes, and its absence on load is the ordinary case
// rather than an error. That is what makes a delta-only format small enough to keep one
// file per chunk.
func TestAnUneditedChunkWritesNoFile(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	ctx := context.Background()
	store := testStore(t, dir, storeSeed)
	cache := NewPersistentCache(store, 2, 16)

	for x := int32(0); x < 3; x++ {
		if _, _, err := cache.Get(ctx, Coord{X: x, Y: 0, Z: 0}); err != nil {
			t.Fatalf("Get %d: %v", x, err)
		}
	}
	if err := cache.Flush(); err != nil {
		t.Fatalf("Flush: %v", err)
	}

	if files := chunkFiles(t, store); len(files) != 0 {
		t.Errorf("reading chunks wrote %v; only edits belong on disk", files)
	}

	// And the absence is not an error on the way back in.
	edits, err := store.LoadChunk(storeCoord)
	if err != nil {
		t.Fatalf("LoadChunk of a chunk with no file: %v", err)
	}
	if edits != nil {
		t.Errorf("LoadChunk of a chunk with no file returned %v, want nil", edits)
	}
}

// The seed's twin, and the hole the review on #65 found in it. Only deltas are stored, so
// opening a world replays edits onto a base recomputed from the seed — and the seed alone
// does not pin that base. Change the generator and the indices still all resolve, which is
// the same silent corruption the seed check refuses, arriving by the other road.
func TestAWorldgenChangeRefusesToOpenTheWorld(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	if _, err := OpenStore(dir, 11); err != nil {
		t.Fatalf("OpenStore: %v", err)
	}

	// A world written by a build whose Generate differs from this one's. Forged rather
	// than produced, because WorldgenVersion is a constant: what is under test is that the
	// stored value is compared, not that a second generator exists.
	path := filepath.Join(dir, worldFileName)
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	binary.LittleEndian.PutUint32(data[8:12], WorldgenVersion+1)
	PutChecksum(data)
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatalf("write: %v", err)
	}

	_, err = OpenStore(dir, 11)
	if err == nil {
		t.Fatal("a world written by another generator was opened as this one's")
	}
	if !errors.Is(err, ErrWorldgenMismatch) {
		t.Errorf("error %v is not an ErrWorldgenMismatch", err)
	}
	// The checksum was recomputed, so this must not be reported as corruption: the file is
	// intact and the refusal is about what it says, not about whether it survived the disk.
	if errors.Is(err, ErrCorruptStore) {
		t.Errorf("a well-formed file from another generator was reported as corrupt: %v", err)
	}
	// The operator has to be able to tell both versions from the message alone.
	for _, want := range []string{fmt.Sprint(WorldgenVersion), fmt.Sprint(WorldgenVersion + 1)} {
		if !strings.Contains(err.Error(), want) {
			t.Errorf("the refusal %q does not name version %s", err, want)
		}
	}
}

// The other half of the sweep, and the other half of what a crash can leave. WriteAtomic
// puts its temporary beside the file it replaces, and the world file does not live under
// chunks/ — so a sweep that globbed only the chunk directory left this one forever.
func TestALeftoverBesideTheWorldFileIsSweptToo(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	if _, err := OpenStore(dir, storeSeed); err != nil {
		t.Fatalf("OpenStore: %v", err)
	}

	leftover := filepath.Join(dir, worldFileName+".tmp2718281")
	if err := os.WriteFile(leftover, []byte("half a world file"), 0o600); err != nil {
		t.Fatalf("write: %v", err)
	}

	if _, err := OpenStore(dir, storeSeed); err != nil {
		t.Fatalf("reopening the world: %v", err)
	}
	if _, err := os.Stat(leftover); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("the leftover beside the world file survived reopening (stat: %v)", err)
	}
	// And the world file itself is untouched by the sweep, which is the way this could go
	// wrong: a glob wide enough to catch the leftover must not catch the real file.
	if _, err := os.Stat(filepath.Join(dir, worldFileName)); err != nil {
		t.Errorf("the sweep took the world file with it: %v", err)
	}
}

// A seed that does not match the stored world is a refusal to start. Mixing them would
// not fail: the indices would all resolve, and the result would be one landscape wearing
// another world's digging.
func TestASeedMismatchRefusesToOpenTheWorld(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	if _, err := OpenStore(dir, 11); err != nil {
		t.Fatalf("OpenStore: %v", err)
	}

	_, err := OpenStore(dir, 12)
	if err == nil {
		t.Fatal("a world recorded under seed 11 was opened as seed 12")
	}
	if !errors.Is(err, ErrSeedMismatch) {
		t.Errorf("error %v is not an ErrSeedMismatch", err)
	}
	// The operator has to be able to tell which seed is wrong from the message alone.
	for _, want := range []string{"11", "12"} {
		if !strings.Contains(err.Error(), want) {
			t.Errorf("the refusal %q does not name seed %s", err, want)
		}
	}

	// The matching seed still opens, so the check is a comparison and not a one-shot lock.
	if _, err := OpenStore(dir, 11); err != nil {
		t.Errorf("reopening with the recorded seed failed: %v", err)
	}

	// A seed is an int64 and the flag accepts the whole range, so the recorded one has to
	// survive the round trip at the edges too — a sign lost in the encoding would turn a
	// match into a refusal, or worse, a mismatch into a match.
	for _, seed := range []int64{0, -1, math.MinInt64, math.MaxInt64} {
		dir := t.TempDir()
		if _, err := OpenStore(dir, seed); err != nil {
			t.Fatalf("OpenStore(%d): %v", seed, err)
		}
		if _, err := OpenStore(dir, seed); err != nil {
			t.Errorf("reopening seed %d failed: %v", seed, err)
		}
		if _, err := OpenStore(dir, seed+1); !errors.Is(err, ErrSeedMismatch) {
			t.Errorf("opening a seed-%d world as %d returned %v, want an ErrSeedMismatch", seed, seed+1, err)
		}
	}
}

// The version field earns its bytes only if an unrecognised value is refused. Reading a
// future layout as though it were this one is the failure it exists to prevent.
func TestAnUnknownFormatVersionIsRefused(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	store := testStore(t, dir, storeSeed)
	if err := store.SaveChunk(storeCoord, map[int]Block{Index(1, 2, 3): Snow}); err != nil {
		t.Fatalf("SaveChunk: %v", err)
	}

	path := store.chunkPath(storeCoord)
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	binary.LittleEndian.PutUint32(data[4:8], StoreVersion+1)
	PutChecksum(data) // a well-formed file of a version this build does not know
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatalf("write: %v", err)
	}

	_, err = store.LoadChunk(storeCoord)
	if err == nil {
		t.Fatal("a file from a later format version was read as this one")
	}
	if !errors.Is(err, ErrCorruptStore) {
		t.Errorf("error %v is not an ErrCorruptStore", err)
	}
	if !strings.Contains(err.Error(), "version") {
		t.Errorf("the refusal %q does not mention the version", err)
	}

	// The same must hold for the world file, or a later layout would be read for its seed.
	worldPath := filepath.Join(dir, worldFileName)
	header, err := os.ReadFile(worldPath)
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	binary.LittleEndian.PutUint32(header[4:8], StoreVersion+1)
	PutChecksum(header)
	if err := os.WriteFile(worldPath, header, 0o600); err != nil {
		t.Fatalf("write: %v", err)
	}
	if _, err := OpenStore(dir, storeSeed); err == nil {
		t.Error("a world file from a later format version was opened")
	}
}

// Every way a file can be wrong has to end in a refusal. The one answer that is never
// acceptable is the generated terrain, because that silently discards what a player built
// and then lets them dig the replacement.
func TestACorruptChunkFileIsRefusedRatherThanReadAsTerrain(t *testing.T) {
	t.Parallel()

	valid := func(t *testing.T) (*Store, []byte, string) {
		t.Helper()
		store := testStore(t, t.TempDir(), storeSeed)
		if err := store.SaveChunk(storeCoord, map[int]Block{Index(1, 2, 3): Snow, Index(4, 5, 6): Dirt}); err != nil {
			t.Fatalf("SaveChunk: %v", err)
		}
		path := store.chunkPath(storeCoord)
		data, err := os.ReadFile(path)
		if err != nil {
			t.Fatalf("read: %v", err)
		}
		return store, data, path
	}

	cases := map[string]func([]byte) []byte{
		"truncated mid-record": func(b []byte) []byte { return b[:len(b)-3] },
		"truncated to nothing": func(b []byte) []byte { return b[:4] },
		"a foreign magic number": func(b []byte) []byte {
			copy(b[0:4], "JUNK")
			PutChecksum(b)
			return b
		},
		"a flipped bit in an edit": func(b []byte) []byte {
			b[chunkHeaderSize+1] ^= 0x40 // checksum still says the old value
			return b
		},
		"a record for another chunk": func(b []byte) []byte {
			binary.LittleEndian.PutUint32(b[8:12], 7)
			PutChecksum(b)
			return b
		},
		"a count larger than the file": func(b []byte) []byte {
			binary.LittleEndian.PutUint32(b[20:24], 4096)
			PutChecksum(b)
			return b
		},
		"an edit outside the chunk": func(b []byte) []byte {
			binary.LittleEndian.PutUint32(b[chunkHeaderSize:chunkHeaderSize+4], ChunkVolume)
			PutChecksum(b)
			return b
		},
		"padded with trailing bytes": func(b []byte) []byte {
			return append(b, 0, 0, 0, 0)
		},
	}

	for name, corrupt := range cases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			store, data, path := valid(t)
			if err := os.WriteFile(path, corrupt(slices.Clone(data)), 0o600); err != nil {
				t.Fatalf("write: %v", err)
			}

			if _, err := store.LoadChunk(storeCoord); err == nil {
				t.Error("LoadChunk accepted a corrupt file")
			} else if !errors.Is(err, ErrCorruptStore) {
				t.Errorf("error %v is not an ErrCorruptStore", err)
			}

			// And the refusal has to reach the caller composing the chunk, rather than
			// being swallowed into a chunk of plain terrain.
			cache := NewPersistentCache(store, 1, 8)
			chunk, _, err := cache.Get(context.Background(), storeCoord)
			if err == nil {
				t.Fatalf("Get served chunk %+v from a corrupt world directory", chunk.Coord)
			}
			if !errors.Is(err, ErrCorruptStore) {
				t.Errorf("Get returned %v, which is not an ErrCorruptStore", err)
			}
		})
	}
}

// A file over the largest one this format can produce is refused before it is read. The
// check is about the allocation, not about the parse: deciding how much memory to reserve
// from a number a corrupt file chose is the bug.
func TestAnAbsurdlyLargeChunkFileIsRefusedBeforeItIsRead(t *testing.T) {
	t.Parallel()

	store := testStore(t, t.TempDir(), storeSeed)
	if err := os.WriteFile(store.chunkPath(storeCoord), make([]byte, maxChunkFileSize+1), 0o600); err != nil {
		t.Fatalf("write: %v", err)
	}

	_, err := store.LoadChunk(storeCoord)
	if !errors.Is(err, ErrCorruptStore) {
		t.Errorf("LoadChunk returned %v, want an ErrCorruptStore", err)
	}
}

// Atomicity, stated as the property that distinguishes temp-and-rename from a plain
// write: a save that cannot complete leaves the previous file exactly as it was.
//
// The read-only directory is what tells the two implementations apart. A write in place
// opens the destination itself, which this process owns and may still truncate; creating a
// temporary file next to it cannot even start. Replace WriteAtomic with an os.WriteFile
// and this test fails.
func TestASaveThatCannotCompleteLeavesThePreviousFileIntact(t *testing.T) {
	t.Parallel()

	if os.Geteuid() == 0 {
		t.Skip("root ignores the directory permissions this test relies on")
	}

	store := testStore(t, t.TempDir(), storeSeed)
	index := Index(1, 2, 3)
	if err := store.SaveChunk(storeCoord, map[int]Block{index: Snow}); err != nil {
		t.Fatalf("SaveChunk: %v", err)
	}
	before, err := os.ReadFile(store.chunkPath(storeCoord))
	if err != nil {
		t.Fatalf("read: %v", err)
	}

	if err := os.Chmod(store.chunkDir, 0o555); err != nil {
		t.Fatalf("chmod: %v", err)
	}
	// Restored whatever happens, or t.TempDir cannot clean up after the test.
	t.Cleanup(func() { _ = os.Chmod(store.chunkDir, 0o755) })

	if err := store.SaveChunk(storeCoord, map[int]Block{index: Dirt, Index(9, 9, 9): Stone}); err == nil {
		t.Error("SaveChunk reported success with the world directory read-only")
	}

	after, err := os.ReadFile(store.chunkPath(storeCoord))
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	if !slices.Equal(before, after) {
		t.Error("the failed save changed the file on disk; the write was not atomic")
	}

	if err := os.Chmod(store.chunkDir, 0o755); err != nil {
		t.Fatalf("chmod: %v", err)
	}
	edits, err := store.LoadChunk(storeCoord)
	if err != nil {
		t.Fatalf("LoadChunk after the failed save: %v", err)
	}
	if got := edits[index]; got != Snow {
		t.Errorf("the voxel holds %d after a failed save, want the previous Snow", got)
	}
}

// The other half of atomicity: what a crash leaves behind is inert, and is tidied up on
// the next start rather than accumulating for the life of the world.
func TestALeftoverTemporaryFileIsIgnoredAndSweptUp(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	store := testStore(t, dir, storeSeed)
	index := Index(1, 2, 3)
	if err := store.SaveChunk(storeCoord, map[int]Block{index: Snow}); err != nil {
		t.Fatalf("SaveChunk: %v", err)
	}

	// What a kill between CreateTemp and Rename leaves: a half-written file whose name is
	// not one any reader looks for.
	leftover := store.chunkPath(storeCoord) + ".tmp3141592"
	if err := os.WriteFile(leftover, []byte("half a chunk"), 0o600); err != nil {
		t.Fatalf("write: %v", err)
	}

	edits, err := store.LoadChunk(storeCoord)
	if err != nil {
		t.Fatalf("LoadChunk beside a leftover temporary file: %v", err)
	}
	if got := edits[index]; got != Snow {
		t.Errorf("the voxel holds %d, want Snow", got)
	}

	if _, err := OpenStore(dir, storeSeed); err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	if _, err := os.Stat(leftover); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("the leftover temporary file survived reopening the world (stat: %v)", err)
	}
	if files := chunkFiles(t, store); len(files) != 1 {
		t.Errorf("the chunk directory holds %v, want only the chunk file", files)
	}
}

// A directory this process may write to and search, but not open — which is exactly the
// set of permissions that lets a temporary file be created and renamed into place and
// then fails the directory flush. It is the only way to reach that branch from a test:
// the alternative is a machine to pull the power out of.
//
// Restored before the test ends, because t.TempDir cannot remove what it cannot read.
// Cleanups run last-registered-first, so this one runs before that one.
func unopenableDir(t *testing.T) string {
	t.Helper()

	if os.Geteuid() == 0 {
		t.Skip("root ignores the directory permissions this test relies on")
	}
	if runtime.GOOS == "windows" {
		t.Skip("the directory flush is a POSIX guarantee; syncDir is a documented no-op here")
	}

	dir := filepath.Join(t.TempDir(), "store")
	if err := os.Mkdir(dir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	if err := os.Chmod(dir, 0o300); err != nil {
		t.Fatalf("chmod: %v", err)
	}
	t.Cleanup(func() { _ = os.Chmod(dir, 0o755) })
	return dir
}

// The property temp-and-rename does not have on its own: a rename WriteAtomic reported as
// successful is one a power loss cannot undo. A crash is not something a test can arrange,
// so what is pinned is the mechanism — the parent directory is opened after the rename,
// and a failure to open it reaches the caller instead of being dropped.
//
// Delete the syncDir call from WriteAtomic and this test fails on its first assertion,
// because the write then succeeds; swallow its error and it fails on the same line.
//
// What this deliberately does not pin is the fsync itself. Flushing a directory has no
// effect any process can observe — that is the entire point of it — so the test's reach
// ends at the handle, and the flush is the kernel's half of the contract.
func TestAnAtomicWriteFlushesTheDirectoryEntryItJustCreated(t *testing.T) {
	t.Parallel()

	path := filepath.Join(unopenableDir(t), worldFileName)

	err := WriteAtomic(path, []byte("the durable one"))
	if err == nil {
		t.Fatal("WriteAtomic reported success without being able to flush the directory entry")
	}
	if !errors.Is(err, fs.ErrPermission) {
		t.Errorf("WriteAtomic returned %v, want the directory flush's permission error", err)
	}
	if !strings.Contains(err.Error(), path) {
		t.Errorf("the error is %q, which does not name %s", err, path)
	}
}

// What that reported failure means for the write underneath it: the file is there.
//
// The rename has already happened and the previous contents are already gone, so
// un-renaming would trade a doubt about durability for certain data loss. The error says
// the true thing instead — this may not survive a power loss — and the answer to it is to
// write again, which is what every caller of this function already does with an error.
func TestADirectoryFlushThatFailedStillLeavesTheFileItRenamed(t *testing.T) {
	t.Parallel()

	dir := unopenableDir(t)
	path := filepath.Join(dir, worldFileName)
	want := []byte("landed, and not promised")

	if err := WriteAtomic(path, want); err == nil {
		t.Fatal("WriteAtomic reported success without being able to flush the directory entry")
	}

	// Search permission is enough to reach a name already known, which is why the file
	// can be read out of a directory that cannot be opened.
	got, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("the file WriteAtomic renamed into place cannot be read back: %v", err)
	}
	if !slices.Equal(got, want) {
		t.Errorf("the file holds %q, want the data of the write that reported failure", got)
	}

	// Nor is there a leftover to sweep: the temporary file *became* the destination, so
	// the cleanup that removes it must not run once the rename has succeeded.
	if err := os.Chmod(dir, 0o755); err != nil {
		t.Fatalf("chmod: %v", err)
	}
	leftovers, err := filepath.Glob(filepath.Join(dir, tempFileGlob))
	if err != nil {
		t.Fatalf("glob: %v", err)
	}
	if len(leftovers) != 0 {
		t.Errorf("the failed flush left %v behind", leftovers)
	}
}

// Saving must not reach for the locks the simulation reads under. Holding both and
// watching a save finish anyway is the whole statement: while the world is being written,
// collision keeps reading chunks and the streamer keeps being handed them.
func TestASaveTakesNeitherTheCompositionNorTheEntryLock(t *testing.T) {
	t.Parallel()

	cache := NewPersistentCache(testStore(t, t.TempDir(), storeSeed), 1, 8)
	if err := cache.Apply(context.Background(), 5, 6, 7, Snow, allowAnything); err != nil {
		t.Fatalf("Apply: %v", err)
	}

	// composeMu is held by every composition and every edit; mu is held by Peek, which is
	// the tick loop's only way into the world.
	cache.composeMu.Lock()
	defer cache.composeMu.Unlock()
	cache.mu.Lock()
	defer cache.mu.Unlock()

	done := make(chan error, 1)
	go func() { done <- cache.Flush() }()

	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Flush: %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("Flush waited on a lock the tick loop and the composition path hold; saving would stall the simulation")
	}
}

// Edits landing while the saver runs, which is the state a real server spends its life in.
// Half the assertion is the absence of a race report under -race; the other half is that
// every accepted edit is on disk at the end, because a saver that loses one is worse than
// one that is slow.
func TestEditsAndSavesRunConcurrently(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	ctx, cancel := context.WithCancel(context.Background())
	cache := NewPersistentCache(testStore(t, dir, storeSeed), 4, 64)

	saved := make(chan struct{})
	go func() {
		defer close(saved)
		// Deliberately faster than the edits, so a save is almost always in flight.
		_ = cache.SaveLoop(ctx, time.Millisecond, nil)
	}()

	const (
		editors = 8
		perEdit = 16
	)
	var writers sync.WaitGroup
	for e := range editors {
		writers.Add(1)
		go func() {
			defer writers.Done()
			for i := range perEdit {
				index := e*perEdit + i
				// y = 992 is local 0 of chunk y = 31, far above the surface, so every
				// target starts as air and Stone is visibly an edit.
				if err := cache.Apply(ctx, int64(index%ChunkSize), 992, int64(index/ChunkSize), Stone, allowAnything); err != nil {
					t.Errorf("Apply(%d): %v", index, err)
					return
				}
			}
		}()
	}
	writers.Wait()

	// The shutdown ordering in miniature: stop the loop, wait for it, then flush once.
	cancel()
	<-saved
	if err := cache.Flush(); err != nil {
		t.Fatalf("Flush: %v", err)
	}

	reloaded := NewPersistentCache(testStore(t, dir, storeSeed), 4, 64)
	for index := range editors * perEdit {
		x, z := int64(index%ChunkSize), int64(index/ChunkSize)
		got, err := reloaded.BlockAt(context.Background(), x, 992, z)
		if err != nil {
			t.Fatalf("BlockAt(%d, 992, %d): %v", x, z, err)
		}
		if got != Stone {
			t.Fatalf("voxel (%d, 992, %d) holds %d after a restart, want Stone: an edit was lost", x, z, got)
		}
	}
}

// Restore is the rule hydration leans on when its fast path does not fire: a read that
// lands after an edit — an orphaned generation of an evicted chunk finishing late — must
// not undo it with the older value the last save wrote. Disk is only ever written from
// memory, so where the two disagree memory is the later of the two, always.
func TestRestoreNeverOverwritesAnEditAlreadyInMemory(t *testing.T) {
	t.Parallel()

	index, untouched := Index(5, 6, 7), Index(1, 1, 1)
	deltas := NewDeltas()
	deltas.Record(storeCoord, index, Dirt)

	deltas.Restore(storeCoord, map[int]Block{index: Snow, untouched: Stone})

	snapshot := deltas.Snapshot(storeCoord)
	if got := snapshot[index]; got != Dirt {
		t.Errorf("the voxel holds %d after a restore, want the newer Dirt", got)
	}
	// And a stored edit the memory does not know about is still installed, or a restart
	// would only ever recover the chunks nobody has touched since.
	if got := snapshot[untouched]; got != Stone {
		t.Errorf("the stored voxel holds %d, want Stone: Restore dropped an edit", got)
	}

	// Restore is not Record's bulk form, and an empty restore is not a way to create an
	// entry for a chunk with nothing in it.
	deltas.Restore(Coord{X: 5}, nil)
	if deltas.Known(Coord{X: 5}) {
		t.Error("restoring nothing made the layer claim it knows a chunk")
	}
}

// The property the two guards deliver together, at the level a player would notice: an
// edit recorded but not yet written outlives the reload of the chunk it belongs to.
func TestHydrationDoesNotUndoAnEditThatOutranTheSaver(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	ctx := context.Background()
	index := Index(5, 6, 7)

	first := NewPersistentCache(testStore(t, dir, storeSeed), 1, 8)
	if err := first.Apply(ctx, 5, 6, 7, Snow, allowAnything); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	if err := first.Flush(); err != nil {
		t.Fatalf("Flush: %v", err)
	}

	// A newer edit recorded but not yet written, on a chunk that is not resident — which
	// is exactly the state an eviction between Apply and the next save leaves behind.
	second := NewPersistentCache(testStore(t, dir, storeSeed), 1, 8)
	second.deltas.Record(storeCoord, index, Dirt)

	chunk, _, err := second.Get(ctx, storeCoord)
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if got := chunk.Blocks[index]; got != Dirt {
		t.Errorf("the composed chunk holds %d, want Dirt: the stored value overwrote a newer edit", got)
	}
}

// Eviction and persistence have to compose: a chunk thrown away and regenerated comes back
// with the edits the file holds, however many times it goes round.
func TestAnEvictedChunkComesBackWithItsStoredEdits(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	ctx := context.Background()

	first := NewPersistentCache(testStore(t, dir, storeSeed), 1, 8)
	if err := first.Apply(ctx, 5, 6, 7, Snow, allowAnything); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	if err := first.Flush(); err != nil {
		t.Fatalf("Flush: %v", err)
	}

	// Capacity 1, so every other chunk evicts this one.
	second := NewPersistentCache(testStore(t, dir, storeSeed), 1, 1)
	for round := range 3 {
		chunk, _, err := second.Get(ctx, storeCoord)
		if err != nil {
			t.Fatalf("Get (round %d): %v", round, err)
		}
		if got := chunk.At(5, 6, 7); got != Snow {
			t.Fatalf("round %d: the regenerated chunk holds %d, want Snow", round, got)
		}
		if _, _, err := second.Get(ctx, Coord{X: 9, Y: 0, Z: 0}); err != nil {
			t.Fatalf("Get of the evicting chunk: %v", err)
		}
	}
}

// The same edits always produce the same bytes. It is what lets a save that changes
// nothing leave the file alone, and it keeps Go's randomised map iteration out of the
// format.
func TestTheEncodedFormIsDeterministic(t *testing.T) {
	t.Parallel()

	edits := map[int]Block{Index(9, 9, 9): Stone, Index(1, 2, 3): Snow, Index(0, 0, 0): Air, Index(31, 31, 31): Dirt}

	first, err := encodeChunkFile(storeCoord, edits)
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	for range 8 {
		again, err := encodeChunkFile(storeCoord, edits)
		if err != nil {
			t.Fatalf("encode: %v", err)
		}
		if !slices.Equal(first, again) {
			t.Fatal("encoding the same edits twice produced different bytes")
		}
	}

	decoded, err := decodeChunkFile(storeCoord, first)
	if err != nil {
		t.Fatalf("decode: %v", err)
	}
	if len(decoded) != len(edits) {
		t.Fatalf("decoded %d edits, want %d", len(decoded), len(edits))
	}
	for index, want := range edits {
		if decoded[index] != want {
			t.Errorf("voxel %d decoded as %d, want %d", index, decoded[index], want)
		}
	}
}

// An ephemeral cache must go through every one of these paths without a store, because
// that is how the tests in this repository and a `-world-dir ""` server both run.
func TestAnEphemeralCacheNeitherReadsNorWrites(t *testing.T) {
	t.Parallel()

	cache := NewCache(storeSeed, 1, 8)
	if err := cache.Apply(context.Background(), 5, 6, 7, Snow, allowAnything); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	if err := cache.Flush(); err != nil {
		t.Errorf("Flush on an ephemeral cache: %v", err)
	}
	if err := cache.SaveLoop(context.Background(), time.Millisecond, nil); err != nil {
		t.Errorf("SaveLoop on an ephemeral cache: %v", err)
	}
}

func TestOpenStoreRefusesAnEmptyDirectory(t *testing.T) {
	t.Parallel()

	if _, err := OpenStore("", 1); err == nil {
		t.Error("OpenStore accepted an empty directory name")
	}
}

// A failed write puts the chunk back in the queue rather than dropping it, so the next
// save — and the flush at shutdown — try again.
func TestAFailedSaveIsRetriedRatherThanForgotten(t *testing.T) {
	t.Parallel()

	if os.Geteuid() == 0 {
		t.Skip("root ignores the directory permissions this test relies on")
	}

	store := testStore(t, t.TempDir(), storeSeed)
	cache := NewPersistentCache(store, 1, 8)
	if err := cache.Apply(context.Background(), 5, 6, 7, Snow, allowAnything); err != nil {
		t.Fatalf("Apply: %v", err)
	}

	if err := os.Chmod(store.chunkDir, 0o555); err != nil {
		t.Fatalf("chmod: %v", err)
	}
	t.Cleanup(func() { _ = os.Chmod(store.chunkDir, 0o755) })

	if err := cache.Flush(); err == nil {
		t.Error("Flush reported success with the world directory read-only")
	}

	if err := os.Chmod(store.chunkDir, 0o755); err != nil {
		t.Fatalf("chmod: %v", err)
	}
	if err := cache.Flush(); err != nil {
		t.Fatalf("the retry failed: %v", err)
	}
	if files := chunkFiles(t, store); len(files) != 1 {
		t.Fatalf("the chunk directory holds %v after the retry, want one chunk file", files)
	}
}
