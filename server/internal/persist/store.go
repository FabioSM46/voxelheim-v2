// Package persist stores what the server keeps about a player between connections.
//
// # Keyed by a character, owned by an account
//
// One file per **character**, named for the [CharacterID] this server minted for it.
// An account may hold several characters on one world — that is the whole reason the
// key is not the account — and the account that owns one is a field inside the record
// rather than the name of the file. Keyed by the account you are back to one character
// per world; keyed by the character, "every character this account owns here" is a
// filter over an index instead of a lookup.
//
// The owner is written down as an [identity.PlayerID], the one-way hash of the account,
// and never as the account itself. A leaked players directory is therefore a directory
// of digests and a list of digests, which is not a way in and is not a list of who
// plays here. That is the whole reason the id exists as a separate type from the
// account.
//
// # The index, and why it is built once
//
// A name is unique within a world and an account's characters have to be found on every
// connection. Both are map hits: [OpenStore] reads the directory once, at startup, and
// keeps a character's id, owner and name in memory. Nothing walks the directory again —
// a join that did would cost an account with one character a read of every character in
// the world. See characters.go, which owns the index and the rules over it.
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
	"io"
	"io/fs"
	"math"
	"os"
	"path/filepath"
	"sync"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
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
// **2 added the life: position, yaw, health and every slot.**
//
// **3 makes the record a character's rather than an identity's.** It gains the id the
// character is keyed by and the account that owns it, and its name stops being a
// display string nothing keyed on. There is no migration and there deliberately is
// none: a v2 record names a player by the hash of their account, which is not enough to
// say which of that account's characters it was, and the code to guess would outlive by
// years the single event it serves. A v2 record is refused by [world.CheckHeader] like
// any other version this build does not speak — and a whole *directory* of them is set
// aside on the first start under this format rather than read. See [OpenStore].
//
// **4 adds the appearance**, because a character's face is chosen once and has to
// survive every session after it: schemas/player.fbs says an appearance is read from
// the stored character and never from anything a client said at join time, and a store
// with nowhere to put one leaves the server nothing truthful to answer with. Still no
// migration, for a smaller version of the same reason: a v3 record does not say what
// its character looks like, and the only value this build could invent is the
// placeholder the contract reserves for an appearance that has not *arrived* — which is
// a different claim entirely.
const StoreVersion uint32 = 4

// On-disk layout, little-endian throughout, one file per character.
//
//	players/<character-id-hex>.bin
//	    magic[4] version:u32 last_seen:i64
//	    character_id:u64 owner:32
//	    skin:u32 shirt:u32 trousers:u32 shoes:u32 hair:u32 hair_model:u8
//	    pos:3×f64 yaw:f64 health:u16
//	    slots:InventorySlots × (item:u16 count:u16 durability:u16 max_durability:u16)
//	    name_len:u16 name[name_len] crc32:u32
//
// Everything fixed-width first and the one variable-length field last, so the only
// length the decoder has to reason about is the name's — a truncated file fails the
// exact-size check below rather than being read as a shorter pack.
//
// The appearance sits with the id, the owner and the name rather than with the life,
// because that is what it is: chosen once when the character is created and never
// written again, where every value below it is whatever the last session left. Its five
// colours are written as the wire carries them — 0x00RRGGBB, the high byte reserved —
// so the number in the file is the number in the frame, with no conversion to agree on.
//
// The slots are a whole table, always, empty ones included. A record is rewritten whole
// on every save, so there is nothing to gain by omitting the empties and something to
// lose: a slot's index *is* its identity to the client, and a sparse encoding would put
// that mapping in the file instead of in the layout.
//
// **The id is in the record as well as in the file name, and that reasoning inverted
// with #103.** It could not be a check while the name was a hash of a secret: a record
// copied to the wrong name was not something this package could detect, because only
// the client's token decided which file was opened. A character id is a number this
// server minted and nothing derives it from a credential, so the two can be compared —
// and the startup scan does compare them, refusing a record that does not agree with
// the name it was found under.
const (
	playersDirName = "players"
	recordFileExt  = ".bin"

	// corruptFileSuffix marks a record this build could not read. See Store.Quarantine.
	corruptFileSuffix = ".corrupt"

	// supersededSuffix marks a players directory written in a format this build does
	// not speak, and it names the format this build *does* speak: a directory set aside
	// by this version becomes players.pre-v4.<timestamp>.
	//
	// It said `.pre-accounts` while there had been exactly one such move, which was
	// true of the format that introduced characters and stopped being true the moment a
	// second version existed — a v3 directory is not one from before accounts, and a
	// name that says so is a name an operator has to disbelieve. Built from
	// [StoreVersion] so the next bump needs no edit here. See [Store.setAsideSuperseded].
	supersededSuffix = ".pre-v"

	// slotSize is one slot's four uint16s: item, count, durability, max durability.
	slotSize  = 8
	slotsSize = int(protocol.InventorySlots) * slotSize

	// appearanceSize is the five colours and the hair model, in the order the layout
	// above names them.
	appearanceSize = 5*4 + 1

	offLastSeen   = world.HeaderSize
	offCharacter  = offLastSeen + 8
	offOwner      = offCharacter + 8
	offAppearance = offOwner + identity.IDSize
	offPos        = offAppearance + appearanceSize
	offYaw        = offPos + 3*8
	offHealth     = offYaw + 8
	offSlots      = offHealth + 2
	offNameLen    = offSlots + slotsSize

	recordHeaderSize = offNameLen + 2
	maxRecordSize    = recordHeaderSize + MaxNameBytes + world.ChecksumSize
)

