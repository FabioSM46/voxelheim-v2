// Tests for the per-character marker file. Same discipline as store_test.go,
// structures_test.go and exploration_test.go: a corrupt file is produced by writing bytes
// rather than by reaching into the encoder, so what is pinned is what a reader on another
// build would find on the disk.
package persist

import (
	"encoding/binary"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// markedFixture is a map with the things worth round-tripping: both axes negative, both
// positive, one of each, the origin, a note with characters that are more than one byte,
// an empty note, and ids with a gap in them where a removal happened.
func markedFixture() StoredMarkers {
	return StoredMarkers{
		NextID: 9,
		Markers: []protocol.Marker{
			{MarkerID: 1, X: -40, Z: -7, Kind: vnet.MarkerKindResource, Note: "iron under the hill"},
			{MarkerID: 2, X: 0, Z: 0, Kind: vnet.MarkerKindCamp, Note: ""},
			{MarkerID: 5, X: 3, Z: -2, Kind: vnet.MarkerKindCave, Note: "hellingrunnr — kaldr ok djúpr"},
			{MarkerID: 8, X: 1 << 20, Z: -(1 << 20), Kind: vnet.MarkerKindNote, Note: "vörðr"},
		},
	}
}

func openMarkers(t *testing.T, dir string) *MarkerStore {
	t.Helper()

	store, err := OpenMarkerStore(dir)
	if err != nil {
		t.Fatalf("OpenMarkerStore: %v", err)
	}
	return store
}

// markerPathOf is where a test writes damaged bytes. Derived from the store rather than
// rebuilt by hand, so a change to the naming rule breaks the tests that depend on it
// instead of silently testing a file nothing reads.
func markerPathOf(store *MarkerStore, id CharacterID) string {
	return filepath.Join(store.Dir(), id.String()+recordFileExt)
}

// loadMarkers is the read half every test here starts from, with the two answers that
// are never the point of the test folded into a fatal.
func loadMarkers(t *testing.T, store *MarkerStore, id CharacterID) StoredMarkers {
	t.Helper()

	got, found, err := store.Load(id)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("the store holds no marks for this character")
	}
	return got
}

// sameMarks reports whether two lists are equal field for field and in order. The order
// is the caller's and this package keeps it — the same contract encodeStructures and
// encodeExploration have.
func sameMarks(a, b []protocol.Marker) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

// The round trip, mark by mark and in the order it was given, counter included.
func TestMarkerStoreRoundTripsAMap(t *testing.T) {
	t.Parallel()

	store := openMarkers(t, t.TempDir())
	want := markedFixture()

	if err := store.Save(theCharacter, want); err != nil {
		t.Fatalf("Save: %v", err)
	}
	got := loadMarkers(t, store, theCharacter)

	if !sameMarks(got.Markers, want.Markers) {
		t.Errorf("the marks came back as %+v, want %+v", got.Markers, want.Markers)
	}
	if got.NextID != want.NextID {
		t.Errorf("the counter came back as %d, want %d", got.NextID, want.NextID)
	}
}

// **The whole point of the header field**: the counter survives a reload, so the id a
// removal freed is never handed out again. Derived as max(id)+1 this would be 6 after the
// highest mark went, and the next placement would mint an id the client already knows.
func TestTheCounterSurvivesAReloadPastTheHighestIdItHolds(t *testing.T) {
	t.Parallel()

	store := openMarkers(t, t.TempDir())
	stored := markedFixture()

	// The highest-numbered mark is removed and the counter is deliberately left alone,
	// which is what a session does.
	stored.Markers = stored.Markers[:len(stored.Markers)-1]
	if err := store.Save(theCharacter, stored); err != nil {
		t.Fatalf("Save: %v", err)
	}

	got := loadMarkers(t, store, theCharacter)
	if got.NextID != 9 {
		t.Fatalf("the counter came back as %d after the highest mark was removed, want 9", got.NextID)
	}
	highest := uint64(0)
	for _, marker := range got.Markers {
		highest = max(highest, marker.MarkerID)
	}
	if got.NextID <= highest {
		t.Errorf("the counter is %d and the highest id held is %d; an id would be reused", got.NextID, highest)
	}
}

