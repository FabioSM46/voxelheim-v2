// Internal tests for the map ledger. Internal because the two things worth pinning here
// — the cap's one warning, and what happens to a ledger this build cannot read — are
// reached through a constructor and a method the package does not export, and reaching
// them through a whole session would test the wiring rather than the rule.
package session

import (
	"context"
	"errors"
	"log/slog"
	"os"
	"path/filepath"
	"slices"
	"sync"
	"testing"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// countingHandler records every log record's level and message, so a test can assert
// that something was said once rather than every time it could have been.
type countingHandler struct {
	mu      sync.Mutex
	records []slog.Record
}

func (h *countingHandler) Enabled(context.Context, slog.Level) bool { return true }

func (h *countingHandler) Handle(_ context.Context, record slog.Record) error {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.records = append(h.records, record)
	return nil
}

func (h *countingHandler) WithAttrs([]slog.Attr) slog.Handler { return h }
func (h *countingHandler) WithGroup(string) slog.Handler      { return h }

func (h *countingHandler) count(level slog.Level) int {
	h.mu.Lock()
	defer h.mu.Unlock()

	n := 0
	for _, record := range h.records {
		if record.Level == level {
			n++
		}
	}
	return n
}

// exploringCharacter is a character in a store, so the ledger under test has a real id
// and a real directory to be written into.
func exploringCharacter(t *testing.T, dir string) (*persist.ExplorationStore, persist.Character) {
	t.Helper()

	players, err := persist.OpenStore(dir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	character, err := players.Create(identity.PlayerID{7}, "Eivor", protocol.Appearance{})
	if err != nil {
		t.Fatalf("Create: %v", err)
	}

	explored, err := persist.OpenExplorationStore(dir)
	if err != nil {
		t.Fatalf("OpenExplorationStore: %v", err)
	}
	return explored, character
}

// A column revealed twice costs nothing twice: the set does not grow and the batch does
// not repeat it. The property the whole hook rests on, because a view diff re-sends a
// chunk whose delivery could not be confirmed and every vertical chunk of a column
// arrives separately.
func TestRevealingAColumnTwiceRecordsItOnce(t *testing.T) {
	t.Parallel()

	explored := newExploration(nil, 1, nil, false, nil)
	explored.Reveal(world.Column{CX: 2, CZ: 3})
	explored.Reveal(world.Column{CX: 2, CZ: 3})
	explored.Reveal(world.Column{CX: -1, CZ: 3})

	if got := explored.Count(); got != 2 {
		t.Errorf("the ledger holds %d columns, want 2", got)
	}
	batch := explored.TakeRevealed()
	if len(batch) != 2 {
		t.Fatalf("the batch holds %d columns, want 2: %+v", len(batch), batch)
	}
	// Sorted by cz then cx, which is what makes a page boundary fall in the same place
	// twice and a test able to assert frames rather than sets.
	if want := []world.Column{{CX: -1, CZ: 3}, {CX: 2, CZ: 3}}; !slices.Equal(batch, want) {
		t.Errorf("the batch is %+v, want %+v", batch, want)
	}
	if again := explored.TakeRevealed(); len(again) != 0 {
		t.Errorf("a drained batch handed out %d columns a second time", len(again))
	}
}

// Explored is the question #450 asks one column at a time, and it must be false for
// everywhere this character has not been.
func TestExploredAnswersOnlyForColumnsThatWereRevealed(t *testing.T) {
	t.Parallel()

	explored := newExploration(nil, 1, []world.Column{{CX: 4, CZ: 4}}, false, nil)
	explored.Reveal(world.Column{CX: 5, CZ: 4})

	for _, column := range []world.Column{{CX: 4, CZ: 4}, {CX: 5, CZ: 4}} {
		if !explored.Explored(column) {
			t.Errorf("%+v reads as unexplored", column)
		}
	}
	for _, column := range []world.Column{{}, {CX: 4, CZ: 5}, {CX: -5, CZ: -4}} {
		if explored.Explored(column) {
			t.Errorf("%+v reads as explored", column)
		}
	}
	// A nil ledger is inert rather than panicking, which is what a test about streaming
	// and not about maps gets.
	var absent *Exploration
	if absent.Explored(world.Column{}) {
		t.Error("a nil ledger claims to have explored somewhere")
	}
}

// At the cap nothing more is recorded, the character keeps playing, and the server says
// so exactly once — not once per chunk crossing for the rest of that session.
func TestTheCapStopsTheLedgerAndIsReportedOnce(t *testing.T) {
	t.Parallel()

	handler := &countingHandler{}
	explored := newExploration(nil, 1, nil, false, slog.New(handler))

	// Filled through Reveal rather than through the constructor, so that what is under
	// test is the path a playing character takes.
	for i := range persist.MaxExploredColumns {
		explored.Reveal(world.Column{CX: int32(i % 256), CZ: int32(i / 256)})
	}
	if got := explored.Count(); got != persist.MaxExploredColumns {
		t.Fatalf("the ledger filled to %d columns, want %d", got, persist.MaxExploredColumns)
	}
	if got := handler.count(slog.LevelWarn); got != 0 {
		t.Fatalf("the cap was reported %d times before it was reached", got)
	}

	_ = explored.TakeRevealed()
	for range 5 {
		explored.Reveal(world.Column{CX: 9999, CZ: 9999})
		explored.Reveal(world.Column{CX: 9998, CZ: 9999})
	}

	if got := explored.Count(); got != persist.MaxExploredColumns {
		t.Errorf("the ledger grew past the cap to %d columns", got)
	}
	if batch := explored.TakeRevealed(); len(batch) != 0 {
		t.Errorf("a ledger at the cap revealed %d columns", len(batch))
	}
	if got := handler.count(slog.LevelWarn); got != 1 {
		t.Errorf("the cap was reported %d times, want exactly once", got)
	}
}

// A ledger that has not changed costs no write, which is what makes the autosave cheap
// for the many connected players who are standing still.
func TestAnUnchangedLedgerIsNotWritten(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	store, character := exploringCharacter(t, dir)
	explored := newExploration(store, character.ID, nil, false, nil)

	explored.Reveal(world.Column{CX: 1, CZ: 1})
	if err := explored.Save(); err != nil {
		t.Fatalf("first Save: %v", err)
	}

	path := filepath.Join(store.Dir(), character.ID.String()+".bin")
	if err := os.Remove(path); err != nil {
		t.Fatalf("removing the written ledger: %v", err)
	}

	// The file is gone and the set is unchanged, so a second save must write nothing.
	// Deleting it is the only way to tell "wrote the same bytes" from "wrote nothing",
	// and the difference is the whole of what the dirty flag buys.
	if err := explored.Save(); err != nil {
		t.Fatalf("second Save: %v", err)
	}
	if _, err := os.Stat(path); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("an unchanged ledger was written again: %v", err)
	}

	// And a change makes it write again, so the flag is not simply stuck.
	explored.Reveal(world.Column{CX: 2, CZ: 1})
	if err := explored.Save(); err != nil {
		t.Fatalf("third Save: %v", err)
	}
	if _, err := os.Stat(path); err != nil {
		t.Errorf("a changed ledger was not written: %v", err)
	}
}

// The union of what was stored and what this session revealed, in one file, sorted.
func TestSavingWritesTheStoredSetAndTheNewOne(t *testing.T) {
	t.Parallel()

	store, character := exploringCharacter(t, t.TempDir())
	explored := newExploration(store, character.ID, []world.Column{{CX: 5, CZ: 5}, {CX: 0, CZ: 9}}, false, nil)
	explored.Reveal(world.Column{CX: 5, CZ: 5}) // already known
	explored.Reveal(world.Column{CX: -3, CZ: 5})

	if err := explored.Save(); err != nil {
		t.Fatalf("Save: %v", err)
	}

	got, found, err := store.Load(character.ID)
	if err != nil || !found {
		t.Fatalf("Load = %v, found=%v", err, found)
	}
	want := []world.Column{{CX: -3, CZ: 5}, {CX: 5, CZ: 5}, {CX: 0, CZ: 9}}
	if !slices.Equal(got, want) {
		t.Errorf("the ledger was written as %+v, want %+v", got, want)
	}
}

// A ledger this build cannot read is set aside and the character plays with a blank map
// — never refused the connection, and never quietly overwritten.
func TestAnUnreadableLedgerIsKeptAndTheMapStartsBlank(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	store, character := exploringCharacter(t, dir)
	path := filepath.Join(store.Dir(), character.ID.String()+".bin")
	if err := os.WriteFile(path, []byte("not a ledger"), 0o600); err != nil {
		t.Fatalf("writing the damaged ledger: %v", err)
	}

	handler := &countingHandler{}
	identities := &Identities{exploration: store, log: slog.New(handler)}
	explored := identities.recallExploration(character)

	if got := explored.Count(); got != 0 {
		t.Errorf("an unreadable ledger produced %d columns", got)
	}
	if got := handler.count(slog.LevelWarn); got != 1 {
		t.Errorf("an unreadable ledger was reported %d times at Warn, want once", got)
	}

	// The bytes are kept under a name of their own, and the session may write again.
	kept, err := filepath.Glob(path + ".corrupt.*")
	if err != nil || len(kept) != 1 {
		t.Fatalf("the damaged ledger was kept at %v (%v), want exactly one file", kept, err)
	}
	explored.Reveal(world.Column{CX: 1, CZ: 2})
	if err := explored.Save(); err != nil {
		t.Fatalf("Save after a quarantine: %v", err)
	}
	if _, err := os.Stat(path); err != nil {
		t.Errorf("the session did not write a fresh ledger: %v", err)
	}
}

// A ledger that cannot be read **and** cannot be moved out of the way is sealed: the
// character plays with a blank map and nothing this session does replaces the evidence.
//
// The direction that matters is the permissive one. A session that wrote anyway would
// destroy the only copy of the bytes that would explain the bug, and it would do it on
// the ordinary autosave rather than as anybody's decision.
func TestALedgerThatCannotBeSetAsideIsNeverWrittenOver(t *testing.T) {
	t.Parallel()

	if os.Geteuid() == 0 {
		t.Skip("root writes through a read-only directory, so there is no failed rename to arrange")
	}

	dir := t.TempDir()
	store, character := exploringCharacter(t, dir)
	path := filepath.Join(store.Dir(), character.ID.String()+".bin")
	const damaged = "not a ledger"
	if err := os.WriteFile(path, []byte(damaged), 0o600); err != nil {
		t.Fatalf("writing the damaged ledger: %v", err)
	}

	// A directory nothing may be created or renamed in, which is what makes both the
	// quarantine and the later write fail. Restored in cleanup so the temp dir can go.
	if err := os.Chmod(store.Dir(), 0o500); err != nil {
		t.Fatalf("chmod: %v", err)
	}
	t.Cleanup(func() { _ = os.Chmod(store.Dir(), 0o700) })

	handler := &countingHandler{}
	identities := &Identities{exploration: store, log: slog.New(handler)}
	explored := identities.recallExploration(character)

	if got := explored.Count(); got != 0 {
		t.Errorf("an unreadable ledger produced %d columns", got)
	}
	if got := handler.count(slog.LevelError); got != 1 {
		t.Errorf("a ledger that could not be set aside was reported %d times at Error, want once", got)
	}

	explored.Reveal(world.Column{CX: 1, CZ: 2})
	if err := explored.Save(); err != nil {
		t.Errorf("Save on a sealed ledger reported %v; it should do nothing at all", err)
	}
	kept, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("reading the ledger back: %v", err)
	}
	if string(kept) != damaged {
		t.Errorf("the sealed ledger now holds %q, want the bytes nobody could read", kept)
	}
}

