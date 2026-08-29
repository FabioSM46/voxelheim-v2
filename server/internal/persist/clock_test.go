// Tests for the clock file. Same discipline as structures_test.go: a corrupt file is
// produced by writing bytes rather than by reaching into the encoder, so what is pinned
// is what a reader on another build would actually find on the disk.
package persist

import (
	"encoding/binary"
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

// The round trip, across the values that mean something and the ones that only this
// layer can see.
//
// **The extreme values are here on purpose.** Ticks that large are not ones this
// simulation can be in, and a storm due before 1970 is not one any world will have —
// and all of them still round-trip, because judging what a number *means* belongs to
// the package that owns the day length and not to the one that writes bytes down. This
// is that layering, stated as a test rather than as a comment.
func TestClockStoreRoundTripsEveryField(t *testing.T) {
	t.Parallel()

	for _, want := range []Clock{
		{},
		{TickOfDay: 1, WorldTick: 1},
		{TickOfDay: 14_400, WorldTick: 14_400, NextStormUnix: 1_700_000_000},
		{TickOfDay: 23_999, WorldTick: 9*24_000 + 23_999, NextStormUnix: -1},
		{TickOfDay: math.MaxUint32, WorldTick: math.MaxUint64, NextStormUnix: math.MaxInt64},
		{TickOfDay: 7, WorldTick: 7, NextStormUnix: math.MinInt64},
	} {
		store := openClock(t, t.TempDir())

		if err := store.Save(want.TickOfDay, want.WorldTick, want.NextStormUnix); err != nil {
			t.Fatalf("Save(%+v): %v", want, err)
		}

		got, found, err := store.Load()
		if err != nil {
			t.Fatalf("Load after Save(%+v): %v", want, err)
		}
		if !found {
			t.Fatalf("the clock that was just saved as %+v was reported as absent", want)
		}
		if got != want {
			t.Errorf("the clock round-tripped as %+v, want %+v", got, want)
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
		t.Errorf("a world with no clock file reported %+v", got)
	}
	if got != (Clock{}) {
		t.Errorf("an absent clock reported %+v, want a zero clock", got)
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

	sound := encodeClock(14_400, 9*24_000+14_400, 1_700_000_000)

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
		// The size picks the layout and the header still decides, so a file of this
		// build's length claiming the previous version is refused rather than read as
		// the format it names.
		"the previous version at this build's length": func(b []byte) []byte {
			broken := append([]byte(nil), b...)
			broken[4] = byte(previousClockVersion)
			world.PutChecksum(broken)
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
			if found || got != (Clock{}) {
				t.Errorf("a corrupt clock was reported as found=%v holding %+v", found, got)
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

	if err := store.Save(9_000, 9_000, 1_700_000_000); err != nil {
		t.Fatalf("first Save: %v", err)
	}
	first, err := os.ReadFile(store.Path())
	if err != nil {
		t.Fatalf("reading the first save: %v", err)
	}

	if err := store.Save(9_000, 9_000, 1_700_000_000); err != nil {
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
	if err := store.Save(1, 1, 0); err != nil {
		t.Fatalf("Save(1): %v", err)
	}
	if err := store.Save(21_600, 21_600, 1_700_000_000); err != nil {
		t.Fatalf("Save(21600): %v", err)
	}

	got, found, err := store.Load()
	if err != nil || !found {
		t.Fatalf("Load = (%+v, %v, %v)", got, found, err)
	}
	want := Clock{TickOfDay: 21_600, WorldTick: 21_600, NextStormUnix: 1_700_000_000}
	if got != want {
		t.Errorf("the file holds %+v, want the second save's %+v", got, want)
	}
}

// A nil store is the ephemeral world: it keeps a clock in memory and writes nothing.
// A no-op rather than a branch at every call site, the shape a nil *Store, a nil
// *StructureStore and a nil world.Store already have.
func TestAnEphemeralWorldWritesNoClock(t *testing.T) {
	t.Parallel()

	var store *ClockStore

	if err := store.Save(14_400, 14_400, 1_700_000_000); err != nil {
		t.Errorf("Save on an ephemeral world: %v", err)
	}
	got, found, err := store.Load()
	if err != nil || found || got != (Clock{}) {
		t.Errorf("Load on an ephemeral world = (%+v, %v, %v), want a zero clock, false, nil", got, found, err)
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

	if clockFileSize != 32 {
		t.Errorf("a clock file is %d bytes, want 32 (magic 4, version 4, tick 4, world tick 8, storm 8, crc 4)", clockFileSize)
	}
	if clockV1FileSize != 16 {
		t.Errorf("a version-%d clock file is %d bytes, want 16", previousClockVersion, clockV1FileSize)
	}
	if got := len(encodeClock(14_400, 14_400, 0)); got != clockFileSize {
		t.Errorf("encodeClock produced %d bytes, want %d", got, clockFileSize)
	}
	// The two formats have to differ in length, because the length is what picks the
	// layout before a version is read.
	if clockFileSize == clockV1FileSize {
		t.Error("the two clock formats are the same size; the read cannot tell them apart")
	}
	// Every version and magic under the world directory is its own, so a file of one
	// kind can never be read as another even at the same size.
	if clockMagic == structuresMagic || clockMagic == playerMagic {
		t.Errorf("the clock file's magic %q collides with another store's", clockMagic[:])
	}
}

// encodeV1Clock writes the sixteen bytes the previous format wrote, byte for byte.
//
// Built here rather than kept in the store, and the asymmetry is the point: this build
// reads that format and must never write it, so the only encoder for it belongs to the
// test that has to produce one.
func encodeV1Clock(tickOfDay uint32) []byte {
	buf := world.NewRecord(offClockTickOfDay, 4, clockMagic, previousClockVersion)
	binary.LittleEndian.PutUint32(buf[offClockTickOfDay:offClockTickOfDay+4], tickOfDay)
	world.PutChecksum(buf)
	return buf
}

// A world written by the previous build comes back, rather than being refused over a
// field that did not exist when it was written. The decodeV7Record choice, for a clock.
//
// What it comes back as is the honest reading of sixteen bytes: the day phase it stopped
// at, an absolute clock starting from that phase, and no storm scheduled. The pair
// satisfies the invariant game.Sim.RestoreClock enforces, which is what makes the
// migration usable rather than merely readable.
func TestAVersionOneClockIsMigratedRatherThanRefused(t *testing.T) {
	t.Parallel()

	for _, tickOfDay := range []uint32{0, 1, 14_400, 23_999} {
		store := openClock(t, t.TempDir())
		if err := os.WriteFile(store.Path(), encodeV1Clock(tickOfDay), 0o600); err != nil {
			t.Fatalf("writing the version-1 clock: %v", err)
		}

		got, found, err := store.Load()
		if err != nil {
			t.Fatalf("Load of a version-1 clock at tick %d: %v", tickOfDay, err)
		}
		if !found {
			t.Fatalf("a version-1 clock at tick %d was reported as absent", tickOfDay)
		}
		want := Clock{TickOfDay: tickOfDay, WorldTick: uint64(tickOfDay)}
		if got != want {
			t.Errorf("a version-1 clock at tick %d migrated to %+v, want %+v", tickOfDay, got, want)
		}
	}
}

// The migration is a read and not a rewrite: loading changes nothing on disk, so the
// file an operator can inspect is the one their previous build wrote. What turns it into
// this build's format is the first save.
func TestAVersionOneClockIsRewrittenByTheFirstSaveAndNotByTheLoad(t *testing.T) {
	t.Parallel()

	store := openClock(t, t.TempDir())
	original := encodeV1Clock(14_400)
	if err := os.WriteFile(store.Path(), original, 0o600); err != nil {
		t.Fatalf("writing the version-1 clock: %v", err)
	}

	if _, _, err := store.Load(); err != nil {
		t.Fatalf("Load: %v", err)
	}
	after, err := os.ReadFile(store.Path())
	if err != nil {
		t.Fatalf("reading the file after the load: %v", err)
	}
	if string(after) != string(original) {
		t.Errorf("the load rewrote the file: %d bytes, was %d", len(after), len(original))
	}

	// The first save, carrying the migrated clock forward with a day of history behind it.
	if err := store.Save(14_401, 24_000+14_401, 1_700_000_000); err != nil {
		t.Fatalf("Save: %v", err)
	}
	migrated, err := os.ReadFile(store.Path())
	if err != nil {
		t.Fatalf("reading the file after the save: %v", err)
	}
	if len(migrated) != clockFileSize {
		t.Errorf("the rewritten file is %d bytes, want %d", len(migrated), clockFileSize)
	}
	if version := binary.LittleEndian.Uint32(migrated[4:8]); version != ClockVersion {
		t.Errorf("the rewritten file claims version %d, want %d", version, ClockVersion)
	}

	got, found, err := store.Load()
	if err != nil || !found {
		t.Fatalf("Load after the rewrite = (%+v, %v, %v)", got, found, err)
	}
	want := Clock{TickOfDay: 14_401, WorldTick: 24_000 + 14_401, NextStormUnix: 1_700_000_000}
	if got != want {
		t.Errorf("the rewritten file holds %+v, want %+v", got, want)
	}
}

// A version-1 file that is damaged is refused like any other, so the migration path is
// not a way past the checks. Each of these is sixteen bytes and so reaches decodeV1Clock,
// which is the point: the older layout is read, not trusted.
func TestADamagedVersionOneClockIsRefusedToo(t *testing.T) {
	t.Parallel()

	sound := encodeV1Clock(14_400)

	damage := map[string]func([]byte) []byte{
		"a wrong magic": func(b []byte) []byte {
			broken := append([]byte(nil), b...)
			broken[0] = 'X'
			return broken
		},
		"a flipped byte under the checksum": func(b []byte) []byte {
			broken := append([]byte(nil), b...)
			broken[offClockTickOfDay] ^= 0xFF
			return broken
		},
		// This build's version at the previous build's length: the length picks the
		// layout, and the header refuses it there.
		"this build's version at the previous length": func(b []byte) []byte {
			broken := append([]byte(nil), b...)
			broken[4] = byte(ClockVersion)
			world.PutChecksum(broken)
			return broken
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
			if found || got != (Clock{}) {
				t.Errorf("a damaged version-1 clock was reported as found=%v holding %+v", found, got)
			}
			if _, err := os.Stat(store.Path()); err != nil {
				t.Errorf("a refused clock file did not survive the read: %v", err)
			}
		})
	}
}
