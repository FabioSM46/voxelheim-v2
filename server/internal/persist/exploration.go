package persist

import (
	"encoding/binary"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// ExplorationVersion is the on-disk format version of a character's exploration file.
//
// Bump it for any change to the layout below, a purely additive one included: a reader
// of an older build must refuse a newer file rather than parse a prefix of it.
// Separate from [StoreVersion], [StructuresVersion], [ClockVersion] and
// world.StoreVersion, because a ledger of where somebody walked, a player record, a
// camp, a clock and a chunk delta change for entirely unrelated reasons.
//
// **This is a second file rather than a field in the record, and that is forced.**
// [StoreVersion]'s layout is fixed-width-then-one-variable-length-name with an exact
// size check at the read, which is what makes a truncated record refusable; it has no
// extensible area, and giving it one to hold a list that grows to sixty-five thousand
// entries would change the shape of the thing every player's life is stored in. The
// structures file made the same argument first — see [StructuresVersion].
const ExplorationVersion uint32 = 1

// MaxExploredColumns is the most chunk columns one character's ledger may hold.
//
// A hard cap in both directions: nothing past it is recorded, and a file declaring more
// is refused before a byte of it is loaded — the shape [Store.Load] and
// [StructureStore.Load] both use, where the declared size is checked before anything is
// allocated from it. At eight bytes a column that is 512 KiB per character, which is
// what makes it a bound on disk as well as on memory.
//
// Sixty-five thousand columns is 65,536 × 32² blocks, about 67 million square blocks —
// a square roughly 8,200 blocks on a side walked corner to corner. It is far past any
// history this game produces and it is deliberately not a play limit: a character that
// reaches it keeps playing, and stops adding to the map.
//
// **Not to be confused with protocol.MaxExploredColumns**, which is 4096 and is the
// most columns one `MapExplored` *frame* may carry. This one bounds the ledger; that
// one bounds a page of it, which is why the ledger is sent in pages at all.
const MaxExploredColumns = 1 << 16

// On-disk layout, little-endian throughout, one file per character.
//
//	exploration/<character-id-hex>.bin
//	    magic[4] version:u32 count:u32
//	    count × (cx:i32, cz:i32)
//	    crc32:u32
//
// Fixed-width entries and one count, so the file's exact size is a function of that
// count and a truncated file fails the size check below rather than being read as a
// shorter history. The same shape [StructureStore] uses, keyed per character the way a
// [Record] is.
//
// **No cy, because a column has none.** Exploration is a property of a place on the
// horizontal plane — a character who has been somewhere has been there at every height
// — which is the same reading schemas/world.fbs gives `MapColumn`.
//
// **The order in the file is the caller's**, exactly as it is for a camp: the session
// hands over a sorted list and nothing here sorts again. A second opinion about an
// order that already has an owner is how two orders come to exist.
const (
	explorationDirName = "exploration"

	explorationEntrySize = 4 + 4

	offExplorationCount   = world.HeaderSize
	explorationHeaderSize = offExplorationCount + 4

	maxExplorationFileSize = explorationHeaderSize + MaxExploredColumns*explorationEntrySize + world.ChecksumSize
)

var explorationMagic = [4]byte{'V', 'X', 'H', 'E'}

// ErrTooManyColumns reports a ledger too large for the format to write down.
//
// A sentinel for the reason [ErrTooManyStructures] is one: the caller's answer is to
// shout rather than to retry. It is unreachable while the only writer enforces
// [MaxExploredColumns] as it records — which it does — and it is checked at the write
// anyway, because a file this build cannot read back is the one failure that looks like
// a success until the next login.
var ErrTooManyColumns = errors.New("persist: more explored columns than the file format can hold")

// ExplorationStore is one world's directory of per-character exploration ledgers.
//
// **A nil *ExplorationStore is the ephemeral world**, and every method is a no-op on
// one rather than a branch at each call site — the shape a nil [Store], a nil
// [StructureStore], a nil [ClockStore] and a nil world.Store all already have. An
// ephemeral world still draws a map of where the character has been this session; what
// it does not do is remember it afterwards, which is the difference the operator chose.
//
// It owns a directory of independent files, like [Store] and unlike [StructureStore],
// and it needs no lock of its own for the same reason [Store] does not: one account
// holds one live session, one session plays one character, and every write for a
// character goes through that session's own serialised save path. Two goroutines never
// write the same file.
//
// **There is no index and there is deliberately none.** [Store] builds one because a
// name must be unique across the world and an account's characters have to be found on
// every connection; nothing ever asks this store a question it could not answer by
// opening the one file it was given a character id for.
type ExplorationStore struct {
	dir string
}

// OpenExplorationStore prepares the exploration directory under worldDir, creating it
// if it is not there.
//
// It creates no file: a character who has walked nowhere has no ledger, and that is the
// same fact as an empty one rather than a state to initialise. worldDir has already
// been seed-checked by world.OpenStore, which runs first, so nothing here re-asks
// whether this directory belongs to this world.
func OpenExplorationStore(worldDir string) (*ExplorationStore, error) {
	if worldDir == "" {
		// Not a nil store returned quietly, for the reason [OpenStore] and
		// [OpenStructureStore] both give: an empty -world-dir is the ephemeral world,
		// and choosing it is main's decision rather than a shape this constructor
		// should accept and forget about.
		return nil, errors.New("persist: the world directory must be named")
	}

	dir := filepath.Join(worldDir, explorationDirName)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return nil, fmt.Errorf("persist: creating %s: %w", dir, err)
	}

	// Whatever a crash left mid-rename. Inert, because a reader only ever opens an
	// exact <character-id>.bin path, so this is housekeeping rather than correctness.
	//
	// A pattern rather than a list, and the division is [OpenStore]'s: this directory
	// is one this store creates and fills, so it may name the shape of its own records
	// — unlike the operator's -world-dir above it, which is swept by literal names
	// (#137). This store writes exactly one kind of file, so one destination is the
	// whole list; the variadic fails closed, so a second kind would have to be named.
	world.SweepTemporaries(dir, "*"+recordFileExt)
	return &ExplorationStore{dir: dir}, nil
}

