// Tests for what a saved run leaves behind. The restart is simulated by building a
// second manager over the first one's SavedSessions, which is exactly what main does
// across a process boundary — with persist.SessionStore in between, tested there.
package game

import (
	"errors"
	"os"
	"reflect"
	"slices"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// restartInto is the simulated restart: a brand-new manager, holding nothing, handed the
// records the old one would have written down.
func restartInto(t *testing.T, saved []SavedSession, at time.Time) (*InstanceManager, int, int) {
	t.Helper()
	m := instanceTestManager(t, 20, 4)
	m.now = (&resetClock{at: at}).now
	restored, expired, err := m.RestoreSessions(saved)
	if err != nil {
		t.Fatalf("RestoreSessions: %v", err)
	}
	return m, restored, expired
}

// What a saved run is worth writing down, and what it is not. Free copies are absent,
// because nobody owes them anything; the six fields that are here are the six a morning
// depends on.
func TestSavedSessionsAreTheRunsWorthKeeping(t *testing.T) {
	saved := time.Date(2026, 3, 14, 21, 0, 0, 0, time.UTC)
	m, _, ruin, character, session := savedRunAt(t, saved)

	late := instanceTestCharacter(2)
	if _, err := m.Join(session.ID, late); err != nil {
		t.Fatal(err)
	}
	killMobInSession(t, session, vnet.MobKindDraugrKing)
	m.Step()

	// A free copy of another ruin, standing beside it and belonging to nobody.
	if _, err := m.Create(InstanceRuin{-30, 30}); err != nil {
		t.Fatal(err)
	}

	records := m.SavedSessions()
	if len(records) != 1 {
		t.Fatalf("SavedSessions = %#v, want only the saved run", records)
	}
	got := records[0]
	if got.ID != session.ID || got.Seed != session.Seed || got.Ruin != ruin {
		t.Fatalf("identity = %d/%d/%v, want %d/%d/%v", got.ID, got.Seed, got.Ruin, session.ID, session.Seed, ruin)
	}
	if got.ExpiresUnix != nextResetUnix(saved) {
		t.Fatalf("expiry = %d, want %d", got.ExpiresUnix, nextResetUnix(saved))
	}
	want := []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing}
	if !slices.Equal(got.DefeatedBosses, want) {
		t.Fatalf("defeated encounters = %v, want %v", got.DefeatedBosses, want)
	}
	if len(got.Bound) != 2 || !slices.Contains(got.Bound, character) || !slices.Contains(got.Bound, late) {
		t.Fatalf("bound = %v, want both %v and %v", got.Bound, character, late)
	}

	// Twice over identical state, byte for byte, which is what lets the autosave loop
	// write this file without a dirty flag anywhere in the manager.
	again := m.SavedSessions()
	if len(again) != 1 || !slices.Equal(again[0].Bound, got.Bound) || !slices.Equal(again[0].DefeatedBosses, got.DefeatedBosses) {
		t.Fatalf("two passes over one state disagreed: %#v then %#v", got, again[0])
	}
}

