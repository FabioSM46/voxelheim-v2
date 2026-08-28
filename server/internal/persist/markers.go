package persist

import (
	"encoding/binary"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"unicode/utf8"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// MarkersVersion is the on-disk format version of a character's marker file.
//
// Bump it for any change to the layout below, a purely additive one included: a reader
// of an older build must refuse a newer file rather than parse a prefix of it. Separate
// from [StoreVersion], [ExplorationVersion], [StructuresVersion], [ClockVersion] and
// world.StoreVersion, because sixty-four marks somebody typed, a ledger of where they
// walked, a player record, a camp, a clock and a chunk delta change for entirely
// unrelated reasons.
//
// **A third file rather than a field in the record, and the argument is
// [ExplorationVersion]'s.** [StoreVersion]'s layout is fixed-width-then-one-
// variable-length-name with an exact size check at the read, which is what makes a
// truncated record refusable; it has no extensible area, and a list of sixty-four
// entries each carrying a hundred and twenty bytes of a player's own text is not what to
// give it one for.
const MarkersVersion uint32 = 1

const (
	// MaxMarkers is the most marks one character's file may hold. Sixty-four, because a
	// map nobody can read is not a map — schemas/player.fbs carries the argument.
	MaxMarkers = 64

	// MaxMarkerNote is the widest note one mark may carry, in bytes. Bytes rather than
	// characters, because a byte is what the file and both decoders actually count.
	MaxMarkerNote = 120
)

// The two numbers above are the *file's*, and these guards are what keep them from
// silently becoming the wire's.
//
// They are the same values protocol.MaxMarkers and protocol.MarkerNoteMaxBytes carry,
// and they must be: a stored mark is put on the wire unchanged, so a file that could
// hold more marks — or a longer note — than a `MarkerList` may carry would be a file
// this server could read and then not send. But they are not *defined* as the wire's,
// because the entry width below is a function of MaxMarkerNote: an alias would let a
// contract change reshape every marker file on disk with nothing saying so and
// [MarkersVersion] unbumped. A literal plus this guard makes that a build failure at the
// line where somebody has to decide.
//
// Both directions, because an untyped constant conversion only refuses a negative: one
// alone would pin the difference in a single direction and let the other drift.
const (
	_ = uint(MaxMarkers - protocol.MaxMarkers)
	_ = uint(protocol.MaxMarkers - MaxMarkers)
	_ = uint(MaxMarkerNote - protocol.MarkerNoteMaxBytes)
	_ = uint(protocol.MarkerNoteMaxBytes - MaxMarkerNote)
)

// On-disk layout, little-endian throughout, one file per character.
//
//	markers/<character-id-hex>.bin
//	    magic[4] version:u32 next_id:u64 count:u32
//	    count × (marker_id:u64, x:i32, z:i32, kind:u8, note_len:u8, note[120])
//	    crc32:u32
//
// Fixed-width entries and one count, so the file's exact size is a function of that
// count and a truncated file fails the size check below rather than being read as a
// shorter map. The note is the only variable-length thing a mark carries and it is
// stored zero-padded to its maximum with an explicit length, which is what keeps the
// decoder's only variable quantity the count — the discipline the player record's
// layout insists on, applied to a field that would otherwise reintroduce a second one.
//
// **next_id is in the header rather than derived from the entries**, and that is the
// whole of "an id is never reused within a character". Derived as max(id)+1 it would
// fall back every time the highest-numbered mark was removed, and the next placement
// would mint an id a client had already been told meant something else. Stored, a
// removal costs nothing and the counter only ever goes up. It is a u64 because it never
// decreases: sixty-four marks at a time, minted forever, and it still does not wrap.
//
// **No y.** A mark is a place on a map and a map has two axes; schemas/player.fbs says
// the same thing about `MarkerPlaceRequest`.
//
// **The order in the file is the caller's**, exactly as it is for a camp and for a
// ledger: the session hands over the list it holds and nothing here sorts it. A second
// opinion about an order that already has an owner is how two orders come to exist.
const (
	markersDirName = "markers"

	markerEntrySize = 8 + 4 + 4 + 1 + 1 + MaxMarkerNote

	offMarkersNextID  = world.HeaderSize
	offMarkersCount   = offMarkersNextID + 8
	markersHeaderSize = offMarkersCount + 4

	// Offsets within one entry.
	offMarkerID      = 0
	offMarkerX       = 8
	offMarkerZ       = 12
	offMarkerKind    = 16
	offMarkerNoteLen = 17
	offMarkerNote    = 18

	maxMarkersFileSize = markersHeaderSize + MaxMarkers*markerEntrySize + world.ChecksumSize
)

var markersMagic = [4]byte{'V', 'X', 'H', 'M'}

// ErrTooManyMarkers reports a map too full for the format to write down.
//
// A sentinel for the reason [ErrTooManyColumns] is one: the caller's answer is to shout
// rather than to retry. It is unreachable while the only writer refuses the sixty-fifth
// placement — which it does — and it is checked at the write anyway, because a file this
// build cannot read back is the one failure that looks like a success until the next
// login.
var ErrTooManyMarkers = errors.New("persist: more marks than the file format can hold")

// StoredMarkers is one character's marker file as a value: the marks, and the counter
// that says what the next one will be called.
//
// The counter travels with the list because it is meaningless without it and because
// the two are written in one atomic file: a caller that could save marks and forget the
// counter would be able to produce exactly the reused id the header exists to prevent.
type StoredMarkers struct {
	// NextID is the id the next mark placed for this character takes. Non-zero in every
	// file this build writes: ids start at 1 because zero is the absent-field value a
	// `MarkerRemoveRequest` is refused for.
	NextID uint64

	// Markers is every mark the character holds, in the order the caller gave.
	//
	// protocol.Marker rather than a declaration of this package's own, and that is the
	// [Record] slots decision rather than the game.Life one: a mark has no simulation
	// half to disagree with. Every field of it is written to be read back and put on the
	// wire unchanged, so a parallel struct would be five field names copied to no end and
	// one more place for the shape to drift.
	Markers []protocol.Marker
}

// MarkerStore is one world's directory of per-character marker files.
//
// **A nil *MarkerStore is the ephemeral world**, and every method is a no-op on one
// rather than a branch at each call site — the shape a nil [Store], a nil
// [ExplorationStore], a nil [StructureStore], a nil [ClockStore] and a nil world.Store
// all already have. An ephemeral world still lets a character put marks on the map and
// still answers with the whole list; what it does not do is remember them afterwards,
// which is the difference the operator chose.
//
// It owns a directory of independent files, like [Store] and [ExplorationStore], and it
// needs no lock of its own for the same reason neither of those does: one account holds
// one live session, one session plays one character, and every write for a character
// goes through that session's own serialised save path. Two goroutines never write the
// same file.
type MarkerStore struct {
	dir string
}

// OpenMarkerStore prepares the markers directory under worldDir, creating it if it is
// not there.
//
// It creates no file: a character who has marked nothing has no marker file, and that is
// the same fact as an empty one rather than a state to initialise. worldDir has already
// been seed-checked by world.OpenStore, which runs first, so nothing here re-asks whether
// this directory belongs to this world.
func OpenMarkerStore(worldDir string) (*MarkerStore, error) {
	if worldDir == "" {
		// Not a nil store returned quietly, for the reason [OpenStore] and
		// [OpenExplorationStore] both give: an empty -world-dir is the ephemeral world,
		// and choosing it is main's decision rather than a shape this constructor should
		// accept and forget about.
		return nil, errors.New("persist: the world directory must be named")
	}

	dir := filepath.Join(worldDir, markersDirName)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return nil, fmt.Errorf("persist: creating %s: %w", dir, err)
	}

	// Whatever a crash left mid-rename. Inert, because a reader only ever opens an exact
	// <character-id>.bin path, so this is housekeeping rather than correctness. A pattern
	// rather than a list for the reason [OpenExplorationStore] gives: this directory is
	// one this store creates and fills, so it may name the shape of its own files.
	world.SweepTemporaries(dir, "*"+recordFileExt)
	return &MarkerStore{dir: dir}, nil
}

