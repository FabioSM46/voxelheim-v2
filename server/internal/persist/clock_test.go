// Tests for the clock file. Same discipline as structures_test.go: a corrupt file is
// produced by writing bytes rather than by reaching into the encoder, so what is pinned
// is what a reader on another build would actually find on the disk.
package persist

import (
	"errors"
	"math"
	"os"
	"path/filepath"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func openClock(t *testing.T, dir string) *ClockStore {
	t.Helper()

	store, err := OpenClockStore(dir)
	if err != nil {
		t.Fatalf("OpenClockStore: %v", err)
	}
	return store
}

// The round trip, across the values that mean something and the two that only this
// layer can see.
//
// **math.MaxUint32 is here on purpose.** A tick that large is not one this simulation
// can be in — game.Sim.RestoreClock refuses anything at or beyond the day length — and
// it still round-trips through the file, because judging what a number *means* belongs
// to the package that owns the day length and not to the one that writes bytes down.
// This is that layering, stated as a test rather than as a comment.
func TestClockStoreRoundTripsATickOfDay(t *testing.T) {
	t.Parallel()

	for _, want := range []uint32{0, 1, 14_400, 23_999, math.MaxUint32} {
		store := openClock(t, t.TempDir())

		if err := store.Save(want); err != nil {
			t.Fatalf("Save(%d): %v", want, err)
		}

		got, found, err := store.Load()
		if err != nil {
			t.Fatalf("Load after Save(%d): %v", want, err)
		}
		if !found {
			t.Fatalf("the clock that was just saved at %d was reported as absent", want)
		}
		if got != want {
			t.Errorf("tick of day round-tripped as %d, want %d", got, want)
		}
	}
}

// A world nobody has played in has no file, and that is a world starting at first light
// rather than a failure. The distinction matters upstream: an error logs, and a log line
// for every fresh world would be noise.
func TestClockStoreReportsAnUnplayedWorldAsAbsent(t *testing.T) {
	t.Parallel()

	got, found, err := openClock(t, t.TempDir()).Load()
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if found {
		t.Errorf("a world with no clock file reported tick %d", got)
	}
	if got != 0 {
		t.Errorf("an absent clock reported tick %d, want 0", got)
	}
}

// Every shape of a clock file this build will not read, and the rule is the one a camp
// and a player record both keep: refused whole, never repaired, never partly believed.
//
// A clock read wrong is worse than one not read at all — it is a world that comes back
// at the wrong time of day and then writes that over the record of the right one — so
// each of these has to be an error rather than a zero.
func TestClockStoreRefusesAFileItCannotReadExactly(t *testing.T) {
	t.Parallel()

	sound := encodeClock(14_400)

	damage := map[string]func([]byte) []byte{
		"a wrong magic": func(b []byte) []byte {
			broken := append([]byte(nil), b...)
			broken[0] = 'X'
			return broken
		},
		"another store's magic": func(b []byte) []byte {
			broken := append([]byte(nil), b...)
			copy(broken[0:4], structuresMagic[:])
			return broken
		},
		"a version this build does not speak": func(b []byte) []byte {
			broken := append([]byte(nil), b...)
			broken[4] = byte(ClockVersion) + 1
			return broken
		},
		"a flipped byte under the checksum": func(b []byte) []byte {
			broken := append([]byte(nil), b...)
			broken[offClockTickOfDay] ^= 0xFF
			return broken
		},
		"a truncated file": func(b []byte) []byte {
			return append([]byte(nil), b[:len(b)-1]...)
		},
		"nothing but a header": func(b []byte) []byte {
			return append([]byte(nil), b[:world.HeaderSize]...)
		},
		"an empty file": func([]byte) []byte { return nil },
		"a longer file": func(b []byte) []byte {
			return append(append([]byte(nil), b...), 0)
		},
	}

	for name, break_ := range damage {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			store := openClock(t, t.TempDir())
			if err := os.WriteFile(store.Path(), break_(sound), 0o600); err != nil {
				t.Fatalf("writing the damaged file: %v", err)
			}

			got, found, err := store.Load()
			if !errors.Is(err, world.ErrCorruptStore) {
				t.Fatalf("Load = %v, want ErrCorruptStore", err)
			}
			// The other half of "refused whole": nothing is handed back for a caller to
			// half-believe, and it is not reported as an unplayed world either — an
			// unplayed world is a start that overwrites the file with a dawn.
			if found || got != 0 {
				t.Errorf("a corrupt clock was reported as found=%v at tick %d", found, got)
			}
			// **And the file is kept.** Reading it is what fails; nothing about that is
			// a licence to destroy the evidence.
			if _, err := os.Stat(store.Path()); err != nil {
				t.Errorf("a refused clock file did not survive the read: %v", err)
			}
		})
	}
}