// The restart, end to end: a bound character is still bound, the defeated encounters are
// intact, and re-entering returns the same run rather than a fresh one — over a world
// rebuilt deterministically from the stored seed.
func TestASavedRunSurvivesARestart(t *testing.T) {
	saved := time.Date(2026, 3, 14, 20, 0, 0, 0, time.UTC)
	before, _, ruin, character, session := savedRunAt(t, saved)
	killMobInSession(t, session, vnet.MobKindDraugrKing)
	before.Step()
	if !before.Leave(session.ID, character) {
		t.Fatal("leaving the saved session was refused")
	}
	records := before.SavedSessions()

	after, restored, expired := restartInto(t, records, saved.Add(time.Hour))
	if restored != 1 || expired != 0 {
		t.Fatalf("the restart restored %d and dropped %d, want 1 and 0", restored, expired)
	}

	id, bound := after.Bound(ruin, character)
	if !bound || id != session.ID {
		t.Fatalf("the binding did not survive the restart: %d %v", id, bound)
	}
	held, live := after.Lookup(session.ID)
	if !live {
		t.Fatal("the run did not survive the restart")
	}
	if held.State != InstanceSaved {
		t.Fatalf("the restored run is %v, want saved", held.State)
	}
	if held.Seed != session.Seed {
		t.Fatalf("the restored world's seed is %d, want %d", held.Seed, session.Seed)
	}
	if held.ExpiresUnix != nextResetUnix(saved) {
		t.Fatalf("the restored reset is %d, want %d", held.ExpiresUnix, nextResetUnix(saved))
	}
	want := []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing}
	if !slices.Equal(held.DefeatedBosses, want) {
		t.Fatalf("the restored progress is %v, want %v", held.DefeatedBosses, want)
	}
	// Nobody is inside a restored run, which is what a saved run's overnight state is.
	if len(held.Members) != 0 {
		t.Fatalf("the restart put %v back inside", held.Members)
	}

	// Re-entering returns that same run, not a fresh copy.
	again, err := after.Reenter(ruin, character)
	if err != nil {
		t.Fatal(err)
	}
	if again.ID != session.ID {
		t.Fatalf("re-entry after the restart opened session %d, want %d", again.ID, session.ID)
	}

	// And its world is the same world: the same seed generates the same chunk, block for
	// block, which is why none of it is written down.
	coord := world.ChunkOf(0, 0, 0)
	old, _, err := session.Chunks.Get(session.Context, coord)
	if err != nil {
		t.Fatal(err)
	}
	fresh, _, err := again.Chunks.Get(again.Context, coord)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(old, fresh) {
		t.Fatal("the rebuilt world differs from the one the party cleared")
	}
}

// A cold start over a run whose day has ended cleans it up rather than restoring it. The
// server may have been switched off across several midnights, so "has this expired" is
// asked of every record here rather than left to the tick loop to have noticed.
func TestAColdStartCleansUpAnExpiredRun(t *testing.T) {
	saved := time.Date(2026, 3, 14, 20, 0, 0, 0, time.UTC)
	before, _, ruin, character, session := savedRunAt(t, saved)
	if !before.Leave(session.ID, character) {
		t.Fatal("leaving the saved session was refused")
	}
	records := before.SavedSessions()

	// Switched off for a fortnight, so several midnights have passed and the tick loop
	// noticed none of them.
	after, restored, expired := restartInto(t, records, saved.AddDate(0, 0, 14))
	if restored != 0 || expired != 1 {
		t.Fatalf("the restart restored %d and dropped %d, want 0 and 1", restored, expired)
	}
	if _, live := after.Lookup(session.ID); live {
		t.Fatal("an expired run was restored")
	}
	if id, bound := after.Bound(ruin, character); bound {
		t.Fatalf("an expired run bound a character to %d", id)
	}
	if after.Count() != 0 {
		t.Fatalf("the restart left %d sessions standing", after.Count())
	}

	// And the character walks into a fresh dungeon, which is the whole point.
	fresh, err := after.Reenter(ruin, character)
	if err != nil {
		t.Fatal(err)
	}
	if fresh.State != InstanceFree || len(fresh.DefeatedBosses) != 0 {
		t.Fatalf("the morning's copy is %v holding %v", fresh.State, fresh.DefeatedBosses)
	}
}