// Dir is the directory this store writes to. Empty for an ephemeral world.
func (s *ExplorationStore) Dir() string {
	if s == nil {
		return ""
	}
	return s.dir
}

// Load reads the columns one character has explored.
//
// Three answers, and the middle one carries the weight it does in [Store.Load]: found,
// absent, or unreadable. A character who has never played — or who played before this
// file existed — has no ledger, which is an empty map rather than an error. A file that
// exists and cannot be read **is** an error and must stay one, so that the caller can
// decide what to do with the evidence rather than have this function decide by
// answering "nowhere".
func (s *ExplorationStore) Load(id CharacterID) ([]world.Column, bool, error) {
	if s == nil || s.dir == "" {
		return nil, false, nil
	}

	path := s.ledgerPath(id)
	info, err := os.Stat(path)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return nil, false, nil
	case err != nil:
		return nil, false, fmt.Errorf("persist: reading %s: %w", path, err)
	}
	// Before the read, not after: a file this large is not one this format wrote, and
	// finding that out by allocating it is how a corrupt directory becomes an OOM.
	if info.Size() > int64(maxExplorationFileSize) {
		return nil, false, fmt.Errorf("%w: %s is %d bytes, more than the %d an exploration ledger can need",
			world.ErrCorruptStore, path, info.Size(), maxExplorationFileSize)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		return nil, false, fmt.Errorf("persist: reading %s: %w", path, err)
	}

	columns, err := decodeExploration(data)
	if err != nil {
		return nil, false, fmt.Errorf("%s: %w", path, err)
	}
	return columns, true, nil
}

// Save writes one character's whole ledger, atomically. A no-op in an ephemeral world.
//
// Whole rather than incremental, for the reason [StructureStore.Save] is: there is no
// removal record to lose and no ordering to get wrong, and the file is a snapshot of
// what the session holds rather than a log to be replayed. Exploration only ever grows,
// so the file only ever grows with it.
//
// **An empty ledger still writes a file**, header and checksum and a zero count. It is
// twelve bytes plus four, it is what says this build has looked at this character under
// this format, and refusing to write it would make "no file" mean two different things.
func (s *ExplorationStore) Save(id CharacterID, columns []world.Column) error {
	if s == nil || s.dir == "" {
		return nil
	}

	data, err := encodeExploration(columns)
	if err != nil {
		return err
	}
	return world.WriteAtomic(s.ledgerPath(id), data)
}

