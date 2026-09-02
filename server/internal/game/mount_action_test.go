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

// bodyOf is the body the simulation sweeps this player as, read under its lock.
func bodyOf(p *Player) body {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	return p.body()
}

// embedded reports whether the player's own body overlaps a solid, under the lock.
func embedded(h *vitalsHarness, p *Player) bool {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	return overlaps(h.sim.terrain, p.box())
}

// builtOn is flat ground with the solids somebody built on it, described by a closure
// so a test raises a wall, a lintel or a corridor in one line — and, because a closure
// can read a variable, puts a block down between two ticks, which is what a neighbour
// with stone in hand does during a two-second cast.
func builtOn(wall func(x, y, z int64) bool) walledTerrain {
	return walledTerrain{dropTerrain: dropTerrain{groundTop: 63}, wall: wall}
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

// The name this test used to carry said the body did not change. It does now, and
// that is the point: the horse's numbers are the speed and the jump, the horse's body
// is the box, and water is still the stronger branch over all three.
func TestMountedMovementUsesHorseNumbersAndTheMountedBodyWithoutChangingWater(t *testing.T) {
	t.Parallel()

	apex := MountJumpImpulse * MountJumpImpulse / (2 * Gravity)
	if apex <= 2 || apex >= 3 {
		t.Fatalf("mounted jump apex = %v blocks, want over two and under three", apex)
	}
	if MountSpeed != 2*WalkSpeed {
		t.Fatalf("MountSpeed = %v, want twice WalkSpeed %v", MountSpeed, WalkSpeed)
	}
	if mountedBody != (body{width: MountedWidth, height: MountedHeight}) {
		t.Fatalf("mounted collision body = %+v, want %.1fx%.1f", mountedBody, MountedWidth, MountedHeight)
	}
	// The walking body lies inside the mounted one on every side, which is what lets a
	// dismount leave the player where they are instead of un-embedding them.
	if MountedWidth <= PlayerWidth || MountedHeight <= PlayerHeight {
		t.Fatalf("mounted body %.1fx%.1f does not contain the walking body %.1fx%.1f",
			MountedWidth, MountedHeight, PlayerWidth, PlayerHeight)
	}

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	prepareMount(player, vnet.MountKindBlackHorse, true)
	if got := bodyOf(player); got != playerBody {
		t.Fatalf("a player on foot is swept as %+v, want playerBody %+v", got, playerBody)
	}
	forceMounted(player, vnet.MountKindBlackHorse)
	if got := bodyOf(player); got != mountedBody {
		t.Fatalf("a mounted player is swept as %+v, want mountedBody %+v", got, mountedBody)
	}
	if err := player.Submit(protocol.PlayerInput{MoveZ: 1, ClientTick: 1}); err != nil {
		t.Fatalf("Submit: %v", err)
	}
	h.step()
	if got := math.Hypot(float64(player.State().Vel[0]), float64(player.State().Vel[2])); math.Abs(got-MountSpeed) > 1e-5 {
		t.Errorf("mounted horizontal speed = %v, want %v", got, MountSpeed)
	}
	player.Dismount()
	if got := bodyOf(player); got != playerBody {
		t.Errorf("dismounting left the player swept as %+v, want playerBody %+v", got, playerBody)
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

// The mounted body is what the alley and the lintel stop. One wall across z = 0 with
// an opening in it, approached from the open side on foot and then in the saddle: the
// walker goes through, the rider is held at the face.
func TestTheMountedBodyIsWhatTheAlleyAndTheLintelStop(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name  string
		wall  func(x, y, z int64) bool
		start [3]float32
	}{
		{
			// A one-block gap in a wall four high. The rider stands a tenth off the gap's
			// centre line, deliberately: boxes are half-open, so a 1.0 body passes a 1.0
			// gap when it is lined up to the ulp, and a rider who is not is stopped —
			// which is what a wall is. The walker's 0.6 has a tenth to spare either side.
			name:  "a one-block gap",
			wall:  func(x, y, z int64) bool { return z == 0 && y >= 64 && y <= 67 && x != 0 },
			start: [3]float32{0.6, 64, 3.5},
		},
		{
			// A lintel two blocks above the ground: the walker's 1.8 passes under a solid
			// at 66, the rider's 2.8 does not.
			name:  "a two-block ceiling",
			wall:  func(_, y, z int64) bool { return z == 0 && (y == 66 || y == 67) },
			start: [3]float32{0.5, 64, 3.5},
		},
	}

	for _, tc := range tests {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			for _, mounted := range []bool{false, true} {
				h := newVitalsHarness(t, DefaultTickRate, builtOn(tc.wall))
				player, _ := h.join(1, tc.start)
				if mounted {
					forceMounted(player, vnet.MountKindBlackHorse)
				}
				// Sixty ticks is three seconds: over twelve blocks on foot, twice that in
				// the saddle, from three and a half blocks short of the wall.
				h.hold(player, protocol.PlayerInput{MoveZ: 1}, 60)
				z := h.position(player)[2]
				switch {
				case !mounted && z >= 0:
					t.Errorf("on foot the player stopped at z=%.2f, want through %s", z, tc.name)
				case mounted && z < 1:
					t.Errorf("mounted the player reached z=%.2f, want held at the face of %s", z, tc.name)
				}
				if embedded(h, player) {
					t.Errorf("mounted=%t: the player ended inside a solid at %v", mounted, h.position(player))
				}
			}
		})
	}
}

