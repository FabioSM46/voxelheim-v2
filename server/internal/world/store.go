package world

import (
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"hash/crc32"
	"io/fs"
	"log/slog"
	"maps"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"time"
)

// Persistence for the edit layer, and only for the edit layer.
//
// # What is stored, and what deliberately is not
//
// **Deltas only. The generated base is never written.** Generate is a pure function of
// (seed, coord), so the base is something this process can always recompute — storing it
// would be caching a computation at the price of the one property the GDD's Fimbulvetr
// storm needs, which is being able to tell a voxel a player placed from a voxel the
// generator produced. On disk that distinction is structural: a stored file *is* the
// delta list, so restoring an unprotected chunk to its original state stays "delete the
// deltas" rather than "diff two worlds". It is also what makes the format small — a chunk
// nobody has touched costs zero bytes, and a shelter costs a few hundred.
//
// # One file per chunk
//
// A chunk's edits live in their own file, named for its coordinate. It is the simplest
// thing that works: a save touches exactly the chunk that changed, a load reads exactly
// the chunk being composed, and the atomic-rename trick below needs no reasoning about
// neighbours sharing a file.
//
// A region format — many chunks packed into one file, as Minecraft's .mca does — is the
// known optimisation, and it is deliberately not here. It trades this file's whole
// correctness argument for fewer inodes, and nothing has yet measured that the inodes
// hurt: the interesting number is how many *edited* chunks a real world accumulates, and
// this format is what will produce it. Optimise when that measurement exists, not before.
//
// # Atomicity
//
// Every write goes to a temporary file in the destination directory, is flushed, is then
// renamed over the destination, and the directory itself is flushed last. Rename is
// atomic within a filesystem, so a crash leaves either the previous file or the new one
// and never a half-written one. Writing in place would leave a truncated file that parses
// as a shorter edit list — which is to say, as a shelter with some of its walls back.
//
// Atomic is not durable, and the second flush is the difference. The rename is one write
// to the *directory*, so a power loss can drop it while keeping the flushed file it
// pointed at, and the reader afterwards sees a chunk nobody ever edited. See
// [WriteAtomic].
//
// # Versioning
//
// Both file kinds carry a magic number and a format version, and a reader refuses
// anything it does not recognise. The point is not migration (there is one version, and
// building tooling for it would be inventing work); the point is that a later change can
// *refuse* an old file rather than misread it as the format it happens to resemble.

// StoreVersion is the on-disk format version.
//
// Bump it for any change to the layouts below, including one that only adds a field: a
// reader of an older build must refuse a newer file rather than parse a prefix of it.
const StoreVersion uint32 = 1

// DefaultWorldDir is where voxelheimd stores a world when the operator names no other
// directory.
const DefaultWorldDir = "world"

// DefaultSaveInterval is how often the autosave loop writes the chunks that changed.
//
// It is a coalescing window rather than a deadline: an edit is durable at the next tick
// of this interval, and a chunk edited fifty times in between is written once. Short
// enough that a crash costs seconds of digging, long enough that a player hammering the
// break key does not turn into a file write per block.
const DefaultSaveInterval = 5 * time.Second

// ErrSeedMismatch reports that a world directory was created by a different seed.
//
// Refusing is the whole point: the stored deltas name voxels by index inside a chunk, and
// those indices only mean anything against the terrain the recorded seed generates. Loading
// them onto another seed's terrain would not fail, it would quietly produce a world that is
// half one landscape and half another's edits.
var ErrSeedMismatch = errors.New("world: the stored world was created with a different seed")

// ErrWorldgenMismatch reports that a world directory was written by a build whose terrain
// generator differs from this one's.
//
// The seed's twin, and it exists for the same reason. Only deltas are stored, so opening a
// world means replaying edits onto a base recomputed from the seed — and the seed alone does
// not pin that base, the generator does too. See [WorldgenVersion].
var ErrWorldgenMismatch = errors.New("world: the stored world was written by a different terrain generator")

// ErrCorruptStore reports a file under the world directory that cannot be read as what it
// claims to be: a bad magic number, an unknown format version, a length that disagrees
// with its own header, a failed checksum, or an edit naming a voxel outside a chunk.
//
// It is an error rather than a fallback to the generated terrain on purpose. "Read it as
// terrain" is the one answer that silently discards what a player built; a refusal keeps
// the file for an operator to look at.
var ErrCorruptStore = errors.New("world: stored data is corrupt")

