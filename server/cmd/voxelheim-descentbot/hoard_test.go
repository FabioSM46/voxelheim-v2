package main

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"strings"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

func TestTerminalHoardDeathPreservesCauseWithoutRetryingLoot(t *testing.T) {
	r := &runner{pilot: &pilot{stats: newRunStats(newTally()), say: func(string, ...any) {}}}
	calls := 0
	err := r.phase(context.Background(), "chest 1", func(context.Context) error {
		calls++
		return fmt.Errorf("partial transfer: %w", &terminalHoardError{cause: errDied})
	})
	var terminal *terminalHoardError
	if calls != 1 || !errors.Is(err, errDied) || !errors.As(err, &terminal) {
		t.Fatalf("terminal reward retried or cause lost: calls %d error %v", calls, err)
	}
}

func TestHoardRequiresThreeMembersAndKeepsOrdinaryRoute(t *testing.T) {
	for _, members := range []string{"1", "2", "4", "5"} {
		if _, err := parseFlags("bot", []string{"-server", "server", "-hoard", "-party", members}); err == nil {
			t.Fatalf("hoard accepted party %s", members)
		}
	}
	o, err := parseFlags("bot", []string{"-server", "server", "-hoard", "-party", "3"})
	if err != nil {
		t.Fatal(err)
	}
	r := &runner{opts: o}
	want := map[string]string{"chest 1": "guardian", "chest 2": "timed grille", "chest 3": "king"}
	route := r.route()
	for i, step := range route {
		if previous, ok := want[step.name]; ok {
			if i == 0 || route[i-1].name != previous {
				t.Fatalf("%s misplaced", step.name)
			}
			delete(want, step.name)
		}
	}
	if len(want) != 0 {
		t.Fatal("missing chest stages", want)
	}
	r.opts.hoard = false
	if len(r.route()) != 11 {
		t.Fatal("ordinary descent route changed")
	}
}

func TestHoardRollAndInventoryAssertionsRejectWrongRewards(t *testing.T) {
	loot := protocol.LootState{CorpseID: 1, Silver: 8, Entries: []protocol.LootEntry{{ItemID: uint16(game.ItemArrow), Count: 8}}}
	if err := validateChestRoll(0, loot); err != nil {
		t.Fatal(err)
	}
	for _, index := range []int{-1, 1, 2, 3} {
		if validateChestRoll(index, loot) == nil {
			t.Fatalf("arrow roll accepted for chest %d", index)
		}
	}
	before := protocol.InventoryState{Stacks: []protocol.InventoryStack{{ItemID: uint16(game.ItemArrow), Count: 2}}}
	after := protocol.InventoryState{Stacks: []protocol.InventoryStack{{ItemID: uint16(game.ItemArrow), Count: 10}}}
	if !lootAdded(before, after, loot) {
		t.Fatal("exact earned delta rejected")
	}
	after.Stacks[0].Count++
	if lootAdded(before, after, loot) {
		t.Fatal("extra grant accepted")
	}
}

func TestHoardRepairSelectsCheapestActualAffordableWear(t *testing.T) {
	member := func(silver uint32, wear ...uint16) *runner {
		state := protocol.InventoryState{Silver: silver}
		for _, missing := range wear {
			state.Stacks = append(state.Stacks, protocol.InventoryStack{ItemID: uint16(game.ItemIronSword), Count: 1, Durability: 200 - missing, MaxDurability: 200})
		}
		return &runner{pilot: &pilot{c: &client{inventory: state}}}
	}
	a, b := member(40, 0, 50, 30), member(65, 10, 10)
	pt := &party{members: []*runner{a, b}}
	got, slot, ok := pt.cheapestRepair()
	if !ok || got != b || slot != 0 {
		t.Fatal("did not select first cheapest affordable actual wear")
	}
	pt.members = []*runner{member(5, 0, 10)}
	if _, _, ok := pt.cheapestRepair(); ok {
		t.Fatal("invented an affordable worn target")
	}
	if repairPrice(protocol.InventoryStack{Count: 1, Durability: 0, MaxDurability: 200}) != 200 {
		t.Fatal("broken item price mismatch")
	}
}

func TestHoardEvidenceFailsClosedAndNeverContainsRawError(t *testing.T) {
	pt := &party{}
	for range 3 {
		r := &runner{pilot: &pilot{stats: newRunStats(newTally())}}
		r.hoard.Bootstrap, r.hoard.KingDone, r.hoard.RuneOut = true, true, true
		for i := range r.hoard.ChestDone {
			r.hoard.ChestDone[i], r.hoard.Restored[i] = true, true
		}
		pt.members = append(pt.members, r)
	}
	pt.members[0].hoard.Crafted = true
	pt.members[1].hoard.Repaired = true
	if !pt.hoardComplete() {
		t.Fatal("complete evidence rejected")
	}
	pt.members[2].hoard.Restored[2] = false
	if pt.hoardComplete() {
		t.Fatal("missing ninth restart assertion accepted")
	}
	var out bytes.Buffer
	writeHoardReport(&out, pt, errors.New("private diagnostic sentinel"))
	if strings.Contains(out.String(), "private diagnostic sentinel") || !strings.Contains(out.String(), `"Success":false`) {
		t.Fatal("raw diagnostic leaked or failure accepted")
	}
}

func TestRestartInventoryEqualityIncludesCurrencyWearAndSlotOrder(t *testing.T) {
	a := protocol.InventoryState{Silver: 40, Stacks: []protocol.InventoryStack{{ItemID: 1, Count: 1, Durability: 20, MaxDurability: 30}}}
	b := protocol.InventoryState{Silver: a.Silver, Stacks: append([]protocol.InventoryStack(nil), a.Stacks...)}
	if !inventoriesEqual(a, b) {
		t.Fatal("identical persistence answer rejected")
	}
	b.Stacks[0].Durability++
	if inventoriesEqual(a, b) {
		t.Fatal("durability loss hidden")
	}
	b.Stacks[0] = a.Stacks[0]
	b.Silver++
	if inventoriesEqual(a, b) {
		t.Fatal("currency drift hidden")
	}
}
