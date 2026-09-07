// Tests for the saved-sessions file. Same discipline as structures_test.go: a corrupt
// file is produced by writing bytes rather than by reaching into the encoder, so what is
// pinned is what a reader on another build would actually find on the disk.
package persist

import (
	"encoding/binary"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// runsFixture is the shapes worth round-tripping: a run with several defeated encounters
// and a party, one with a single kill and one owner, and one that is bound by nobody and
// has killed nothing — which is the entry a corrupt-length bug is most likely to survive,
// because both of its variable-length lists are empty.
//
// Negative seeds and negative ruin cells are deliberate: every one of those fields is an
// int64 written through a uint64, and a fixture with no negative value in it would pass
// with the sign thrown away.
func runsFixture() []SessionRecord {
	return []SessionRecord{
		{
			ID:             41,
			Seed:           -7_712_884_223_119_004_001,
			Ruin:           [2]int64{-9_000_000_000, 55},
			ExpiresUnix:    1_773_446_400,
			DefeatedBosses: []vnet.MobKind{vnet.MobKindDraugrKing, vnet.MobKindVargrGuardian},
			Bound: []SessionCharacter{
				{PlayerID: identity.PlayerID{1, 2, 3}, CharacterID: 4},
				{PlayerID: identity.PlayerID{9, 9, 9, 9}, CharacterID: 18_446_744_073_709_551_615},
			},
		},
		{
			ID:             8_000_000_009,
			Seed:           1,
			Ruin:           [2]int64{0, -1},
			ExpiresUnix:    1_773_532_800,
			DefeatedBosses: []vnet.MobKind{vnet.MobKindVargrGuardian},
			Bound:          []SessionCharacter{{PlayerID: identity.PlayerID{7}, CharacterID: 1}},
		},
		{
			ID:          2,
			Seed:        0,
			Ruin:        [2]int64{9_000_000_000, -9_000_000_000},
			ExpiresUnix: 1_773_360_000,
		},
	}
}

func openRuns(t *testing.T, dir string) *SessionStore {
	t.Helper()

	store, err := OpenSessionStore(dir)
	if err != nil {
		t.Fatalf("OpenSessionStore: %v", err)
	}
	return store
}

// The round trip, field by field. Every one of the six is something the next morning
// depends on: which run, which world, which dungeon, when it ends, what it has put down
// and who owes it.
func TestSessionStoreRoundTripsSavedRuns(t *testing.T) {
	t.Parallel()

	store := openRuns(t, t.TempDir())
	want := runsFixture()

	if err := store.Save(want); err != nil {
		t.Fatalf("Save: %v", err)
	}
	got, found, err := store.Load()
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("a saved file was reported absent")
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("round trip = %#v, want %#v", got, want)
	}
}

// A world in which nobody has cleared a dungeon has no file, which is absence rather than
// an error — and an ephemeral world has no store at all.
func TestSessionStoreAnswersAbsenceAndTheEphemeralWorld(t *testing.T) {
	t.Parallel()

	store := openRuns(t, t.TempDir())
	got, found, err := store.Load()
	if err != nil || found || got != nil {
		t.Fatalf("an unplayed world answered %#v %v %v", got, found, err)
	}

	var ephemeral *SessionStore
	if got, found, err := ephemeral.Load(); err != nil || found || got != nil {
		t.Fatalf("a nil store answered %#v %v %v", got, found, err)
	}
	if err := ephemeral.Save(runsFixture()); err != nil {
		t.Fatalf("a nil store refused a save: %v", err)
	}
	if path := ephemeral.Path(); path != "" {
		t.Fatalf("a nil store named the file %q", path)
	}
	if _, err := OpenSessionStore(""); err == nil {
		t.Fatal("an unnamed world directory was accepted")
	}
}

// An empty list is written rather than skipped: a world whose runs have all reset has to
// be able to say so, and leaving yesterday's file behind would restore lockouts nobody
// owns.
func TestSessionStoreWritesAnEmptyListOverAFullOne(t *testing.T) {
	t.Parallel()

	store := openRuns(t, t.TempDir())
	if err := store.Save(runsFixture()); err != nil {
		t.Fatalf("Save: %v", err)
	}
	if err := store.Save(nil); err != nil {
		t.Fatalf("Save empty: %v", err)
	}

	got, found, err := store.Load()
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("an emptied file was reported absent, which would restore nothing rather than say nothing is owed")
	}
	if len(got) != 0 {
		t.Fatalf("the emptied file still holds %#v", got)
	}
}

