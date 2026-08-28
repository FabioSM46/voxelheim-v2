// Tests for the per-character exploration ledger. Same discipline as store_test.go and
// structures_test.go: a corrupt file is produced by writing bytes rather than by reaching
// into the encoder, so what is pinned is what a reader on another build would find on
// the disk.
package persist

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// exploredFixture is a history with the things worth round-tripping: both axes
// negative, both positive, one of each, and the origin.
func exploredFixture() []world.Column {
	return []world.Column{
		{CX: -40, CZ: -7},
		{CX: -1, CZ: 0},
		{CX: 0, CZ: 0},
		{CX: 3, CZ: -2},
		{CX: 1024, CZ: 2048},
	}
}

// theCharacter is the same id in every test here, because none of these are about which
// character it is: an id is a number this server minted and the file is named for it.
const theCharacter CharacterID = 0x00000000000000a1

func openExploration(t *testing.T, dir string) *ExplorationStore {
	t.Helper()

	store, err := OpenExplorationStore(dir)
	if err != nil {
		t.Fatalf("OpenExplorationStore: %v", err)
	}
	return store
}

// ledgerPathOf is where a test writes damaged bytes. Derived from the store rather than
// rebuilt by hand, so a change to the naming rule breaks the tests that depend on it
// instead of silently testing a file nothing reads.
func ledgerPathOf(store *ExplorationStore, id CharacterID) string {
	return filepath.Join(store.Dir(), id.String()+recordFileExt)
}

// The round trip, column by column and in the order it was given. The order is the
// caller's and this package keeps it — the same contract encodeStructures has.
func TestExplorationStoreRoundTripsALedger(t *testing.T) {
	t.Parallel()

	store := openExploration(t, t.TempDir())
	want := exploredFixture()

	if err := store.Save(theCharacter, want); err != nil {
		t.Fatalf("Save: %v", err)
	}

	got, found, err := store.Load(theCharacter)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("the ledger that was just saved was reported as absent")
	}
	if len(got) != len(want) {
		t.Fatalf("loaded %d columns, want %d", len(got), len(want))
	}
	for i := range want {
		if got[i] != want[i] {
			t.Errorf("column %d round-tripped as %+v, want %+v", i, got[i], want[i])
		}
	}
}

// A character who has walked nowhere has no file, and that is an empty history rather
// than a failure. The distinction is what keeps a first login from being an event.
func TestExplorationStoreReportsAnUnwalkedCharacterAsAbsent(t *testing.T) {
	t.Parallel()

	got, found, err := openExploration(t, t.TempDir()).Load(theCharacter)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if found {
		t.Errorf("a character with no ledger reported %d columns", len(got))
	}
}

// An empty ledger is a file, not an absence, and it reads back as a found-and-empty
// history. Two things would break if it were not: "no file" would mean both "never
// looked" and "walked nowhere", and a save of an emptied set would leave the previous
// one on disk.
func TestAnEmptyLedgerIsAValidFile(t *testing.T) {
	t.Parallel()

	store := openExploration(t, t.TempDir())
	if err := store.Save(theCharacter, nil); err != nil {
		t.Fatalf("Save: %v", err)
	}

	info, err := os.Stat(ledgerPathOf(store, theCharacter))
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if want := int64(explorationHeaderSize + world.ChecksumSize); info.Size() != want {
		t.Errorf("an empty ledger is %d bytes, want %d", info.Size(), want)
	}

	got, found, err := store.Load(theCharacter)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Error("an empty ledger was reported as absent")
	}
	if len(got) != 0 {
		t.Errorf("an empty ledger loaded %d columns", len(got))
	}
}

// Two saves of the same ledger are the same bytes, which is what makes the sorted order
// the session hands over worth having: a file that changed is a history that changed.
func TestSavingTheSameLedgerTwiceWritesTheSameBytes(t *testing.T) {
	t.Parallel()

	store := openExploration(t, t.TempDir())
	columns := exploredFixture()

	if err := store.Save(theCharacter, columns); err != nil {
		t.Fatalf("first Save: %v", err)
	}
	first, err := os.ReadFile(ledgerPathOf(store, theCharacter))
	if err != nil {
		t.Fatalf("reading the first save: %v", err)
	}

	if err := store.Save(theCharacter, columns); err != nil {
		t.Fatalf("second Save: %v", err)
	}
	second, err := os.ReadFile(ledgerPathOf(store, theCharacter))
	if err != nil {
		t.Fatalf("reading the second save: %v", err)
	}

	if string(first) != string(second) {
		t.Errorf("two saves of one ledger differ: %d bytes then %d", len(first), len(second))
	}
}