// A ledger that could not be *reached* — a permission, a failing disk — is the other
// half of the same rule: blank, sealed, and loud, because the file may well be readable
// later and this session must not be the thing that replaces it.
func TestAnUnreachableLedgerIsSealedRatherThanQuarantined(t *testing.T) {
	t.Parallel()

	if os.Geteuid() == 0 {
		t.Skip("root reads through a mode-000 file, so there is no unreachable ledger to arrange")
	}

	dir := t.TempDir()
	store, character := exploringCharacter(t, dir)
	path := filepath.Join(store.Dir(), character.ID.String()+".bin")
	if err := os.WriteFile(path, []byte("unreadable"), 0o000); err != nil {
		t.Fatalf("writing the unreachable ledger: %v", err)
	}

	handler := &countingHandler{}
	identities := &Identities{exploration: store, log: slog.New(handler)}
	explored := identities.recallExploration(character)

	if got := explored.Count(); got != 0 {
		t.Errorf("an unreachable ledger produced %d columns", got)
	}
	if got := handler.count(slog.LevelError); got != 1 {
		t.Errorf("an unreachable ledger was reported %d times at Error, want once", got)
	}
	// Not set aside: the file is where it was, because nothing here decided it was
	// beyond saving.
	kept, _ := filepath.Glob(path + ".corrupt.*")
	if len(kept) != 0 {
		t.Errorf("an unreachable ledger was quarantined to %v", kept)
	}

	explored.Reveal(world.Column{CX: 1, CZ: 2})
	if err := explored.Save(); err != nil {
		t.Errorf("Save on a sealed ledger reported %v; it should do nothing at all", err)
	}
}