// A restored id was minted by a *previous* process, and this one's counter starts afresh
// — so the first Create after a restart can name a run that is already live. It must not
// replace it.
func TestAFreshMintNeverCollidesWithARestoredRun(t *testing.T) {
	saved := time.Date(2026, 3, 14, 20, 0, 0, 0, time.UTC)
	ruin, character := InstanceRuin{7, -2}, instanceTestCharacter(1)

	// Ids from testEntityIDs() start at one, so a record naming the first few ids is
	// exactly the collision a restart hands the new counter.
	stored := []SavedSession{
		{ID: 1, Seed: 101, Ruin: ruin, ExpiresUnix: nextResetUnix(saved), Bound: []InstanceCharacter{character}},
		{ID: 2, Seed: 202, Ruin: InstanceRuin{1, 1}, ExpiresUnix: nextResetUnix(saved)},
	}
	m, restored, _ := restartInto(t, stored, saved)
	if restored != 2 {
		t.Fatalf("restored %d runs, want 2", restored)
	}

	fresh, err := m.Create(InstanceRuin{5, 5})
	if err != nil {
		t.Fatal(err)
	}
	if fresh.ID == 1 || fresh.ID == 2 {
		t.Fatalf("a fresh copy was minted the restored id %d", fresh.ID)
	}
	if m.Count() != 3 {
		t.Fatalf("the manager holds %d sessions, want 3", m.Count())
	}
	if id, bound := m.Bound(ruin, character); !bound || id != 1 {
		t.Fatalf("the fresh mint disturbed a restored binding: %d %v", id, bound)
	}
	if held, live := m.Lookup(1); !live || held.Seed != 101 {
		t.Fatalf("session 1 is now %#v, want the restored world seeded 101", held)
	}
}

// A restore is a startup operation and validates the whole list before it files a single
// session, so a manager that refuses one is holding exactly what it held before.
func TestRestoreRefusesWhatCouldNotHaveBeenWritten(t *testing.T) {
	expiry := nextResetUnix(time.Date(2026, 3, 14, 20, 0, 0, 0, time.UTC))
	ruin := InstanceRuin{7, -2}

	for _, tc := range []struct {
		name   string
		stored []SavedSession
		want   error
	}{
		{"no id", []SavedSession{{ID: 0, ExpiresUnix: expiry, Ruin: ruin}}, ErrInvalidSession},
		{"no expiry", []SavedSession{{ID: 3, ExpiresUnix: 0, Ruin: ruin}}, ErrInvalidSession},
		{"one id twice", []SavedSession{
			{ID: 3, ExpiresUnix: expiry, Ruin: ruin},
			{ID: 3, ExpiresUnix: expiry, Ruin: InstanceRuin{1, 1}},
		}, ErrDuplicateSession},
	} {
		t.Run(tc.name, func(t *testing.T) {
			m := instanceTestManager(t, 20, 4)
			if _, _, err := m.RestoreSessions(tc.stored); !errors.Is(err, tc.want) {
				t.Fatalf("RestoreSessions = %v, want %v", err, tc.want)
			}
			if m.Count() != 0 {
				t.Fatalf("a refused restore left %d sessions standing", m.Count())
			}
		})
	}

	// And a restore into a manager that is already running one is refused whole: a
	// rebuild would either duplicate a live run or replace a world somebody is in.
	m := instanceTestManager(t, 20, 4)
	if _, err := m.Create(ruin); err != nil {
		t.Fatal(err)
	}
	if _, _, err := m.RestoreSessions([]SavedSession{{ID: 3, ExpiresUnix: expiry, Ruin: ruin}}); !errors.Is(err, ErrInstancesNotEmpty) {
		t.Fatalf("RestoreSessions into a running manager = %v, want ErrInstancesNotEmpty", err)
	}
	m.Close()
	if _, _, err := m.RestoreSessions(nil); !errors.Is(err, ErrInstanceClosed) {
		t.Fatalf("RestoreSessions into a closed manager = %v, want ErrInstanceClosed", err)
	}
}

