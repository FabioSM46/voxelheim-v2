package main

import (
	"reflect"
	"strings"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

func TestRejoinSelectsTheServerCharacterAndNeverCreatesAReplacement(t *testing.T) {
	characters := []protocol.CharacterSummary{{CharacterID: 71, Name: "Other"}, {CharacterID: 92, Name: "Delver"}}
	frame, err := characterChoice("Delver", characters, true)
	if err != nil {
		t.Fatal(err)
	}
	message, err := protocol.Decode(frame)
	if err != nil || message.SelectCharacter == nil || message.SelectCharacter.CharacterID != 92 {
		t.Fatalf("selection did not echo the saved server identity: %v", err)
	}
	for _, list := range [][]protocol.CharacterSummary{nil, characters[:1], {{Name: "Delver"}}} {
		if _, err := characterChoice("Delver", list, true); err == nil {
			t.Fatal("missing saved character became a fresh one")
		}
	}
	if _, err := characterChoice("Delver", characters, false); err == nil {
		t.Fatal("fresh run reused an existing account")
	}
	frame, err = characterChoice("Delver", nil, false)
	if err != nil {
		t.Fatal(err)
	}
	message, err = protocol.Decode(frame)
	if err != nil || message.CreateCharacter == nil || message.CreateCharacter.Name != "Delver" {
		t.Fatalf("fresh create: %v", err)
	}
}

func TestInventoryAnswersRetainWearSilverAndGenerationWithoutAliasing(t *testing.T) {
	c := &client{}
	want := protocol.InventoryState{Silver: 87, Stacks: make([]protocol.InventoryStack, protocol.InventorySlots)}
	want.Stacks[0] = protocol.InventoryStack{ItemID: 10, Count: 1, Durability: 12, MaxDurability: 200}
	want.Stacks[1] = protocol.InventoryStack{ItemID: 47, Count: 1}
	frame := protocol.EncodeInventoryState(want)
	c.absorb(vnet.GetRootAsEnvelope(frame, 0))
	got, revision := c.inventoryAnswer()
	if revision != 1 || !reflect.DeepEqual(got, want) {
		t.Fatalf("first answer = %+v generation %d", got, revision)
	}
	got.Stacks[0].Durability = 200
	clear(frame)
	got, _ = c.inventoryAnswer()
	if !reflect.DeepEqual(got, want) {
		t.Fatal("caller or frame mutation changed the stored answer")
	}
	want.Silver = 42
	want.Stacks[0].Durability = 200
	c.absorb(vnet.GetRootAsEnvelope(protocol.EncodeInventoryState(want), 0))
	got, revision = c.inventoryAnswer()
	if revision != 2 || !reflect.DeepEqual(got, want) {
		t.Fatalf("replacement = %+v generation %d", got, revision)
	}
}

func TestLootEventsKeepEachPersonalProjectionAndTypedRefusal(t *testing.T) {
	c := &client{}
	want := protocol.LootState{CorpseID: 101, Revision: 3, Silver: 12, Entries: []protocol.LootEntry{{EntryID: 8, ItemID: 47, Count: 1}}}
	frame := protocol.EncodeLootState(want)
	c.absorb(vnet.GetRootAsEnvelope(frame, 0))
	c.absorb(vnet.GetRootAsEnvelope(frame, 0))
	clear(frame)
	refused := protocol.ActionRefused{Action: vnet.RefusedActionUseMechanism, Reason: vnet.RefusalReasonChestAlreadyOpened, HasAnchor: true, Anchor: [3]int32{2, 3, 4}}
	c.absorb(vnet.GetRootAsEnvelope(protocol.EncodeActionRefused(refused), 0))
	c.absorb(vnet.GetRootAsEnvelope(protocol.EncodeLootClosed(protocol.LootClosed{CorpseID: 101}), 0))
	states, closed, refusals := c.takeLootEvents()
	if len(states) != 2 || !reflect.DeepEqual(states[0], want) || !reflect.DeepEqual(states[1], want) {
		t.Fatalf("loot projections: %+v", states)
	}
	if !reflect.DeepEqual(closed, []uint64{101}) || !reflect.DeepEqual(refusals, []protocol.ActionRefused{refused}) {
		t.Fatalf("closed %v refusals %+v", closed, refusals)
	}
	states, closed, refusals = c.takeLootEvents()
	if len(states)+len(closed)+len(refusals) != 0 {
		t.Fatal("drained events replayed")
	}
}

func TestSnapshotReplacesAccessibleLootAndStations(t *testing.T) {
	c := &client{}
	station := protocol.StructureState{StructureID: 12, Kind: vnet.StructureKindForge, Anchor: [3]int32{1, 2, 3}, Facing: vnet.FacingNorth, OwnerEntityID: 9}
	c.absorb(vnet.GetRootAsEnvelope(protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{Structures: []protocol.StructureState{station}, AccessibleLootCorpses: []uint64{73}}), 0))
	if !reflect.DeepEqual(c.structures, []protocol.StructureState{station}) || !reflect.DeepEqual(c.accessibleLoot, []uint64{73}) {
		t.Fatal("snapshot omitted station or accessible corpse")
	}
	c.absorb(vnet.GetRootAsEnvelope(protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{}), 0))
	if len(c.structures)+len(c.accessibleLoot) != 0 {
		t.Fatal("snapshot retained a vanished target")
	}
}

func TestServerUsesDurableStorageButReportOmitsOperationalArguments(t *testing.T) {
	o := options{worldDir: "fixture-private-storage", worldName: "descent", seed: 1, viewDistance: 4}
	args := strings.Join(serverArgs(o, "fixture-ticket-key"), " ")
	if !strings.Contains(args, "-world-dir fixture-private-storage") {
		t.Fatal("server did not receive the durable directory")
	}
	report := reportServerArgs(o)
	for _, private := range []string{o.worldDir, "fixture-ticket-key", "127.0.0.1"} {
		if strings.Contains(report, private) {
			t.Fatal("report includes operational arguments")
		}
	}
	if !strings.Contains(report, "<temporary-world>") {
		t.Fatal("report hides the persistence mode")
	}
}
