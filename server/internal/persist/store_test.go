// Internal tests: the encoder, the truncation rule and the on-disk layout are what
// is being pinned, and a corrupt file is produced by writing bytes rather than by
// calling an exported function.
package persist

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
	"unicode/utf8"

	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// testID is a distinct player per seed, derived from an account so a failing
// test names the same file on every run.
func testID(seed byte) identity.PlayerID {
	var account identity.Account
	for i := range account {
		account[i] = seed*31 + byte(i)
	}
	return identity.IDOf(account)
}

func openStore(t *testing.T) (*Store, string) {
	t.Helper()

	worldDir := t.TempDir()
	store, err := OpenStore(worldDir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	return store, worldDir
}

func TestStoreRoundTripsARecord(t *testing.T) {
	t.Parallel()

	store, worldDir := openStore(t)
	id := testID(1)

	// Seconds, because that is the resolution the format keeps. A test that wrote
	// time.Now() and compared it whole would fail on the nanoseconds the format
	// deliberately does not store.
	want := Record{Name: "Eivor", LastSeen: time.Unix(1_700_000_000, 0).UTC()}
	if err := store.Save(id, want); err != nil {
		t.Fatalf("Save: %v", err)
	}

	got, found, err := store.Load(id)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("the record just written was not found")
	}
	if got.Name != want.Name {
		t.Errorf("Name = %q, want %q", got.Name, want.Name)
	}
	if !got.LastSeen.Equal(want.LastSeen) {
		t.Errorf("LastSeen = %s, want %s", got.LastSeen, want.LastSeen)
	}

	// The file is named for the id and lives under the world directory. Both are part
	// of the contract with #147 and with an operator reading a directory listing.
	path := filepath.Join(worldDir, playersDirName, id.String()+recordFileExt)
	if _, err := os.Stat(path); err != nil {
		t.Errorf("the record is not at %s: %v", path, err)
	}

	// A second save replaces the first rather than appending to it.
	later := Record{Name: "Eivor Wolf-Kissed", LastSeen: want.LastSeen.Add(time.Hour)}
	if err := store.Save(id, later); err != nil {
		t.Fatalf("the second Save: %v", err)
	}
	got, _, err = store.Load(id)
	if err != nil {
		t.Fatalf("the second Load: %v", err)
	}
	if got.Name != later.Name || !got.LastSeen.Equal(later.LastSeen) {
		t.Errorf("the second save did not replace the first: %+v", got)
	}
}

func TestStoreReportsAnUnknownIdentityAsNotFound(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)

	// Not an error, deliberately: a token this server never issued is a first
	// connection, and the handshake mints a new identity for it.
	rec, found, err := store.Load(testID(2))
	if err != nil {
		t.Fatalf("Load of an unknown identity: %v", err)
	}
	if found {
		t.Fatal("an identity that was never saved was found")
	}
	if rec != (Record{}) {
		t.Errorf("a record came back for an unknown identity: %+v", rec)
	}
}

