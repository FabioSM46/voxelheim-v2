package persist

import (
	"encoding/binary"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// StructuresVersion is the on-disk format version of the structures file.
//
// Bump it for any change to the layout below, a purely additive one included: a
// reader of an older build must refuse a newer file rather than parse a prefix of
// it. Separate from both [StoreVersion] and world.StoreVersion, because a camp, a
// player record and a chunk delta change for unrelated reasons.
//
// **A new structure kind is not such a change**, and the campfire is the case that
// settled it: the kind is one byte whatever it names, so a file holding one is the same
// shape as a file holding a tent. What an older build does with it is refuse the *camp*
// through game.Sim.RestoreStructures — a kind it cannot place has no footprint — which
// is a refusal about content rather than about layout, and bumping the version to
// express it would make every older build reject every newer file including the ones it
// could read perfectly.
const StructuresVersion uint32 = 1

// MaxStructures is the most entries a structures file may declare.
//
// A bound on the allocation the count field can ask for, in the shape [Store.Load]
// uses for a player record: the size is checked before anything is read, so a
// corrupt count is refused rather than turned into a multi-gigabyte make(). The
// number is far above any world this game produces — a structure costs a crafted
// item and a placement, and sixty-five thousand of them is not a co-op camp — and
// the writer refuses to exceed it too, so this server can never write a file it
// would then refuse to read.
const MaxStructures = 1 << 16

// On-disk layout, little-endian throughout, one file for the whole world.
//
//	structures.bin
//	    magic[4] version:u32 count:u32
//	    count × (kind:u8 facing:u8 anchor:3×i32 owner:32)
//	    crc32:u32
//
// Fixed-width entries and one count, so the file's exact size is a function of that
// count and a truncated file fails the size check below rather than being read as a
// shorter camp.
//
// **No structure id.** Ids are minted from the counter that names every entity the
// simulation owns, and that counter is not serialised; a stored id would either be
// re-used by something else after a restart or force the counter onto disk beside
// it. Ids are re-minted on load instead — see game.Sim.RestoreStructures — which
// keeps "one id names one thing" true without the file having to help.
//
// The owner is an [identity.PlayerID], the same hash the player records are named
// by, and never the entity id it resolves to at runtime: an entity id names one
// session, and a structure outlives every session its owner will ever open.
const (
	structuresFileName = "structures.bin"

	structureEntrySize = 1 + 1 + 3*4 + identity.IDSize

	offStructureCount    = world.HeaderSize
	structuresHeaderSize = offStructureCount + 4

	maxStructuresFileSize = structuresHeaderSize + MaxStructures*structureEntrySize + world.ChecksumSize
)

var structuresMagic = [4]byte{'V', 'X', 'H', 'S'}

// ErrTooManyStructures reports a camp too large for the format to write down.
//
// A sentinel because the caller's answer is to shout rather than to retry: the world
// is past [MaxStructures] and the file cannot describe it, which is an operational
// problem and not a transient one.
var ErrTooManyStructures = errors.New("persist: more structures than the file format can hold")

// StructureRecord is one placed tent, forge or campfire, as it is written down.
//
// The four fields that outlive a process, and no more: what it is, where it rests,
// which way it faces and whose it is. Everything else about a live structure — its
// id, the chunk its anchor falls in — is derived on load rather than stored, because
// both are functions of values already here.
//
// **This package judges none of it**, exactly as it judges nothing in a [Record]:
// whether a kind has a footprint and whether two entries may stand together are
// questions internal/game answers, and game is where they are asked. What is checked
// here is what a *file* can be wrong about — magic, version, checksum, declared size.
type StructureRecord struct {
	Kind   vnet.StructureKind
	Anchor [3]int32
	Facing vnet.Facing
	Owner  identity.PlayerID
}

// StructureStore is one world's structures file.
//
// **A nil *StructureStore is the ephemeral world**, and every method is a no-op on
// one rather than a branch at each call site — the same shape a nil [Store] and a nil
// world.Store already have.
//
// Unlike [Store], which owns a directory of independent files, this owns exactly one
// file rewritten whole. That is what makes it safe with no lock of its own for the
// single writer it has: the autosave loop and the shutdown flush are ordered against
// each other by the worker wait group, never concurrent.
type StructureStore struct {
	path string
}

// OpenStructureStore prepares the structures file under worldDir.
//
// It does not create the file: a world with no camp in it has no structures file, and
// that is the same fact as an empty one rather than a state to initialise. worldDir has
// already been seed-checked by world.OpenStore, which runs first, so nothing here
// re-asks whether this directory belongs to this world.
func OpenStructureStore(worldDir string) (*StructureStore, error) {
	if worldDir == "" {
		// Not a nil store returned quietly, for the reason [OpenStore] gives: an empty
		// -world-dir is the ephemeral world, and choosing it is main's decision rather
		// than a shape this constructor should accept and forget about.
		return nil, errors.New("persist: the world directory must be named")
	}
	if err := os.MkdirAll(worldDir, 0o755); err != nil {
		return nil, fmt.Errorf("persist: creating %s: %w", worldDir, err)
	}

	// Whatever a crash left mid-rename, for the reason [OpenStore] sweeps the players
	// directory: this store writes through world.WriteAtomic and inherits its
	// leftovers. Inert either way — a reader only ever opens the exact path below.
	world.SweepTemporaries(worldDir)
	return &StructureStore{path: filepath.Join(worldDir, structuresFileName)}, nil
}

// Path is the file this store writes. Empty for an ephemeral world.
func (s *StructureStore) Path() string {
	if s == nil {
		return ""
	}
	return s.path
}

// Load reads every structure this world last wrote down.
//
// Three answers, and the middle one carries the same weight it does in [Store.Load]:
// found, absent, or unreadable. A world nobody has built in has no file, which is not
// an error — it is an empty camp. A file that exists and cannot be read **is** an
// error and must stay one: reporting it as "no structures" would start the server with
// an empty world whose first flush writes over the only record of what was standing.
func (s *StructureStore) Load() ([]StructureRecord, bool, error) {
	if s == nil {
		return nil, false, nil
	}

	info, err := os.Stat(s.path)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return nil, false, nil
	case err != nil:
		return nil, false, fmt.Errorf("persist: reading %s: %w", s.path, err)
	}
	// Before the read, not after: a file this large is not one this format wrote, and
	// finding that out by allocating it is how a corrupt directory becomes an OOM.
	if info.Size() > int64(maxStructuresFileSize) {
		return nil, false, fmt.Errorf("%w: %s is %d bytes, more than the %d a structures file can need",
			world.ErrCorruptStore, s.path, info.Size(), maxStructuresFileSize)
	}

	data, err := os.ReadFile(s.path)
	if err != nil {
		return nil, false, fmt.Errorf("persist: reading %s: %w", s.path, err)
	}

	records, err := decodeStructures(data)
	if err != nil {
		return nil, false, fmt.Errorf("%s: %w", s.path, err)
	}
	return records, true, nil
}