// On-disk layout, little-endian throughout.
//
//	world.bin      magic[4] version:u32 worldgen:u32 seed:i64 crc32:u32       = 24 bytes
//	chunks/c.X.Y.Z.vxd
//	               magic[4] version:u32 x:i32 y:i32 z:i32 count:u32
//	               count × (index:u32 block:u16)
//	               crc32:u32
//
// The chunk coordinate is written into the file as well as into its name, so a file that
// has been renamed or copied into the wrong place is caught rather than applied to the
// wrong chunk. The checksum covers everything before it and catches the corruption a
// length check cannot: a flipped bit inside an otherwise well-formed record.
const (
	worldFileName = "world.bin"
	chunkDirName  = "chunks"
	chunkFileExt  = ".vxd"

	// chunkFileGlob is every name this store gives a chunk file and nothing else, in
	// the form [filepath.Match] reads. It is what [Store.sweepTemporaries] hands
	// [SweepTemporaries] for the chunk directory: a directory this store creates and
	// fills with its own files can name the whole of its contents in one pattern.
	chunkFileGlob = "c.*" + chunkFileExt

	// tempFileMarker separates the name of the file [WriteAtomic] is replacing from the
	// run of digits os.CreateTemp appends to it, so a temporary is always exactly
	// <destination>.tmp<random>. Recognising that shape — against a destination the
	// caller names — is the whole of [SweepTemporaries]; see the reasoning there.
	tempFileMarker = ".tmp"

	worldFileSize = 4 + 4 + 4 + 8 + 4

	chunkHeaderSize = 4 + 4 + 4 + 4 + 4 + 4
	chunkEntrySize  = 4 + 2

	// maxChunkFileSize is what a chunk file can be when every voxel in it has been
	// edited. Checked before the file is read, because the alternative is letting a
	// corrupt length field decide how much memory to allocate.
	maxChunkFileSize = chunkHeaderSize + ChunkVolume*chunkEntrySize + ChecksumSize
)

var (
	worldMagic = [4]byte{'V', 'X', 'H', 'W'}
	chunkMagic = [4]byte{'V', 'X', 'H', 'D'}
)

// The record discipline the two layouts above share, exported so that a second
// store under the world directory obeys it rather than reimplementing it.
//
// internal/persist is the first: it keeps one file per player identity and needs
// exactly this — a magic number it chooses, a format version it owns, a trailing
// CRC, and the temporary-file-and-rename write. Copying those four into another
// package would be four more places for the same bug, and the one that matters is
// silent: a store that forgets the rename leaves half-written records that parse.
//
// What is deliberately *not* shared is the version number. Each store passes its
// own to [CheckHeader], because the player record and the delta record change for
// unrelated reasons — one shared counter would make a chunk-format bump refuse
// every player file, and the reverse.
const (
	// HeaderSize is the magic number and the format version: the prefix every
	// record in this directory starts with.
	HeaderSize = 4 + 4

	// ChecksumSize is the CRC-32 every record ends with.
	ChecksumSize = 4
)

// Store is one world's directory on disk.
//
// It is pure I/O: it holds no edits of its own, knows nothing about which chunks are
// resident, and none of its methods block on anything but the filesystem. The bookkeeping
// of *which* chunks still need writing belongs to the Cache, which is what learns about
// an edit.
//
// Safe for concurrent use — every method touches a distinct path, and the Cache
// serialises the writes to any one of them.
type Store struct {
	dir      string
	chunkDir string
	seed     int64
}

// OpenStore opens the world directory at dir for seed, creating it if it is not there.
//
// An existing world is only opened when its recorded seed is the one given: a mismatch is
// ErrSeedMismatch and the caller is expected to refuse to start. This is the first thing
// voxelheimd does with the operator's flags, before it binds a port, because a server that
// has already accepted a connection is a worse place to discover the world is not the one
// the operator meant.
func OpenStore(dir string, seed int64) (*Store, error) {
	if dir == "" {
		return nil, errors.New("world: the world directory must be named")
	}

	chunkDir := filepath.Join(dir, chunkDirName)
	if err := os.MkdirAll(chunkDir, 0o755); err != nil {
		return nil, fmt.Errorf("world: creating %s: %w", chunkDir, err)
	}

	s := &Store{dir: dir, chunkDir: chunkDir, seed: seed}
	if err := s.checkWorldFile(); err != nil {
		return nil, err
	}
	s.sweepTemporaries()
	return s, nil
}

// Dir is the world directory this store writes to.
func (s *Store) Dir() string { return s.dir }

// Seed is the seed the stored world was created with, which OpenStore has already proved
// is the seed the server is running.
func (s *Store) Seed() int64 { return s.seed }