// Two saves of the same tick are the same bytes, which is what makes a test able to
// compare files rather than parse them.
func TestSavingTheSameTickTwiceWritesTheSameBytes(t *testing.T) {
	t.Parallel()

	store := openClock(t, t.TempDir())

	if err := store.Save(9_000); err != nil {
		t.Fatalf("first Save: %v", err)
	}
	first, err := os.ReadFile(store.Path())
	if err != nil {
		t.Fatalf("reading the first save: %v", err)
	}

	if err := store.Save(9_000); err != nil {
		t.Fatalf("second Save: %v", err)
	}
	second, err := os.ReadFile(store.Path())
	if err != nil {
		t.Fatalf("reading the second save: %v", err)
	}

	if string(first) != string(second) {
		t.Errorf("two saves of one tick differ: %d bytes then %d", len(first), len(second))
	}
}

// A later save replaces the earlier one whole, which is the property the atomic write
// exists to give: there is no state in which the file holds half of each.
func TestSavingAgainReplacesTheStoredTick(t *testing.T) {
	t.Parallel()

	store := openClock(t, t.TempDir())
	if err := store.Save(1); err != nil {
		t.Fatalf("Save(1): %v", err)
	}
	if err := store.Save(21_600); err != nil {
		t.Fatalf("Save(21600): %v", err)
	}

	got, found, err := store.Load()
	if err != nil || !found {
		t.Fatalf("Load = (%d, %v, %v)", got, found, err)
	}
	if got != 21_600 {
		t.Errorf("the file holds tick %d, want the second save's 21600", got)
	}
}

// A nil store is the ephemeral world: it keeps a clock in memory and writes nothing.
// A no-op rather than a branch at every call site, the shape a nil *Store, a nil
// *StructureStore and a nil world.Store already have.
func TestAnEphemeralWorldWritesNoClock(t *testing.T) {
	t.Parallel()

	var store *ClockStore

	if err := store.Save(14_400); err != nil {
		t.Errorf("Save on an ephemeral world: %v", err)
	}
	got, found, err := store.Load()
	if err != nil || found || got != 0 {
		t.Errorf("Load on an ephemeral world = (%d, %v, %v), want (0, false, nil)", got, found, err)
	}
	if store.Path() != "" {
		t.Errorf("an ephemeral world names a path: %q", store.Path())
	}
}

func TestOpenClockStoreRefusesAnUnnamedWorldDirectory(t *testing.T) {
	t.Parallel()

	if _, err := OpenClockStore(""); err == nil {
		t.Fatal("an empty world directory was accepted; the ephemeral world is main's decision")
	}
}

// The leftovers of a crash mid-rename are swept on open, for the reason OpenStore and
// OpenStructureStore sweep: this store writes through world.WriteAtomic and inherits
// them.
func TestOpenClockStoreSweepsTemporaries(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	leftover := filepath.Join(dir, clockFileName+".tmp1234")
	if err := os.WriteFile(leftover, []byte("half a clock"), 0o600); err != nil {
		t.Fatalf("writing the leftover: %v", err)
	}

	openClock(t, dir)

	if _, err := os.Stat(leftover); !os.IsNotExist(err) {
		t.Errorf("the temporary file survived the open: %v", err)
	}
}

// The file's size is fixed by the format rather than by its contents, which is what
// makes the read's size check an equality instead of a ceiling. Pinned so a field added
// to the layout cannot change it without ClockVersion moving too.
func TestAClockFileIsTheSizeTheFormatSaysItIs(t *testing.T) {
	t.Parallel()

	if clockFileSize != 16 {
		t.Errorf("a clock file is %d bytes, want 16 (magic 4, version 4, tick 4, crc 4)", clockFileSize)
	}
	if got := len(encodeClock(14_400)); got != clockFileSize {
		t.Errorf("encodeClock produced %d bytes, want %d", got, clockFileSize)
	}
	// Every version and magic under the world directory is its own, so a file of one
	// kind can never be read as another even at the same size.
	if clockMagic == structuresMagic || clockMagic == playerMagic {
		t.Errorf("the clock file's magic %q collides with another store's", clockMagic[:])
	}
}