// Nothing to mark is a file, not an absence — and the file is what carries the counter
// forward for a character who has just removed their last mark.
func TestAnEmptyMapIsStillAFile(t *testing.T) {
	t.Parallel()

	store := openMarkers(t, t.TempDir())
	if err := store.Save(theCharacter, StoredMarkers{NextID: 12}); err != nil {
		t.Fatalf("Save: %v", err)
	}

	got := loadMarkers(t, store, theCharacter)
	if len(got.Markers) != 0 {
		t.Errorf("an empty map came back with %d marks", len(got.Markers))
	}
	if got.NextID != 12 {
		t.Errorf("an empty map came back with the counter at %d, want 12", got.NextID)
	}
}

// A character who has marked nothing has no file, which is not an error. Three answers,
// and this is the one that must not be reported as the third.
func TestAnUnmarkedCharacterHasNoFileAndThatIsNotAnError(t *testing.T) {
	t.Parallel()

	store := openMarkers(t, t.TempDir())
	got, found, err := store.Load(theCharacter)
	if err != nil {
		t.Fatalf("Load on a character with no file: %v", err)
	}
	if found {
		t.Error("a character who has marked nothing was reported as having a file")
	}
	if len(got.Markers) != 0 || got.NextID != 0 {
		t.Errorf("a character with no file loaded %+v, want the zero value", got)
	}
}

// The widest note the format holds, at the boundary rather than near it, and a full map
// of sixty-four beside it: the two numbers the file's size is a function of.
func TestAFullMapOfWidestNotesRoundTrips(t *testing.T) {
	t.Parallel()

	note := strings.Repeat("a", MaxMarkerNote)
	full := StoredMarkers{NextID: MaxMarkers + 1}
	for i := range MaxMarkers {
		full.Markers = append(full.Markers, protocol.Marker{
			MarkerID: uint64(i + 1),
			X:        int32(i),
			Z:        int32(-i),
			Kind:     vnet.MarkerKindMonster,
			Note:     note,
		})
	}

	store := openMarkers(t, t.TempDir())
	if err := store.Save(theCharacter, full); err != nil {
		t.Fatalf("Save: %v", err)
	}
	got := loadMarkers(t, store, theCharacter)
	if len(got.Markers) != MaxMarkers {
		t.Fatalf("a full map loaded %d marks, want %d", len(got.Markers), MaxMarkers)
	}
	if got.Markers[MaxMarkers-1].Note != note {
		t.Error("the widest note did not survive the round trip")
	}

	// And the file is exactly the size the layout says it is, which is what makes a
	// truncated one refusable rather than readable as a shorter map.
	info, err := os.Stat(markerPathOf(store, theCharacter))
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if info.Size() != int64(maxMarkersFileSize) {
		t.Errorf("a full map is %d bytes on disk, want the %d the layout describes", info.Size(), maxMarkersFileSize)
	}
}

// The sixty-fifth mark is refused at the write rather than truncated, because a file
// this build cannot read back is the failure that looks like a success until the next
// login.
func TestSavingMoreMarksThanTheFormatHoldsIsRefused(t *testing.T) {
	t.Parallel()

	tooMany := StoredMarkers{NextID: MaxMarkers + 2}
	for i := range MaxMarkers + 1 {
		tooMany.Markers = append(tooMany.Markers, protocol.Marker{
			MarkerID: uint64(i + 1), Kind: vnet.MarkerKindNote,
		})
	}

	store := openMarkers(t, t.TempDir())
	err := store.Save(theCharacter, tooMany)
	if !errors.Is(err, ErrTooManyMarkers) {
		t.Fatalf("Save of %d marks = %v, want ErrTooManyMarkers", len(tooMany.Markers), err)
	}
	if _, err := os.Stat(markerPathOf(store, theCharacter)); !errors.Is(err, os.ErrNotExist) {
		t.Error("a refused save left a file behind")
	}
}