// checkWorldFile reads the world file, or writes it when this directory is new.
//
// Two comparisons, not one, because two things have to hold before a stored delta may be
// replayed: the seed, and the generator the seed is fed to. Either mismatch is a refusal.
func (s *Store) checkWorldFile() error {
	path := filepath.Join(s.dir, worldFileName)

	data, err := os.ReadFile(path)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return WriteAtomic(path, encodeWorldFile(s.seed))
	case err != nil:
		return fmt.Errorf("world: reading %s: %w", path, err)
	}

	stored, worldgen, err := decodeWorldFile(data)
	if err != nil {
		return fmt.Errorf("%s: %w", path, err)
	}
	if stored != s.seed {
		return fmt.Errorf("%w: %s holds seed %d, the server is running seed %d",
			ErrSeedMismatch, s.dir, stored, s.seed)
	}
	if worldgen != WorldgenVersion {
		return fmt.Errorf("%w: %s was written by worldgen version %d, this build generates version %d",
			ErrWorldgenMismatch, s.dir, worldgen, WorldgenVersion)
	}
	return nil
}

// sweepTemporaries removes the temporary files a crash mid-write leaves behind.
//
// They are inert — a reader only ever opens an exact chunk path, and a temporary name
// never is one — so this is housekeeping rather than correctness, and a failure to sweep
// is not a reason to refuse to start a world that is otherwise readable.
func (s *Store) sweepTemporaries() {
	// Both directories, because WriteAtomic puts the temporary beside the file it is
	// replacing and the world file does not live under chunks/. Sweeping only the chunk
	// directory left `world.bin.tmp*` behind for the life of the world, which is a small
	// leak and a large contradiction of the sentence above.
	//
	// **Two calls rather than one loop, because the two directories are owned
	// differently and the sweep now says so.** chunks/ is this store's own creation and
	// holds nothing but chunk files, so one pattern names the whole of its contents. The
	// world directory is the operator's `-world-dir`, holding this store's world file
	// beside files that belong to internal/certs and internal/persist and possibly to
	// nobody here at all — so this names the one file in it that is ours, and each of the
	// other stores names its own on open.
	SweepTemporaries(s.dir, worldFileName)
	SweepTemporaries(s.chunkDir, chunkFileGlob)
}

// SweepTemporaries removes the temporaries a crash mid-[WriteAtomic] left in dir for the
// destination files named by destinations.
//
// Exported for the same reason the framing helpers are: a second store under the world
// directory writes through WriteAtomic and therefore inherits its leftovers. Best
// effort, and silent about failures, because a temporary file is inert — a reader only
// ever opens an exact record path, and a temporary name never is one.
//
// # What it removes, and why the destinations are not optional
//
// A file in dir is removed only when its name is one WriteAtomic could have produced for
// one of the destinations: exactly <destination>.tmp followed by the run of decimal
// digits os.CreateTemp appends, and a regular file rather than a directory or a link.
// Each destination is matched against that recovered name with [filepath.Match], so a
// caller passing a literal file name is asking for that file and nothing else, and a
// caller that owns a whole directory of its own records can name their shape instead
// ("*.bin", [chunkFileGlob]).
//
// **It used to glob `*.tmp*` over whatever directory it was handed, and that is a
// different function than the one its own documentation described.** The name was
// anchored to nothing, so it matched every name a crash could have left *and* every name
// this code never writes — which was tolerable while the only caller was this file, over
// a world directory the server generates, and stopped being tolerable the moment
// internal/ticket called it on `-auth-dir`. That is a path an operator typed, may well
// be shared or pre-existing, and the account service deleted files out of it on startup
// that nothing here had ever written (#137).
//
// The fix is the narrower statement rather than no statement: the sweep is what keeps a
// crash mid-write from leaving a half-written record — and, in `-auth-dir`, a stray copy
// of an Ed25519 seed — lying around for the life of the deployment, so it stays. What
// changed is that it now removes only files it can show are its own.
//
// **Naming nothing sweeps nothing.** A caller that passes no destination gets a no-op
// rather than the old behaviour, because the way this fails safely is by deleting too
// little. The converse is worth stating too: a destination of "*" would restore exactly
// the bug this signature exists to prevent, so name what you write.
func SweepTemporaries(dir string, destinations ...string) {
	if len(destinations) == 0 {
		return
	}

	// Read rather than globbed, because the pattern belongs to the destination name and
	// the destination is not recoverable from a glob of the temporary's.
	entries, err := os.ReadDir(dir)
	if err != nil {
		return
	}
	for _, entry := range entries {
		// os.CreateTemp makes a regular file. A directory or a symlink wearing the
		// shape below is therefore not something this code left, whatever it is.
		if !entry.Type().IsRegular() {
			continue
		}
		destination, ok := temporaryDestination(entry.Name())
		if !ok {
			continue
		}
		for _, want := range destinations {
			// A malformed pattern is reported as no match: these are compile-time
			// constants at every call site, and the failing direction to pick if one
			// ever is not is the one that removes nothing.
			if matched, err := filepath.Match(want, destination); err == nil && matched {
				_ = os.Remove(filepath.Join(dir, entry.Name()))
				break
			}
		}
	}
}

