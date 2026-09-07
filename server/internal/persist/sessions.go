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

// SessionsVersion is the on-disk format version of the saved-sessions file.
//
// Bump it for any change to the layout below, a purely additive one included: a reader
// of an older build must refuse a newer file rather than parse a prefix of it. Separate
// from [StoreVersion], [StructuresVersion], [ClockVersion] and world.StoreVersion,
// because a binding, a camp, a clock, a player record and a chunk delta change for
// unrelated reasons — and **world.StoreVersion in particular must not be reached for**,
// since bumping it invalidates every stored chunk delta in every existing world.
//
// **A new boss species is not such a change.** The species is one byte whatever it
// names, so a file recording a defeated Vargr Guardian is the same shape as one
// recording a Draugr King. What an older build does with a species it does not know is
// game's decision about content, not this format's about layout.
const SessionsVersion uint32 = 1

// MaxSavedSessions is the most saved sessions a file may declare.
//
// A bound on the allocation the count field can ask for, in the shape [Store.Load] and
// [StructureStore.Load] both use: the size is checked before anything is read, so a
// corrupt count is refused rather than turned into a multi-gigabyte make(). The number
// is far above any server this game runs — game.DefaultMaxInstances is 32 concurrent
// worlds and a saved session is one of those — and the writer refuses to exceed it too,
// so this server can never write a file it would then refuse to read.
const MaxSavedSessions = 1 << 10

// MaxDefeatedBosses is the most defeated encounters one saved session may record.
//
// The count is a single byte in the layout below and this is well under what that byte
// could hold, because the bound that matters is the one a dungeon can actually contain:
// a handful of boss-rank species, not two hundred and fifty-five.
const MaxDefeatedBosses = 64

// MaxBoundCharacters is the most characters one saved session may bind.
//
// A saved session binds the party that put its first boss down plus anyone admitted
// afterwards, which is a party and not a server. The cap exists so a corrupt length
// field cannot ask for an arbitrary allocation, and it is deliberately far above any
// party this game forms.
const MaxBoundCharacters = 1 << 8

// On-disk layout, little-endian throughout, one file for the whole world.
//
//	sessions.bin
//	    magic[4] version:u32 count:u32
//	    count × ( id:u64 seed:i64 cell_x:i64 cell_z:i64 expires_unix:i64
//	              defeated:u8 bound:u16
//	              defeated × kind:u8
//	              bound × (player:32 character:u64) )
//	    crc32:u32
//
// **This is the binding and not the world**, which is the whole reason the file is this
// small. A session's world is a pure function of its seed — world.NewInstanceCache
// rebuilds it — so the durable part of a cleared dungeon is a handful of scalars and a
// short list of who owes it. Nothing here describes a chunk, a block or an entity, and
// nothing may be added that does: an instance that writes terrain has stopped being
// cheap to restore and has started being a second world to keep in step.
//
// **expires_unix is a wall-clock second, and the only field here that is not an
// identity.** The reset falls on the server's own midnight on the real calendar (GDD's
// day is the unit a dungeon is measured in), and a real day that includes the hours a
// server spent switched off is not a quantity any tick counter can hold — which is also
// why this file does not reach for the tick_of_day in [Clock] beside it. Those are two
// different clocks on purpose and confusing them is the mistake this paragraph exists to
// stop.
//
// **The bound characters are the durable half of a session's membership, and its
// occupants are not stored at all.** Who was standing inside when the process ended is a
// fact about that process; who owes this ruin this run is a fact about the world. A
// restored session therefore comes back empty and fully bound, which is exactly the
// state a saved session is in between one night and the next morning.
//
// **The session id is stored rather than re-minted**, unlike a structure's. A structure
// id names a thing inside one simulation and nothing outside it refers to it; a session
// id is what a binding points at, so re-minting on load would mean rewriting every
// binding in the same breath and would buy nothing. The counter that mints ids is not
// serialised, so the manager is what keeps a fresh mint from colliding with a restored
// id — see game.InstanceManager.createLocked.
//
// Variable-width entries and one count, so the file's size is not a function of the
// count alone: the decoder walks the entries and requires the walk to end exactly on
// the checksum, which is what makes a truncated file a refusal rather than a shorter
// list of sessions.
const (
	sessionsFileName = "sessions.bin"

	offSessionCount    = world.HeaderSize
	sessionsHeaderSize = offSessionCount + 4

	// sessionEntryHeadSize is everything before the two variable-length lists.
	sessionEntryHeadSize = 8 + 8 + 8 + 8 + 8 + 1 + 2
	sessionBoundSize     = identity.IDSize + 8

	maxSessionEntrySize = sessionEntryHeadSize + MaxDefeatedBosses + MaxBoundCharacters*sessionBoundSize
	maxSessionsFileSize = sessionsHeaderSize + MaxSavedSessions*maxSessionEntrySize + world.ChecksumSize
)