// Mounting is admitted only where the mounted body fits, and the completion asks again.
func TestMountingNeedsTheMountedBodyToFitAtAdmissionAndAtCompletion(t *testing.T) {
	t.Parallel()

	// A wall whose face is 0.4 blocks from the player's centre line: a tenth clear of the
	// walking body's side and a tenth inside the mounted one's.
	const playerX = 0.6
	beside := func(x, y, _ int64) bool { return x >= 1 && y >= 64 && y <= 67 }

	t.Run("a wall beside the walking body refuses admission", func(t *testing.T) {
		t.Parallel()

		h := newVitalsHarness(t, DefaultTickRate, builtOn(beside))
		player, _ := h.join(1, [3]float32{playerX, 64, 0.5})
		prepareMount(player, vnet.MountKindBlackHorse, true)
		// The walking body is clear of that wall, so the refusal below is about the body
		// the player is asking to become and nothing else.
		if embedded(h, player) {
			t.Fatal("the walking body overlaps the wall, so a refusal would prove nothing about the mounted one")
		}

		reason, err := player.Mount(vnet.MountKindBlackHorse)
		if err == nil || reason != vnet.RefusalReasonMountLowCeiling {
			t.Fatalf("Mount beside a wall: reason %s, error %v; want %s", reason, err, vnet.RefusalReasonMountLowCeiling)
		}
		if castRunning(player) {
			t.Error("a refused mount request started a cast")
		}
	})

	t.Run("a block placed beside the caster refuses the completion", func(t *testing.T) {
		t.Parallel()

		placed := false
		h := newVitalsHarness(t, DefaultTickRate, builtOn(func(x, y, z int64) bool {
			return placed && x == 1 && y == 64 && z == 0
		}))
		player, out := h.join(1, [3]float32{playerX, 64, 0.5})
		prepareMount(player, vnet.MountKindBlackHorse, true)
		if reason, err := player.Mount(vnet.MountKindBlackHorse); err != nil || reason != vnet.RefusalReasonUnknown {
			t.Fatalf("Mount in the open: reason %s, error %v", reason, err)
		}

		h.advance(int(h.sim.castTicks) / 2)
		placed = true // a neighbour puts stone down beside the caster's knee
		h.advance(int(h.sim.castTicks))

		if got := mountedKind(player); got != vnet.MountKindUnknown {
			t.Fatalf("the completion mounted a %s inside the block placed beside it", got)
		}
		if castRunning(player) {
			t.Error("the refused completion left the cast running")
		}
		if embedded(h, player) {
			t.Error("the player on foot overlaps the placed block, so the completion had a body to refuse for")
		}
		want := protocol.ActionRefused{Action: vnet.RefusedActionMount, Reason: vnet.RefusalReasonMountLowCeiling}
		if refusals := actionRefusals(t, out); len(refusals) != 1 || refusals[0] != want {
			t.Errorf("refusals = %+v, want exactly %+v", refusals, want)
		}
	})
}