// Every cap is refused at the write rather than truncated, because writing a file this
// build cannot read back is the one failure that looks like a success until a restart.
func TestSessionStoreRefusesMoreThanTheFormatHolds(t *testing.T) {
	t.Parallel()

	store := openRuns(t, t.TempDir())

	tooMany := make([]SessionRecord, MaxSavedSessions+1)
	for i := range tooMany {
		tooMany[i] = SessionRecord{ID: uint64(i + 1), ExpiresUnix: 1}
	}
	for _, tc := range []struct {
		name    string
		records []SessionRecord
	}{
		{"sessions", tooMany},
		{"defeated encounters", []SessionRecord{{ID: 1, ExpiresUnix: 1, DefeatedBosses: make([]vnet.MobKind, MaxDefeatedBosses+1)}}},
		{"bound characters", []SessionRecord{{ID: 1, ExpiresUnix: 1, Bound: make([]SessionCharacter, MaxBoundCharacters+1)}}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if err := store.Save(tc.records); !errors.Is(err, ErrTooManySessions) {
				t.Fatalf("Save = %v, want ErrTooManySessions", err)
			}
		})
	}
	if _, err := os.Stat(store.Path()); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("a refused save wrote a file anyway: %v", err)
	}
}

// Every way a file can be wrong, produced by writing bytes rather than through the
// encoder. **The truncation and the trailing byte are the pair this format needs most**:
// entries are variable-width, so there is no size arithmetic to catch them and the walk
// is the only thing that does.
func TestSessionStoreRefusesACorruptFile(t *testing.T) {
	t.Parallel()

	good, err := encodeSessions(runsFixture())
	if err != nil {
		t.Fatalf("encodeSessions: %v", err)
	}

	// rechecksum makes every case below a claim about the *layout*, not about the
	// checksum: a corrupt file that also fails its CRC would be refused for the wrong
	// reason and would say nothing about the check under test. The one case that
	// deliberately skips it is the flipped byte, which is the CRC's own test.
	rechecksum := func(b []byte) []byte {
		world.PutChecksum(b)
		return b
	}
	// resize rewrites the body to a different length, keeping the header, and re-stamps
	// the checksum. A negative delta truncates, a positive one pads.
	resize := func(b []byte, delta int) []byte {
		body := b[:len(b)-world.ChecksumSize]
		if delta < 0 {
			body = body[:len(body)+delta]
		} else {
			body = append(append([]byte(nil), body...), make([]byte, delta)...)
		}
		return rechecksum(append(append([]byte(nil), body...), make([]byte, world.ChecksumSize)...))
	}

	// **Every case names the check that must refuse it, and that is not decoration.** Six
	// of these shapes are caught by more than one guard, so an assertion that only asked
	// for an ErrCorruptStore would pass with the intended check deleted — which is exactly
	// what a mutation run over this file found: dropping the declared-count cap, the
	// per-entry list cap and the oversized-file size guard each left every case green,
	// because a later check refused the same bytes for a different reason. `want` is what
	// tells those apart.
	for _, tc := range []struct {
		name    string
		corrupt func([]byte) []byte
		want    string
	}{
		{"another store's magic", func(b []byte) []byte { copy(b[0:4], structuresMagic[:]); return rechecksum(b) }, "magic"},
		{"a version this build does not speak", func(b []byte) []byte {
			binary.LittleEndian.PutUint32(b[4:8], SessionsVersion+1)
			return rechecksum(b)
		}, "version"},
		{"a flipped byte under the checksum", func(b []byte) []byte { b[sessionsHeaderSize] ^= 0xff; return b }, "checksum"},
		// Nine bytes off the end lands inside the last entry's fixed head — the entry with
		// both lists empty — so this is the head check. The fit check over the two
		// variable-length lists is the "more bound characters than are there" case below.
		{"truncated mid-entry", func(b []byte) []byte { return resize(b, -9) }, "runs out of bytes in entry"},
		{"a trailing byte", func(b []byte) []byte { return resize(b, 1) }, "before the checksum"},
		{"a count past the cap", func(b []byte) []byte {
			binary.LittleEndian.PutUint32(b[offSessionCount:offSessionCount+4], MaxSavedSessions+1)
			return rechecksum(b)
		}, "more than the"},
		{"a count no allocation should be made for", func(b []byte) []byte {
			// The declared count is what sizes the slice the walk appends into, so this is
			// the case the cap exists for: refused on the number itself, before a byte of
			// the entries is looked at. Caught by the walk too, but only after the
			// allocation this guard is here to prevent.
			binary.LittleEndian.PutUint32(b[offSessionCount:offSessionCount+4], ^uint32(0))
			return rechecksum(b)
		}, "more than the"},
		{"a count larger than the entries", func(b []byte) []byte {
			binary.LittleEndian.PutUint32(b[offSessionCount:offSessionCount+4], 9)
			return rechecksum(b)
		}, "runs out of bytes in entry"},
		{"a count smaller than the entries", func(b []byte) []byte {
			binary.LittleEndian.PutUint32(b[offSessionCount:offSessionCount+4], 1)
			return rechecksum(b)
		}, "before the checksum"},
		{"an entry claiming more bound characters than are there", func(b []byte) []byte {
			// Under the per-entry cap and past the end of the file, so this is the fit
			// check rather than the cap — the pair below is the cap.
			binary.LittleEndian.PutUint16(b[sessionsHeaderSize+41:sessionsHeaderSize+43], MaxBoundCharacters)
			return rechecksum(b)
		}, "do not fit in the remaining"},
		{"an entry claiming more bound characters than one may hold", func(b []byte) []byte {
			binary.LittleEndian.PutUint16(b[sessionsHeaderSize+41:sessionsHeaderSize+43], MaxBoundCharacters+1)
			return rechecksum(b)
		}, "one entry can hold"},
		{"an entry claiming more defeated encounters than one may hold", func(b []byte) []byte {
			b[sessionsHeaderSize+40] = MaxDefeatedBosses + 1
			return rechecksum(b)
		}, "one entry can hold"},
		{"shorter than an empty file", func(b []byte) []byte { return b[:sessionsHeaderSize] }, "shorter than an empty sessions file"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			store := openRuns(t, dir)
			broken := tc.corrupt(append([]byte(nil), good...))
			if err := os.WriteFile(store.Path(), broken, 0o644); err != nil {
				t.Fatal(err)
			}

			got, found, err := store.Load()
			if err == nil {
				t.Fatalf("a corrupt file loaded as %#v (found=%v)", got, found)
			}
			if !errors.Is(err, world.ErrCorruptStore) {
				t.Fatalf("Load = %v, want an ErrCorruptStore", err)
			}
			if !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("Load = %v, want the refusal to come from the check naming %q", err, tc.want)
			}
			// The evidence stays exactly where it is: nothing here deletes or rewrites the
			// file it could not read.
			if _, statErr := os.Stat(store.Path()); statErr != nil {
				t.Fatalf("the unreadable file was not kept: %v", statErr)
			}
		})
	}
}

