// Package persist stores what the server keeps about a player between connections.
//
// # Keyed by a hash, never by the credential
//
// One file per identity, named for its [identity.PlayerID] — the SHA-256 of the
// token the client presents. The token itself is never written here, and neither is
// anything derived from it that could be presented in its place: a leaked players
// directory is a list of hashes, and a hash is not a way in. That is the whole
// reason the id exists as a separate type from the token.
//
// # The delta store's discipline, reused rather than re-derived
//
// Magic number, format version, trailing CRC-32, temporary-file-and-rename writes,
// temporaries swept on open, unknown versions refused. Every one of those comes
// from internal/world through the helpers it exports for the purpose ([world.WriteAtomic],
// [world.CheckHeader], [world.CheckChecksum], [world.PutChecksum],
// [world.SweepTemporaries]) rather than being written a second time here. The
// version number is this package's own, because a player record and a chunk delta
// change for unrelated reasons.
//
// # Where it lives, and what already guards it
//
// <world-dir>/players/. The world directory's seed and worldgen checks run first
// and are what refuse a directory this server did not write, so nothing
// player-specific needs to re-ask that question — and nothing player-specific is
// written outside it.
//
// # What game does not know about this package
//
// game never imports persist, and must not: this package imports identity, and the
// dependency runs session → persist → world, one way, exactly as the rest of the
// server does. Nor does this package import game, which is the other half of the same
// rule and the reason a life below is four plain fields rather than a game.Life: a
// store writes bytes down, and what those bytes are allowed to *mean* is decided by
// the package holding the item registry and the health bound. session loads a record,
// puts its values through game.Life.Validate, and refuses the whole record if that
// says no — see session.Identities.recall.
package persist

