package game

import (
	"bytes"
	"fmt"
	"testing"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// beginTestCast is the caster-less surface this issue promises. Production has no
// reason to start one until mounting consumes the primitive; the test still exercises
// the same package-private transition that caller will use, without inventing a second
// gameplay action merely to prove the abstraction.
func beginTestCast(t *testing.T, p *Player, completed *int) (vnet.RefusalReason, error) {
	t.Helper()
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	return p.startCastLocked(vnet.CastKindMount, vnet.RefusedActionMount, func() { *completed++ })
}

func castRunning(p *Player) bool {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	return p.cast != nil
}

func actionRefusals(t *testing.T, out *dropSink) []protocol.ActionRefused {
	t.Helper()
	var refusals []protocol.ActionRefused
	for _, frame := range out.all() {
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		if envelope.PayloadType() != vnet.PayloadActionRefused {
			continue
		}
		var table flatbuffers.Table
		if !envelope.Payload(&table) {
			t.Fatal("ActionRefused envelope has no payload")
		}
		var payload vnet.ActionRefused
		payload.Init(table.Bytes, table.Pos)
		refusals = append(refusals, protocol.ActionRefused{
			Action: payload.Action(),
			Reason: payload.Reason(),
		})
	}
	return refusals
}

func TestAPlayerHoldsAtMostOneCast(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	completed := 0
	if reason, err := beginTestCast(t, player, &completed); err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("first cast: reason %s, error %v", reason, err)
	}
	if reason, err := beginTestCast(t, player, &completed); err == nil || reason != vnet.RefusalReasonCastAlreadyInProgress {
		t.Fatalf("second cast: reason %s, error %v; want CastAlreadyInProgress", reason, err)
	}
	if completed != 0 {
		t.Fatalf("refusing a second cast completed the first %d times", completed)
	}
}

func TestACastUsesTheSameWallTimeAtFiveAndSixtyHertz(t *testing.T) {
	t.Parallel()

	for _, rate := range []uint8{5, 60} {
		rate := rate
		t.Run(rateName(rate), func(t *testing.T) {
			t.Parallel()

			h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
			player, out := h.join(1, [3]float32{0.5, 64, 0.5})
			completed := 0
			if reason, err := beginTestCast(t, player, &completed); err != nil || reason != vnet.RefusalReasonUnknown {
				t.Fatalf("begin cast: reason %s, error %v", reason, err)
			}

			wantTicks := int(CastDuration.Seconds()) * int(rate)
			if h.sim.castTicks != uint32(wantTicks) {
				t.Fatalf("cast duration is %d ticks at %d Hz, want %d", h.sim.castTicks, rate, wantTicks)
			}
			for tick := 1; tick < wantTicks; tick++ {
				h.step()
				if completed != 0 || !castRunning(player) {
					t.Fatalf("cast completed after %d/%d ticks", tick, wantTicks)
				}
				cast := newestSnapshot(t, out).SelfCast(nil)
				if cast == nil {
					t.Fatalf("tick %d carries no running cast", tick)
				}
				wantProgress := uint8(uint64(tick) * 255 / uint64(wantTicks))
				if cast.Kind() != vnet.CastKindMount || cast.Progress() != wantProgress {
					t.Errorf("tick %d cast is %s/%d, want Mount/%d", tick, cast.Kind(), cast.Progress(), wantProgress)
				}
				if cast.Progress() == 255 {
					t.Errorf("tick %d exposed the completed progress the contract excludes", tick)
				}
			}

			h.step()
			if completed != 1 || castRunning(player) {
				t.Fatalf("after %d ticks completed = %d, running = %t; want one completion and no cast",
					wantTicks, completed, castRunning(player))
			}
			if cast := newestSnapshot(t, out).SelfCast(nil); cast != nil {
				t.Errorf("completion snapshot still carries %s/%d", cast.Kind(), cast.Progress())
			}
		})
	}
}