// temporaryDestination reports the file a temporary name was on its way to becoming, and
// whether name is a name [WriteAtomic] could have produced at all.
//
// The shape is os.CreateTemp's: a pattern with no `*` in it is used whole as the prefix
// and the random run is appended, so `base+".tmp"` yields `base.tmp` + digits. Nothing in
// the standard library promises those characters are digits — it is documented only as a
// random string — which is why this is asserted against the real os.CreateTemp by a test
// rather than by reading. Should it ever stop being digits, this stops recognising the
// temporary and the sweep leaves it behind: the harmless direction, since a leftover is
// inert, and a red test rather than a silent one.
//
// The last `.tmp` rather than the first, so that a destination whose own name ends in
// `.tmp` is recovered whole instead of being cut in half.
func temporaryDestination(name string) (string, bool) {
	cut := strings.LastIndex(name, tempFileMarker)
	if cut <= 0 {
		return "", false
	}
	random := name[cut+len(tempFileMarker):]
	if random == "" {
		return "", false
	}
	for i := 0; i < len(random); i++ {
		if random[i] < '0' || random[i] > '9' {
			return "", false
		}
	}
	return name[:cut], true
}

// chunkPath is where one chunk's edits live. The coordinate is in the name so a directory
// listing is readable, and in the file so a misplaced one is caught.
func (s *Store) chunkPath(coord Coord) string {
	return filepath.Join(s.chunkDir, fmt.Sprintf("c.%d.%d.%d%s", coord.X, coord.Y, coord.Z, chunkFileExt))
}

// LoadChunk reads the edits stored for coord.
//
// A chunk nobody has edited has no file, and that is not an error: it returns a nil map,
// which is exactly "the generated base is the whole truth here". Anything else that stops
// the file being read is an error, because the alternative — an empty map — is the same
// value as "no edits" and would hand a player the terrain their shelter used to be.
func (s *Store) LoadChunk(coord Coord) (map[int]Block, error) {
	path := s.chunkPath(coord)

	info, err := os.Stat(path)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return nil, nil
	case err != nil:
		return nil, fmt.Errorf("world: reading %s: %w", path, err)
	}
	// Before the read, not after: a file this large is not one this format wrote, and
	// finding that out by allocating it is how a corrupt directory becomes an OOM.
	if info.Size() > maxChunkFileSize {
		return nil, fmt.Errorf("%w: %s is %d bytes, more than the %d a chunk can need",
			ErrCorruptStore, path, info.Size(), maxChunkFileSize)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("world: reading %s: %w", path, err)
	}

	edits, err := decodeChunkFile(coord, data)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	return edits, nil
}

// SaveChunk writes the edits recorded for coord, atomically.
//
// An empty set writes nothing and removes nothing: Deltas never forgets an edit, so a
// chunk with no edits has never been saved either, and creating an empty file for it would
// spend an inode saying what the file's absence already says.
func (s *Store) SaveChunk(coord Coord, edits map[int]Block) error {
	if len(edits) == 0 {
		return nil
	}

	encoded, err := encodeChunkFile(coord, edits)
	if err != nil {
		return err
	}
	return WriteAtomic(s.chunkPath(coord), encoded)
}