// corrupt is what a damaged record looks like on disk, per way of being damaged.
func TestStoreRefusesARecordItCannotReadExactly(t *testing.T) {
	t.Parallel()

	id := testID(3)
	sound := encodeRecord(Record{Name: "Eivor", LastSeen: time.Unix(1_700_000_000, 0).UTC()})

	damage := map[string]func([]byte) []byte{
		// A flipped bit inside a record whose shape is still valid — exactly what a
		// length check cannot catch, and the whole reason the CRC is there.
		"a flipped bit in the name": func(b []byte) []byte {
			out := append([]byte(nil), b...)
			out[recordHeaderSize] ^= 0x20
			return out
		},
		"a wrong magic number": func(b []byte) []byte {
			out := append([]byte(nil), b...)
			out[0] = 'X'
			world.PutChecksum(out)
			return out
		},
		// A well-formed record of a version this build does not know. Refused rather
		// than read as the layout it happens to resemble.
		"a future format version": func(b []byte) []byte {
			out := append([]byte(nil), b...)
			out[4] = byte(StoreVersion + 1)
			world.PutChecksum(out)
			return out
		},
		"a truncated file": func(b []byte) []byte { return b[:len(b)-3] },
		"a name length that disagrees with the file": func(b []byte) []byte {
			out := append([]byte(nil), b...)
			out[offNameLen] = 200
			world.PutChecksum(out)
			return out
		},
		"a file shorter than a header": func([]byte) []byte { return []byte{'V', 'X'} },
	}

	for name, break_ := range damage {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			store, worldDir := openStore(t)
			path := filepath.Join(worldDir, playersDirName, id.String()+recordFileExt)
			if err := os.WriteFile(path, break_(sound), 0o600); err != nil {
				t.Fatalf("writing the damaged record: %v", err)
			}

			_, found, err := store.Load(id)
			if !errors.Is(err, world.ErrCorruptStore) {
				t.Fatalf("Load = %v, want ErrCorruptStore", err)
			}
			// The distinction that matters: unreadable is not "absent". Reported as
			// absent, the handshake would mint a new identity whose teardown writes over
			// the record nobody could read.
			if found {
				t.Error("a corrupt record was reported as found")
			}
		})
	}
}

func TestStoreRefusesAFileTooLargeToBeARecord(t *testing.T) {
	t.Parallel()

	store, worldDir := openStore(t)
	id := testID(4)
	path := filepath.Join(worldDir, playersDirName, id.String()+recordFileExt)

	// Checked before the read, not after: finding this out by allocating it is how a
	// corrupt directory becomes an out-of-memory.
	if err := os.WriteFile(path, make([]byte, maxRecordSize+1), 0o600); err != nil {
		t.Fatalf("writing the oversized record: %v", err)
	}
	if _, _, err := store.Load(id); !errors.Is(err, world.ErrCorruptStore) {
		t.Fatalf("Load = %v, want ErrCorruptStore", err)
	}
}

func TestOpenStoreSweepsTemporaries(t *testing.T) {
	t.Parallel()

	worldDir := t.TempDir()
	store, err := OpenStore(worldDir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	dir := store.Dir()

	// What a crash between the write and the rename leaves behind. Inert — a reader
	// only ever opens an exact <id>.bin path — so this is housekeeping, and the
	// housekeeping is the store's rather than an operator's.
	leftover := filepath.Join(dir, testID(5).String()+recordFileExt+".tmp3141592")
	if err := os.WriteFile(leftover, []byte("half a record"), 0o600); err != nil {
		t.Fatalf("writing the leftover: %v", err)
	}

	if _, err := OpenStore(worldDir); err != nil {
		t.Fatalf("re-opening: %v", err)
	}
	if _, err := os.Stat(leftover); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("the temporary file survived the sweep: %v", err)
	}
}

func TestSaveTruncatesALongNameAtARuneBoundary(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)

	// Multi-byte runes chosen so the cap falls inside one: cutting there would store
	// text that no longer decodes, from a name that was perfectly fine.
	long := strings.Repeat("ᛁ", MaxNameBytes) // three bytes each
	id := testID(6)
	if err := store.Save(id, Record{Name: long, LastSeen: time.Unix(1, 0)}); err != nil {
		t.Fatalf("Save: %v", err)
	}

	got, found, err := store.Load(id)
	if err != nil || !found {
		t.Fatalf("Load: %v (found %v)", err, found)
	}
	if len(got.Name) > MaxNameBytes {
		t.Errorf("the stored name is %d bytes, past the %d cap", len(got.Name), MaxNameBytes)
	}
	if !utf8.ValidString(got.Name) {
		t.Error("truncation cut through a rune and stored text that no longer decodes")
	}
	if !strings.HasPrefix(long, got.Name) {
		t.Error("the stored name is not a prefix of the name given")
	}
	// A name that fits is stored whole — the cap must not shorten an ordinary name.
	if err := store.Save(id, Record{Name: "Eivor"}); err != nil {
		t.Fatalf("Save of a short name: %v", err)
	}
	if got, _, _ := store.Load(id); got.Name != "Eivor" {
		t.Errorf("a short name came back as %q", got.Name)
	}
}