func TestEveryCastInterruptionEndsItAndNamesWhy(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name   string
		want   vnet.RefusalReason
		cancel func(*testing.T, *Player)
	}{
		{
			name: "damage",
			want: vnet.RefusalReasonCastInterruptedByDamage,
			cancel: func(_ *testing.T, player *Player) {
				player.sim.mu.Lock()
				player.damageLocked(1)
				player.sim.mu.Unlock()
			},
		},
		{
			name: "horizontal x intent",
			want: vnet.RefusalReasonCastInterruptedByMovement,
			cancel: func(t *testing.T, player *Player) {
				if err := player.Submit(protocol.PlayerInput{ClientTick: 1, MoveX: 1}); err != nil {
					t.Fatalf("Submit: %v", err)
				}
			},
		},
		{
			name: "horizontal z intent",
			want: vnet.RefusalReasonCastInterruptedByMovement,
			cancel: func(t *testing.T, player *Player) {
				if err := player.Submit(protocol.PlayerInput{ClientTick: 1, MoveZ: -1}); err != nil {
					t.Fatalf("Submit: %v", err)
				}
			},
		},
		{
			name: "jump intent",
			want: vnet.RefusalReasonCastInterruptedByJump,
			cancel: func(t *testing.T, player *Player) {
				if err := player.Submit(protocol.PlayerInput{ClientTick: 1, Jump: true}); err != nil {
					t.Fatalf("Submit: %v", err)
				}
			},
		},
		{
			name: "death",
			want: vnet.RefusalReasonCastInterruptedByDeath,
			cancel: func(_ *testing.T, player *Player) {
				player.sim.mu.Lock()
				player.damageLocked(PlayerMaxHealth)
				player.sim.mu.Unlock()
			},
		},
	}

	for _, tc := range tests {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
			player, out := h.join(1, [3]float32{0.5, 64, 0.5})
			completed := 0
			if reason, err := beginTestCast(t, player, &completed); err != nil || reason != vnet.RefusalReasonUnknown {
				t.Fatalf("begin cast: reason %s, error %v", reason, err)
			}

			tc.cancel(t, player)
			if castRunning(player) {
				t.Fatal("the interrupted cast is still running")
			}
			if completed != 0 {
				t.Fatalf("the interrupted cast completed %d times", completed)
			}
			refusals := actionRefusals(t, out)
			if len(refusals) != 1 {
				t.Fatalf("delivered %d cast refusals, want one", len(refusals))
			}
			want := protocol.ActionRefused{Action: vnet.RefusedActionMount, Reason: tc.want}
			if refusals[0] != want {
				t.Errorf("refusal = %+v, want %+v", refusals[0], want)
			}
		})
	}
}

func TestCastCancellationReasonsAreOneCompleteEnumeration(t *testing.T) {
	t.Parallel()

	want := [...]vnet.RefusalReason{
		vnet.RefusalReasonCastInterruptedByDamage,
		vnet.RefusalReasonCastInterruptedByMovement,
		vnet.RefusalReasonCastInterruptedByJump,
		vnet.RefusalReasonCastInterruptedByDeath,
	}
	if len(castInterruptionReasons) != len(want) {
		t.Fatalf("cast interruption list has %d entries, want %d", len(castInterruptionReasons), len(want))
	}
	for interruption, reason := range castInterruptionReasons {
		if reason != want[interruption] {
			t.Errorf("interruption %d maps to %s, want %s", interruption, reason, want[interruption])
		}
	}
}

func TestTurningTheCameraDoesNotInterruptACast(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	completed := 0
	if reason, err := beginTestCast(t, player, &completed); err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("begin cast: reason %s, error %v", reason, err)
	}
	if err := player.Submit(protocol.PlayerInput{ClientTick: 1, Yaw: 1.25, Pitch: -0.5}); err != nil {
		t.Fatalf("Submit: %v", err)
	}
	h.step()

	if !castRunning(player) || completed != 0 {
		t.Fatalf("camera-only input left running = %t, completed = %d", castRunning(player), completed)
	}
	if got := len(actionRefusals(t, out)); got != 0 {
		t.Errorf("camera-only input produced %d refusals", got)
	}
}