// One character's ledger is one file, so two characters cannot read each other's.
func TestEachCharacterHasItsOwnLedger(t *testing.T) {
	t.Parallel()

	store := openExploration(t, t.TempDir())
	const other CharacterID = 0x00000000000000b2

	if err := store.Save(theCharacter, exploredFixture()); err != nil {
		t.Fatalf("Save: %v", err)
	}

	got, found, err := store.Load(other)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if found {
		t.Errorf("another character's ledger reported %d columns", len(got))
	}
}

// Every shape of a ledger this build will not read, and the rule is the one a player
// record and a camp both keep: refused whole, never repaired, never partly believed. A
// history half restored is a map with holes nobody can account for.
func TestExplorationStoreRefusesAFileItCannotReadExactly(t *testing.T) {
	t.Parallel()

	sound, err := encodeExploration(exploredFixture())
	if err != nil {
		t.Fatalf("encodeExploration: %v", err)
	}

	damage := map[string]func([]byte) []byte{
		"a wrong magic": func(b []byte) []byte {
			broken := append([]byte(nil), b...)
			broken[0] = 'X'
			return broken
		},
		"a version this build does not speak": func(b []byte) []byte {
			broken := append([]byte(nil), b...)
			broken[4] = byte(ExplorationVersion) + 1
			return broken
		},
		"a flipped byte under the checksum": func(b []byte) []byte {
			broken := append([]byte(nil), b...)
			broken[explorationHeaderSize] ^= 0xFF
			return broken
		},
		"a count larger than the file": func(b []byte) []byte {
			broken := append([]byte(nil), b...)
			broken[offExplorationCount] = 0xFF
			return broken
		},
		"a truncated entry": func(b []byte) []byte {
			return append([]byte(nil), b[:len(b)-1]...)
		},
		"nothing but a header": func(b []byte) []byte {
			return append([]byte(nil), b[:explorationHeaderSize]...)
		},
		"an empty file": func([]byte) []byte { return nil },
	}

	for name, break_ := range damage {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			store := openExploration(t, t.TempDir())
			if err := os.WriteFile(ledgerPathOf(store, theCharacter), break_(sound), 0o600); err != nil {
				t.Fatalf("writing the damaged file: %v", err)
			}

			got, found, err := store.Load(theCharacter)
			if !errors.Is(err, world.ErrCorruptStore) {
				t.Fatalf("Load = %v, want ErrCorruptStore", err)
			}
			// The other half of "refused whole": nothing is handed back for a caller to
			// half-believe, and it is not reported as an unwalked character either —
			// that is a start whose first save would write over the file.
			if found || got != nil {
				t.Errorf("a corrupt ledger was reported as found=%v with %d columns", found, len(got))
			}
		})
	}
}

// The size check runs before the read, so a corrupt directory cannot be turned into an
// out-of-memory by a length field. Pinned because the guard is invisible when it works.
func TestExplorationStoreRefusesAFileTooLargeToBeALedger(t *testing.T) {
	t.Parallel()

	store := openExploration(t, t.TempDir())
	oversized := make([]byte, maxExplorationFileSize+1)
	copy(oversized[0:4], explorationMagic[:])
	if err := os.WriteFile(ledgerPathOf(store, theCharacter), oversized, 0o600); err != nil {
		t.Fatalf("writing the oversized file: %v", err)
	}

	if _, _, err := store.Load(theCharacter); !errors.Is(err, world.ErrCorruptStore) {
		t.Fatalf("Load = %v, want ErrCorruptStore", err)
	}
}

// The writer refuses to exceed the cap, so this build can never write a file it would
// then refuse to read. Refused at the write is the whole point: the other order is the
// failure that looks like a success until the next login.
func TestExplorationStoreRefusesToWriteMoreThanTheCap(t *testing.T) {
	t.Parallel()

	tooMany := make([]world.Column, MaxExploredColumns+1)
	for i := range tooMany {
		tooMany[i] = world.Column{CX: int32(i), CZ: 1}
	}

	err := openExploration(t, t.TempDir()).Save(theCharacter, tooMany)
	if !errors.Is(err, ErrTooManyColumns) {
		t.Fatalf("Save = %v, want ErrTooManyColumns", err)
	}
}

