// Tests for the structures file. Same discipline as store_test.go: a corrupt file is
// produced by writing bytes rather than by reaching into the encoder, so what is pinned
// is what a reader on another build would actually find on the disk.
package persist

import (
	"errors"
	"os"
	"path/filepath"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// campFixture is a camp with the things worth round-tripping: every kind this build can
// place, two owners, and anchors with a negative axis.
//
// **Every kind, and the campfire is why that is written down as a rule rather than as a
// list.** A record is fixed-width and the kind is one byte of it, so a new kind is a new
// value rather than a new shape — but the way to know that is to put one through the file
// and read it back, and the cheapest place to do that is the fixture every test here
// already uses. A kind added to the game and not to this list is a kind nothing round-trips.
func campFixture() []StructureRecord {
	return []StructureRecord{
		{
			Kind:   vnet.StructureKindTent,
			Anchor: [3]int32{12, 63, -8},
			Facing: vnet.FacingEast,
			Owner:  identity.PlayerID{1, 2, 3},
		},
		{
			Kind:   vnet.StructureKindForge,
			Anchor: [3]int32{-40, 70, 5},
			Facing: vnet.FacingSouth,
			Owner:  identity.PlayerID{9, 9, 9, 9},
		},
		{
			Kind:   vnet.StructureKindCampfire,
			Anchor: [3]int32{-7, 64, 21},
			Facing: vnet.FacingWest,
			Owner:  identity.PlayerID{9, 9, 9, 9},
		},
	}
}

func openCamp(t *testing.T, dir string) *StructureStore {
	t.Helper()

	store, err := OpenStructureStore(dir)
	if err != nil {
		t.Fatalf("OpenStructureStore: %v", err)
	}
	return store
}

// The round trip, field by field. Every one of the four is something a player can see:
// what it is, where it is, which way it opens, and whether it is theirs.
func TestStructureStoreRoundTripsACamp(t *testing.T) {
	t.Parallel()

	store := openCamp(t, t.TempDir())
	want := campFixture()

	if err := store.Save(want); err != nil {
		t.Fatalf("Save: %v", err)
	}

	got, found, err := store.Load()
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("the camp that was just saved was reported as absent")
	}
	if len(got) != len(want) {
		t.Fatalf("loaded %d structures, want %d", len(got), len(want))
	}
	for i := range want {
		if got[i] != want[i] {
			t.Errorf("structure %d round-tripped as %+v, want %+v", i, got[i], want[i])
		}
	}
}

// A world nobody has built in has no file, and that is an empty camp rather than a
// failure. The distinction matters upstream: an error starts the server with no
// structures *and logs*, and a log line for every fresh world would be noise.
func TestStructureStoreReportsAnUnbuiltWorldAsAbsent(t *testing.T) {
	t.Parallel()

	got, found, err := openCamp(t, t.TempDir()).Load()
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if found {
		t.Errorf("a world with no structures file reported %d structures", len(got))
	}
}

// Two saves of the same camp are the same bytes, which is what makes the ordering
// contract testable at all: a caller comparing files is comparing state, not map order.
func TestSavingTheSameCampTwiceWritesTheSameBytes(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	store := openCamp(t, dir)
	camp := campFixture()

	if err := store.Save(camp); err != nil {
		t.Fatalf("first Save: %v", err)
	}
	first, err := os.ReadFile(store.Path())
	if err != nil {
		t.Fatalf("reading the first save: %v", err)
	}

	if err := store.Save(camp); err != nil {
		t.Fatalf("second Save: %v", err)
	}
	second, err := os.ReadFile(store.Path())
	if err != nil {
		t.Fatalf("reading the second save: %v", err)
	}

	if string(first) != string(second) {
		t.Errorf("two saves of one camp differ: %d bytes then %d", len(first), len(second))
	}
}

// Every shape of a structures file this build will not read, and the rule is the one a
// player record keeps: refused whole, never repaired, never partly believed. A camp half
// restored is a player who cannot tell which of their buildings a bug ate.
func TestStructureStoreRefusesAFileItCannotReadExactly(t *testing.T) {
	t.Parallel()

	sound, err := encodeStructures(campFixture())
	if err != nil {
		t.Fatalf("encodeStructures: %v", err)
	}

	damage := map[string]func([]byte) []byte{
		"a wrong magic": func(b []byte) []byte {
			broken := append([]byte(nil), b...)
			broken[0] = 'X'
			return broken
		},
		"a version this build does not speak": func(b []byte) []byte {
			broken := append([]byte(nil), b...)
			broken[4] = byte(StructuresVersion) + 1
			return broken
		},
		"a flipped byte under the checksum": func(b []byte) []byte {
			broken := append([]byte(nil), b...)
			broken[structuresHeaderSize] ^= 0xFF
			return broken
		},
		"a count larger than the file": func(b []byte) []byte {
			broken := append([]byte(nil), b...)
			broken[offStructureCount] = 0xFF
			return broken
		},
		"a truncated entry": func(b []byte) []byte {
			return append([]byte(nil), b[:len(b)-1]...)
		},
		"nothing but a header": func(b []byte) []byte {
			return append([]byte(nil), b[:structuresHeaderSize]...)
		},
	}

	for name, break_ := range damage {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			store := openCamp(t, t.TempDir())
			if err := os.WriteFile(store.Path(), break_(sound), 0o600); err != nil {
				t.Fatalf("writing the damaged file: %v", err)
			}

			got, found, err := store.Load()
			if !errors.Is(err, world.ErrCorruptStore) {
				t.Fatalf("Load = %v, want ErrCorruptStore", err)
			}
			// The other half of "refused whole": nothing is handed back for a caller to
			// half-believe, and it is not reported as an empty world either — an empty
			// world is a start that would overwrite the file.
			if found || got != nil {
				t.Errorf("a corrupt camp was reported as found=%v with %d structures", found, len(got))
			}
		})
	}
}