import (
	"encoding/binary"
	"errors"
	"fmt"
	"io/fs"
	"math"
	"os"
	"path/filepath"
	"time"
	"unicode/utf8"

	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// StoreVersion is the on-disk format version of a player record.
//
// Bump it for any change to the layout below, including one that only adds a field:
// a reader of an older build must refuse a newer record rather than parse a prefix
// of it. Deliberately separate from world.StoreVersion — see the package comment.
//
// **2 adds the life: position, yaw, health and every slot.** No migration, and none is
// possible: a v1 record holds a display name and a timestamp, which is not enough to
// reconstruct where anybody stood. Nothing has shipped, so there is nothing to migrate
// — a v1 file is refused by [world.CheckHeader] like any other version this build does
// not speak, and the connection presenting its token is admitted as a new player.
const StoreVersion uint32 = 2

// MaxNameBytes is the longest display name a record keeps.
//
// A cap rather than a validation: player_name is untrusted display text of any
// length the frame can hold, and a record is a fixed-shape thing on the server's own
// disk. Names longer than this are truncated on the way in — at a rune boundary, so
// what is stored is still the text it was a prefix of — and never refused, because a
// long name is not a reason to turn a player away.
const MaxNameBytes = 64

// On-disk layout, little-endian throughout, one file per identity.
//
//	players/<player-id-hex>.bin
//	    magic[4] version:u32 last_seen:i64
//	    pos:3×f64 yaw:f64 health:u16
//	    slots:InventorySlots × (item:u16 count:u16 durability:u16 max_durability:u16)
//	    name_len:u16 name[name_len] crc32:u32
//
// Everything fixed-width first and the one variable-length field last, so the only
// length the decoder has to reason about is the name's — a truncated file fails the
// exact-size check below rather than being read as a shorter pack.
//
// The slots are a whole table, always, empty ones included. A record is rewritten whole
// on every save, so there is nothing to gain by omitting the empties and something to
// lose: a slot's index *is* its identity to the client, and a sparse encoding would put
// that mapping in the file instead of in the layout.
//
// The id is in the file name and not in the record, unlike the chunk store's
// coordinate. It could not be a check if it were: the name is a hash of a secret, so
// a record copied to the wrong name is not something this package can detect by
// re-reading a field — only the client's token decides which file is looked up.
const (
	playersDirName = "players"
	recordFileExt  = ".bin"

	// corruptFileSuffix marks a record this build could not read. See Store.Quarantine.
	corruptFileSuffix = ".corrupt"

	// slotSize is one slot's four uint16s: item, count, durability, max durability.
	slotSize  = 8
	slotsSize = int(protocol.InventorySlots) * slotSize

	offLastSeen = world.HeaderSize
	offPos      = offLastSeen + 8
	offYaw      = offPos + 3*8
	offHealth   = offYaw + 8
	offSlots    = offHealth + 2
	offNameLen  = offSlots + slotsSize

	recordHeaderSize = offNameLen + 2
	maxRecordSize    = recordHeaderSize + MaxNameBytes + world.ChecksumSize
)

var playerMagic = [4]byte{'V', 'X', 'H', 'P'}

// Record is what the server remembers about one player between connections.
//
// The life — position, yaw, health, slots — is written verbatim and read back
// verbatim. **This package judges none of it**, and that is deliberate rather than an
// omission: whether an item id exists and how much health is a full bar are the item
// registry's answers, and it lives in internal/game. Everything here checks is what a
// *file* can be wrong about — magic, version, checksum, size — and the caller puts the
// values through game.Life.Validate before a player is built from them. Two half-copies
// of one rule is two rules the first time either is edited.
type Record struct {
	// Name is the display name the player last connected with, truncated to
	// MaxNameBytes. Untrusted text, kept for a log line and an operator's eye;
	// nothing keys on it and it is not unique.
	Name string

	// LastSeen is when the player's last session ended, to the second. Written at
	// teardown, which is the only moment the server knows the answer.
	LastSeen time.Time

	// Pos is where the player stood, in the simulation's own float64. Not narrowed to
	// the float32 the wire carries: the server's position is the authoritative one, and
	// rounding it through a save would move every player a hair on every reconnect.
	Pos [3]float64

	// Yaw is which way they faced, in radians.
	Yaw float64

	// Health is what they had left. Always non-zero in a record this server wrote — a
	// record describes a living player, because a dead one is written as their respawn
	// would have left them.
	Health uint16

	// Slots is the whole pack, in the shape the wire announces, so a stored pack and a
	// sent InventoryState are the same value rather than two that have to agree.
	Slots [protocol.InventorySlots]protocol.InventoryStack
}

// Store is one world's players directory.
//
// Pure I/O and safe for concurrent use: every method touches the path of exactly one
// identity, and one identity has at most one live session by construction (see
// session.Identities), so two goroutines never write the same file.
//
// **A nil *Store is the ephemeral world**, and every method is a no-op on one rather
// than a branch at each call site — the same shape world.Cache uses for a nil
// world.Store, and for the same reason.
type Store struct {
	dir string
}

// OpenStore opens the players directory under worldDir, creating it if it is not
// there.
//
// worldDir is the operator's -world-dir, already opened and seed-checked by
// world.OpenStore: this runs after it, so a directory belonging to another seed has
// already been refused and no player record is written into it.
func OpenStore(worldDir string) (*Store, error) {
	if worldDir == "" {
		// Not a nil store returned quietly: an empty -world-dir is the ephemeral
		// world, and choosing it is main's decision to make rather than a shape this
		// constructor should accept and forget about.
		return nil, errors.New("persist: the world directory must be named")
	}

	dir := filepath.Join(worldDir, playersDirName)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return nil, fmt.Errorf("persist: creating %s: %w", dir, err)
	}

	// Whatever a crash left mid-rename. Inert, because a reader only ever opens an
	// exact <id>.bin path, so this is housekeeping rather than correctness.
	world.SweepTemporaries(dir)
	return &Store{dir: dir}, nil
}

// Dir is the players directory this store writes to. Empty for an ephemeral world.
func (s *Store) Dir() string {
	if s == nil {
		return ""
	}
	return s.dir
}