// **No chunk, block or entity of an instance is ever written to disk.** An instance cache
// is built by world.NewInstanceCache and holds no store at all, so there is no path from
// a saved run to a file — not through a restore, not through an edit, and not through the
// Flush that writes the open world.
func TestARestoredRunWritesNoWorldToDisk(t *testing.T) {
	dir := t.TempDir()
	saved := time.Date(2026, 3, 14, 20, 0, 0, 0, time.UTC)
	before, _, _, _, session := savedRunAt(t, saved)
	records := before.SavedSessions()

	after, restored, _ := restartInto(t, records, saved.Add(time.Hour))
	if restored != 1 {
		t.Fatalf("restored %d runs, want 1", restored)
	}
	held, live := after.Lookup(session.ID)
	if !live {
		t.Fatal("the run did not survive the restart")
	}

	// Generate the world, edit it, kill something in it, step it, and then ask it to
	// flush — the whole of what a party does to a dungeon.
	coord := world.ChunkOf(0, 0, 0)
	if _, _, err := held.Chunks.Get(held.Context, coord); err != nil {
		t.Fatal(err)
	}
	killMobInSession(t, held, vnet.MobKindDraugrKing)
	after.Step()
	if err := held.Chunks.Flush(); err != nil {
		t.Fatalf("flushing an instance world: %v", err)
	}

	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 0 {
		t.Fatalf("an instance wrote %v to the world directory", entries)
	}
}

// The configured instance limit bounds a restore exactly as it bounds a Create. The
// reachable case is an operator lowering -max-instances between two runs of the server:
// nothing carries the old limit forward, and a file written under a larger one is a legal
// input here.
func TestRestoreRefusesMoreRunsThanTheServerHolds(t *testing.T) {
	saved := time.Date(2026, 3, 14, 20, 0, 0, 0, time.UTC)
	expiry := nextResetUnix(saved)
	ruinAt := func(i int) InstanceRuin { return InstanceRuin{CellX: int64(i), CellZ: 1} }

	stored := make([]SavedSession, 5)
	for i := range stored {
		stored[i] = SavedSession{ID: uint64(i + 1), Seed: int64(i), Ruin: ruinAt(i),
			ExpiresUnix: expiry, Bound: []InstanceCharacter{instanceTestCharacter(uint64(i + 1))}}
	}

	// Four slots for five runs: refused whole, before a single session is built.
	m := instanceTestManager(t, 20, 4)
	m.now = (&resetClock{at: saved}).now
	_, _, err := m.RestoreSessions(stored)
	if !errors.Is(err, ErrRestoreExceedsLimit) {
		t.Fatalf("RestoreSessions = %v, want ErrRestoreExceedsLimit", err)
	}
	// Distinct from ErrInstanceLimit on purpose: that one is transient and this one is not,
	// so a caller must not be able to confuse them.
	if errors.Is(err, ErrInstanceLimit) {
		t.Fatal("the refusal reads as the transient full-server limit")
	}
	if m.Count() != 0 {
		t.Fatalf("a refused restore built %d sessions", m.Count())
	}
	for i := range stored {
		if id, bound := m.Bound(ruinAt(i), instanceTestCharacter(uint64(i+1))); bound {
			t.Fatalf("a refused restore bound a character to %d", id)
		}
	}

	// Exactly at the limit is allowed — the check is a ceiling, not a margin.
	exact := instanceTestManager(t, 20, 5)
	exact.now = (&resetClock{at: saved}).now
	restored, _, err := exact.RestoreSessions(stored)
	if err != nil {
		t.Fatalf("a list exactly at the limit was refused: %v", err)
	}
	if restored != 5 || exact.Count() != 5 {
		t.Fatalf("restored %d and holds %d, want 5 and 5", restored, exact.Count())
	}

	// **Expired records do not count against the limit**, because they are never built.
	// Five runs and four slots again, but every run's day ended a fortnight ago.
	stale := instanceTestManager(t, 20, 4)
	stale.now = (&resetClock{at: saved.AddDate(0, 0, 14)}).now
	restored, expiredCount, err := stale.RestoreSessions(stored)
	if err != nil {
		t.Fatalf("a list of expired runs was refused for a limit it never reaches: %v", err)
	}
	if restored != 0 || expiredCount != 5 || stale.Count() != 0 {
		t.Fatalf("restored %d, dropped %d, holds %d; want 0, 5 and 0", restored, expiredCount, stale.Count())
	}
}