// WriteAtomic replaces path with data, or leaves it exactly as it was.
//
// Temporary file, flush, rename, flush the directory — in that order, and the temporary
// file is created *in the destination directory* because rename is only atomic within one
// filesystem.
//
// The two flushes answer two different questions, and only both together make a
// successful return mean "this is on disk". Flushing the temporary file is what makes the
// rename mean something: without it the directory entry can reach the disk ahead of the
// bytes it points at. Flushing the directory is what makes the rename itself survive:
// the entry is the *directory's* metadata, a separate write from the file's, and on ext4
// and XFS a power loss can leave the flushed data in an inode nothing links to. The
// rename is visible to every reader on this machine the instant it returns — durability
// is the part that is not free.
//
// **The second flush is a POSIX guarantee, and so is the durability half of that
// sentence.** [os.File.Sync] is FlushFileBuffers on Windows, which refuses the read-only
// handle [os.Open] returns for a directory, so [syncDir] is a documented no-op there and a
// successful return goes on meaning atomic while it stops meaning durable. Closing that
// would need MoveFileEx with MOVEFILE_WRITE_THROUGH, which [os.Rename] does not use and
// which no standard-library call reaches — so the limit is stated here, where the callers
// read it, rather than left to be discovered.
//
// A failure before the rename removes the temporary file and returns; the destination has
// not been opened, so whatever it held is still what it holds. A failure of the directory
// flush is the one case where the write has already landed — see the comment on it.
//
// The property this buys is what [SweepTemporaries]'s half of the crash story does not
// cover: that one tidies what a crash left *mid*-write, this one keeps a finished write
// from disappearing. It matters most where an absent file is indistinguishable from a
// directory nobody has written yet — internal/ticket refuses a half-present signing key
// pair, but a pair that both vanished takes the first-start branch and mints a new one,
// with nothing left for the refusal to fire on. That is precisely the case the missing
// directory flush leaves open on Windows.
func WriteAtomic(path string, data []byte) (err error) {
	dir, base := filepath.Dir(path), filepath.Base(path)

	// The suffix keeps a temporary file from ever being named like a chunk file, so a
	// crash cannot leave something a reader would open.
	tmp, err := os.CreateTemp(dir, base+".tmp")
	if err != nil {
		return fmt.Errorf("world: creating a temporary file in %s: %w", dir, err)
	}
	name := tmp.Name()
	renamed := false
	defer func() {
		// Not after the rename: `name` is no longer a file, and the destination that
		// now wears it is the caller's data rather than this function's leftover.
		if err != nil && !renamed {
			_ = tmp.Close()
			_ = os.Remove(name)
		}
	}()

	if _, err = tmp.Write(data); err != nil {
		return fmt.Errorf("world: writing %s: %w", name, err)
	}
	if err = tmp.Sync(); err != nil {
		return fmt.Errorf("world: flushing %s: %w", name, err)
	}
	if err = tmp.Close(); err != nil {
		return fmt.Errorf("world: closing %s: %w", name, err)
	}
	if err = os.Rename(name, path); err != nil {
		return fmt.Errorf("world: renaming %s onto %s: %w", name, path, err)
	}
	renamed = true

	// Reported, never swallowed. A write this function calls durable and is not is the
	// one outcome worse than never having synced at all: every caller here refuses
	// rather than regenerates, and a refusal cannot fire on a file that is simply gone.
	//
	// The new file stays where it is. It is already the visible content of path, the
	// previous contents are gone whatever happens next, and un-renaming would trade a
	// doubt about durability for certain data loss. What the error says is the true
	// statement — the write may not survive a power loss — and the answer to it is to
	// write again, which is what the callers already do: the chunk cache re-queues a
	// save it was told failed, and the record stores hand the error up.
	if err = syncDir(dir); err != nil {
		return fmt.Errorf("world: flushing the directory entry for %s: %w", path, err)
	}
	return nil
}

// syncDir flushes dir's own contents — the entries in it, not the files they name — so
// that a rename this process has already seen survives a power loss.
//
// One open, one fsync, one close per atomic write. **It roughly doubles the cost of a
// write, and that is the honest number rather than the hoped-for one**: measured on ext4
// over NVMe, a 1 KiB record went from ~2.2 ms to ~4.3 ms per write. Neither the record
// size (1 KiB against 64 KiB) nor the number of files already in the directory (none
// against two thousand) moved it, which says what it is — a second fsync, costing about
// what the first one costs.
//
// It is still one entry point rather than two, and the doubling is why that needed
// deciding rather than assuming. The writes are made by autosave workers, never by the
// tick loop: a chunk edited fifty times inside one [DefaultSaveInterval] is written once,
// out of any lock, so what doubled is background I/O and not a frame. A second
// non-durable variant would save ~2 ms there and cost the thing this function exists to
// promise, on whichever call site picks it by mistake.
//
// The number to watch is dirty chunks per pass, since a pass costs that many times
// ~4.3 ms: somewhere around a thousand of them the pass stops fitting inside its
// five-second window. The answer there is the region format the file header already
// argues for — fewer, larger writes — and not a write that lies about being on disk.
//
// # Windows
//
// There is no directory flush there: [os.File.Sync] is FlushFileBuffers on Windows, and
// that call wants a writable handle to a file — the read-only directory handle os.Open
// returns is refused. Doing this unconditionally would therefore fail every write on a
// platform where the write itself is fine. The guarantee above is therefore a POSIX one
// and this is a no-op on Windows — stated here and in [WriteAtomic] rather than
// discovered, because a durability claim that silently does not hold is the failure this
// whole change is about. `runtime.GOOS` is a constant, so the branch is settled at
// compile time; the reason it is a branch rather than a build-tagged pair of files is
// that CI builds Linux only, and a file it never compiles is a file that rots unnoticed.
func syncDir(dir string) error {
	if runtime.GOOS == "windows" {
		return nil
	}

	d, err := os.Open(dir)
	if err != nil {
		return err
	}
	if err := d.Sync(); err != nil {
		_ = d.Close() // The sync error is the one worth reporting.
		return err
	}
	return d.Close()
}