// The writer refuses what the reader would refuse, which is the whole of "this build can
// read back everything it writes". Every one of these is a caller bug rather than a state
// a player can reach, and each would otherwise be discovered as a quarantined file.
func TestTheWriterRefusesWhatTheReaderWould(t *testing.T) {
	t.Parallel()

	for name, stored := range map[string]StoredMarkers{
		"a note wider than the field": {
			NextID:  2,
			Markers: []protocol.Marker{{MarkerID: 1, Kind: vnet.MarkerKindNote, Note: strings.Repeat("b", MaxMarkerNote+1)}},
		},
		"a mark with no id": {
			NextID:  2,
			Markers: []protocol.Marker{{MarkerID: 0, Kind: vnet.MarkerKindNote}},
		},
		"a counter behind an id it handed out": {
			NextID:  3,
			Markers: []protocol.Marker{{MarkerID: 3, Kind: vnet.MarkerKindNote}},
		},
		"a counter of zero": {
			NextID: 0,
		},
		"one id naming two marks": {
			NextID: 3,
			Markers: []protocol.Marker{
				{MarkerID: 2, Kind: vnet.MarkerKindCave},
				{MarkerID: 2, Kind: vnet.MarkerKindNote},
			},
		},
		"a kind this contract does not name": {
			NextID:  2,
			Markers: []protocol.Marker{{MarkerID: 1, Kind: vnet.MarkerKind(200)}},
		},
		"the absent-field kind": {
			NextID:  2,
			Markers: []protocol.Marker{{MarkerID: 1, Kind: vnet.MarkerKindUnknown}},
		},
		// The bytes the reader's own UTF-8 case uses: a lead byte promising a
		// continuation and an ASCII '(' where that continuation should be.
		"a note that is not valid UTF-8": {
			NextID:  2,
			Markers: []protocol.Marker{{MarkerID: 1, Kind: vnet.MarkerKindNote, Note: "\xc3("}},
		},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			store := openMarkers(t, t.TempDir())
			if err := store.Save(theCharacter, stored); err == nil {
				t.Fatal("a file this build could not read back was written")
			}
		})
	}
}