// The ledger is paged at the contract's bound and an empty one is not a message at all.
//
// The empty case is the one worth pinning: schemas/world.fbs requires `columns` to be
// present and non-empty because the ledger is additive, so a client reading an empty
// page as "you have explored nothing" would erase its own map.
func TestTheLedgerIsSentInPagesAndNeverEmpty(t *testing.T) {
	t.Parallel()

	frames := 0
	sizes := []int{}
	send := func(frame []byte) error {
		frames++
		// Read through the generated accessors rather than protocol.Decode: Decode
		// answers for what a *server* receives, and this is a frame it sends.
		env := vnet.GetRootAsEnvelope(frame, 0)
		if got := env.PayloadType(); got != vnet.PayloadMapExplored {
			t.Fatalf("a page is a %s, want a MapExplored", got)
		}
		var table flatbuffers.Table
		if !env.Payload(&table) {
			t.Fatal("a page carries no payload")
		}
		page := new(vnet.MapExplored)
		page.Init(table.Bytes, table.Pos)
		if page.ColumnsLength() == 0 {
			t.Fatal("a page carries no columns, which the contract forbids")
		}
		sizes = append(sizes, page.ColumnsLength())
		return nil
	}

	if err := sendExplored(send, nil); err != nil {
		t.Fatalf("sendExplored on an empty ledger: %v", err)
	}
	if frames != 0 {
		t.Fatalf("an empty ledger produced %d frames, want none", frames)
	}

	// One past the page bound, which is the boundary a page-per-frame implementation
	// gets wrong in both directions.
	columns := make([]world.Column, protocol.MaxExploredColumns+1)
	for i := range columns {
		columns[i] = world.Column{CX: int32(i), CZ: int32(i / 64)}
	}
	if err := sendExplored(send, columns); err != nil {
		t.Fatalf("sendExplored: %v", err)
	}
	if want := []int{protocol.MaxExploredColumns, 1}; !slices.Equal(sizes, want) {
		t.Errorf("the ledger was paged as %v, want %v", sizes, want)
	}
}