var playerMagic = [4]byte{'V', 'X', 'H', 'P'}

// Record is what the server remembers about one character between connections.
//
// The life — position, yaw, health, slots — is written verbatim and read back
// verbatim. **This package judges none of it**, and that is deliberate rather than an
// omission: whether an item id exists and how much health is a full bar are the item
// registry's answers, and it lives in internal/game. Everything here checks is what a
// *file* can be wrong about — magic, version, checksum, size — and the caller puts the
// values through game.Life.Validate before a player is built from them. Two half-copies
// of one rule is two rules the first time either is edited.
type Record struct {
	// Character is the character this record belongs to, and Owner is the account that
	// owns it. Name is that character's name.
	//
	// **All three are the store's and not the caller's.** [Store.Save] fills them from
	// its index and ignores whatever a caller put here, because a caller that could set
	// them could move a character to another account or rename it past the uniqueness
	// check — which is authoritative logic, and it lives in [Store.Create]. They are
	// fields of this struct because they are read back out of the file: it is the
	// records that the index is rebuilt from at startup.
	Character CharacterID
	Owner     identity.PlayerID
	Name      string

	// Appearance is what this character looks like, and it is the store's too, for the
	// same reason and one more: it is chosen once, at creation, and there is no message
	// in this contract that changes it. A save that could restate it would be a way to
	// become somebody else between two sessions.
	//
	// Written down as given. **This package judges no more of it than it judges a life**
	// — schemas/common.fbs puts that obligation on whatever accepts a
	// CreateCharacterRequest, before the value ever reaches here. What the startup scan
	// does check is the narrower thing it checks a name for: whether the *index* can
	// carry it. See [Store.readIndexed].
	Appearance protocol.Appearance

	// LastSeen is when the character's last session ended, to the second. Written at
	// teardown, which is the only moment the server knows the answer.
	LastSeen time.Time

	// Pos is where the player stood, in the simulation's own float64. Not narrowed to
	// the float32 the wire carries: the server's position is the authoritative one, and
	// rounding it through a save would move every player a hair on every reconnect.
	Pos [3]float64

	// Yaw is which way they faced, in radians.
	Yaw float64

	// Health is what they had left. Always non-zero in a record a session wrote — a
	// record describes a living player, because a dead one is written as their respawn
	// would have left them.
	//
	// Zero in the one record no session wrote: the first, laid down by [Store.Create].
	// See [Record.Unplayed].
	Health uint16

	// Slots is the whole pack, in the shape the wire announces, so a stored pack and a
	// sent InventoryState are the same value rather than two that have to agree.
	Slots [protocol.InventorySlots]protocol.InventoryStack
}

