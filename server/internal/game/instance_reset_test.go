// Tests for the midnight reset. The wall clock is injected rather than waited for, which
// is the whole reason InstanceManager holds a `now` at all: a test that has to sit
// through a real midnight is a test nobody runs, and a reset that can only be observed
// once a day is a rule nobody can pin.
package game

import (
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// resetClock is a wall clock a test moves by hand.
type resetClock struct{ at time.Time }

func (c *resetClock) now() time.Time { return c.at }

// savedRunAt clears a dungeon at a named wall-clock moment and hands back the manager,
// the clock driving it, the ruin and the party.
func savedRunAt(t *testing.T, when time.Time) (*InstanceManager, *resetClock, InstanceRuin, InstanceCharacter, InstanceSession) {
	t.Helper()
	m := instanceTestManager(t, 20, 4)
	clock := &resetClock{at: when}
	m.now = clock.now

	ruin, character := InstanceRuin{7, -2}, instanceTestCharacter(1)
	session, err := m.Reenter(ruin, character)
	if err != nil {
		t.Fatal(err)
	}
	killMobInSession(t, session, vnet.MobKindVargrGuardian)
	m.Step()
	if held, _ := m.Lookup(session.ID); held.State != InstanceSaved {
		t.Fatalf("the run was not saved: %v", held.State)
	}
	return m, clock, ruin, character, session
}

// The reset is the server's next midnight on the real calendar, in the server's own
// timezone, and it is strictly after the moment the run was saved. A run cleared at
// 00:00:00 therefore lasts the whole day rather than resetting in the second it was made.
func TestTheResetIsTheServersNextMidnight(t *testing.T) {
	// A location with a fixed offset that is not UTC, so a test passing here cannot be
	// passing because the two happen to agree on the machine it ran on.
	east := time.FixedZone("Test/East", 5*3600)

	for _, tc := range []struct {
		name  string
		saved time.Time
		want  time.Time
	}{
		{"an evening run", time.Date(2026, 3, 14, 21, 47, 13, 0, east), time.Date(2026, 3, 15, 0, 0, 0, 0, east)},
		{"one minute to midnight", time.Date(2026, 3, 14, 23, 59, 0, 0, east), time.Date(2026, 3, 15, 0, 0, 0, 0, east)},
		{"exactly midnight lasts a day", time.Date(2026, 3, 14, 0, 0, 0, 0, east), time.Date(2026, 3, 15, 0, 0, 0, 0, east)},
		{"the last day of a month", time.Date(2026, 1, 31, 12, 0, 0, 0, east), time.Date(2026, 2, 1, 0, 0, 0, 0, east)},
		{"the last day of a leap February", time.Date(2028, 2, 29, 12, 0, 0, 0, east), time.Date(2028, 3, 1, 0, 0, 0, 0, east)},
		{"the last day of a year", time.Date(2026, 12, 31, 23, 0, 0, 0, east), time.Date(2027, 1, 1, 0, 0, 0, 0, east)},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := nextResetUnix(tc.saved); got != tc.want.Unix() {
				t.Fatalf("a run saved at %s resets at %s, want %s",
					tc.saved, time.Unix(got, 0).In(east), tc.want)
			}
		})
	}
}

// The reset is measured on the real calendar and never on the in-game clock. Twenty
// minutes of world time — a whole in-game day and night — take a saved run nowhere.
func TestTheResetIgnoresTheInGameDay(t *testing.T) {
	saved := time.Date(2026, 3, 14, 12, 0, 0, 0, time.UTC)
	m, clock, ruin, character, session := savedRunAt(t, saved)
	if !m.Leave(session.ID, character) {
		t.Fatal("leaving the saved session was refused")
	}

	// Two full in-game days at twenty ticks a second, and a minute of real time to go
	// with them: the world has been round twice and the calendar has not moved.
	for range 2 * DayLengthTicks {
		m.Step()
	}
	clock.at = saved.Add(time.Minute)
	m.Step()

	held, live := m.Lookup(session.ID)
	if !live {
		t.Fatal("two in-game days reset a run the calendar had not reached")
	}
	if held.ExpiresUnix != nextResetUnix(saved) {
		t.Fatalf("the reset moved to %d, want the midnight after the kill, %d",
			held.ExpiresUnix, nextResetUnix(saved))
	}
	if id, bound := m.Bound(ruin, character); !bound || id != session.ID {
		t.Fatalf("the binding did not survive the in-game days: %d %v", id, bound)
	}
}