// A dismount never needs an un-embed: the walking body lies inside the mounted one by
// construction, so wherever the horse fitted the walker fits. Said in the tightest
// place there is — a corridor exactly one block wide, which the mounted body fills
// flush against both faces.
func TestDismountingInAOneBlockCorridorMovesNobody(t *testing.T) {
	t.Parallel()

	corridor := func(x, y, _ int64) bool { return (x < 0 || x >= 1) && y >= 64 && y <= 67 }
	h := newVitalsHarness(t, DefaultTickRate, builtOn(corridor))
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	prepareMount(player, vnet.MountKindGreyHorse, true)
	if reason, err := player.Mount(vnet.MountKindGreyHorse); err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("Mount in a one-block corridor: reason %s, error %v", reason, err)
	}
	h.advance(int(h.sim.castTicks))
	if got := mountedKind(player); got != vnet.MountKindGreyHorse {
		t.Fatalf("mounted = %s, want GreyHorse", got)
	}
	before := h.position(player)

	player.Dismount()
	h.step()
	if got := h.position(player); got[0] != before[0] || got[2] != before[2] {
		t.Fatalf("dismounting moved the player from %v to %v", before, got)
	}
	if embedded(h, player) {
		t.Fatal("the walking body overlaps the corridor wall after the dismount")
	}

	// Free, not merely in place: the corridor is theirs to walk down.
	h.hold(player, protocol.PlayerInput{MoveZ: 1}, 10)
	after := h.position(player)
	if after[2] >= before[2] {
		t.Errorf("after dismounting the player could not walk down the corridor: z stayed at %.2f", after[2])
	}
	if after[0] != before[0] {
		t.Errorf("walking down the corridor moved the player sideways from x=%.2f to %.2f", before[0], after[0])
	}
}