var sessionsMagic = [4]byte{'V', 'X', 'H', 'I'}

// ErrTooManySessions reports a world with more saved sessions, defeated encounters or
// bound characters than the format can write down.
//
// A sentinel because the caller's answer is to shout rather than to retry: the server is
// past a cap the file cannot describe, which is an operational problem and not a
// transient one.
var ErrTooManySessions = errors.New("persist: more saved sessions than the file format can hold")

// SessionCharacter names one character across restarts.
//
// The player id is the same hash the player records are named by, never the entity id it
// resolves to at runtime: an entity id names one session of one process, and a binding
// outlives both. The character id is per account, which is why neither half stands alone.
type SessionCharacter struct {
	PlayerID    identity.PlayerID
	CharacterID uint64
}

// SessionRecord is one saved dungeon run, as it is written down.
//
// **The six things a saved session is**, and no more: which run it is, the seed its world
// is rebuilt from, the ruin it belongs to, when it resets, which encounters it has
// already put down, and who owes it. Everything else about a live session — its
// simulation, its chunk cache, its occupants, its lifetime context — is reconstructed on
// load or is a property of the process rather than of the world.
//
// **This package judges none of it**, exactly as it judges nothing in a [Record] or a
// [StructureRecord]: whether a species can be a boss, whether an expiry is in the past
// and whether a seed names a world this build can generate are questions internal/game
// answers, and game is where they are asked. What is checked here is what a *file* can be
// wrong about — magic, version, checksum, declared lengths.
type SessionRecord struct {
	ID   uint64
	Seed int64
	// Ruin is the lattice cell of the ruin this run belongs to, X then Z.
	Ruin [2]int64
	// ExpiresUnix is the wall-clock second the run resets at: the server's next
	// midnight, on the real calendar, in the server's own timezone.
	ExpiresUnix int64
	// DefeatedBosses names every encounter this run has put down, in the order they
	// died, by the species that identifies it. Species rather than entity id precisely
	// so that it survives the restart this file exists for.
	DefeatedBosses []vnet.MobKind
	// Bound is every character this run is owed by. It is not who was inside.
	Bound []SessionCharacter
}

// SessionStore is one world's saved-sessions file.
//
// **A nil *SessionStore is the ephemeral world**, and every method is a no-op on one
// rather than a branch at each call site — the shape a nil [Store], a nil
// [StructureStore], a nil [ClockStore] and a nil world.Store all already have. An
// ephemeral world still saves a run when its first boss falls and still resets it at
// midnight; what it does not do is remember either after the process ends, which is
// exactly the difference the operator chose.
//
// Like [StructureStore] and [ClockStore] and unlike [Store], it owns exactly one file
// rewritten whole. That is what makes it safe with no lock of its own for the single
// writer it has: the autosave loop and the shutdown flush are ordered against each other
// by the worker wait group, never concurrent.
type SessionStore struct {
	path string
}

// OpenSessionStore prepares the saved-sessions file under worldDir.
//
// It does not create the file: a world in which nobody has cleared a dungeon has no
// saved sessions, and that is the same fact as an empty list rather than a state to
// initialise. worldDir has already been seed-checked by world.OpenStore, which runs
// first, so nothing here re-asks whether this directory belongs to this world.
func OpenSessionStore(worldDir string) (*SessionStore, error) {
	if worldDir == "" {
		// Not a nil store returned quietly, for the reason [OpenStore],
		// [OpenStructureStore] and [OpenClockStore] all give: an empty -world-dir is the
		// ephemeral world, and choosing it is main's decision rather than a shape this
		// constructor should accept and forget about.
		return nil, errors.New("persist: the world directory must be named")
	}
	if err := os.MkdirAll(worldDir, 0o755); err != nil {
		return nil, fmt.Errorf("persist: creating %s: %w", worldDir, err)
	}

	// Whatever a crash left mid-rename, for the reason [OpenStructureStore] sweeps: this
	// store writes through world.WriteAtomic and inherits its leftovers. Inert either way
	// — a reader only ever opens the exact path below. One name, because this store writes
	// one file and the directory it writes it into is the operator's `-world-dir` rather
	// than one of ours (#137).
	world.SweepTemporaries(worldDir, sessionsFileName)
	return &SessionStore{path: filepath.Join(worldDir, sessionsFileName)}, nil
}