// Dir is the directory this store writes to. Empty for an ephemeral world.
func (s *MarkerStore) Dir() string {
	if s == nil {
		return ""
	}
	return s.dir
}

// Load reads the marks one character holds.
//
// Three answers, and the middle one carries the weight it does in [Store.Load] and
// [ExplorationStore.Load]: found, absent, or unreadable. A character who has marked
// nothing — or who played before this file existed — has no file, which is an empty map
// rather than an error. A file that exists and cannot be read **is** an error and must
// stay one, so that the caller can decide what to do with the evidence rather than have
// this function decide by answering "no marks".
func (s *MarkerStore) Load(id CharacterID) (StoredMarkers, bool, error) {
	if s == nil || s.dir == "" {
		return StoredMarkers{}, false, nil
	}

	path := s.markerPath(id)
	info, err := os.Stat(path)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return StoredMarkers{}, false, nil
	case err != nil:
		return StoredMarkers{}, false, fmt.Errorf("persist: reading %s: %w", path, err)
	}
	// Before the read, not after: a file this large is not one this format wrote, and
	// finding that out by allocating it is how a corrupt directory becomes an OOM.
	if info.Size() > int64(maxMarkersFileSize) {
		return StoredMarkers{}, false, fmt.Errorf("%w: %s is %d bytes, more than the %d a marker file can need",
			world.ErrCorruptStore, path, info.Size(), maxMarkersFileSize)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		return StoredMarkers{}, false, fmt.Errorf("persist: reading %s: %w", path, err)
	}

	stored, err := decodeMarkers(data)
	if err != nil {
		return StoredMarkers{}, false, fmt.Errorf("%s: %w", path, err)
	}
	return stored, true, nil
}

