package main

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"net"
	"strings"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Only terrain is a stub. Requests decoded from the bot's socket are resolved
// by production Player.Chat and Player.MoveInventory, as the session does.
type replacementTerrain struct{}

func (replacementTerrain) Block(int64, int64, int64) (world.Block, bool) { return world.Air, true }
func (replacementTerrain) Solid(int64, int64, int64) bool                { return false }
func (replacementTerrain) Fluid(int64, int64, int64) bool                { return false }
func (replacementTerrain) ApplyGuarded(context.Context, int64, int64, int64, world.Block, func() error, func(world.Block) error) error {
	return fmt.Errorf("unexpected terrain edit")
}

func replacementPlayer(t *testing.T, state protocol.InventoryState, commands bool) *game.Player {
	t.Helper()
	terrain := replacementTerrain{}
	sim, err := game.NewSim(20, 0, 1, terrain, terrain, func() uint64 { return 100 }, slog.New(slog.NewTextHandler(io.Discard, nil)), game.WithDevCommands(commands))
	if err != nil {
		t.Fatal(err)
	}
	life := game.Life{Health: 100, Hunger: 100}
	copy(life.Slots[:], state.Stacks)
	player, err := sim.Join(1, identity.PlayerID{1}, "Fixture", [3]float32{}, protocol.Appearance{HairModel: vnet.HairModelBraided}, &life, func([]byte) bool { return true })
	if err != nil {
		t.Fatal(err)
	}
	return player
}

func absorbReplacement(p *pilot, state protocol.InventoryState) {
	p.c.absorb(vnet.GetRootAsEnvelope(protocol.EncodeInventoryState(state), 0))
}

func TestWornSceptreReplacementUsesAuthoritativeMoves(t *testing.T) {
	for _, outcome := range []string{"confirmed", "command-refused", "park-refused", "equip-refused", "unconfirmed"} {
		t.Run(outcome, func(t *testing.T) {
			p := healerPilotFixture()
			state := sceptreInventory(0)
			// Two successive replacements retain both worn items. Exhaustion can follow
			// launches or death wear; only the authoritative durability value is read.
			rounds := 1
			if outcome == "confirmed" {
				rounds = 2
			}
			for round := 0; round < rounds; round++ {
				state.Stacks[mainHandSlot].Durability = 0
				player := replacementPlayer(t, state, outcome != "command-refused")
				absorbReplacement(p, player.InventoryState())
				local, remote := net.Pipe()
				defer func() { _ = local.Close(); _ = remote.Close() }()
				if err := remote.SetDeadline(time.Now().Add(2 * time.Second)); err != nil {
					t.Fatal(err)
				}
				p.c.conn = local
				ctx, cancel := context.WithTimeout(context.Background(), time.Second)
				defer cancel()
				done := make(chan error, 1)
				go func() { done <- p.ensureSceptre(ctx) }()
				msg := readBotMessage(t, remote)
				if msg.Chat == nil {
					t.Fatal("missing grant command")
				}
				reply, err := player.Chat(msg.Chat.Text)
				if err != nil {
					t.Fatal(err)
				}
				p.c.chat <- reply.PrivateText
				if reply.Inventory != nil {
					// Pin the old failure explicitly: production refuses a same-item swap.
					if _, err := player.MoveInventory(protocol.InventoryMoveRequest{From: 0, To: mainHandSlot, Count: 1}); err == nil {
						t.Fatal("production accepted direct durable swap")
					}
					absorbReplacement(p, *reply.Inventory)
					msg = readBotMessage(t, remote)
					if msg.InventoryMove == nil || msg.InventoryMove.From != mainHandSlot {
						t.Fatal("did not park worn item first")
					}
					if outcome == "park-refused" {
						player.BeginLeaving()
					}
					parked, moveErr := player.MoveInventory(*msg.InventoryMove)
					if moveErr != nil {
						if outcome != "park-refused" {
							t.Fatal(moveErr)
						}
						cancel()
					} else {
						if p.c.healingTotals().replacements != uint64(round) {
							t.Fatal("counted before parking confirmation")
						}
						absorbReplacement(p, parked)
						msg = readBotMessage(t, remote)
						if msg.InventoryMove == nil || msg.InventoryMove.To != mainHandSlot {
							t.Fatal("missing equip request")
						}
						if outcome == "equip-refused" {
							player.BeginLeaving()
						}
						equipped, moveErr := player.MoveInventory(*msg.InventoryMove)
						if moveErr != nil {
							if outcome != "equip-refused" {
								t.Fatal(moveErr)
							}
							cancel()
						} else {
							if p.c.healingTotals().replacements != uint64(round) {
								t.Fatal("counted before equipment confirmation")
							}
							if outcome == "unconfirmed" {
								cancel()
							} else {
								absorbReplacement(p, equipped)
							}
						}
					}
				}
				err = <-done
				if (err == nil) != (outcome == "confirmed") {
					t.Fatalf("replacement: %v", err)
				}
				state = player.InventoryState()
				if outcome == "confirmed" {
					worn := 0
					for _, s := range state.Stacks[:protocol.InventorySlots-protocol.EquipmentSlots] {
						if s.ItemID == uint16(game.ItemWoodenSceptre) && s.Durability == 0 {
							worn++
						}
					}
					if worn != round+1 || !usableSceptre(state) {
						t.Fatalf("lost worn item or fresh hand: %+v", state)
					}
				}
			}
			want := uint64(0)
			if outcome == "confirmed" {
				want = 2
			}
			if p.c.healingTotals().replacements != want || len(p.stats.commands) != rounds {
				t.Fatal("assistance accounting mismatch")
			}
		})
	}
}

func TestReplacementCapacityAndHealthyHand(t *testing.T) {
	for _, free := range []int{0, 1} {
		p := healerPilotFixture()
		p.c.inventory = sceptreInventory(0)
		for i := free; i < int(protocol.InventorySlots-protocol.EquipmentSlots); i++ {
			p.c.inventory.Stacks[i] = protocol.InventoryStack{ItemID: uint16(game.ItemStone), Count: 1}
		}
		if err := p.ensureSceptre(context.Background()); err == nil || !strings.Contains(err.Error(), "two empty pack slots") {
			t.Fatalf("capacity: %v", err)
		}
		if len(p.stats.commands) != 0 || p.c.healingTotals().replacements != 0 {
			t.Fatal("full pack received assistance")
		}
	}
	p := healerPilotFixture()
	if err := p.ensureSceptre(context.Background()); err != nil || len(p.stats.commands) != 0 {
		t.Fatal("healthy sceptre replaced")
	}
	p.c.inventory = protocol.InventoryState{}
	if err := p.ensureSceptre(context.Background()); err == nil {
		t.Fatal("missing state accepted")
	}
}