// Midnight releases every binding to the run, and a character freed by it can open a
// fresh copy of that dungeon — which is the whole of "available again this morning".
func TestMidnightReleasesEveryBinding(t *testing.T) {
	saved := time.Date(2026, 3, 14, 22, 0, 0, 0, time.UTC)
	m, clock, ruin, character, session := savedRunAt(t, saved)

	late := instanceTestCharacter(2)
	if _, err := m.Join(session.ID, late); err != nil {
		t.Fatal(err)
	}
	for _, who := range []InstanceCharacter{character, late} {
		if !m.Leave(session.ID, who) {
			t.Fatalf("leaving the saved session was refused for %v", who.PlayerID)
		}
	}

	// One second before the reset, the run is still theirs.
	clock.at = time.Unix(nextResetUnix(saved)-1, 0)
	m.Step()
	if _, live := m.Lookup(session.ID); !live {
		t.Fatal("the run reset a second early")
	}

	// And on the second itself, it is gone and so is every binding to it.
	clock.at = time.Unix(nextResetUnix(saved), 0)
	m.Step()
	if _, live := m.Lookup(session.ID); live {
		t.Fatal("the run survived its midnight")
	}
	for _, who := range []InstanceCharacter{character, late} {
		if id, bound := m.Bound(ruin, who); bound {
			t.Fatalf("a binding outlived the reset, naming %d", id)
		}
	}

	fresh, err := m.Reenter(ruin, character)
	if err != nil {
		t.Fatalf("a freed character could not re-enter the dungeon: %v", err)
	}
	if fresh.ID == session.ID {
		t.Fatal("re-entry after the reset returned the run that had expired")
	}
	if fresh.State != InstanceFree {
		t.Fatalf("the morning's copy is %v, want free", fresh.State)
	}
	if len(fresh.DefeatedBosses) != 0 {
		t.Fatalf("the morning's copy remembers %v", fresh.DefeatedBosses)
	}
}

// A party inside at midnight is not disturbed: the run survives until it empties, keeps
// what it has put down, and the reset applies to the next entry.
func TestAPartyInsideAtMidnightIsNotDisturbed(t *testing.T) {
	saved := time.Date(2026, 3, 14, 23, 30, 0, 0, time.UTC)
	m, clock, ruin, character, session := savedRunAt(t, saved)
	midnight := nextResetUnix(saved)

	// Midnight arrives with them still inside, and several minutes pass.
	clock.at = time.Unix(midnight, 0)
	for range 5 {
		m.Step()
	}
	held, live := m.Lookup(session.ID)
	if !live {
		t.Fatal("the reset took a world out from under the party standing in it")
	}
	if len(held.Members) != 1 || held.Members[0] != character {
		t.Fatalf("the occupants changed at midnight: %v", held.Members)
	}
	if len(held.DefeatedBosses) != 1 {
		t.Fatalf("the party's progress reset under them: %v", held.DefeatedBosses)
	}
	if id, bound := m.Bound(ruin, character); !bound || id != session.ID {
		t.Fatalf("the binding was released while the party was inside: %d %v", id, bound)
	}

	// And a character who arrives while they are still in there joins that same run: the
	// reset applies to the next entry, and this is not one yet.
	again, err := m.Reenter(ruin, character)
	if err != nil {
		t.Fatal(err)
	}
	if again.ID != session.ID {
		t.Fatalf("a re-entry mid-run opened session %d, want %d", again.ID, session.ID)
	}

	// The moment the last of them leaves, the day that already ended catches up.
	if !m.Leave(session.ID, character) {
		t.Fatal("leaving the saved session was refused")
	}
	m.Step()
	if _, live := m.Lookup(session.ID); live {
		t.Fatal("the expired run outlived the party that was protecting it")
	}
	if id, bound := m.Bound(ruin, character); bound {
		t.Fatalf("a binding outlived the reset, naming %d", id)
	}
}

// A free copy has no reset at all: it ends on the empty grace, and its zero expiry must
// never read as "expired in 1970".
func TestAFreeCopyHasNoReset(t *testing.T) {
	m := instanceTestManager(t, 20, 4)
	clock := &resetClock{at: time.Date(2026, 3, 14, 12, 0, 0, 0, time.UTC)}
	m.now = clock.now

	free, err := m.Create(InstanceRuin{1, 1})
	if err != nil {
		t.Fatal(err)
	}
	if free.ExpiresUnix != 0 {
		t.Fatalf("a free copy carries a reset at %d", free.ExpiresUnix)
	}
	m.Step()
	if m.sessions[free.ID] == nil {
		t.Fatal("a free copy was reset instead of graced")
	}
	if m.sessions[free.ID].emptyTicks != 1 {
		t.Fatalf("a free copy stopped counting empty ticks: %d", m.sessions[free.ID].emptyTicks)
	}
	if m.resetDueLocked(m.sessions[free.ID], clock.at.Unix()) {
		t.Fatal("a free copy's zero expiry read as a reset that has already passed")
	}
}