// Save writes one character's whole map, atomically. A no-op in an ephemeral world.
//
// Whole rather than incremental, for the reason [ExplorationStore.Save] and
// [StructureStore.Save] are: the file is a snapshot of what the session holds rather
// than a log to be replayed, so there is no removal record to lose and no ordering to
// get wrong. It is also what makes a removal free — sixty-four fixed-width entries are
// four kilobytes, rewritten.
//
// **An empty map still writes a file**, header and checksum and a zero count, and it
// still carries the counter. That is the whole reason it is not skipped: a character who
// has removed their last mark must not have their next placement mint id 1 again, and
// "no file" would say exactly that.
func (s *MarkerStore) Save(id CharacterID, stored StoredMarkers) error {
	if s == nil || s.dir == "" {
		return nil
	}

	data, err := encodeMarkers(stored)
	if err != nil {
		return err
	}
	return world.WriteAtomic(s.markerPath(id), data)
}

// Quarantine moves a marker file this build could not read out of the way, and returns
// where it went. A no-op in an ephemeral world, which reports an empty path.
//
// **The file is kept, never deleted and never written over**, which is the doctrine
// [Store.Quarantine] and [ExplorationStore.Quarantine] both keep and the reason all three
// go through the same `setAside`: the bytes are the only evidence of what went wrong, and
// the next save would otherwise replace them. The timestamp in the name is what keeps a
// second quarantine from destroying the first.
//
// What it costs the player is a page of their own writing, which is more than a ledger of
// fog and less than a life — so the caller that reaches for this is expected to survive it
// and let the character in rather than refuse a connection over a map. See
// session.Markers, and the sealed flag there for what happens when the move itself fails.
func (s *MarkerStore) Quarantine(id CharacterID) (string, error) {
	if s == nil || s.dir == "" {
		return "", nil
	}
	return setAside(s.markerPath(id), corruptFileSuffix)
}

// markerPath is where one character's marks live. The hex id is the whole name, for the
// reason [Store.recordPath] gives: every character of it comes from a number this server
// minted, so nothing a client sends reaches the filesystem.
func (s *MarkerStore) markerPath(id CharacterID) string {
	return filepath.Join(s.dir, id.String()+recordFileExt)
}