// A file larger than the format can need is refused on its size, before it is read: this
// is the check that keeps a corrupt directory from becoming an out-of-memory.
func TestSessionStoreRefusesAnOversizedFileBeforeReadingIt(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	store := openRuns(t, dir)
	if err := os.WriteFile(store.Path(), make([]byte, maxSessionsFileSize+1), 0o644); err != nil {
		t.Fatal(err)
	}

	_, _, err := store.Load()
	if !errors.Is(err, world.ErrCorruptStore) {
		t.Fatalf("Load = %v, want an ErrCorruptStore", err)
	}
	// Named, because the bytes written here are also refused by the header check further
	// down — so an assertion that stopped at ErrCorruptStore would pass with this guard
	// deleted, and the guard is the whole point: it refuses on the stat, before the file
	// is read into memory.
	if !strings.Contains(err.Error(), "a sessions file can need") {
		t.Fatalf("Load = %v, want the refusal to come from the size guard", err)
	}
}

// The store sweeps whatever a crash left mid-rename, and writes through the same atomic
// discipline every other store here uses.
func TestSessionStoreSweepsATemporaryLeftByACrash(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	// os.CreateTemp's shape exactly — the destination, ".tmp", and a run of digits —
	// because that is what world.WriteAtomic leaves behind and what the sweep matches.
	leftover := filepath.Join(dir, sessionsFileName+".tmp2748193056")
	if err := os.WriteFile(leftover, []byte("half a rename"), 0o644); err != nil {
		t.Fatal(err)
	}

	store := openRuns(t, dir)
	if _, err := os.Stat(leftover); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("the leftover survived OpenSessionStore: %v", err)
	}
	if err := store.Save(runsFixture()); err != nil {
		t.Fatalf("Save: %v", err)
	}

	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 1 || entries[0].Name() != sessionsFileName {
		t.Fatalf("the world directory holds %v, want only %s", entries, sessionsFileName)
	}
}
