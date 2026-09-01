package game

import (
	"context"
	"errors"
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

type roofedMountTerrain struct {
	groundTop int64
	roofY     int64
}

func (w roofedMountTerrain) Block(_, y, _ int64) (world.Block, bool) {
	if y <= w.groundTop || y == w.roofY {
		return world.Stone, true
	}
	return world.Air, true
}

func (w roofedMountTerrain) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || world.Solid(block)
}

func (w roofedMountTerrain) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

func prepareMount(p *Player, kind vnet.MountKind, grounded bool) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	p.learnedMounts, _ = p.learnedMounts.Learn(kind)
	p.onGround = grounded
}

func mountedKind(p *Player) vnet.MountKind {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	return p.mounted
}

func forceMounted(p *Player, kind vnet.MountKind) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	p.mounted = kind
}

func TestMountAdmissionNamesEveryAuthoritativeRefusal(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name      string
		terrain   Terrain
		learned   bool
		grounded  bool
		already   bool
		requested vnet.MountKind
		want      vnet.RefusalReason
	}{
		{"unknown mount", dropTerrain{groundTop: 63}, false, true, false, vnet.MountKindUnknown, vnet.RefusalReasonMountNotLearned},
		{"unlearned mount", dropTerrain{groundTop: 63}, false, true, false, vnet.MountKindBlackHorse, vnet.RefusalReasonMountNotLearned},
		{"already mounted", dropTerrain{groundTop: 63}, true, true, true, vnet.MountKindBlackHorse, vnet.RefusalReasonAlreadyMounted},
		{"not grounded", dropTerrain{groundTop: 63}, true, false, false, vnet.MountKindBlackHorse, vnet.RefusalReasonMountNotGrounded},
		{"low ceiling", roofedMountTerrain{groundTop: 63, roofY: 66}, true, true, false, vnet.MountKindBlackHorse, vnet.RefusalReasonMountLowCeiling},
		{"indoors", roofedMountTerrain{groundTop: 63, roofY: 70}, true, true, false, vnet.MountKindBlackHorse, vnet.RefusalReasonMountIndoors},
	}

	for _, tc := range tests {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			h := newVitalsHarness(t, DefaultTickRate, tc.terrain)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			if tc.learned {
				prepareMount(player, vnet.MountKindBlackHorse, tc.grounded)
			} else {
				player.sim.mu.Lock()
				player.onGround = tc.grounded
				player.sim.mu.Unlock()
			}
			if tc.already {
				forceMounted(player, vnet.MountKindBrownHorse)
			}

			reason, err := player.Mount(tc.requested)
			if err == nil || reason != tc.want {
				t.Fatalf("Mount(%s): reason %s, error %v; want %s", tc.requested, reason, err, tc.want)
			}
			if castRunning(player) {
				t.Error("a refused mount request started a cast")
			}
		})
	}
}

func TestMountCastCompletesIntoSnapshotsAndDismountIsImmediate(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	prepareMount(player, vnet.MountKindGreyHorse, true)

	if reason, err := player.Mount(vnet.MountKindGreyHorse); err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("Mount: reason %s, error %v", reason, err)
	}
	h.advance(int(h.sim.castTicks))
	if got := mountedKind(player); got != vnet.MountKindGreyHorse {
		t.Fatalf("mounted = %s, want GreyHorse", got)
	}

	snapshot := newestSnapshot(t, out)
	if snapshot.MountsLength() != 1 {
		t.Fatalf("snapshot carries %d mounts, want one", snapshot.MountsLength())
	}
	state := new(vnet.MountState)
	if !snapshot.Mounts(state, 0) || state.EntityId() != player.entityID || state.Mount() != vnet.MountKindGreyHorse {
		t.Errorf("snapshot mount = entity %d/%s, want %d/GreyHorse", state.EntityId(), state.Mount(), player.entityID)
	}

	player.Dismount()
	if got := mountedKind(player); got != vnet.MountKindUnknown {
		t.Fatalf("Dismount left %s mounted", got)
	}
	h.step()
	if got := newestSnapshot(t, out).MountsLength(); got != 0 {
		t.Errorf("snapshot after dismount carries %d mounts, want none", got)
	}

	// The same unconditional request cancels a pending mount without manufacturing an
	// interruption event or letting the completion callback mount later.
	if reason, err := player.Mount(vnet.MountKindGreyHorse); err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("second Mount: reason %s, error %v", reason, err)
	}
	player.Dismount()
	h.advance(int(h.sim.castTicks))
	if castRunning(player) || mountedKind(player) != vnet.MountKindUnknown {
		t.Error("dismount did not cancel the pending mount cast")
	}
}