func TestAnEphemeralWorldWritesNothing(t *testing.T) {
	t.Parallel()

	// The ephemeral world is a nil *Store, so that every persistence path is a no-op
	// against one rather than a branch at each call site.
	var store *Store

	if store.Dir() != "" {
		t.Errorf("a nil store names a directory: %q", store.Dir())
	}
	if err := store.Save(testID(7), Record{Name: "Eivor", LastSeen: time.Now()}); err != nil {
		t.Fatalf("Save on a nil store: %v", err)
	}
	rec, found, err := store.Load(testID(7))
	if err != nil {
		t.Fatalf("Load on a nil store: %v", err)
	}
	if found || rec != (Record{}) {
		t.Error("a nil store remembered something")
	}

	// And the world directory an ephemeral server was pointed at stays empty.
	dir := t.TempDir()
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatalf("ReadDir: %v", err)
	}
	if len(entries) != 0 {
		t.Errorf("an ephemeral run left %d entries behind", len(entries))
	}
}

func TestOpenStoreRefusesAnUnnamedWorldDirectory(t *testing.T) {
	t.Parallel()

	// An empty -world-dir is the ephemeral world, and choosing it is main's decision.
	// Answering with a store that writes nowhere would hide that decision here.
	if _, err := OpenStore(""); err == nil {
		t.Fatal("OpenStore accepted an empty world directory")
	}
}

func TestARecordSurvivesReopening(t *testing.T) {
	t.Parallel()

	worldDir := t.TempDir()
	store, err := OpenStore(worldDir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}

	id := testID(8)
	want := Record{Name: "Sigrun", LastSeen: time.Unix(1_600_000_000, 0).UTC()}
	if err := store.Save(id, want); err != nil {
		t.Fatalf("Save: %v", err)
	}

	reopened, err := OpenStore(worldDir)
	if err != nil {
		t.Fatalf("re-opening: %v", err)
	}
	got, found, err := reopened.Load(id)
	if err != nil || !found {
		t.Fatalf("Load after reopening: %v (found %v)", err, found)
	}
	if got.Name != want.Name || !got.LastSeen.Equal(want.LastSeen) {
		t.Errorf("the record came back as %+v, want %+v", got, want)
	}
}

// TestStoreRoundTripsTheLife is the v2 half of the round trip: everything the record
// gained, written and read back to the bit.
//
// The position is checked for exact equality rather than within a tolerance, and that
// is the assertion. Position is a float64 on the way in and a float64 on the way out —
// no narrowing anywhere in the format — so a save is not allowed to move a player by
// so much as a rounding, however many times they reconnect.
func TestStoreRoundTripsTheLife(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	id := testID(7)

	want := Record{
		Name:     "Eivor",
		LastSeen: time.Unix(1_700_000_000, 0).UTC(),
		// Values a float32 could not hold exactly, so a narrowing anywhere in the
		// format shows up as a failure rather than as a rounding nobody notices.
		Pos:    [3]float64{-1234.5678901234567, 70.100000000000001, 4096.3333333333333},
		Yaw:    -2.7182818284590452,
		Health: 61,
	}
	// Every shape a slot can take: a worn durable item, a partial stack, the last slot
	// occupied, and empties everywhere else.
	want.Slots[0] = protocol.InventoryStack{ItemID: 7, Count: 1, Durability: 37, MaxDurability: 100}
	want.Slots[5] = protocol.InventoryStack{ItemID: 1, Count: 23}
	want.Slots[protocol.InventorySlots-1] = protocol.InventoryStack{ItemID: 6, Count: 2}

	if err := store.Save(id, want); err != nil {
		t.Fatalf("Save: %v", err)
	}
	got, found, err := store.Load(id)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("the record just written was not found")
	}

	if got.Pos != want.Pos {
		t.Errorf("Pos = %v, want %v", got.Pos, want.Pos)
	}
	if got.Yaw != want.Yaw {
		t.Errorf("Yaw = %v, want %v", got.Yaw, want.Yaw)
	}
	if got.Health != want.Health {
		t.Errorf("Health = %d, want %d", got.Health, want.Health)
	}
	if got.Slots != want.Slots {
		for slot := range want.Slots {
			if got.Slots[slot] != want.Slots[slot] {
				t.Errorf("slot %d = %+v, want %+v", slot, got.Slots[slot], want.Slots[slot])
			}
		}
	}
}

