package game

import (
	"slices"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// The list is complete every time, and the three events that change it all send it.
func TestTheBindingsListIsWholeAndIsSentOnEveryChange(t *testing.T) {
	m := instanceTestManager(t, 20, 4)
	first, second := InstanceRuin{7, -2}, InstanceRuin{-9, 41}
	character := instanceTestCharacter(1)

	var announced [][]CharacterBinding
	token, stated := m.WatchBindings(character, func(bindings []CharacterBinding) bool {
		announced = append(announced, slices.Clone(bindings))
		return true
	})
	// Registering states the list, under the same lock that installs the watcher: a
	// caller never reads it separately, so there is no window a change can fall into.
	// A character who owes nothing is a statement rather than the absence of one.
	if token == 0 || !stated {
		t.Fatalf("registering did not state the list: token %d, stated %v", token, stated)
	}
	if len(announced) != 1 || len(announced[0]) != 0 {
		t.Fatalf("the first statement was not an empty list: %+v", announced)
	}
	if got := m.Bindings(character); len(got) != 0 {
		t.Fatalf("an unbound character owes %+v", got)
	}

	one, err := m.Reenter(first, character)
	if err != nil {
		t.Fatal(err)
	}
	killMobInSession(t, one, vnet.MobKindVargrGuardian)
	m.Step()
	if len(announced) != 2 || len(announced[1]) != 1 {
		t.Fatalf("the first binding was not announced whole: %+v", announced)
	}

	// A second dungeon. The list is per character and spans every ruin they owe, which
	// is the one place "binding is per dungeon" is visible as a list rather than a rule.
	// Leaving first, because a character is inside at most one instance at a time — the
	// binding is what stays behind, which is the whole point of the list.
	if !m.Leave(one.ID, character) {
		t.Fatal("leaving the first run failed")
	}
	two, err := m.Reenter(second, character)
	if err != nil {
		t.Fatal(err)
	}
	killMobInSession(t, two, vnet.MobKindDraugrKing)
	m.Step()
	bindings := m.Bindings(character)
	if len(bindings) != 2 || len(announced) != 3 || len(announced[2]) != 2 {
		t.Fatalf("the second binding did not arrive as a whole list: %+v / %+v", bindings, announced)
	}
	// Ordered, so that two reads of unchanged state produce the same list.
	if bindings[0].Ruin != second || bindings[1].Ruin != first {
		t.Fatalf("the list has no stable order: %+v", bindings)
	}
	if !slices.Equal(bindings, m.Bindings(character)) {
		t.Fatal("two reads of unchanged state disagreed")
	}
	for _, binding := range bindings {
		if binding.BossesDefeated != 1 || binding.BossesTotal != bossEncounterTotal() || binding.ExpiresUnix == 0 {
			t.Fatalf("a binding states %+v", binding)
		}
	}

	// A reset releases one binding and the whole remaining list is sent again — not the
	// entry that moved, and not silence.
	m.mu.Lock()
	m.removeLocked(one.ID, m.sessions[one.ID])
	m.mu.Unlock()
	if len(announced) != 4 || len(announced[3]) != 1 || announced[3][0].Ruin != second {
		t.Fatalf("a reset did not restate the list: %+v", announced)
	}

	// And an unwatched connection is told nothing more.
	m.UnwatchBindings(character, token)
	m.Leave(two.ID, character)
	m.mu.Lock()
	m.removeLocked(two.ID, m.sessions[two.ID])
	m.mu.Unlock()
	if len(announced) != 4 {
		t.Fatalf("an unwatched connection was still delivered to: %+v", announced)
	}
	if got := m.Bindings(character); len(got) != 0 {
		t.Fatalf("the last reset left %+v", got)
	}
}

// One character's list says nothing about anybody else's.
func TestTheBindingsListIsPerCharacter(t *testing.T) {
	m := instanceTestManager(t, 20, 4)
	ruin := InstanceRuin{3, 3}
	mine, theirs := instanceTestCharacter(1), instanceTestCharacter(2)

	told := -1 // the registration states the list once, which is not news about anybody.
	m.WatchBindings(theirs, func([]CharacterBinding) bool { told++; return true })

	session, err := m.Reenter(ruin, mine)
	if err != nil {
		t.Fatal(err)
	}
	killMobInSession(t, session, vnet.MobKindVargrGuardian)
	m.Step()

	if got := m.Bindings(theirs); len(got) != 0 {
		t.Fatalf("somebody else's run reached this character's list: %+v", got)
	}
	if told != 0 {
		t.Fatal("a character was told about a binding that is not theirs")
	}
	if got := m.Bindings(mine); len(got) != 1 || got[0].Ruin != ruin {
		t.Fatalf("the bound character owes %+v", got)
	}
}

// A teardown may only end its own registration.
//
// **The race this closes is not reachable today**, and the test is here anyway. A second
// connection for one account is refused admission while the first holds its identity
// claim, and the first releases that claim strictly after its teardown has unwatched — so
// a reconnection cannot register before the connection it replaces has unregistered. That
// argument spans two packages and depends on where one statement sits inside a teardown;
// what is pinned here is the property that makes it unnecessary.
func TestAnUnwatchOnlyEndsTheRegistrationItNames(t *testing.T) {
	m := instanceTestManager(t, 20, 2)
	ruin := InstanceRuin{5, 5}
	character := instanceTestCharacter(1)

	var told int
	first, _ := m.WatchBindings(character, func([]CharacterBinding) bool { told++; return true })

	// The reconnection: a second registration replaces the first, and the token moves.
	second, _ := m.WatchBindings(character, func([]CharacterBinding) bool { told++; return true })
	if second == first || second == 0 {
		t.Fatalf("a second registration reused token %d", first)
	}

	// The connection it replaced now tears down and unwatches with the token it was
	// given. Under a delete-by-character pairing this would take the live watcher with it.
	m.UnwatchBindings(character, first)
	m.UnwatchBindings(character, 0)

	before := told
	session, err := m.Reenter(ruin, character)
	if err != nil {
		t.Fatal(err)
	}
	killMobInSession(t, session, vnet.MobKindVargrGuardian)
	m.Step()
	if told != before+1 {
		t.Fatalf("the live registration was told %d times, want one more than %d", told, before)
	}

	// A first statement the connection could not take is reported rather than swallowed:
	// it is the one delivery nothing restates, because the list is sent again only when it
	// changes.
	if _, stated := m.WatchBindings(instanceTestCharacter(2), func([]CharacterBinding) bool { return false }); stated {
		t.Fatal("a dropped first statement was reported as delivered")
	}

	// Its own token does end it.
	m.UnwatchBindings(character, second)
	if !m.Leave(session.ID, character) {
		t.Fatal("leaving failed")
	}
	after := told
	m.mu.Lock()
	m.removeLocked(session.ID, m.sessions[session.ID])
	m.mu.Unlock()
	if told != after {
		t.Fatalf("an ended registration was still delivered to: %d, want %d", told, after)
	}
}