// The size check runs before the read, so a corrupt directory cannot be turned into an
// out-of-memory by a length field. Pinned because the guard is invisible when it works.
func TestStructureStoreRefusesAFileTooLargeToBeACamp(t *testing.T) {
	t.Parallel()

	store := openCamp(t, t.TempDir())
	if err := os.WriteFile(store.Path(), make([]byte, maxStructuresFileSize+1), 0o600); err != nil {
		t.Fatalf("writing the oversized file: %v", err)
	}
	if _, _, err := store.Load(); !errors.Is(err, world.ErrCorruptStore) {
		t.Fatalf("Load = %v, want ErrCorruptStore", err)
	}
}

// The writer refuses what the reader would refuse, which is the only reason MaxStructures
// is safe to enforce at all: a cap applied on one side only is a server that writes a
// file it cannot read back, and finds out at the next restart.
func TestSaveRefusesMoreStructuresThanTheFormatHolds(t *testing.T) {
	t.Parallel()

	store := openCamp(t, t.TempDir())
	camp := make([]StructureRecord, MaxStructures+1)
	for i := range camp {
		camp[i] = StructureRecord{Kind: vnet.StructureKindTent, Facing: vnet.FacingNorth, Owner: identity.PlayerID{1}}
	}

	if err := store.Save(camp); !errors.Is(err, ErrTooManyStructures) {
		t.Fatalf("Save = %v, want ErrTooManyStructures", err)
	}
	if _, err := os.Stat(store.Path()); !os.IsNotExist(err) {
		t.Error("a refused save left a file behind")
	}
}

// A nil store is the ephemeral world, and it is a no-op rather than a branch at every
// call site — the same shape a nil *Store and a nil world.Store already have.
func TestAnEphemeralWorldKeepsNoCamp(t *testing.T) {
	t.Parallel()

	var store *StructureStore

	if err := store.Save(campFixture()); err != nil {
		t.Errorf("Save on an ephemeral world: %v", err)
	}
	got, found, err := store.Load()
	if err != nil || found || got != nil {
		t.Errorf("Load on an ephemeral world = (%v, %v, %v), want (nil, false, nil)", got, found, err)
	}
	if store.Path() != "" {
		t.Errorf("an ephemeral world names a path: %q", store.Path())
	}
}

func TestOpenStructureStoreRefusesAnUnnamedWorldDirectory(t *testing.T) {
	t.Parallel()

	if _, err := OpenStructureStore(""); err == nil {
		t.Fatal("an empty world directory was accepted; the ephemeral world is main's decision")
	}
}

// The leftovers of a crash mid-rename are swept on open, for the reason OpenStore sweeps
// the players directory: this store writes through world.WriteAtomic and inherits them.
func TestOpenStructureStoreSweepsTemporaries(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	leftover := filepath.Join(dir, structuresFileName+".tmp1234")
	if err := os.WriteFile(leftover, []byte("half a camp"), 0o600); err != nil {
		t.Fatalf("writing the leftover: %v", err)
	}

	openCamp(t, dir)

	if _, err := os.Stat(leftover); !os.IsNotExist(err) {
		t.Errorf("the temporary file survived the open: %v", err)
	}
}

// Both size constants are derived, and a reader on another build has to compute the same
// ones. Pinned so a field added to an entry cannot silently change the layout without
// StructuresVersion moving with it.
func TestACampIsTheSizeTheFormatSaysItIs(t *testing.T) {
	t.Parallel()

	if structureEntrySize != 46 {
		t.Errorf("one entry is %d bytes, want 46 (kind 1, facing 1, anchor 12, owner 32)", structureEntrySize)
	}
	// The version tracks the layout above and nothing else, which is the claim the campfire
	// tested: a new kind is a new *value* of a byte that was always there, so it changed no
	// size here and moved no version. A layout that really does change breaks the size
	// assertion above and this one together, which is the pair being read.
	if StructuresVersion != 1 {
		t.Errorf("StructuresVersion = %d beside an unchanged %d-byte entry; a new kind is not a layout change",
			StructuresVersion, structureEntrySize)
	}

	camp := campFixture()
	encoded, err := encodeStructures(camp)
	if err != nil {
		t.Fatalf("encodeStructures: %v", err)
	}
	want := structuresHeaderSize + len(camp)*structureEntrySize + world.ChecksumSize
	if len(encoded) != want {
		t.Errorf("a %d-structure camp encodes to %d bytes, want %d", len(camp), len(encoded), want)
	}
}