func encodeWorldFile(seed int64) []byte {
	// No body: world.bin is header and checksum, so every field below is a header field.
	buf := NewRecord(worldFileSize-ChecksumSize, 0, worldMagic, StoreVersion)
	binary.LittleEndian.PutUint32(buf[8:12], WorldgenVersion)
	binary.LittleEndian.PutUint64(buf[12:20], uint64(seed))
	PutChecksum(buf)
	return buf
}

// decodeWorldFile returns the seed and the worldgen version the directory was written
// with. Both are the caller's to compare; neither is checked here, because a mismatch is a
// refusal to start rather than a corrupt file.
func decodeWorldFile(data []byte) (int64, uint32, error) {
	if len(data) != worldFileSize {
		return 0, 0, fmt.Errorf("%w: the world file is %d bytes, want %d", ErrCorruptStore, len(data), worldFileSize)
	}
	if err := CheckHeader(data, worldMagic, StoreVersion); err != nil {
		return 0, 0, err
	}
	if err := CheckChecksum(data); err != nil {
		return 0, 0, err
	}
	return int64(binary.LittleEndian.Uint64(data[12:20])), binary.LittleEndian.Uint32(data[8:12]), nil
}

// encodeChunkFile serialises one chunk's edits.
//
// Sorted by index, so the same edits always produce the same bytes: a save that rewrites
// an unchanged chunk leaves an identical file, and a test can compare two saves without
// depending on Go's randomised map iteration.
func encodeChunkFile(coord Coord, edits map[int]Block) ([]byte, error) {
	indices := slices.Sorted(maps.Keys(edits))

	buf := NewRecord(chunkHeaderSize, len(indices)*chunkEntrySize, chunkMagic, StoreVersion)
	binary.LittleEndian.PutUint32(buf[8:12], uint32(coord.X))
	binary.LittleEndian.PutUint32(buf[12:16], uint32(coord.Y))
	binary.LittleEndian.PutUint32(buf[16:20], uint32(coord.Z))
	binary.LittleEndian.PutUint32(buf[20:24], uint32(len(indices)))

	at := chunkHeaderSize
	for _, index := range indices {
		if index < 0 || index >= ChunkVolume {
			// Unreachable through Deltas.Record, whose callers derive the index from
			// Local. Refused rather than truncated into the field, because a value that
			// cannot be written back correctly must not be written at all.
			return nil, fmt.Errorf("world: chunk %+v holds an edit at voxel %d, outside 0..%d", coord, index, ChunkVolume-1)
		}
		binary.LittleEndian.PutUint32(buf[at:at+4], uint32(index))
		binary.LittleEndian.PutUint16(buf[at+4:at+6], uint16(edits[index]))
		at += chunkEntrySize
	}

	PutChecksum(buf)
	return buf, nil
}

// decodeChunkFile parses one chunk's edits, refusing anything it cannot read exactly.
//
// want is the coordinate the caller asked for; the file has to agree, which is what makes
// a file copied to the wrong name a refusal rather than someone else's shelter.
func decodeChunkFile(want Coord, data []byte) (map[int]Block, error) {
	if len(data) < chunkHeaderSize+ChecksumSize {
		return nil, fmt.Errorf("%w: %d bytes is shorter than an empty chunk record", ErrCorruptStore, len(data))
	}
	if err := CheckHeader(data, chunkMagic, StoreVersion); err != nil {
		return nil, err
	}
	if err := CheckChecksum(data); err != nil {
		return nil, err
	}

	got := Coord{
		X: int32(binary.LittleEndian.Uint32(data[8:12])),
		Y: int32(binary.LittleEndian.Uint32(data[12:16])),
		Z: int32(binary.LittleEndian.Uint32(data[16:20])),
	}
	if got != want {
		return nil, fmt.Errorf("%w: the record is for chunk %+v, not %+v", ErrCorruptStore, got, want)
	}

	// The count is checked against the length the file actually has before it is used to
	// size anything. A truncated file fails here — which is the case this whole check
	// exists for, because a shorter edit list is a perfectly plausible one.
	count := uint64(binary.LittleEndian.Uint32(data[20:24]))
	if want := uint64(chunkHeaderSize) + count*chunkEntrySize + ChecksumSize; want != uint64(len(data)) {
		return nil, fmt.Errorf("%w: the record claims %d edits, which needs %d bytes, but the file is %d",
			ErrCorruptStore, count, want, len(data))
	}

	edits := make(map[int]Block, count)
	at := chunkHeaderSize
	for range count {
		index := binary.LittleEndian.Uint32(data[at : at+4])
		if index >= ChunkVolume {
			return nil, fmt.Errorf("%w: an edit names voxel %d, outside 0..%d", ErrCorruptStore, index, ChunkVolume-1)
		}
		edits[int(index)] = Block(binary.LittleEndian.Uint16(data[at+4 : at+6]))
		at += chunkEntrySize
	}
	return edits, nil
}