// Every way a file can be wrong that the bytes alone can show, refused as corrupt so the
// caller can keep the evidence. Written as bytes for the reason the header of this file
// gives: what is pinned is what another build would find on the disk.
func TestADamagedMarkerFileIsRefused(t *testing.T) {
	t.Parallel()

	// The starting point every case damages: a real file this build wrote.
	sound := func(t *testing.T) []byte {
		t.Helper()
		data, err := encodeMarkers(markedFixture())
		if err != nil {
			t.Fatalf("encodeMarkers: %v", err)
		}
		return data
	}

	for name, damage := range map[string]func(*testing.T, []byte) []byte{
		"truncated in the middle of an entry": func(t *testing.T, data []byte) []byte {
			return data[:len(data)-markerEntrySize/2]
		},
		"a count that claims one mark more than the file holds": func(t *testing.T, data []byte) []byte {
			binary.LittleEndian.PutUint32(data[offMarkersCount:offMarkersCount+4], 5)
			world.PutChecksum(data)
			return data
		},
		"a count past the cap": func(t *testing.T, data []byte) []byte {
			binary.LittleEndian.PutUint32(data[offMarkersCount:offMarkersCount+4], MaxMarkers+1)
			world.PutChecksum(data)
			return data
		},
		"a flipped bit with the old checksum": func(t *testing.T, data []byte) []byte {
			data[markersHeaderSize+offMarkerX] ^= 0x40
			return data
		},
		"another store's magic": func(t *testing.T, data []byte) []byte {
			copy(data[0:4], explorationMagic[:])
			world.PutChecksum(data)
			return data
		},
		"a version this build does not know": func(t *testing.T, data []byte) []byte {
			binary.LittleEndian.PutUint32(data[4:8], MarkersVersion+1)
			world.PutChecksum(data)
			return data
		},
		"a mark with no id": func(t *testing.T, data []byte) []byte {
			binary.LittleEndian.PutUint64(data[markersHeaderSize+offMarkerID:markersHeaderSize+offMarkerID+8], 0)
			world.PutChecksum(data)
			return data
		},
		"one id naming two marks": func(t *testing.T, data []byte) []byte {
			second := markersHeaderSize + markerEntrySize
			binary.LittleEndian.PutUint64(data[second+offMarkerID:second+offMarkerID+8], 1)
			world.PutChecksum(data)
			return data
		},
		"a counter behind an id the file holds": func(t *testing.T, data []byte) []byte {
			binary.LittleEndian.PutUint64(data[offMarkersNextID:offMarkersNextID+8], 2)
			world.PutChecksum(data)
			return data
		},
		"a counter of zero": func(t *testing.T, data []byte) []byte {
			binary.LittleEndian.PutUint64(data[offMarkersNextID:offMarkersNextID+8], 0)
			world.PutChecksum(data)
			return data
		},
		"a kind this contract has no member for": func(t *testing.T, data []byte) []byte {
			data[markersHeaderSize+offMarkerKind] = 200
			world.PutChecksum(data)
			return data
		},
		"the absent-field kind": func(t *testing.T, data []byte) []byte {
			data[markersHeaderSize+offMarkerKind] = byte(vnet.MarkerKindUnknown)
			world.PutChecksum(data)
			return data
		},
		"a note length past the field": func(t *testing.T, data []byte) []byte {
			data[markersHeaderSize+offMarkerNoteLen] = MaxMarkerNote + 1
			world.PutChecksum(data)
			return data
		},
		"a note that is not valid UTF-8": func(t *testing.T, data []byte) []byte {
			data[markersHeaderSize+offMarkerNoteLen] = 2
			data[markersHeaderSize+offMarkerNote] = 0xC3
			data[markersHeaderSize+offMarkerNote+1] = 0x28
			world.PutChecksum(data)
			return data
		},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			store := openMarkers(t, t.TempDir())
			path := markerPathOf(store, theCharacter)
			if err := os.WriteFile(path, damage(t, sound(t)), 0o600); err != nil {
				t.Fatalf("writing the damaged file: %v", err)
			}

			got, found, err := store.Load(theCharacter)
			if !errors.Is(err, world.ErrCorruptStore) {
				t.Fatalf("Load = %v, want a corrupt-store error", err)
			}
			if found || len(got.Markers) != 0 {
				t.Errorf("a refused file still produced %d marks with found=%v", len(got.Markers), found)
			}
		})
	}
}

// A file too large to be one of ours is refused on its size, before a byte of it is
// read: the shape Store.Load and ExplorationStore.Load both use, and the reason a corrupt
// directory is not an OOM.
func TestAnOversizedMarkerFileIsRefusedBeforeItIsRead(t *testing.T) {
	t.Parallel()

	store := openMarkers(t, t.TempDir())
	path := markerPathOf(store, theCharacter)
	if err := os.WriteFile(path, make([]byte, maxMarkersFileSize+1), 0o600); err != nil {
		t.Fatalf("writing the oversized file: %v", err)
	}

	if _, _, err := store.Load(theCharacter); !errors.Is(err, world.ErrCorruptStore) {
		t.Fatalf("Load of an oversized file = %v, want a corrupt-store error", err)
	}
}