// A ledger exactly at the cap is written and read back whole. The bound is inclusive,
// and an off-by-one here would make the last column a character can explore the one
// that loses them the file.
func TestALedgerExactlyAtTheCapRoundTrips(t *testing.T) {
	t.Parallel()

	full := make([]world.Column, MaxExploredColumns)
	for i := range full {
		full[i] = world.Column{CX: int32(i % 256), CZ: int32(i / 256)}
	}

	store := openExploration(t, t.TempDir())
	if err := store.Save(theCharacter, full); err != nil {
		t.Fatalf("Save: %v", err)
	}
	got, found, err := store.Load(theCharacter)
	if err != nil || !found {
		t.Fatalf("Load = %v, found=%v", err, found)
	}
	if len(got) != MaxExploredColumns {
		t.Fatalf("a full ledger loaded %d columns, want %d", len(got), MaxExploredColumns)
	}
}

// Quarantine keeps the bytes and moves them out of the way, which is the doctrine
// Store.Quarantine keeps: the file is the only evidence of what went wrong, and the
// session that could not read it is about to write to that exact path.
func TestQuarantineKeepsALedgerItCouldNotRead(t *testing.T) {
	t.Parallel()

	store := openExploration(t, t.TempDir())
	path := ledgerPathOf(store, theCharacter)
	if err := os.WriteFile(path, []byte("not a ledger"), 0o600); err != nil {
		t.Fatalf("writing the damaged file: %v", err)
	}

	aside, err := store.Quarantine(theCharacter)
	if err != nil {
		t.Fatalf("Quarantine: %v", err)
	}
	if !strings.HasPrefix(aside, path+corruptFileSuffix) {
		t.Errorf("a quarantined ledger went to %q, which does not name the file it came from", aside)
	}
	if _, err := os.Stat(path); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("the ledger is still at its own path after being set aside: %v", err)
	}
	kept, err := os.ReadFile(aside)
	if err != nil {
		t.Fatalf("reading the kept file: %v", err)
	}
	if string(kept) != "not a ledger" {
		t.Errorf("the kept file holds %q, want the bytes that could not be read", kept)
	}
}

// The ephemeral world: a nil store keeps nothing and every method is a no-op on one,
// the shape a nil Store, StructureStore and ClockStore all have. Pinned because the
// alternative is a nil check at every call site, and the one that is forgotten panics.
func TestANilExplorationStoreKeepsNothing(t *testing.T) {
	t.Parallel()

	var store *ExplorationStore

	if dir := store.Dir(); dir != "" {
		t.Errorf("a nil store names the directory %q", dir)
	}
	if err := store.Save(theCharacter, exploredFixture()); err != nil {
		t.Errorf("Save on a nil store: %v", err)
	}
	got, found, err := store.Load(theCharacter)
	if err != nil || found || got != nil {
		t.Errorf("Load on a nil store = %v, found=%v, %d columns", err, found, len(got))
	}
	aside, err := store.Quarantine(theCharacter)
	if err != nil || aside != "" {
		t.Errorf("Quarantine on a nil store = %q, %v", aside, err)
	}
}

// An empty -world-dir is main's decision to make, so the constructor refuses it rather
// than quietly answering with a store that writes nowhere. The same refusal OpenStore,
// OpenStructureStore and OpenClockStore all make.
func TestOpenExplorationStoreRefusesAnUnnamedWorldDirectory(t *testing.T) {
	t.Parallel()

	store, err := OpenExplorationStore("")
	if err == nil {
		t.Fatal("an unnamed world directory was accepted")
	}
	if store != nil {
		t.Error("OpenExplorationStore returned a store beside its error")
	}
}

// A temporary a crash left mid-rename is swept on open, and nothing else is: the
// directory is one this store creates and fills, so it may name the shape of its own
// records — the division #137 drew between the operator's directory and ours.
func TestOpeningSweepsTemporariesAndNothingElse(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	explorationDir := filepath.Join(dir, explorationDirName)
	if err := os.MkdirAll(explorationDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	leftover := filepath.Join(explorationDir, theCharacter.String()+recordFileExt+".tmp1234")
	bystander := filepath.Join(explorationDir, "notes.txt")
	for _, path := range []string{leftover, bystander} {
		if err := os.WriteFile(path, []byte("x"), 0o600); err != nil {
			t.Fatalf("writing %s: %v", path, err)
		}
	}

	openExploration(t, dir)

	if _, err := os.Stat(leftover); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("a leftover temporary survived the sweep: %v", err)
	}
	if _, err := os.Stat(bystander); err != nil {
		t.Errorf("the sweep removed a file this store never wrote: %v", err)
	}
}