// Path is the file this store writes. Empty for an ephemeral world.
func (s *SessionStore) Path() string {
	if s == nil {
		return ""
	}
	return s.path
}

// Load reads every saved session this world last wrote down.
//
// Three answers, the same three [Store.Load], [StructureStore.Load] and [ClockStore.Load]
// give: found, absent, or unreadable. A world in which no dungeon has been cleared has no
// file, which is not an error — it is a server whose ruins are all free. A file that
// exists and cannot be read **is** an error and must stay one: reporting it as "no saved
// sessions" would free every binding in the world and then write that over the only
// record of them.
func (s *SessionStore) Load() ([]SessionRecord, bool, error) {
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
	// Before the read, not after, for the reason every store here checks a size first: a
	// file this large is not one this format wrote, and finding that out by allocating it
	// is how a corrupt directory becomes an out-of-memory.
	if info.Size() > int64(maxSessionsFileSize) {
		return nil, false, fmt.Errorf("%w: %s is %d bytes, more than the %d a sessions file can need",
			world.ErrCorruptStore, s.path, info.Size(), maxSessionsFileSize)
	}

	data, err := os.ReadFile(s.path)
	if err != nil {
		return nil, false, fmt.Errorf("persist: reading %s: %w", s.path, err)
	}

	records, err := decodeSessions(data)
	if err != nil {
		return nil, false, fmt.Errorf("%s: %w", s.path, err)
	}
	return records, true, nil
}

// Save writes every saved session, atomically. A no-op in an ephemeral world.
//
// Whole rather than incremental, which is what makes the file a snapshot of the manager
// rather than a log to be replayed: there is no expiry record to lose and no ordering to
// get wrong, and a server whose runs have all reset writes a file with no entries in it.
//
// **An empty list is written rather than skipped.** A world that had three saved sessions
// this morning and none tonight has to be able to say so; leaving yesterday's file in
// place would restore three runs nobody owns any more, which is the one failure this
// whole file exists to prevent.
func (s *SessionStore) Save(records []SessionRecord) error {
	if s == nil {
		return nil
	}

	data, err := encodeSessions(records)
	if err != nil {
		return err
	}
	return world.WriteAtomic(s.path, data)
}

// encodeSessions lays the sessions out, in the order it was given.
//
// The order is the caller's and is preserved exactly, because the caller is the one with
// a deterministic one to give (game.InstanceManager.SavedSessions sorts by id). Sorting
// again here would be a second opinion about an order that already has an owner.
func encodeSessions(records []SessionRecord) ([]byte, error) {
	// Refused rather than truncated, and refused *here* rather than at the read: writing
	// a file this build cannot read back is the one failure that looks like a success
	// until a restart. See [MaxSavedSessions].
	if len(records) > MaxSavedSessions {
		return nil, fmt.Errorf("%w: %d sessions are saved, more than the %d one file can hold",
			ErrTooManySessions, len(records), MaxSavedSessions)
	}
	body := 0
	for _, rec := range records {
		if len(rec.DefeatedBosses) > MaxDefeatedBosses {
			return nil, fmt.Errorf("%w: session %d records %d defeated encounters, more than the %d one entry can hold",
				ErrTooManySessions, rec.ID, len(rec.DefeatedBosses), MaxDefeatedBosses)
		}
		if len(rec.Bound) > MaxBoundCharacters {
			return nil, fmt.Errorf("%w: session %d binds %d characters, more than the %d one entry can hold",
				ErrTooManySessions, rec.ID, len(rec.Bound), MaxBoundCharacters)
		}
		body += sessionEntryHeadSize + len(rec.DefeatedBosses) + len(rec.Bound)*sessionBoundSize
	}

	buf := world.NewRecord(sessionsHeaderSize, body, sessionsMagic, SessionsVersion)
	binary.LittleEndian.PutUint32(buf[offSessionCount:offSessionCount+4], uint32(len(records)))

	at := sessionsHeaderSize
	for _, rec := range records {
		binary.LittleEndian.PutUint64(buf[at:at+8], rec.ID)
		// Two's-complement in both directions, undone exactly by the decoder, so each
		// field carries the whole of an int64 rather than half of one.
		binary.LittleEndian.PutUint64(buf[at+8:at+16], uint64(rec.Seed))
		binary.LittleEndian.PutUint64(buf[at+16:at+24], uint64(rec.Ruin[0]))
		binary.LittleEndian.PutUint64(buf[at+24:at+32], uint64(rec.Ruin[1]))
		binary.LittleEndian.PutUint64(buf[at+32:at+40], uint64(rec.ExpiresUnix))
		buf[at+40] = byte(len(rec.DefeatedBosses))
		binary.LittleEndian.PutUint16(buf[at+41:at+43], uint16(len(rec.Bound)))
		at += sessionEntryHeadSize

		for _, kind := range rec.DefeatedBosses {
			buf[at] = byte(kind)
			at++
		}
		for _, who := range rec.Bound {
			copy(buf[at:at+identity.IDSize], who.PlayerID[:])
			binary.LittleEndian.PutUint64(buf[at+identity.IDSize:at+sessionBoundSize], who.CharacterID)
			at += sessionBoundSize
		}
	}

	world.PutChecksum(buf)
	return buf, nil
}