// Load reads the record stored for id.
//
// Three answers, and the middle one is the point: found, not found, or unreadable.
// An identity with no file is not an error — it is a token this server has never
// issued, and the handshake mints a new one for it. A file that exists and cannot be
// read is an error and must stay one: reporting it as "not found" would mint a new
// identity whose first teardown writes over the record nobody could read, which
// turns one corrupt file into a lost player.
func (s *Store) Load(id identity.PlayerID) (Record, bool, error) {
	if s == nil {
		return Record{}, false, nil
	}
	path := s.recordPath(id)

	info, err := os.Stat(path)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return Record{}, false, nil
	case err != nil:
		return Record{}, false, fmt.Errorf("persist: reading %s: %w", path, err)
	}
	// Before the read, not after: a file this large is not one this format wrote, and
	// finding that out by allocating it is how a corrupt directory becomes an OOM.
	// The conversion is explicit because the size constants stopped being untyped when
	// the slot table gave them an int factor; the comparison is the same one.
	if info.Size() > int64(maxRecordSize) {
		return Record{}, false, fmt.Errorf("%w: %s is %d bytes, more than the %d a player record can need",
			world.ErrCorruptStore, path, info.Size(), maxRecordSize)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		return Record{}, false, fmt.Errorf("persist: reading %s: %w", path, err)
	}

	rec, err := decodeRecord(data)
	if err != nil {
		return Record{}, false, fmt.Errorf("%s: %w", path, err)
	}
	return rec, true, nil
}

// Save writes id's record, atomically. A no-op in an ephemeral world.
//
// The name is truncated here rather than by the caller, so the cap is a property of
// the format instead of a rule every writer has to remember.
func (s *Store) Save(id identity.PlayerID, rec Record) error {
	if s == nil {
		return nil
	}
	return world.WriteAtomic(s.recordPath(id), encodeRecord(rec))
}

// Quarantine moves a record this build could not use out of the way, and returns where
// it went. A no-op in an ephemeral world, which reports an empty path.
//
// **The file is kept, never deleted and never written over.** A record that fails to
// load is the only evidence of what a player had, and the bug that produced it is a bug
// somebody will want to read the bytes of. Deleting it — or leaving it in place for the
// next save to replace — turns "one player lost an evening" into "nobody can ever find
// out why".
//
// The timestamp in the name is not decoration: renaming to a fixed `.corrupt` would
// destroy the *previous* corrupt record the second time this ran, which is the same
// silent overwrite this function exists to prevent.
func (s *Store) Quarantine(id identity.PlayerID) (string, error) {
	if s == nil {
		return "", nil
	}

	path := s.recordPath(id)
	aside := fmt.Sprintf("%s%s.%d", path, corruptFileSuffix, time.Now().UTC().UnixNano())
	if err := os.Rename(path, aside); err != nil {
		return "", fmt.Errorf("persist: setting %s aside: %w", path, err)
	}
	return aside, nil
}

// recordPath is where one identity's record lives. The hex id is the whole name:
// fixed length, and every character comes from a digest, so nothing a client sends
// reaches the filesystem.
func (s *Store) recordPath(id identity.PlayerID) string {
	return filepath.Join(s.dir, id.String()+recordFileExt)
}