// encodeMarkers lays the map out, in the order it was given.
//
// **What is refused here is what this build could not read back**, which is the
// [encodeExploration] rule and is stricter here because a mark carries more that can be
// wrong: too many marks, a note too long for its field, a zero id, and an id at or above
// the counter that is supposed to be past all of them. Each is a bug in the caller rather
// than a state a player can reach, and each would otherwise be discovered at the next
// login as a quarantined file.
func encodeMarkers(stored StoredMarkers) ([]byte, error) {
	if len(stored.Markers) > MaxMarkers {
		return nil, fmt.Errorf("%w: %d marks, more than the %d one character's file can hold",
			ErrTooManyMarkers, len(stored.Markers), MaxMarkers)
	}
	if stored.NextID == 0 {
		return nil, errors.New("persist: a marker file must carry a next id of at least 1; zero is the absent-field value no mark may take")
	}
	for _, marker := range stored.Markers {
		if len(marker.Note) > MaxMarkerNote {
			return nil, fmt.Errorf("persist: a mark's note is %d bytes, more than the %d one may carry",
				len(marker.Note), MaxMarkerNote)
		}
		if marker.MarkerID == 0 {
			return nil, errors.New("persist: a mark with no id cannot be written down; zero is the absent-field value")
		}
		if marker.MarkerID >= stored.NextID {
			return nil, fmt.Errorf("persist: a mark carries id %d and the next id to mint is %d; the counter must be past every id it has handed out",
				marker.MarkerID, stored.NextID)
		}
	}

	buf := world.NewRecord(markersHeaderSize, len(stored.Markers)*markerEntrySize,
		markersMagic, MarkersVersion)
	binary.LittleEndian.PutUint64(buf[offMarkersNextID:offMarkersNextID+8], stored.NextID)
	binary.LittleEndian.PutUint32(buf[offMarkersCount:offMarkersCount+4], uint32(len(stored.Markers)))

	for i, marker := range stored.Markers {
		at := markersHeaderSize + i*markerEntrySize
		entry := buf[at : at+markerEntrySize]
		binary.LittleEndian.PutUint64(entry[offMarkerID:offMarkerID+8], marker.MarkerID)
		binary.LittleEndian.PutUint32(entry[offMarkerX:offMarkerX+4], uint32(marker.X))
		binary.LittleEndian.PutUint32(entry[offMarkerZ:offMarkerZ+4], uint32(marker.Z))
		entry[offMarkerKind] = byte(marker.Kind)
		entry[offMarkerNoteLen] = byte(len(marker.Note))
		// The rest of the field is already zero: NewRecord allocates it. Zero-padding is
		// what makes an unchanged map the same bytes twice, which is what lets a test
		// compare files rather than parse them.
		copy(entry[offMarkerNote:offMarkerNote+MaxMarkerNote], marker.Note)
	}

	world.PutChecksum(buf)
	return buf, nil
}