// Save writes the whole camp, atomically. A no-op in an ephemeral world.
//
// Whole rather than incremental, which is what makes the file a snapshot of the
// simulation rather than a log to be replayed: there is no removal record to lose and
// no ordering to get wrong, and a world that shrinks writes a smaller file.
func (s *StructureStore) Save(records []StructureRecord) error {
	if s == nil {
		return nil
	}

	data, err := encodeStructures(records)
	if err != nil {
		return err
	}
	return world.WriteAtomic(s.path, data)
}

// encodeStructures lays the camp out, in the order it was given.
//
// The order is the caller's and is preserved exactly, because the caller is the one
// with a deterministic one to give (game.Sim.Structures sorts by identity). Sorting
// again here would be a second opinion about an order that already has an owner.
func encodeStructures(records []StructureRecord) ([]byte, error) {
	if len(records) > MaxStructures {
		// Refused rather than truncated, and refused *here* rather than at the read:
		// writing a file this build cannot read back is the one failure that looks like
		// a success until a restart. See [MaxStructures].
		return nil, fmt.Errorf("%w: %d structures stand, more than the %d one file can hold",
			ErrTooManyStructures, len(records), MaxStructures)
	}

	buf := make([]byte, structuresHeaderSize+len(records)*structureEntrySize+world.ChecksumSize)
	copy(buf[0:4], structuresMagic[:])
	binary.LittleEndian.PutUint32(buf[4:8], StructuresVersion)
	binary.LittleEndian.PutUint32(buf[offStructureCount:offStructureCount+4], uint32(len(records)))

	for i, rec := range records {
		at := structuresHeaderSize + i*structureEntrySize
		buf[at] = byte(rec.Kind)
		buf[at+1] = byte(rec.Facing)
		for axis, value := range rec.Anchor {
			axisAt := at + 2 + axis*4
			binary.LittleEndian.PutUint32(buf[axisAt:axisAt+4], uint32(value))
		}
		copy(buf[at+14:at+14+identity.IDSize], rec.Owner[:])
	}

	world.PutChecksum(buf)
	return buf, nil
}

// decodeStructures parses the camp, refusing anything it cannot read exactly.
//
// Validate-everything-then-return, the shape [decodeRecord] and world.decodeChunkFile
// both use: nothing is assembled until every check has passed, so a half-valid camp is
// never a value a caller can hold.
func decodeStructures(data []byte) ([]StructureRecord, error) {
	if len(data) < structuresHeaderSize+world.ChecksumSize {
		return nil, fmt.Errorf("%w: %d bytes is shorter than an empty structures file",
			world.ErrCorruptStore, len(data))
	}
	if err := world.CheckHeader(data, structuresMagic, StructuresVersion); err != nil {
		return nil, err
	}
	if err := world.CheckChecksum(data); err != nil {
		return nil, err
	}

	// The declared count is checked against the length the file actually has before it
	// indexes anything. A truncated file fails here, which is the case this check exists
	// for: a smaller camp is a perfectly plausible one.
	count := uint64(binary.LittleEndian.Uint32(data[offStructureCount : offStructureCount+4]))
	want := uint64(structuresHeaderSize) + count*structureEntrySize + world.ChecksumSize
	if want != uint64(len(data)) {
		return nil, fmt.Errorf("%w: the file claims %d structures, which need %d bytes, but the file is %d",
			world.ErrCorruptStore, count, want, len(data))
	}

	records := make([]StructureRecord, count)
	for i := range records {
		at := structuresHeaderSize + i*structureEntrySize
		rec := StructureRecord{
			Kind:   vnet.StructureKind(data[at]),
			Facing: vnet.Facing(data[at+1]),
		}
		for axis := range rec.Anchor {
			axisAt := at + 2 + axis*4
			rec.Anchor[axis] = int32(binary.LittleEndian.Uint32(data[axisAt : axisAt+4]))
		}
		copy(rec.Owner[:], data[at+14:at+14+identity.IDSize])
		records[i] = rec
	}
	return records, nil
}