// decodeSessions parses the saved sessions, refusing anything it cannot read exactly.
//
// Validate-everything-then-return, the shape [decodeRecord], [decodeStructures] and
// world.decodeChunkFile all use: nothing is assembled until every check has passed, so a
// half-valid list is never a value a caller can hold.
//
// **The walk is the length check.** Entries are variable-width, so unlike a structures
// file there is no arithmetic that turns the declared count into an expected size. Each
// entry's two lengths are checked against what is left of the file before either list is
// read, and the walk must finish exactly on the checksum — which makes both a truncated
// file and one with trailing bytes a refusal rather than a shorter list of sessions.
func decodeSessions(data []byte) ([]SessionRecord, error) {
	if len(data) < sessionsHeaderSize+world.ChecksumSize {
		return nil, fmt.Errorf("%w: %d bytes is shorter than an empty sessions file",
			world.ErrCorruptStore, len(data))
	}
	if err := world.CheckHeader(data, sessionsMagic, SessionsVersion); err != nil {
		return nil, err
	}
	if err := world.CheckChecksum(data); err != nil {
		return nil, err
	}

	count := binary.LittleEndian.Uint32(data[offSessionCount : offSessionCount+4])
	if count > MaxSavedSessions {
		return nil, fmt.Errorf("%w: the file claims %d sessions, more than the %d one file can hold",
			world.ErrCorruptStore, count, MaxSavedSessions)
	}

	end := len(data) - world.ChecksumSize
	at := sessionsHeaderSize
	records := make([]SessionRecord, 0, count)
	for i := uint32(0); i < count; i++ {
		if at+sessionEntryHeadSize > end {
			return nil, fmt.Errorf("%w: the file claims %d sessions and runs out of bytes in entry %d",
				world.ErrCorruptStore, count, i)
		}
		rec := SessionRecord{
			ID:          binary.LittleEndian.Uint64(data[at : at+8]),
			Seed:        int64(binary.LittleEndian.Uint64(data[at+8 : at+16])),
			Ruin:        [2]int64{int64(binary.LittleEndian.Uint64(data[at+16 : at+24])), int64(binary.LittleEndian.Uint64(data[at+24 : at+32]))},
			ExpiresUnix: int64(binary.LittleEndian.Uint64(data[at+32 : at+40])),
		}
		defeated := int(data[at+40])
		bound := int(binary.LittleEndian.Uint16(data[at+41 : at+43]))
		at += sessionEntryHeadSize

		if defeated > MaxDefeatedBosses || bound > MaxBoundCharacters {
			return nil, fmt.Errorf("%w: entry %d claims %d defeated encounters and %d bound characters, past the %d and %d one entry can hold",
				world.ErrCorruptStore, i, defeated, bound, MaxDefeatedBosses, MaxBoundCharacters)
		}
		if at+defeated+bound*sessionBoundSize > end {
			return nil, fmt.Errorf("%w: entry %d claims %d defeated encounters and %d bound characters, which do not fit in the remaining %d bytes",
				world.ErrCorruptStore, i, defeated, bound, end-at)
		}

		if defeated > 0 {
			rec.DefeatedBosses = make([]vnet.MobKind, defeated)
			for k := range rec.DefeatedBosses {
				rec.DefeatedBosses[k] = vnet.MobKind(data[at+k])
			}
			at += defeated
		}
		if bound > 0 {
			rec.Bound = make([]SessionCharacter, bound)
			for k := range rec.Bound {
				who := at + k*sessionBoundSize
				copy(rec.Bound[k].PlayerID[:], data[who:who+identity.IDSize])
				rec.Bound[k].CharacterID = binary.LittleEndian.Uint64(data[who+identity.IDSize : who+sessionBoundSize])
			}
			at += bound * sessionBoundSize
		}
		records = append(records, rec)
	}
	if at != end {
		return nil, fmt.Errorf("%w: the file claims %d sessions, which end %d bytes before the checksum",
			world.ErrCorruptStore, count, end-at)
	}
	return records, nil
}