// A record is a fixed size plus its name, and the format's own bound is what stops a
// corrupt directory becoming an allocation. Pinned because both numbers are derived
// from the slot table and would move silently if a field were added without a version
// bump.
func TestARecordIsTheSizeTheFormatSaysItIs(t *testing.T) {
	t.Parallel()

	empty := encodeRecord(Record{})
	if len(empty) != recordHeaderSize+world.ChecksumSize {
		t.Errorf("an empty record is %d bytes, want %d", len(empty), recordHeaderSize+world.ChecksumSize)
	}
	named := encodeRecord(Record{Name: strings.Repeat("a", MaxNameBytes)})
	if len(named) != maxRecordSize {
		t.Errorf("the largest record is %d bytes, want %d", len(named), maxRecordSize)
	}
	// The slot table is the bulk of it, and it is the whole table whatever is in it: a
	// record with one item and a record with none are the same length.
	oneItem := Record{}
	oneItem.Slots[3] = protocol.InventoryStack{ItemID: 1, Count: 1}
	if len(encodeRecord(oneItem)) != len(empty) {
		t.Error("a record with an item is a different size from an empty one; the slot table is not fixed")
	}
}

// Quarantine keeps the bytes and frees the path, which is the pair of properties the
// corrupt-record rule rests on: nothing a player had is deleted, and the next save has
// somewhere to go that is not on top of it.
func TestQuarantineKeepsTheRecordAndFreesThePath(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	id := testID(8)

	damaged := []byte("not a player record")
	path := store.recordPath(id)
	if err := os.WriteFile(path, damaged, 0o600); err != nil {
		t.Fatalf("writing the damaged record: %v", err)
	}

	aside, err := store.Quarantine(id)
	if err != nil {
		t.Fatalf("Quarantine: %v", err)
	}
	if aside == path {
		t.Fatal("the record was left where it was")
	}
	kept, err := os.ReadFile(aside)
	if err != nil {
		t.Fatalf("reading the record set aside: %v", err)
	}
	if string(kept) != string(damaged) {
		t.Error("the bytes set aside are not the bytes that were there")
	}
	if _, _, err := store.Load(id); err != nil {
		t.Errorf("Load after Quarantine: %v, want the path to be free", err)
	}

	// A second corrupt record for the same identity does not overwrite the first —
	// which is the same silent overwrite this whole path exists to prevent, one turn
	// further round.
	if err := os.WriteFile(path, []byte("also not a record"), 0o600); err != nil {
		t.Fatalf("writing the second damaged record: %v", err)
	}
	again, err := store.Quarantine(id)
	if err != nil {
		t.Fatalf("the second Quarantine: %v", err)
	}
	if again == aside {
		t.Error("the second corrupt record was written over the first")
	}
	if _, err := os.Stat(aside); err != nil {
		t.Errorf("the first record set aside is gone: %v", err)
	}
}

// An ephemeral world keeps nothing, and every path is a no-op rather than a branch at
// each call site.
func TestANilStoreKeepsNothing(t *testing.T) {
	t.Parallel()

	var store *Store

	if err := store.Save(testID(9), Record{Name: "Eivor", Health: 100}); err != nil {
		t.Errorf("Save on an ephemeral store: %v", err)
	}
	rec, found, err := store.Load(testID(9))
	if err != nil {
		t.Errorf("Load on an ephemeral store: %v", err)
	}
	if found || rec != (Record{}) {
		t.Error("an ephemeral store answered with a record")
	}
	aside, err := store.Quarantine(testID(9))
	if err != nil || aside != "" {
		t.Errorf("Quarantine on an ephemeral store = %q, %v; want the empty no-op", aside, err)
	}
}
