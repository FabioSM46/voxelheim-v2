package main

import (
	"errors"
	"slices"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// characterChoice never replaces a missing saved character. A fresh run requires
// an empty account; a rejoin selects the one matching name sent by this server.
func characterChoice(name string, characters []protocol.CharacterSummary, existing bool) ([]byte, error) {
	if existing {
		for _, character := range characters {
			if character.Name == name && character.CharacterID != 0 {
				return protocol.EncodeSelectCharacterRequest(protocol.SelectCharacterRequest{CharacterID: character.CharacterID}), nil
			}
		}
		return nil, errors.New("saved acceptance character missing from server list")
	}
	if len(characters) != 0 {
		return nil, errors.New("fresh acceptance account unexpectedly already has characters")
	}
	return protocol.EncodeCreateCharacterRequest(protocol.CreateCharacterRequest{
		Name: name, Appearance: protocol.Appearance{
			SkinColor: 0x00E3C4A0, ShirtColor: 0x004A5D3B, TrousersColor: 0x002B2118,
			ShoesColor: 0x00553311, HairModel: vnet.HairModelBraided, HairColor: 0x00B07A32,
		}, HasAppearance: true,
	}), nil
}

func inventoryFromWire(state *vnet.InventoryState) protocol.InventoryState {
	result := protocol.InventoryState{Silver: state.Silver(), Stacks: make([]protocol.InventoryStack, state.StacksLength()/2)}
	for i := range result.Stacks {
		result.Stacks[i] = protocol.InventoryStack{ItemID: state.Stacks(i * 2), Count: state.Stacks(i*2 + 1), Durability: state.Durability(i), MaxDurability: state.MaxDurability(i)}
	}
	return result
}

func lootFromWire(state *vnet.LootState) protocol.LootState {
	result := protocol.LootState{CorpseID: state.CorpseId(), Revision: state.Revision(), Silver: state.Silver()}
	var entry vnet.LootEntry
	for i := range state.EntriesLength() {
		if state.Entries(&entry, i) {
			result.Entries = append(result.Entries, protocol.LootEntry{EntryID: entry.EntryId(), ItemID: entry.ItemId(), Count: entry.Count(), Durability: entry.Durability(), MaxDurability: entry.MaxDurability()})
		}
	}
	return result
}

func (c *client) absorbRefusal(refused *vnet.ActionRefused) {
	refusal := protocol.ActionRefused{Action: refused.Action(), Reason: refused.Reason()}
	if anchor := refused.Anchor(nil); anchor != nil {
		refusal.Anchor, refusal.HasAnchor = [3]int32{anchor.X(), anchor.Y(), anchor.Z()}, true
	}
	c.mu.Lock()
	c.actionRefusals = append(c.actionRefusals, refusal)
	c.mu.Unlock()
}

// Called only while absorbSnapshot holds mu. These lists replace the previous
// snapshot: disappearance must never leave a stale corpse or station target.
func (c *client) absorbHoardSnapshot(snapshot *vnet.EntitySnapshot) {
	c.accessibleLoot = c.accessibleLoot[:0]
	for i := range snapshot.AccessibleLootCorpsesLength() {
		c.accessibleLoot = append(c.accessibleLoot, snapshot.AccessibleLootCorpses(i))
	}
	c.structures = c.structures[:0]
	var state vnet.StructureState
	for i := range snapshot.StructuresLength() {
		if !snapshot.Structures(&state, i) {
			continue
		}
		anchor := state.Anchor(nil)
		if anchor == nil {
			continue
		}
		c.structures = append(c.structures, protocol.StructureState{StructureID: state.StructureId(), Kind: state.Kind(), Anchor: [3]int32{anchor.X(), anchor.Y(), anchor.Z()}, Facing: state.Facing(), OwnerEntityID: state.OwnerEntityId(), Doused: !state.Lit()})
	}
}

// inventoryAnswer is a detached, complete authoritative answer plus its receive
// generation. Callers can await a new answer without assuming a tick or a sleep.
func (c *client) inventoryAnswer() (protocol.InventoryState, uint64) {
	c.mu.Lock()
	defer c.mu.Unlock()
	return protocol.InventoryState{Stacks: slices.Clone(c.inventory.Stacks), Silver: c.inventory.Silver}, c.inventoryRevision
}

// takeLootEvents drains lossless typed observations atomically. They are finite
// request responses, not per-tick snapshots. No overflow silently hides a refusal.
func (c *client) takeLootEvents() ([]protocol.LootState, []uint64, []protocol.ActionRefused) {
	c.mu.Lock()
	defer c.mu.Unlock()
	states, closed, refused := c.lootStates, c.lootClosed, c.actionRefusals
	c.lootStates, c.lootClosed, c.actionRefusals = nil, nil, nil
	return states, closed, refused
}