// Quarantine moves a ledger this build could not read out of the way, and returns where
// it went. A no-op in an ephemeral world, which reports an empty path.
//
// **The file is kept, never deleted and never written over**, which is the doctrine
// [Store.Quarantine] keeps and the reason both go through the same `setAside`: the
// bytes are the only evidence of what went wrong, and the next save would otherwise
// replace them. The timestamp in the name is what keeps a second quarantine from
// destroying the first.
//
// What it costs the player is smaller than a quarantined record costs them — a map that
// starts blank again, where that one is a life — so the caller that reaches for this is
// expected to survive it and let the character in, rather than refusing a connection
// over a map. Where a record's caller refuses when the move *fails*, this one has the
// other answer available: write nothing for the rest of that session, which keeps the
// evidence without costing anybody their evening.
func (s *ExplorationStore) Quarantine(id CharacterID) (string, error) {
	if s == nil || s.dir == "" {
		return "", nil
	}
	return setAside(s.ledgerPath(id), corruptFileSuffix)
}

// ledgerPath is where one character's ledger lives. The hex id is the whole name, for
// the reason [Store.recordPath] gives: every character of it comes from a number this
// server minted, so nothing a client sends reaches the filesystem.
func (s *ExplorationStore) ledgerPath(id CharacterID) string {
	return filepath.Join(s.dir, id.String()+recordFileExt)
}

// encodeExploration lays the ledger out, in the order it was given.
func encodeExploration(columns []world.Column) ([]byte, error) {
	if len(columns) > MaxExploredColumns {
		// Refused rather than truncated, and refused here rather than at the read: a
		// file this build cannot read back is the one failure that looks like a success
		// until the next login. See [MaxExploredColumns].
		return nil, fmt.Errorf("%w: %d columns are explored, more than the %d one ledger can hold",
			ErrTooManyColumns, len(columns), MaxExploredColumns)
	}

	buf := world.NewRecord(explorationHeaderSize, len(columns)*explorationEntrySize,
		explorationMagic, ExplorationVersion)
	binary.LittleEndian.PutUint32(buf[offExplorationCount:offExplorationCount+4], uint32(len(columns)))

	for i, column := range columns {
		at := explorationHeaderSize + i*explorationEntrySize
		binary.LittleEndian.PutUint32(buf[at:at+4], uint32(column.CX))
		binary.LittleEndian.PutUint32(buf[at+4:at+8], uint32(column.CZ))
	}

	world.PutChecksum(buf)
	return buf, nil
}

// decodeExploration parses the ledger, refusing anything it cannot read exactly.
//
// Validate-everything-then-return, the shape [decodeRecord] and [decodeStructures] both
// use: nothing is assembled until every check has passed, so a half-valid history is
// never a value a caller can hold.
//
// **No column is judged.** Every pair of int32s is a chunk column this world could have
// streamed, so there is nothing here for a range check to catch that the checksum has
// not already caught — and this package judges what a *file* can be wrong about, never
// what its contents are allowed to mean. The one thing that could be wrong about a
// column is being a duplicate, and that is the caller's set to build.
func decodeExploration(data []byte) ([]world.Column, error) {
	if len(data) < explorationHeaderSize+world.ChecksumSize {
		return nil, fmt.Errorf("%w: %d bytes is shorter than an empty exploration ledger",
			world.ErrCorruptStore, len(data))
	}
	if err := world.CheckHeader(data, explorationMagic, ExplorationVersion); err != nil {
		return nil, err
	}
	if err := world.CheckChecksum(data); err != nil {
		return nil, err
	}

	// The declared count is checked against the length the file actually has before it
	// indexes anything. A truncated file fails here, which is the case this check exists
	// for: a shorter history is a perfectly plausible one.
	count := uint64(binary.LittleEndian.Uint32(data[offExplorationCount : offExplorationCount+4]))
	want := uint64(explorationHeaderSize) + count*explorationEntrySize + world.ChecksumSize
	if want != uint64(len(data)) {
		return nil, fmt.Errorf("%w: the file claims %d columns, which need %d bytes, but the file is %d",
			world.ErrCorruptStore, count, want, len(data))
	}

	columns := make([]world.Column, count)
	for i := range columns {
		at := explorationHeaderSize + i*explorationEntrySize
		columns[i] = world.Column{
			CX: int32(binary.LittleEndian.Uint32(data[at : at+4])),
			CZ: int32(binary.LittleEndian.Uint32(data[at+4 : at+8])),
		}
	}
	return columns, nil
}