// Unplayed reports the record of a character that exists and has never had a session.
//
// **Zero health is the marker, and it is not an arbitrary one.** This format has always
// documented that a record written for a live player carries a non-zero health, because
// a dead player is written as their respawn would have left them — so zero was already
// a value no session could produce. [Store.Create] lays one down so that a character
// exists on disk the moment it exists in the index, which is what lets the index be
// rebuilt from the records alone, and this is how a reader tells that first record from
// a life.
//
// A reader that treated it as a life instead would hand game a health of zero, be told
// the record is invalid, and set aside the file the store had just written — turning
// every new character into a quarantine.
func (r Record) Unplayed() bool { return r.Health == 0 }

// Store is one world's players directory and the index over it.
//
// The file half needs no lock of its own: every path is derived from one character id,
// and a character has at most one live session by construction — one account holds one
// session (see session.Identities) and one session plays one character — so two
// goroutines never write the same file. The index half is guarded by [Store.mu].
// [Store.Load] never takes it; [Store.Save] takes it only to look up which character it
// is writing; [Store.Create] is the one method that holds it across a write, and
// characters.go says why.
//
// **A nil *Store keeps nothing at all** and every method is a no-op on one rather than a
// branch at each call site, the same shape world.Cache uses for a nil world.Store. It is
// not the ephemeral world, though: an ephemeral world still has to refuse a name
// somebody else is already playing under, because that is a rule about the world rather
// than about the disk. [NewMemoryStore] is what an ephemeral world gets — a real index
// with no directory under it.
type Store struct {
	// dir is the players directory. Empty for a store that writes nothing.
	dir string

	// setAside is where a pre-character players directory was moved, and empty when
	// there was nothing to move. Reported so that whoever opened the store can say so
	// in a log line: this package writes to no logger.
	setAside string

	// unreadable is every file the startup scan could not index and set aside instead,
	// by the path it went to. Reported for the same reason.
	unreadable []string

	mu      sync.Mutex
	byID    map[CharacterID]Character
	byName  map[string]CharacterID
	byOwner map[identity.PlayerID][]CharacterID
}

// NewMemoryStore returns a store with an index and no directory: the ephemeral world.
//
// Names are still unique, an account still holds at most
// [MaxCharactersPerAccount] characters, and every refusal is decided exactly as it is
// with a directory — because those are the world's rules and not the disk's. What an
// ephemeral world costs is the life: nothing is written, so nothing is ever found, and
// every character it minted is gone when the process ends.
func NewMemoryStore() *Store {
	return &Store{
		byID:    make(map[CharacterID]Character),
		byName:  make(map[string]CharacterID),
		byOwner: make(map[identity.PlayerID][]CharacterID),
	}
}

// OpenStore opens the players directory under worldDir, creating it if it is not
// there, and builds the character index from what it holds.
//
// worldDir is the operator's -world-dir, already opened and seed-checked by
// world.OpenStore: this runs after it, so a directory belonging to another seed has
// already been refused and no player record is written into it.
//
// **The first start under this format sets a superseded directory aside**, before
// anything else happens to it. See [Store.setAsideSuperseded].
func OpenStore(worldDir string) (*Store, error) {
	if worldDir == "" {
		// Not a nil store returned quietly: an empty -world-dir is the ephemeral
		// world, and choosing it is main's decision to make rather than a shape this
		// constructor should accept and forget about. NewMemoryStore is the store an
		// ephemeral world gets, and main is what asks for it.
		return nil, errors.New("persist: the world directory must be named")
	}

	s := NewMemoryStore()
	s.dir = filepath.Join(worldDir, playersDirName)
	if err := os.MkdirAll(s.dir, 0o755); err != nil {
		return nil, fmt.Errorf("persist: creating %s: %w", s.dir, err)
	}

	// Before the sweep and before the scan: whatever is in a superseded directory moves
	// whole, temporaries and all, so that "nothing was deleted" is true of every byte in
	// it rather than of the records alone.
	moved, err := s.setAsideSuperseded()

	if err != nil {
		return nil, err
	}
	if moved {
		return s, nil
	}

	// Whatever a crash left mid-rename. Inert, because a reader only ever opens an
	// exact <character-id>.bin path, so this is housekeeping rather than correctness.
	//
	// The pattern is what keeps it to temporaries of this store's own records. It can
	// be a pattern rather than a list because this directory is one this store creates
	// and fills — unlike the world directory above it, which is the operator's (#137).
	// This store writes exactly one kind of file, so one destination name is the whole
	// list; the variadic fails closed, so a second kind would have to be named here.
	world.SweepTemporaries(s.dir, "*"+recordFileExt)

	if err := s.index(); err != nil {
		return nil, err
	}
	return s, nil
}