// Quarantine keeps the bytes and moves them out of the way, which is the doctrine
// Store.Quarantine and ExplorationStore.Quarantine both keep: the file is the only
// evidence of what went wrong, and the session that could not read it is about to write
// to that exact path.
func TestQuarantineKeepsAMapItCouldNotRead(t *testing.T) {
	t.Parallel()

	store := openMarkers(t, t.TempDir())
	path := markerPathOf(store, theCharacter)
	if err := os.WriteFile(path, []byte("not a map"), 0o600); err != nil {
		t.Fatalf("writing the damaged file: %v", err)
	}

	aside, err := store.Quarantine(theCharacter)
	if err != nil {
		t.Fatalf("Quarantine: %v", err)
	}
	if !strings.HasPrefix(aside, path+corruptFileSuffix) {
		t.Errorf("a quarantined map went to %q, which does not name the file it came from", aside)
	}
	if _, err := os.Stat(path); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("the map is still at its own path after being set aside: %v", err)
	}
	kept, err := os.ReadFile(aside)
	if err != nil {
		t.Fatalf("reading the kept file: %v", err)
	}
	if string(kept) != "not a map" {
		t.Errorf("the kept file holds %q, want the bytes that could not be read", kept)
	}
}

// The ephemeral world: a nil store keeps nothing and every method is a no-op on one, the
// shape a nil Store, ExplorationStore, StructureStore and ClockStore all have. Pinned
// because the alternative is a nil check at every call site, and the one that is
// forgotten panics.
func TestANilMarkerStoreKeepsNothing(t *testing.T) {
	t.Parallel()

	var store *MarkerStore

	if dir := store.Dir(); dir != "" {
		t.Errorf("a nil store names the directory %q", dir)
	}
	if err := store.Save(theCharacter, markedFixture()); err != nil {
		t.Errorf("Save on a nil store: %v", err)
	}
	got, found, err := store.Load(theCharacter)
	if err != nil || found || len(got.Markers) != 0 {
		t.Errorf("Load on a nil store = %v, found=%v, %d marks", err, found, len(got.Markers))
	}
	aside, err := store.Quarantine(theCharacter)
	if err != nil || aside != "" {
		t.Errorf("Quarantine on a nil store = %q, %v", aside, err)
	}
}

// An empty -world-dir is main's decision to make, so the constructor refuses it rather
// than quietly answering with a store that writes nowhere. The same refusal OpenStore,
// OpenStructureStore, OpenClockStore and OpenExplorationStore all make.
func TestOpenMarkerStoreRefusesAnUnnamedWorldDirectory(t *testing.T) {
	t.Parallel()

	store, err := OpenMarkerStore("")
	if err == nil {
		t.Fatal("an unnamed world directory was accepted")
	}
	if store != nil {
		t.Error("OpenMarkerStore returned a store beside its error")
	}
}

// A temporary a crash left mid-rename is swept on open, and nothing else is: the
// directory is one this store creates and fills, so it may name the shape of its own
// files — the division #137 drew between the operator's directory and ours.
func TestOpeningMarkersSweepsTemporariesAndNothingElse(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	markerDir := filepath.Join(dir, markersDirName)
	if err := os.MkdirAll(markerDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	leftover := filepath.Join(markerDir, theCharacter.String()+recordFileExt+".tmp1234")
	bystander := filepath.Join(markerDir, "notes.txt")
	for _, path := range []string{leftover, bystander} {
		if err := os.WriteFile(path, []byte("x"), 0o600); err != nil {
			t.Fatalf("writing %s: %v", path, err)
		}
	}

	openMarkers(t, dir)

	if _, err := os.Stat(leftover); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("a leftover temporary survived the sweep: %v", err)
	}
	if _, err := os.Stat(bystander); err != nil {
		t.Errorf("the sweep removed a file this store never wrote: %v", err)
	}
}

// The marker directory is beside the players and the ledgers rather than inside either:
// one world directory, one subdirectory per kind of thing kept per character.
func TestMarksLiveInTheirOwnDirectoryUnderTheWorld(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	store := openMarkers(t, dir)
	if want := filepath.Join(dir, markersDirName); store.Dir() != want {
		t.Errorf("the marker store writes to %q, want %q", store.Dir(), want)
	}
}