func encodeRecord(rec Record) []byte {
	name := truncateName(rec.Name)

	buf := make([]byte, recordHeaderSize+len(name)+world.ChecksumSize)
	copy(buf[0:4], playerMagic[:])
	binary.LittleEndian.PutUint32(buf[4:8], StoreVersion)
	// Seconds, in UTC, because a record is compared by a person reading a log rather
	// than by anything that needs sub-second resolution — and because a zero time
	// round-trips through Unix seconds unambiguously.
	binary.LittleEndian.PutUint64(buf[offLastSeen:offLastSeen+8], uint64(rec.LastSeen.UTC().Unix()))

	for axis, value := range rec.Pos {
		at := offPos + axis*8
		binary.LittleEndian.PutUint64(buf[at:at+8], math.Float64bits(value))
	}
	binary.LittleEndian.PutUint64(buf[offYaw:offYaw+8], math.Float64bits(rec.Yaw))
	binary.LittleEndian.PutUint16(buf[offHealth:offHealth+2], rec.Health)

	for slot, stack := range rec.Slots {
		at := offSlots + slot*slotSize
		binary.LittleEndian.PutUint16(buf[at:at+2], stack.ItemID)
		binary.LittleEndian.PutUint16(buf[at+2:at+4], stack.Count)
		binary.LittleEndian.PutUint16(buf[at+4:at+6], stack.Durability)
		binary.LittleEndian.PutUint16(buf[at+6:at+8], stack.MaxDurability)
	}

	binary.LittleEndian.PutUint16(buf[offNameLen:offNameLen+2], uint16(len(name)))
	copy(buf[recordHeaderSize:], name)

	world.PutChecksum(buf)
	return buf
}

// decodeRecord parses one player record, refusing anything it cannot read exactly.
//
// Validate-everything-then-return, the shape world.decodeChunkFile uses: nothing is
// assembled until every check has passed, so a half-valid record is never a value a
// caller can hold. What it checks is the *file* — see the Record doc for why the life's
// own values are judged one layer up.
func decodeRecord(data []byte) (Record, error) {
	if len(data) < recordHeaderSize+world.ChecksumSize {
		return Record{}, fmt.Errorf("%w: %d bytes is shorter than an empty player record",
			world.ErrCorruptStore, len(data))
	}
	if err := world.CheckHeader(data, playerMagic, StoreVersion); err != nil {
		return Record{}, err
	}
	if err := world.CheckChecksum(data); err != nil {
		return Record{}, err
	}

	// The declared length is checked against the length the file actually has before
	// it indexes anything. A truncated record fails here, which is the case this check
	// exists for: a shorter name is a perfectly plausible one.
	nameLen := uint64(binary.LittleEndian.Uint16(data[offNameLen : offNameLen+2]))
	if want := uint64(recordHeaderSize) + nameLen + world.ChecksumSize; want != uint64(len(data)) {
		return Record{}, fmt.Errorf("%w: the record claims a %d-byte name, which needs %d bytes, but the file is %d",
			world.ErrCorruptStore, nameLen, want, len(data))
	}

	rec := Record{
		Name:     string(data[recordHeaderSize : uint64(recordHeaderSize)+nameLen]),
		LastSeen: time.Unix(int64(binary.LittleEndian.Uint64(data[offLastSeen:offLastSeen+8])), 0).UTC(),
		Yaw:      math.Float64frombits(binary.LittleEndian.Uint64(data[offYaw : offYaw+8])),
		Health:   binary.LittleEndian.Uint16(data[offHealth : offHealth+2]),
	}
	for axis := range rec.Pos {
		at := offPos + axis*8
		rec.Pos[axis] = math.Float64frombits(binary.LittleEndian.Uint64(data[at : at+8]))
	}
	for slot := range rec.Slots {
		at := offSlots + slot*slotSize
		rec.Slots[slot] = protocol.InventoryStack{
			ItemID:        binary.LittleEndian.Uint16(data[at : at+2]),
			Count:         binary.LittleEndian.Uint16(data[at+2 : at+4]),
			Durability:    binary.LittleEndian.Uint16(data[at+4 : at+6]),
			MaxDurability: binary.LittleEndian.Uint16(data[at+6 : at+8]),
		}
	}
	return rec, nil
}

// truncateName cuts name to at most MaxNameBytes without splitting a rune.
//
// The rune boundary is the whole subtlety: player_name is untrusted UTF-8 of the
// client's choosing, and a cut through the middle of a multi-byte rune stores text
// that no longer decodes — a replacement character in an operator's log, from a name
// that was fine.
func truncateName(name string) string {
	if len(name) <= MaxNameBytes {
		return name
	}
	cut := MaxNameBytes
	for cut > 0 && !utf8.RuneStart(name[cut]) {
		cut--
	}
	return name[:cut]
}