// Dir is the players directory this store writes to. Empty for a store with no
// directory under it.
func (s *Store) Dir() string {
	if s == nil {
		return ""
	}
	return s.dir
}

// SetAside is where a superseded players directory was moved when this store was
// opened, and empty when there was nothing to move.
func (s *Store) SetAside() string {
	if s == nil {
		return ""
	}
	return s.setAside
}

// Unreadable is every file the startup scan could not index, by the path it was set
// aside to. Empty for the ordinary case.
func (s *Store) Unreadable() []string {
	if s == nil {
		return nil
	}
	return s.unreadable
}

// setAsideSuperseded moves the whole players directory out of the way when it holds
// records in a format this build does not speak, and reports whether it did.
//
// **Nothing is deleted and nothing is written over.** It is the doctrine
// [Store.Quarantine] keeps, one level up: the directory is the only evidence of what
// every player on this world had, and a format change is not a reason to lose it. The
// records inside are unreadable to this build — a v2 record names a player by the hash
// of their account and cannot say which character it was; a v3 record cannot say what
// its character looks like — so there is no migration to run and deliberately none
// written. What there is, is a directory an operator can copy somewhere and open at
// their leisure.
//
// The timestamp in the name is the same decision Quarantine records and not decoration:
// a fixed name would be destroyed by the second run that found something to move, which
// is the silent overwrite this exists to prevent. The version in it is the other half:
// an operator who finds two of these needs to know which format each holds, and this
// build can only name the one it speaks.
//
// A directory is detected from its contents rather than from a marker file: any record
// carrying this store's magic and a version that is not this build's is a record from
// another format, and one is enough. A version *newer* than this build's is the one
// case that refuses to start instead — moving a newer build's directory aside would be
// this build deciding it knows better than the one that wrote it, and an operator who
// downgraded by accident should find that out before a player does.
func (s *Store) setAsideSuperseded() (bool, error) {

	entries, err := os.ReadDir(s.dir)
	if err != nil {
		return false, fmt.Errorf("persist: reading %s: %w", s.dir, err)
	}

	older := false
	for _, entry := range entries {
		if !entry.Type().IsRegular() || filepath.Ext(entry.Name()) != recordFileExt {
			continue
		}
		version, ours := recordVersion(filepath.Join(s.dir, entry.Name()))
		switch {
		case !ours || version == StoreVersion:
			continue
		case version > StoreVersion:
			return false, fmt.Errorf("%w: %s was written by a build that speaks format version %d; this build speaks %d and will not move a newer world aside",
				world.ErrCorruptStore, s.dir, version, StoreVersion)
		default:
			older = true
		}
	}
	if !older {
		return false, nil
	}

	aside := fmt.Sprintf("%s%s%d.%d", s.dir, supersededSuffix, StoreVersion, time.Now().UTC().UnixNano())

	if err := os.Rename(s.dir, aside); err != nil {
		return false, fmt.Errorf("persist: setting %s aside: %w", s.dir, err)
	}
	if err := os.MkdirAll(s.dir, 0o755); err != nil {
		return false, fmt.Errorf("persist: creating %s after setting the previous one aside: %w", s.dir, err)
	}
	s.setAside = aside
	return true, nil
}

// recordVersion reads the format version out of a file's header, and reports whether
// the file is one this package wrote at all.
//
// Eight bytes, not the whole file: this runs over every record in the directory before
// anything has been indexed, and the question at that point is only which format the
// directory is in.
func recordVersion(path string) (uint32, bool) {
	file, err := os.Open(path)
	if err != nil {
		return 0, false
	}
	defer func() { _ = file.Close() }()

	var header [world.HeaderSize]byte
	if _, err := io.ReadFull(file, header[:]); err != nil {
		return 0, false
	}
	if [4]byte(header[0:4]) != playerMagic {
		return 0, false
	}
	return binary.LittleEndian.Uint32(header[4:8]), true
}