// What aims at a player measures the body they have: a blow, a creature's notice and a
// projectile all reach a rider where they miss a walker.
func TestAMountedPlayerIsAimedAtAsTheMountedBody(t *testing.T) {
	t.Parallel()

	// The draugr's far face sits a tenth past the walking body's range, which puts it a
	// tenth inside the mounted body's: the mounted side is 0.2 nearer, and 0.1 < 0.2.
	const margin = 0.1
	if (MountedWidth-PlayerWidth)/2 <= margin {
		t.Fatalf("the mounted body is only %.2f wider per side, so a %.1f margin proves nothing", (MountedWidth-PlayerWidth)/2, margin)
	}
	playerPos := [3]float64{0.5, 64, 0.5}
	draugrAt := func(t *testing.T, rangeBlocks float64) [3]float32 {
		t.Helper()
		z := playerPos[2] - PlayerWidth/2 - (rangeBlocks + margin) - draugrRow.body.width/2
		pos := [3]float64{playerPos[0], playerPos[1], z}
		creature := draugrRow.body.boxAt(pos)
		if got := boxDistance(creature, playerBox(playerPos)); got <= rangeBlocks {
			t.Fatalf("the walking body is %.2f from the draugr, inside %.1f", got, rangeBlocks)
		}
		if got := boxDistance(creature, mountedBody.boxAt(playerPos)); got > rangeBlocks {
			t.Fatalf("the mounted body is %.2f from the draugr, outside %.1f", got, rangeBlocks)
		}
		return [3]float32{float32(pos[0]), float32(pos[1]), float32(pos[2])}
	}

	t.Run("a blow just past the walking body lands on the rider", func(t *testing.T) {
		t.Parallel()
		for _, mounted := range []bool{false, true} {
			h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
			player, _ := h.join(1, [3]float32{float32(playerPos[0]), float32(playerPos[1]), float32(playerPos[2])})
			if mounted {
				forceMounted(player, vnet.MountKindBlackHorse)
			}
			mobID := h.spawnDraugrAt(draugrAt(t, draugrRow.attackRange))
			armMobBlow(t, h, mobID, player)

			before := h.vitals(player).Health
			h.step()
			after := h.vitals(player).Health
			if mounted && after >= before {
				t.Errorf("the blow missed the rider: health %d -> %d", before, after)
			}
			if !mounted && after != before {
				t.Errorf("the blow reached the walker past its range: health %d -> %d", before, after)
			}
		}
	})

	t.Run("notice just past the walking body finds the rider", func(t *testing.T) {
		t.Parallel()
		for _, mounted := range []bool{false, true} {
			h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
			player, _ := h.join(1, [3]float32{float32(playerPos[0]), float32(playerPos[1]), float32(playerPos[2])})
			if mounted {
				forceMounted(player, vnet.MountKindBlackHorse)
			}
			mobID := h.spawnDraugrAt(draugrAt(t, draugrRow.aggroRange))

			h.sim.mu.Lock()
			target := h.sim.mobs[mobID].chooseTargetLocked(h.sim, []*Player{player})
			h.sim.mu.Unlock()
			if mounted && target != player {
				t.Error("the draugr did not notice the rider inside its aggro range")
			}
			if !mounted && target != nil {
				t.Error("the draugr noticed the walker past its aggro range")
			}
		}
	})

	t.Run("an orb two and a half blocks up hits the rider and misses the walker", func(t *testing.T) {
		t.Parallel()
		for _, mounted := range []bool{false, true} {
			h := newVitalsHarness(t, DefaultTickRate, emptyProjectileTerrain{})
			owner, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			target, _ := h.join(2, [3]float32{4.5, 64, 0.5})
			h.hurt(target, 20)
			if mounted {
				forceMounted(target, vnet.MountKindBlackHorse)
			}

			// Above the walking body's 1.8 and inside the mounted body's 2.8, flying level.
			h.sim.mu.Lock()
			orbID, ok := h.sim.spawnProjectileLocked(vnet.ProjectileKindEnergyOrb, owner, [3]float64{0.5, 64 + 2.5, 0.5}, [3]float64{1, 0, 0}, OrbSpeed)
			h.sim.mu.Unlock()
			if !ok {
				t.Fatal("the orb was refused")
			}
			for range 8 {
				advanceTestProjectiles(h)
			}

			_, live := projectileState(h, orbID)
			got := h.vitals(target).Health
			if mounted && (live || got != PlayerMaxHealth-20+OrbHeal) {
				t.Errorf("the orb passed the rider: live=%t health=%d, want spent and %d", live, got, PlayerMaxHealth-20+OrbHeal)
			}
			if !mounted && (!live || got != PlayerMaxHealth-20) {
				t.Errorf("the orb found the walker over their head: live=%t health=%d, want flying on and %d", live, got, PlayerMaxHealth-20)
			}
		}
	})
}

// Another player's placement is refused where it overlaps the mounted body — the
// voxel above a walker's head, the voxel beside a walker's shoulder — and not only
// the walking one. The check is the one Edit consults for every player in the world.
func TestPlacementIsRefusedInsideTheMountedBody(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	// Off the voxel's centre by a tenth, so the mounted body's outer side crosses into
	// x = 3 while the walking body's stays inside x = 2.
	player, _ := h.join(1, [3]float32{2.6, 64, 0.5})

	tests := []struct {
		name  string
		voxel [3]int64
	}{
		{"the voxel above the walking body's head", [3]int64{2, 66, 0}},
		{"the voxel beside the walking body's shoulder", [3]int64{3, 64, 0}},
	}
	for _, tc := range tests {
		player.Dismount()
		if id, held := h.sim.voxelHoldsAPlayer(tc.voxel); held {
			t.Errorf("%s is held by player %d on foot", tc.name, id)
		}
		forceMounted(player, vnet.MountKindBlackHorse)
		if id, held := h.sim.voxelHoldsAPlayer(tc.voxel); !held || id != player.entityID {
			t.Errorf("%s is not held by the mounted player: held=%t id=%d", tc.name, held, id)
		}
	}

	// And the voxel at the feet is held either way: the control that says the check
	// still reads a body at all.
	player.Dismount()
	if _, held := h.sim.voxelHoldsAPlayer([3]int64{2, 64, 0}); !held {
		t.Error("the voxel at the walking player's feet is not held")
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
