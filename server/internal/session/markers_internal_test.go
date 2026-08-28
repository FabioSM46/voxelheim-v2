// Internal tests for a character's marks. Internal because the three things worth
// pinning here — the counter that never hands an id out twice, an unchanged map costing
// no write, and what happens to a file this build cannot read — are reached through a
// constructor and a method the package does not export, and reaching them through a whole
// session would test the wiring rather than the rule.
package session

import (
	"log/slog"
	"os"
	"path/filepath"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// markingCharacter is a character in a store, so the map under test has a real id and a
// real directory to be written into.
func markingCharacter(t *testing.T, dir string) (*persist.MarkerStore, persist.Character) {
	t.Helper()

	players, err := persist.OpenStore(dir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	character, err := players.Create(identity.PlayerID{9}, "Eivor", protocol.Appearance{})
	if err != nil {
		t.Fatalf("Create: %v", err)
	}

	marks, err := persist.OpenMarkerStore(dir)
	if err != nil {
		t.Fatalf("OpenMarkerStore: %v", err)
	}
	return marks, character
}

// aMark is one placement request with the fields that are never the point of the test.
func aMark(x, z int32) protocol.MarkerPlaceRequest {
	return protocol.MarkerPlaceRequest{X: x, Z: z, Kind: vnet.MarkerKindCave}
}

// The counter only ever goes up, across removals and across a reload. Derived from the
// marks it holds it would fall back the moment the newest one went — and a client would
// then be told that a fresh mark carries an id it has already drawn somewhere else.
func TestTheCounterNeverHandsOutAnIdTwice(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	store, character := markingCharacter(t, dir)
	marks := newMarkers(store, character.ID, persist.StoredMarkers{NextID: 1}, false, nil)

	var minted []uint64
	for i := range 3 {
		list, _, err := marks.Place(aMark(int32(i), int32(i)))
		if err != nil {
			t.Fatalf("Place: %v", err)
		}
		minted = append(minted, list.Markers[len(list.Markers)-1].MarkerID)
	}

	// The newest goes, which is the case a derived counter gets wrong.
	if _, _, err := marks.Remove(minted[2]); err != nil {
		t.Fatalf("Remove: %v", err)
	}
	if err := marks.Save(); err != nil {
		t.Fatalf("Save: %v", err)
	}

	// A second session over the same file, which is what a reconnect is.
	stored, found, err := store.Load(character.ID)
	if err != nil || !found {
		t.Fatalf("Load = %v, found=%v", err, found)
	}
	again := newMarkers(store, character.ID, stored, false, nil)
	list, _, err := again.Place(aMark(9, 9))
	if err != nil {
		t.Fatalf("Place after a reload: %v", err)
	}

	fresh := list.Markers[len(list.Markers)-1].MarkerID
	for _, old := range minted {
		if fresh == old {
			t.Fatalf("the mark placed after a reload took id %d, which has already been handed out", fresh)
		}
	}
}

// A counter that is somehow behind an id the file holds is floored past it rather than
// trusted. Unreachable through a file this build wrote — the decoder refuses exactly this
// — and checked anyway, because this is the last place that can notice before a duplicate
// id is minted.
func TestAStaleCounterIsFlooredPastEveryIdItHolds(t *testing.T) {
	t.Parallel()

	marks := newMarkers(nil, 0, persist.StoredMarkers{
		NextID:  2,
		Markers: []protocol.Marker{{MarkerID: 7, Kind: vnet.MarkerKindNote}},
	}, false, nil)

	list, _, err := marks.Place(aMark(1, 1))
	if err != nil {
		t.Fatalf("Place: %v", err)
	}
	if got := list.Markers[len(list.Markers)-1].MarkerID; got <= 7 {
		t.Errorf("the next mark took id %d, which is not past the id already held", got)
	}
}

// An unchanged map costs no write, which is what makes it safe to call Save from the
// autosave pass that visits every connected player.
func TestAnUnchangedMapIsNotWritten(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	store, character := markingCharacter(t, dir)
	path := filepath.Join(store.Dir(), character.ID.String()+".bin")
	marks := newMarkers(store, character.ID, persist.StoredMarkers{NextID: 1}, false, nil)

	// Nothing placed: nothing written, and "no file" is the observable form of that.
	if err := marks.Save(); err != nil {
		t.Fatalf("Save: %v", err)
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Fatalf("an unchanged map wrote a file: %v", err)
	}

	if _, _, err := marks.Place(aMark(4, 4)); err != nil {
		t.Fatalf("Place: %v", err)
	}
	if err := marks.Save(); err != nil {
		t.Fatalf("Save after a placement: %v", err)
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatalf("a placement wrote no file: %v", err)
	}

	// A refused placement does not dirty the map either, so the second save is still a
	// no-op — which the modification time is the only observable form of.
	if _, _, err := marks.Remove(9999); err == nil {
		t.Fatal("removing an id nothing holds was accepted")
	}
	if err := marks.Save(); err != nil {
		t.Fatalf("Save after a refused removal: %v", err)
	}
	after, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if !after.ModTime().Equal(info.ModTime()) {
		t.Error("a refused removal caused a write")
	}
}

// A map this build cannot read is kept and the character plays with none: the doctrine
// recallExploration settled, applied to a file that costs a little more to lose.
func TestUnreadableMarksAreKeptAndTheMapStartsUnmarked(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	store, character := markingCharacter(t, dir)
	path := filepath.Join(store.Dir(), character.ID.String()+".bin")
	if err := os.WriteFile(path, []byte("not a map"), 0o600); err != nil {
		t.Fatalf("writing the damaged file: %v", err)
	}

	handler := &countingHandler{}
	identities := &Identities{markers: store, log: slog.New(handler)}
	marks := identities.recallMarkers(character)

	if got := marks.Count(); got != 0 {
		t.Errorf("an unreadable map produced %d marks", got)
	}
	if got := handler.count(slog.LevelWarn); got != 1 {
		t.Errorf("an unreadable map was reported %d times at Warn, want once", got)
	}

	// The bytes are kept under a name of their own, and the session may write again.
	kept, err := filepath.Glob(path + ".corrupt.*")
	if err != nil || len(kept) != 1 {
		t.Fatalf("the damaged file was kept at %v (%v), want exactly one file", kept, err)
	}
	if _, _, pErr := marks.Place(aMark(1, 2)); pErr != nil {
		t.Fatalf("Place after a quarantine: %v", pErr)
	}
	if err := marks.Save(); err != nil {
		t.Fatalf("Save after a quarantine: %v", err)
	}
	if _, err := os.Stat(path); err != nil {
		t.Errorf("the session did not write a fresh file: %v", err)
	}
}

// A map that cannot be read **and** cannot be moved out of the way is sealed: the
// character plays unmarked and nothing this session does replaces the evidence.
//
// The direction that matters is the permissive one. A session that wrote anyway would
// destroy the only copy of the bytes that would explain the bug, and it would do it on
// the ordinary autosave rather than as anybody's decision.
func TestMarksThatCannotBeSetAsideAreNeverWrittenOver(t *testing.T) {
	t.Parallel()

	if os.Geteuid() == 0 {
		t.Skip("root writes through a read-only directory, so there is no failed rename to arrange")
	}

	dir := t.TempDir()
	store, character := markingCharacter(t, dir)
	path := filepath.Join(store.Dir(), character.ID.String()+".bin")
	const damaged = "not a map"
	if err := os.WriteFile(path, []byte(damaged), 0o600); err != nil {
		t.Fatalf("writing the damaged file: %v", err)
	}

	// A directory nothing may be created or renamed in, which is what makes both the
	// quarantine and the later write fail. Restored in cleanup so the temp dir can go.
	if err := os.Chmod(store.Dir(), 0o500); err != nil {
		t.Fatalf("chmod: %v", err)
	}
	t.Cleanup(func() { _ = os.Chmod(store.Dir(), 0o700) })

	handler := &countingHandler{}
	identities := &Identities{markers: store, log: slog.New(handler)}
	marks := identities.recallMarkers(character)

	if got := handler.count(slog.LevelError); got != 1 {
		t.Errorf("a map that could not be set aside was reported %d times at Error, want once", got)
	}
	if _, _, err := marks.Place(aMark(1, 2)); err != nil {
		t.Fatalf("Place on a sealed map: %v", err)
	}
	if err := marks.Save(); err != nil {
		t.Errorf("Save on a sealed map reported %v; it should do nothing at all", err)
	}
	kept, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("reading the file back: %v", err)
	}
	if string(kept) != damaged {
		t.Errorf("the sealed file now holds %q, want the bytes nobody could read", kept)
	}
}

// The world's edge, at the boundary and one block past it on each axis. The first
// coordinate a client ever chooses is a mark's, which is why this check did not exist
// before there were marks.
func TestAMarkMustNameAPlaceInsideTheWorld(t *testing.T) {
	t.Parallel()

	for name, place := range map[string]struct {
		x, z    int32
		outside bool
	}{
		"the origin":            {0, 0, false},
		"the eastern edge":      {world.BlockLimit, 0, false},
		"the western edge":      {-world.BlockLimit, 0, false},
		"one block past east":   {world.BlockLimit + 1, 0, true},
		"one block past west":   {-(world.BlockLimit + 1), 0, true},
		"one block past south":  {0, world.BlockLimit + 1, true},
		"one block past north":  {0, -(world.BlockLimit + 1), true},
		"past on both axes":     {world.BlockLimit + 1, world.BlockLimit + 1, true},
		"the far corner itself": {world.BlockLimit, -world.BlockLimit, false},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			marks := newMarkers(nil, 0, persist.StoredMarkers{NextID: 1}, false, nil)
			_, reason, err := marks.Place(aMark(place.x, place.z))
			if place.outside {
				if err == nil {
					t.Fatalf("a mark at (%d, %d) was accepted", place.x, place.z)
				}
				// Silence, because the contract has no member for it: a reason of
				// Unknown is what tells the session to send nothing at all.
				if reason != vnet.RefusalReasonUnknown {
					t.Errorf("a mark outside the world was refused with %s, want a silent refusal", reason)
				}
				return
			}
			if err != nil {
				t.Fatalf("a mark at (%d, %d) was refused: %v", place.x, place.z, err)
			}
		})
	}
}