func TestZeroDamageDoesNotInterruptACast(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	completed := 0
	if reason, err := beginTestCast(t, player, &completed); err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("begin cast: reason %s, error %v", reason, err)
	}

	player.sim.mu.Lock()
	landed := player.damageLocked(0)
	player.sim.mu.Unlock()

	if landed {
		t.Fatal("zero damage was reported as a landed hit")
	}
	if !castRunning(player) || completed != 0 {
		t.Fatalf("zero damage left running = %t, completed = %d", castRunning(player), completed)
	}
	if got := len(actionRefusals(t, out)); got != 0 {
		t.Errorf("zero damage produced %d refusals", got)
	}
}

func TestACurrentDisplacingAStillPlayerDoesNotInterruptACast(t *testing.T) {
	t.Parallel()

	current := lakeWorld{bedTop: 0, waterTop: 200, waterKind: world.WaterCurrentXPos}
	h := newVitalsHarness(t, DefaultTickRate, current)
	player, out := h.join(1, [3]float32{0.5, 100, 0.5})
	completed := 0
	if reason, err := beginTestCast(t, player, &completed); err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("begin cast: reason %s, error %v", reason, err)
	}
	before := player.State()
	h.step()
	after := player.State()

	if after.Pos[0] <= before.Pos[0] {
		t.Fatalf("the current did not displace the still player: x %v then %v", before.Pos[0], after.Pos[0])
	}
	if !castRunning(player) || completed != 0 {
		t.Fatalf("current displacement left running = %t, completed = %d", castRunning(player), completed)
	}
	if got := len(actionRefusals(t, out)); got != 0 {
		t.Errorf("current displacement produced %d refusals", got)
	}
}

func TestACastInterruptionIsRetriedWhenTheOutboundQueueIsFull(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	completed := 0
	if reason, err := beginTestCast(t, player, &completed); err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("begin cast: reason %s, error %v", reason, err)
	}

	out.setFull(true)
	if err := player.Submit(protocol.PlayerInput{ClientTick: 1, Jump: true}); err != nil {
		t.Fatalf("Submit: %v", err)
	}
	if got := len(actionRefusals(t, out)); got != 0 {
		t.Fatalf("full queue accepted %d refusals", got)
	}
	out.setFull(false)
	h.step()

	refusals := actionRefusals(t, out)
	want := protocol.ActionRefused{Action: vnet.RefusedActionMount, Reason: vnet.RefusalReasonCastInterruptedByJump}
	if len(refusals) != 1 || refusals[0] != want {
		t.Fatalf("retried refusals = %+v, want [%+v]", refusals, want)
	}
}

func TestCastRefusalRetriesStayBoundedWhenAClientStopsDraining(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	out.setFull(true)

	oldest := protocol.ActionRefused{
		Action: vnet.RefusedActionMount,
		Reason: vnet.RefusalReasonCastInterruptedByDamage,
	}
	middle := protocol.ActionRefused{
		Action: vnet.RefusedActionMount,
		Reason: vnet.RefusalReasonCastInterruptedByMovement,
	}
	newest := protocol.ActionRefused{
		Action: vnet.RefusedActionMount,
		Reason: vnet.RefusalReasonCastInterruptedByJump,
	}

	player.sim.mu.Lock()
	player.queueCastRefusalLocked(oldest)
	for range maxPendingCastRefusals - 1 {
		player.queueCastRefusalLocked(middle)
	}
	player.queueCastRefusalLocked(newest)
	queued := append([][]byte(nil), player.pendingCastRefusals...)
	player.sim.mu.Unlock()

	if len(queued) != maxPendingCastRefusals {
		t.Fatalf("pending refusals = %d, want cap %d", len(queued), maxPendingCastRefusals)
	}
	if bytes.Equal(queued[0], protocol.EncodeActionRefused(oldest)) {
		t.Error("the oldest refusal survived after the retry queue reached its cap")
	}
	if !bytes.Equal(queued[0], protocol.EncodeActionRefused(middle)) {
		t.Error("capping the queue disturbed the retained FIFO order")
	}
	if !bytes.Equal(queued[len(queued)-1], protocol.EncodeActionRefused(newest)) {
		t.Error("the newest refusal was not retained at the tail of the retry queue")
	}
}

func rateName(rate uint8) string {
	return fmt.Sprintf("%d_hz", rate)
}