// NewRecord allocates a record of the shape every store under this directory writes: a
// header, a body, and a trailing checksum — with the magic and the format version already
// stamped into the first [HeaderSize] bytes.
//
// **The write-side twin of [CheckHeader], and the reason it exists is that it had none.**
// The magic and the version were read in one place and written in eight, so the two halves
// of one format decision sat on opposite sides of the package boundary: a change to
// [HeaderSize], to the field order, or to the endianness of the version would have been one
// edit here and eight to find. Nothing about the bytes changes — this writes exactly what
// those eight sites wrote.
//
// headerSize is the caller's own, because a store's header is this package's magic and
// version followed by whatever fixed fields that store keeps. bodyLen is everything after
// the header and before the checksum. The caller fills both and calls [PutChecksum] last,
// which is the one step that has to happen after the body is written.
//
// A headerSize below [HeaderSize] panics, and the check is explicit because the slice is
// not one. `buf[4:HeaderSize]` is out of bounds only when the *whole record* is shorter
// than a header — so a two-byte header with a hundred-byte body stays in range and writes
// the version over the caller's first body bytes instead, which is silent corruption of a
// record in the one package whose job is refusing corrupt records. Found by the review of
// #139, where this comment claimed the slice was the guard.
//
// A panic rather than an error: every caller passes a constant, so this is a build that
// cannot store anything correctly rather than a file that cannot be read.
func NewRecord(headerSize, bodyLen int, magic [4]byte, version uint32) []byte {
	if headerSize < HeaderSize {
		panic(fmt.Sprintf("world.NewRecord: headerSize %d is smaller than the %d-byte record header",
			headerSize, HeaderSize))
	}
	buf := make([]byte, headerSize+bodyLen+ChecksumSize)
	copy(buf[0:4], magic[:])
	binary.LittleEndian.PutUint32(buf[4:HeaderSize], version)
	return buf
}

// CheckHeader validates the magic number and the format version a record claims.
//
// The version check is the reason the field exists: a build that does not know a layout
// says so, instead of reading the bytes it recognises and guessing at the rest. want is
// the caller's own version rather than a package constant, so two stores under this
// directory can version independently — see the [HeaderSize] block.
//
// The length guard is not redundant with the callers that already have one. This is
// exported, so the next caller may not, and a short slice here would be an index panic
// rather than the refusal every other malformed record gets.
func CheckHeader(data []byte, magic [4]byte, want uint32) error {
	if len(data) < HeaderSize {
		return fmt.Errorf("%w: %d bytes cannot hold a %d-byte record header", ErrCorruptStore, len(data), HeaderSize)
	}
	if [4]byte(data[0:4]) != magic {
		return fmt.Errorf("%w: %q is not a %q record", ErrCorruptStore, data[0:4], magic[:])
	}
	if version := binary.LittleEndian.Uint32(data[4:8]); version != want {
		return fmt.Errorf("%w: format version %d, this build reads version %d",
			ErrCorruptStore, version, want)
	}
	return nil
}

// CheckChecksum verifies the trailing CRC over everything before it. It catches what a
// length check cannot: a flipped bit inside a record whose shape is still valid.
func CheckChecksum(data []byte) error {
	if len(data) < ChecksumSize {
		return fmt.Errorf("%w: %d bytes cannot hold a %d-byte checksum", ErrCorruptStore, len(data), ChecksumSize)
	}
	body := data[:len(data)-ChecksumSize]
	stored := binary.LittleEndian.Uint32(data[len(data)-ChecksumSize:])
	if got := checksum(body); got != stored {
		return fmt.Errorf("%w: checksum %#08x, the record says %#08x", ErrCorruptStore, got, stored)
	}
	return nil
}

// PutChecksum writes the CRC of everything before it into buf's last four bytes.
func PutChecksum(buf []byte) {
	binary.LittleEndian.PutUint32(buf[len(buf)-ChecksumSize:], checksum(buf[:len(buf)-ChecksumSize]))
}