// decodeMarkers parses the map, refusing anything it cannot read exactly.
//
// Validate-everything-then-return, the shape [decodeRecord], [decodeStructures] and
// [decodeExploration] all use: nothing is assembled until every check has passed, so a
// half-valid map is never a value a caller can hold.
//
// **This judges more than a ledger does, and the reason is where the value goes next.** A
// column is two int32s and any pair of them is a place this world could have streamed, so
// there was nothing to check that the checksum had not already caught. A mark is put
// straight on the wire as part of a `MarkerList`, whose decoder invariants are stated in
// schemas/player.fbs: a non-zero id, unique within the list, a known kind, and a note of
// at most 120 valid UTF-8 bytes. A file that cannot produce that is a file this server
// would answer with an illegal frame, so it is refused as corrupt here — which is the one
// place that can still keep the bytes and tell somebody.
func decodeMarkers(data []byte) (StoredMarkers, error) {
	if len(data) < markersHeaderSize+world.ChecksumSize {
		return StoredMarkers{}, fmt.Errorf("%w: %d bytes is shorter than an empty marker file",
			world.ErrCorruptStore, len(data))
	}
	if err := world.CheckHeader(data, markersMagic, MarkersVersion); err != nil {
		return StoredMarkers{}, err
	}
	if err := world.CheckChecksum(data); err != nil {
		return StoredMarkers{}, err
	}

	nextID := binary.LittleEndian.Uint64(data[offMarkersNextID : offMarkersNextID+8])
	count := uint64(binary.LittleEndian.Uint32(data[offMarkersCount : offMarkersCount+4]))
	if count > MaxMarkers {
		return StoredMarkers{}, fmt.Errorf("%w: the file claims %d marks, more than the %d one character may hold",
			world.ErrCorruptStore, count, MaxMarkers)
	}
	// The declared count is checked against the length the file actually has before it
	// indexes anything. A truncated file fails here, which is the case this check exists
	// for: a shorter map is a perfectly plausible one.
	want := uint64(markersHeaderSize) + count*markerEntrySize + world.ChecksumSize
	if want != uint64(len(data)) {
		return StoredMarkers{}, fmt.Errorf("%w: the file claims %d marks, which need %d bytes, but the file is %d",
			world.ErrCorruptStore, count, want, len(data))
	}
	if nextID == 0 {
		// Unconditional, empty file included. Ids start at 1 because zero is the
		// absent-field value, so every file this build writes carries a counter of at
		// least 1 — and a zero one read back would mint that absent value as the next
		// mark's id.
		return StoredMarkers{}, fmt.Errorf("%w: the file says the next id to mint is 0, which is the absent-field value no mark may carry",
			world.ErrCorruptStore)
	}

	markers := make([]protocol.Marker, count)
	// Seen rather than a sort: the order in the file is the caller's, and rearranging it
	// to find duplicates would be this package forming the second opinion about an order
	// that its own doc comment says it has no business having.
	seen := make(map[uint64]struct{}, count)
	for i := range markers {
		at := markersHeaderSize + i*markerEntrySize
		entry := data[at : at+markerEntrySize]

		id := binary.LittleEndian.Uint64(entry[offMarkerID : offMarkerID+8])
		if id == 0 {
			return StoredMarkers{}, fmt.Errorf("%w: mark %d has no id, and zero is the absent-field value a MarkerList may not carry",
				world.ErrCorruptStore, i)
		}
		if id >= nextID {
			return StoredMarkers{}, fmt.Errorf("%w: mark %d carries id %d and the file says the next id to mint is %d; the counter must be past every id it has handed out",
				world.ErrCorruptStore, i, id, nextID)
		}
		if _, twice := seen[id]; twice {
			return StoredMarkers{}, fmt.Errorf("%w: id %d names two marks, and a MarkerList may not carry one id twice",
				world.ErrCorruptStore, id)
		}
		seen[id] = struct{}{}

		kind := vnet.MarkerKind(entry[offMarkerKind])
		if !protocol.MarkerKindOK(kind) {
			return StoredMarkers{}, fmt.Errorf("%w: mark %d carries kind %d, which is not one this contract names",
				world.ErrCorruptStore, i, entry[offMarkerKind])
		}

		noteLen := int(entry[offMarkerNoteLen])
		if noteLen > MaxMarkerNote {
			return StoredMarkers{}, fmt.Errorf("%w: mark %d says its note is %d bytes, more than the %d the field holds",
				world.ErrCorruptStore, i, noteLen, MaxMarkerNote)
		}
		note := entry[offMarkerNote : offMarkerNote+noteLen]
		if !utf8.Valid(note) {
			// string() over invalid bytes succeeds silently in Go, so nothing downstream
			// could tell — the same reason the envelope decoder checks it at the wire.
			return StoredMarkers{}, fmt.Errorf("%w: mark %d carries a note that is not valid UTF-8",
				world.ErrCorruptStore, i)
		}

		markers[i] = protocol.Marker{
			MarkerID: id,
			X:        int32(binary.LittleEndian.Uint32(entry[offMarkerX : offMarkerX+4])),
			Z:        int32(binary.LittleEndian.Uint32(entry[offMarkerZ : offMarkerZ+4])),
			Kind:     kind,
			Note:     string(note),
		}
	}

	return StoredMarkers{NextID: nextID, Markers: markers}, nil
}