// errRecordGone reports a record that disappeared between the directory listing and the
// read of it. It is deliberately not a [world.ErrCorruptStore]: nothing is wrong with the
// store, and treating it as corruption is what made a vanished file refuse a start.
var errRecordGone = errors.New("persist: a record went away while the store was being opened")

// index reads every record in the directory once and builds the character index from
// it.
//
// **The records are the source of truth and the index is derived**, which is what makes
// a restart preserve every character without a second file to keep in step with the
// first. A file this scan cannot use is set aside rather than skipped: skipping it would
// leave its id free for a later mint to write over and its name free for a later
// character to take, and the first of those loses the evidence Quarantine exists to keep.
//
// A damaged record does not refuse to start. A world with one is a world that has lost
// one character, and refusing to open would take every other character, the terrain and
// the ability to log in at all hostage to it — the same call restoreStructures makes
// about the camp. A set-aside that *fails* does refuse, for the reason
// [Store.setAsideUnreadable] gives: a file left where the index would read it is one a
// later mint can write over.
//
// It reads and writes the index without taking [Store.mu], which is safe for the one
// reason that never generalises: this runs inside [OpenStore], before the store has
// been returned to anybody, so there is no second goroutine for the lock to exclude.
func (s *Store) index() error {
	entries, err := os.ReadDir(s.dir)
	if err != nil {
		return fmt.Errorf("persist: reading %s: %w", s.dir, err)
	}

	for _, entry := range entries {
		if !entry.Type().IsRegular() || filepath.Ext(entry.Name()) != recordFileExt {
			continue
		}
		path := filepath.Join(s.dir, entry.Name())

		character, err := s.readIndexed(path, entry.Name())
		if err != nil {
			if errors.Is(err, errRecordGone) {
				// Nothing to set aside and nothing lost: a record that vanished between
				// the directory listing and the read is a transient, and a retry sails
				// through it. Refusing to start would be refusing over a condition that
				// has already resolved itself.
				continue
			}
			if aErr := s.setAsideUnreadable(path); aErr != nil {
				return aErr
			}
			continue
		}
		s.insertLocked(character)
	}
	return nil
}

// readIndexed reads one record and answers the character it describes, refusing
// anything the index cannot hold.
//
// Five ways a file is refused beyond the ones decodeRecord already covers, and each is
// a way the index would otherwise be wrong rather than a way the file is:
//
//   - a name that is not sixteen hex characters — not a name this store writes;
//   - a record whose own character id is not the one it was found under. The id is in
//     both places precisely so this can be asked;
//   - a name this world would not accept, which includes the empty one;
//   - a name a character already indexed is wearing. Only this server writes here, so
//     that is tampering or a bug rather than a race — and either way one of the two has
//     to go somewhere an operator can find it. Which one loses is deterministic rather
//     than arbitrary: os.ReadDir sorts by file name, a file name is the character id in
//     fixed-width hex, so the higher id is the one that arrives second and is kept aside;
//   - an appearance the contract forbids. **The one check here that is not about a key,
//     and it earns its place from what the index feeds**: every character in it is a row
//     in a `ServerCharacterList`, and a summary carrying a hair model no member names is
//     a frame every client is required to refuse. Left in, one damaged record would cost
//     that account not one character but the whole list — and with it every way into the
//     world. The rule itself is not restated here: [protocol.Appearance.Validate] is the
//     one implementation, and this package still judges no life.
func (s *Store) readIndexed(path, base string) (Character, error) {

	id, named := parseCharacterID(base[:len(base)-len(recordFileExt)])
	if !named {
		return Character{}, fmt.Errorf("%w: %s is not a name a character record is written under", world.ErrCorruptStore, base)
	}

	rec, found, err := s.read(path)
	switch {
	case err != nil:
		return Character{}, err
	case !found:
		// Raced by something outside this server; there is nothing to index and
		// nothing to set aside — and this sentinel is what makes that comment true.
		// It said so already while returning ErrCorruptStore, which index() sets aside
		// like any other unreadable record: the rename then failed with "no such file
		// or directory" and the server refused to boot over a file that had simply
		// gone, naming a path that no longer existed (found in review on #156).
		return Character{}, fmt.Errorf("%w: %s", errRecordGone, base)
	case rec.Character != id:
		return Character{}, fmt.Errorf("%w: %s holds the record of character %s", world.ErrCorruptStore, base, rec.Character)
	}

	accepted, folded, err := acceptName(rec.Name)
	if err != nil {
		return Character{}, fmt.Errorf("%w: %s: %w", world.ErrCorruptStore, base, err)
	}
	if _, taken := s.byName[folded]; taken {
		return Character{}, fmt.Errorf("%w: %s wears a name another character already has", world.ErrCorruptStore, base)
	}
	if err := rec.Appearance.Validate(); err != nil {
		return Character{}, fmt.Errorf("%w: %s: %w", world.ErrCorruptStore, base, err)
	}
	return Character{ID: id, Owner: rec.Owner, Name: accepted, Appearance: rec.Appearance}, nil
}