func TestMountedMovementUsesHorseNumbersWithoutChangingTheBodyOrWater(t *testing.T) {
	t.Parallel()

	apex := MountJumpImpulse * MountJumpImpulse / (2 * Gravity)
	if apex <= 2 || apex >= 3 {
		t.Fatalf("mounted jump apex = %v blocks, want over two and under three", apex)
	}
	if MountSpeed != 2*WalkSpeed {
		t.Fatalf("MountSpeed = %v, want twice WalkSpeed %v", MountSpeed, WalkSpeed)
	}
	if got := playerBody; got != (body{width: PlayerWidth, height: PlayerHeight}) {
		t.Fatalf("player collision body = %+v, want unchanged %.1fx%.1f", got, PlayerWidth, PlayerHeight)
	}

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	prepareMount(player, vnet.MountKindBlackHorse, true)
	forceMounted(player, vnet.MountKindBlackHorse)
	if err := player.Submit(protocol.PlayerInput{MoveZ: 1, ClientTick: 1}); err != nil {
		t.Fatalf("Submit: %v", err)
	}
	h.step()
	if got := math.Hypot(float64(player.State().Vel[0]), float64(player.State().Vel[2])); math.Abs(got-MountSpeed) > 1e-5 {
		t.Errorf("mounted horizontal speed = %v, want %v", got, MountSpeed)
	}

	// Water remains the stronger branch: a horse that enters it swims at the existing
	// cap and rises with the existing swim rule rather than carrying its land bonuses in.
	lake := newVitalsHarness(t, DefaultTickRate, lakeWorld{bedTop: 40, waterTop: 60})
	swimmer, _ := lake.join(2, [3]float32{0.5, 50, 0.5})
	forceMounted(swimmer, vnet.MountKindBrownHorse)
	if err := swimmer.Submit(protocol.PlayerInput{MoveZ: 1, Jump: true, ClientTick: 1}); err != nil {
		t.Fatalf("swim Submit: %v", err)
	}
	lake.advance(swimSettleTicks)
	swimState := swimmer.State()
	if got := math.Hypot(float64(swimState.Vel[0]), float64(swimState.Vel[2])); got > SwimSpeed+1e-5 {
		t.Errorf("mounted swim speed = %v, above SwimSpeed %v", got, SwimSpeed)
	}
	if swimState.Vel[1] >= float32(MountJumpImpulse) {
		t.Errorf("mounted swim rise = %v, carried land jump impulse %v", swimState.Vel[1], MountJumpImpulse)
	}
}

func TestMountedFallUsesTheExistingImpactRule(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	rider, _ := h.join(1, [3]float32{0.5, 84, 0.5})
	walker, _ := h.join(2, [3]float32{4.5, 84, 0.5})
	forceMounted(rider, vnet.MountKindBlackHorse)

	for range 300 {
		h.step()
		if rider.State().OnGround && walker.State().OnGround {
			break
		}
	}
	riderVitals, walkerVitals := h.vitals(rider), h.vitals(walker)
	if riderVitals.Health != walkerVitals.Health {
		t.Errorf("same fall left rider health %d and walker health %d", riderVitals.Health, walkerVitals.Health)
	}
	if riderVitals.Health >= riderVitals.MaxHealth {
		t.Error("the comparison fall caused no damage, so it did not exercise impact pricing")
	}
}

func TestDamageKeepsAMountDeathAndReconnectDoNot(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	forceMounted(player, vnet.MountKindGreyHorse)
	h.hurt(player, 1)
	if got := mountedKind(player); got != vnet.MountKindGreyHorse {
		t.Fatalf("damage dismounted %s", got)
	}
	h.hurt(player, PlayerMaxHealth)
	if got := mountedKind(player); got != vnet.MountKindUnknown {
		t.Fatalf("death left %s mounted", got)
	}

	life := Life{Pos: [3]float64{0.5, 64, 0.5}, Health: PlayerMaxHealth, Hunger: PlayerMaxHunger,
		LearnedMounts: LearnedMounts(1), Slots: [protocol.InventorySlots]protocol.InventoryStack{}}
	reconnected, _ := h.joinLife(2, [3]float32{0.5, 64, 0.5}, &life)
	if got := mountedKind(reconnected); got != vnet.MountKindUnknown {
		t.Errorf("reconnected player arrived mounted on %s", got)
	}
}

func TestEverySaddleActionIsRefusedByTheServer(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	forceMounted(player, vnet.MountKindBlackHorse)
	h.stock(player, 0, ItemTent, 1)

	if reason, err := player.Attack(protocol.AttackRequest{}); !errors.Is(err, ErrActionForbiddenWhileMounted) || reason != vnet.RefusalReasonActionForbiddenWhileMounted {
		t.Errorf("Attack: reason %s, error %v", reason, err)
	}
	if err := player.Mine(protocol.MineRequest{Pos: [3]int32{0, 63, 0}, HasPos: true, Active: true}, true); !errors.Is(err, ErrActionForbiddenWhileMounted) {
		t.Errorf("Mine: error %v", err)
	}
	if _, err := player.Edit(context.Background(), protocol.BlockEditRequest{Pos: [3]int32{0, 64, 0}, HasPos: true, Action: vnet.EditActionPlace}); !errors.Is(err, ErrActionForbiddenWhileMounted) {
		t.Errorf("Edit: error %v", err)
	}
	if _, reason, err := player.PlaceStructure(protocol.PlaceStructureRequest{Slot: 0, Anchor: [3]int32{0, 63, 0}, HasAnchor: true, Facing: vnet.FacingNorth}); !errors.Is(err, ErrActionForbiddenWhileMounted) || reason != vnet.RefusalReasonActionForbiddenWhileMounted {
		t.Errorf("PlaceStructure: reason %s, error %v", reason, err)
	}
	if _, reason, err := player.Consume(protocol.ConsumeRequest{Slot: 0}); !errors.Is(err, ErrActionForbiddenWhileMounted) || reason != vnet.RefusalReasonActionForbiddenWhileMounted {
		t.Errorf("Consume: reason %s, error %v", reason, err)
	}
	if reason, err := player.InteractNPC(protocol.NpcInteractRequest{EntityID: 99}); !errors.Is(err, ErrActionForbiddenWhileMounted) || reason != vnet.RefusalReasonActionForbiddenWhileMounted {
		t.Errorf("InteractNPC: reason %s, error %v", reason, err)
	}
	if reason, err := player.Trade(protocol.TradeRequest{EntityID: 99, Count: 1}); !errors.Is(err, ErrActionForbiddenWhileMounted) || reason != vnet.RefusalReasonActionForbiddenWhileMounted {
		t.Errorf("Trade: reason %s, error %v", reason, err)
	}
}