// checksum is CRC-32 (IEEE). Integrity, not authenticity: it catches a bad sector and a
// truncated write, and it is not trying to catch anybody. Nothing under the world
// directory crosses a trust boundary — it is the server's own state on the server's own
// disk — so a cryptographic hash here would cost every save and defend against nothing
// that is not already game over.
func checksum(body []byte) uint32 { return crc32.ChecksumIEEE(body) }

// --- the Cache's half: which chunks still need writing, and when they are written ---

// markDirty records that coord has edits the store does not have yet.
//
// Called from Apply with composeMu held, so it must not block on anything: it takes one
// mutex that is never held across a write, and does a map insert under it.
func (c *Cache) markDirty(coord Coord) {
	if c.store == nil {
		return
	}
	c.dirtyMu.Lock()
	c.dirty[coord] = struct{}{}
	c.dirtyMu.Unlock()
}

// takeDirty removes and returns the chunks awaiting a write.
//
// Clearing *before* the snapshot is taken, rather than after, is the ordering that cannot
// lose an edit. An edit landing in between re-marks the chunk, so the worst case is
// writing it twice — the same bytes. The other order has a window in which an edit is in
// neither the file being written nor the set of chunks still to write, and that edit is
// simply gone at the next restart.
func (c *Cache) takeDirty() []Coord {
	c.dirtyMu.Lock()
	defer c.dirtyMu.Unlock()

	if len(c.dirty) == 0 {
		return nil
	}
	coords := slices.Collect(maps.Keys(c.dirty))
	clear(c.dirty)
	return coords
}

// Flush writes every chunk whose edits the store does not have yet, and returns once they
// are on disk.
//
// **It takes neither composeMu nor mu**, which is what "saving does not block the tick
// loop or a session's read path" means concretely: while a save runs, collision keeps
// reading resident chunks, sessions keep being handed them, and edits keep being applied.
// The only lock a save shares with an edit is the delta layer's own, and it is held for
// the length of a map copy.
//
// Saves are serialised against each other, so two Flushes can never race a stale snapshot
// onto a fresher one. A chunk whose write fails goes back into the dirty set and is
// retried by the next save, and its error is returned so a caller can say so.
func (c *Cache) Flush() error {
	if c.store == nil {
		return nil
	}

	c.saveMu.Lock()
	defer c.saveMu.Unlock()

	var errs []error
	for _, coord := range c.takeDirty() {
		if err := c.store.SaveChunk(coord, c.deltas.Snapshot(coord)); err != nil {
			c.markDirty(coord)
			errs = append(errs, err)
		}
	}
	return errors.Join(errs...)
}

// SaveLoop writes the chunks that have changed, every interval, until ctx ends.
//
// It returns ctx.Err() on cancellation and does not stop for a failed write: a full disk
// is a reason to shout, not a reason for a server to quietly stop saving for the rest of
// its life. The chunk stays dirty, so the next pass — and the final flush at shutdown —
// tries again.
//
// The final flush is *not* here. This loop is a worker like any other; shutdown waits for
// it to exit and then flushes once, when no session can still be recording an edit.
func (c *Cache) SaveLoop(ctx context.Context, every time.Duration, log *slog.Logger) error {
	if c.store == nil {
		return nil
	}
	if every <= 0 {
		every = DefaultSaveInterval
	}
	if log == nil {
		log = slog.New(slog.DiscardHandler)
	}

	ticker := time.NewTicker(every)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
			if err := c.Flush(); err != nil {
				log.Error("saving the world failed; the chunks will be retried", "error", err)
			}
		}
	}
}

// hydrate brings coord's stored edits into the delta layer, once, before the chunk is
// composed for the first time.
//
// Called from Get with no lock held and inside the generation semaphore, so a burst of
// chunk requests cannot turn into a burst of simultaneous reads.
//
// There is no separate "already hydrated" set: the delta layer already holding edits for a
// coordinate *is* that record. The equivalence holds in one direction only, and it is the
// direction that matters — memory is never behind disk, because disk is only ever written
// from memory and Deltas never forgets an edit. So a chunk with edits in memory has
// nothing to gain from a file, and a chunk with none has never been saved.
//
// That check is a fast path rather than a guarantee, which is why Restore is the one that
// settles precedence. A chunk evicted while it was being generated leaves an orphaned
// generation running beside the one that replaced it, so a slow read can still land after
// an edit has been recorded through the newer entry. Restore refusing to overwrite is what
// makes that harmless instead of a lost edit.
func (c *Cache) hydrate(coord Coord) error {
	if c.store == nil || c.deltas.Known(coord) {
		return nil
	}

	stored, err := c.store.LoadChunk(coord)
	if err != nil {
		return err
	}
	c.deltas.Restore(coord, stored)
	return nil
}