// setAsideUnreadable moves a file the index could not use out of the way and records
// where it went.
//
// A failure is returned rather than survived, and for the reason [Store.Quarantine]
// gives: a file left in place is one a later mint or a later name can write over, and
// this runs at startup where an operator is reading.
func (s *Store) setAsideUnreadable(path string) error {
	aside, err := setAside(path, corruptFileSuffix)
	if err != nil {
		return err
	}
	s.unreadable = append(s.unreadable, aside)
	return nil
}

// Load reads the record stored for a character.
//
// Three answers, and the middle one is the point: found, not found, or unreadable.
// A character with no file is not an error — [Store.Create] writes one, but an
// ephemeral world writes nothing at all — and the caller starts that character fresh.
// A file that exists and cannot be read is an error and must stay one: reporting it as
// "not found" would admit the character and let its first teardown write over the
// record nobody could read, which turns one corrupt file into a lost character.
func (s *Store) Load(id CharacterID) (Record, bool, error) {
	if s == nil || s.dir == "" {
		return Record{}, false, nil
	}
	return s.read(s.recordPath(id))
}

// read is Load without the ephemeral guard and without deriving the path, so that the
// startup scan can read a file it found rather than one it named.
func (s *Store) read(path string) (Record, bool, error) {
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

// Save writes a character's record, atomically. A no-op on a store with no directory.
//
// **The four fields that name a character are taken from the index and not from rec**,
// whatever the caller put in them. Which account owns a character, what it is called and
// what it looks like are decisions [Store.Create] made under a lock, and a save that
// could restate them would be a second, unlocked way to rename a character, move it
// between accounts, or come back from a session wearing somebody else's face.
//
// An id this store has never minted is [ErrUnknownCharacter] rather than a new file:
// the index is what says a character exists, so writing one it does not know about
// would create a character no lookup could find.
func (s *Store) Save(id CharacterID, rec Record) error {
	if s == nil {
		return nil
	}

	character, known := s.Character(id)
	if !known {
		return fmt.Errorf("%w: %s", ErrUnknownCharacter, id)
	}
	return s.writeRecord(character, rec)
}

// writeRecord puts one record on disk under the character's own name, owner and id. It
// takes no lock, so [Store.Create] can call it while holding one.
func (s *Store) writeRecord(character Character, rec Record) error {
	if s.dir == "" {
		return nil
	}

	rec.Character = character.ID
	rec.Owner = character.Owner
	rec.Name = character.Name
	rec.Appearance = character.Appearance
	return world.WriteAtomic(s.recordPath(character.ID), encodeRecord(rec))

}

// Quarantine moves a record this build could not use out of the way, and returns where
// it went. A no-op on a store with no directory, which reports an empty path.
//
// **The file is kept, never deleted and never written over.** A record that fails to
// load is the only evidence of what a player had, and the bug that produced it is a bug
// somebody will want to read the bytes of. Deleting it — or leaving it in place for the
// next save to replace — turns "one player lost an evening" into "nobody can ever find
// out why".
//
// **The character survives; only its life is gone.** The index is untouched, so the
// name stays that character's and the account still owns it — a player comes back to
// the character they had, standing where a character that has never played stands. The
// alternative, dropping it from the index, would free a name that a record on disk is
// still wearing.
//
// The timestamp in the name is not decoration: renaming to a fixed `.corrupt` would
// destroy the *previous* corrupt record the second time this ran, which is the same
// silent overwrite this function exists to prevent.
func (s *Store) Quarantine(id CharacterID) (string, error) {
	if s == nil || s.dir == "" {
		return "", nil
	}
	return setAside(s.recordPath(id), corruptFileSuffix)
}

// setAside renames path out of the way under a suffix and a timestamp, and answers
// where it went. The one mechanism behind [Store.Quarantine], the startup scan's
// refusals and the pre-character directory move — one rename, one naming rule, one
// place to be right about not overwriting anything.
func setAside(path, suffix string) (string, error) {
	aside := fmt.Sprintf("%s%s.%d", path, suffix, time.Now().UTC().UnixNano())
	if err := os.Rename(path, aside); err != nil {
		return "", fmt.Errorf("persist: setting %s aside: %w", path, err)
	}
	return aside, nil
}

// recordPath is where one character's record lives. The hex id is the whole name:
// fixed length, and every character of it comes from a number this server minted, so
// nothing a client sends reaches the filesystem.
func (s *Store) recordPath(id CharacterID) string {
	return filepath.Join(s.dir, id.String()+recordFileExt)
}

func encodeRecord(rec Record) []byte {
	name := rec.Name

	buf := make([]byte, recordHeaderSize+len(name)+world.ChecksumSize)
	copy(buf[0:4], playerMagic[:])
	binary.LittleEndian.PutUint32(buf[4:8], StoreVersion)
	// Seconds, in UTC, because a record is compared by a person reading a log rather
	// than by anything that needs sub-second resolution — and because a zero time
	// round-trips through Unix seconds unambiguously.
	binary.LittleEndian.PutUint64(buf[offLastSeen:offLastSeen+8], uint64(rec.LastSeen.UTC().Unix()))
	binary.LittleEndian.PutUint64(buf[offCharacter:offCharacter+8], uint64(rec.Character))
	copy(buf[offOwner:offOwner+identity.IDSize], rec.Owner[:])
	putAppearance(buf, rec.Appearance)

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
// own values are judged one layer up, and readIndexed for the checks that are about the
// index rather than about either.
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
		Name:       string(data[recordHeaderSize : uint64(recordHeaderSize)+nameLen]),
		Character:  CharacterID(binary.LittleEndian.Uint64(data[offCharacter : offCharacter+8])),
		LastSeen:   time.Unix(int64(binary.LittleEndian.Uint64(data[offLastSeen:offLastSeen+8])), 0).UTC(),
		Appearance: appearanceAt(data),
		Yaw:        math.Float64frombits(binary.LittleEndian.Uint64(data[offYaw : offYaw+8])),
		Health:     binary.LittleEndian.Uint16(data[offHealth : offHealth+2]),
	}

	rec.Owner = identity.PlayerID(data[offOwner : offOwner+identity.IDSize])
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

// putAppearance writes one appearance into a record buffer, and appearanceAt reads one
// back out. A pair rather than two loose blocks inside encodeRecord and decodeRecord,
// because the order of six values is exactly the kind of thing two copies get wrong in
// opposite directions — and the failure would be a character who comes back wearing
// their trousers on their head.
//
// The colours go down as the wire carries them, 0x00RRGGBB. Nothing here refuses a
// reserved high byte: that is [protocol.Appearance.Validate]'s question, asked before a
// value is stored and again by the startup scan, and a refusal in the middle of an
// encode would be a third answer in a place that cannot report one.
func putAppearance(buf []byte, a protocol.Appearance) {
	for i, color := range [...]uint32{a.SkinColor, a.ShirtColor, a.TrousersColor, a.ShoesColor, a.HairColor} {
		at := offAppearance + i*4
		binary.LittleEndian.PutUint32(buf[at:at+4], color)
	}
	buf[offAppearance+5*4] = byte(a.HairModel)
}

// appearanceAt reads the appearance out of a record whose length has already been
// checked. Every offset it touches is inside the fixed-width header, which decodeRecord
// has bounded before it calls this.
func appearanceAt(data []byte) protocol.Appearance {
	color := func(i int) uint32 {
		at := offAppearance + i*4
		return binary.LittleEndian.Uint32(data[at : at+4])
	}
	return protocol.Appearance{
		SkinColor:     color(0),
		ShirtColor:    color(1),
		TrousersColor: color(2),
		ShoesColor:    color(3),
		HairColor:     color(4),
		HairModel:     vnet.HairModel(data[offAppearance+5*4]),
	}
}
