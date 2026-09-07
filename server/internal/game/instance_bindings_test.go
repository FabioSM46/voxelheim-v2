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
	m.WatchBindings(character, func(bindings []CharacterBinding) {
		announced = append(announced, slices.Clone(bindings))
	})

	// A character who owes nothing is a statement rather than the absence of one.
	if got := m.Bindings(character); len(got) != 0 {
		t.Fatalf("an unbound character owes %+v", got)
	}
	if len(announced) != 0 {
		t.Fatal("registering a watcher announced something on its own")
	}

	one, err := m.Reenter(first, character)
	if err != nil {
		t.Fatal(err)
	}
	killMobInSession(t, one, vnet.MobKindVargrGuardian)
	m.Step()
	if len(announced) != 1 || len(announced[0]) != 1 {
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
	if len(bindings) != 2 || len(announced) != 2 || len(announced[1]) != 2 {
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
	if len(announced) != 3 || len(announced[2]) != 1 || announced[2][0].Ruin != second {
		t.Fatalf("a reset did not restate the list: %+v", announced)
	}

	// And an unwatched connection is told nothing more.
	m.UnwatchBindings(character)
	m.Leave(two.ID, character)
	m.mu.Lock()
	m.removeLocked(two.ID, m.sessions[two.ID])
	m.mu.Unlock()
	if len(announced) != 3 {
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

	var told int
	m.WatchBindings(theirs, func([]CharacterBinding) { told++ })

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
