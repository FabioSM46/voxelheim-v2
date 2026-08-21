package game_test

import (
	"context"
	"encoding/binary"
	"fmt"
	"math"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// testWorldSeed is the world these external tests build their simulations over. The
// value is arbitrary; sharing one is what keeps the spawn director's draws the same
// from test to test.
const testWorldSeed = 1337

// tolerance is how close a position has to be for a test to call it exact.
//
// A millimetre. Collision deliberately stops a hair short of the face it hit, so an
// assertion of literal equality would be asserting the size of that hair rather than
// the behaviour — see collisionSkin.
const tolerance = 1e-3

// The yaws that point the movement basis along an axis. yaw 0 looks along -Z with +X
// to its right, so these are the four cardinal directions a test can walk in.
const (
	yawNorth = 0.0           // -Z
	yawSouth = math.Pi       // +Z
	yawEast  = -math.Pi / 2  // +X
	yawWest  = math.Pi / 2   // -X
	forward  = float32(1.0)  // move_z at full intent
	backward = float32(-1.0) //nolint:unused // named for the reader; kept beside forward
)

// ---------------------------------------------------------------------------
// Terrain fixtures
//
// Plain functions of a coordinate, so a test states the world it needs in a line and
// the collision code is exercised against a shape nobody had to generate.
// ---------------------------------------------------------------------------

// flatWorld is solid at and below groundTop, air above. The top face of the surface
// is therefore at groundTop+1, which is where a player comes to rest.
type flatWorld struct{ groundTop int64 }

func (w flatWorld) Solid(_, y, _ int64) bool { return y <= w.groundTop }
func (w flatWorld) Block(x, y, z int64) (world.Block, bool) {
	if w.Solid(x, y, z) {
		return world.Stone, true
	}
	return world.Air, true
}

// thinFloor is a single solid layer with nothing above or below it. What a long fall
// has to be stopped by: three blocks of travel in one tick would step straight over
// it if the collision did not sub-step.
type thinFloor struct{ y int64 }

func (w thinFloor) Solid(_, y, _ int64) bool { return y == w.y }
func (w thinFloor) Block(x, y, z int64) (world.Block, bool) {
	if w.Solid(x, y, z) {
		return world.Stone, true
	}
	return world.Air, true
}

// wallWorld is a flat world with a solid slab from wallX eastwards.
type wallWorld struct {
	floor flatWorld
	wallX int64
}

func (w wallWorld) Solid(x, y, z int64) bool {
	return w.floor.Solid(x, y, z) || x >= w.wallX
}
func (w wallWorld) Block(x, y, z int64) (world.Block, bool) {
	if w.Solid(x, y, z) {
		return world.Stone, true
	}
	return world.Air, true
}

// ledgeWorld is high ground north of edgeZ and low ground south of it.
type ledgeWorld struct {
	edgeZ   int64
	highTop int64
	lowTop  int64
}

func (w ledgeWorld) Solid(_, y, z int64) bool {
	if z < w.edgeZ {
		return y <= w.highTop
	}
	return y <= w.lowTop
}
func (w ledgeWorld) Block(x, y, z int64) (world.Block, bool) {
	if w.Solid(x, y, z) {
		return world.Stone, true
	}
	return world.Air, true
}

// emptyWorld is all air. Nothing to stand on, which is the only way to observe an
// unimpeded fall.
type emptyWorld struct{}

func (emptyWorld) Solid(_, _, _ int64) bool { return false }
func (emptyWorld) Block(_, _, _ int64) (world.Block, bool) {
	return world.Air, true
}

// ---------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------

// sink stands in for a session's outbound queue.
//
// Guarded, because Sim.Step calls deliver from the tick goroutine while a test may be
// reading the frames from its own — which is exactly the arrangement `go test -race`
// is here to check.
type sink struct {
	mu     sync.Mutex
	frames [][]byte
	full   bool
}

func (s *sink) deliver(frame []byte) bool {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.full {
		return false
	}
	s.frames = append(s.frames, frame)
	return true
}

func (s *sink) count() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.frames)
}

func (s *sink) last() []byte {
	s.mu.Lock()
	defer s.mu.Unlock()

	if len(s.frames) == 0 {
		return nil
	}
	return s.frames[len(s.frames)-1]
}

// harness drives a simulation the way the server does: input from a client, then a
// tick from the loop.
type harness struct {
	t          *testing.T
	sim        *game.Sim
	tick       uint64
	clientTick uint32
}

func newHarness(t *testing.T, terrain game.Terrain) *harness {
	t.Helper()
	return newHarnessAt(t, terrain, game.DefaultTickRate, 8)
}

func newHarnessAt(t *testing.T, terrain game.Terrain, tickRate, viewDistance uint8) *harness {
	t.Helper()

	sim, err := game.NewSim(tickRate, viewDistance, testWorldSeed, terrain, refusingEditor{}, testEntityIDs(), discard())
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	return &harness{t: t, sim: sim}
}

// testEntityIDs is the identity source session.Registry provides in production: one
// counter shared by every entity, so no id ever names two things.
//
// It starts well above the identities these tests hand to Join, because those are
// chosen by hand — a counter starting at 1 would mint a drop called 1 beside a player
// called 1 and make a snapshot assertion ambiguous for a reason that has nothing to do
// with the code under test.
func testEntityIDs() func() uint64 {
	var next atomic.Uint64
	next.Store(100)
	return func() uint64 { return next.Add(1) }
}

// testPlayerID is a distinct identity per entity id.
//
// The simulation keys players by entity id and never by identity, so these only have
// to differ from one another and to be non-zero — Join refuses the zero id, which is
// the digest of nothing and names nobody. Derived from the entity id rather than
// minted so that a failing test names the same identity on every run.
func testPlayerID(entityID uint64) identity.PlayerID {
	var token identity.Token
	binary.LittleEndian.PutUint64(token[:8], entityID)
	return identity.IDOf(token)
}

func (h *harness) join(entityID uint64, pos [3]float32) (*game.Player, *sink) {
	h.t.Helper()

	out := &sink{}
	player, err := h.sim.Join(entityID, testPlayerID(entityID), pos, nil, out.deliver)
	if err != nil {
		h.t.Fatalf("Join: %v", err)
	}
	return player, out
}

// step advances one tick.
func (h *harness) step() {
	h.tick++
	h.sim.Step(h.tick)
}

// advance runs n ticks with no new input.
func (h *harness) advance(n int) {
	h.t.Helper()
	for range n {
		h.step()
	}
}

// hold submits `what` for p on every one of n ticks and steps the simulation between
// them — a client holding a control down, at the rate it is asked to send.
func (h *harness) hold(p *game.Player, what protocol.PlayerInput, n int) {
	h.t.Helper()

	for range n {
		h.submit(p, what)
		h.step()
	}
}

// submit hands one input to the simulation with the next client tick, and fails the
// test if it is refused.
func (h *harness) submit(p *game.Player, what protocol.PlayerInput) {
	h.t.Helper()

	h.clientTick++
	what.ClientTick = h.clientTick
	if err := p.Submit(what); err != nil {
		h.t.Fatalf("Submit: %v", err)
	}
}

// settle runs ticks until the player is standing on something, so a test about
// walking is not also a test about falling.
func (h *harness) settle(p *game.Player) game.PlayerState {
	h.t.Helper()

	for range 200 {
		h.step()
		if state := p.State(); state.OnGround {
			return state
		}
	}
	h.t.Fatalf("the player never reached the ground; it is at %v", p.State().Pos)
	return game.PlayerState{}
}

// walking is one tick of "hold forward, facing yaw".
func walking(yaw float64) protocol.PlayerInput {
	return protocol.PlayerInput{MoveZ: forward, Yaw: float32(yaw)}
}

// decodeSnapshot reads a snapshot frame the server produced. Free to use the
// generated accessors directly: unlike protocol.Decode, its input is not a client's
// choice.
func decodeSnapshot(t *testing.T, frame []byte) (uint32, []protocol.EntityState) {
	t.Helper()

	if frame == nil {
		t.Fatal("no snapshot was delivered")
	}

	env := vnet.GetRootAsEnvelope(frame, 0)
	if env.PayloadType() != vnet.PayloadEntitySnapshot {
		t.Fatalf("frame is %s, want %s", env.PayloadType(), vnet.PayloadEntitySnapshot)
	}

	var table flatbuffers.Table
	if !env.Payload(&table) {
		t.Fatal("the snapshot payload is absent")
	}
	snapshot := new(vnet.EntitySnapshot)
	snapshot.Init(table.Bytes, table.Pos)

	states := make([]protocol.EntityState, snapshot.EntitiesLength())
	for i := range states {
		var entity vnet.EntityState
		if !snapshot.Entities(&entity, i) {
			t.Fatalf("entity %d is missing from a vector of %d", i, len(states))
		}
		pos, vel := new(vnet.Vec3), new(vnet.Vec3)
		entity.Pos(pos)
		entity.Vel(vel)
		states[i] = protocol.EntityState{
			EntityID: entity.EntityId(),
			Pos:      [3]float32{pos.X(), pos.Y(), pos.Z()},
			Vel:      [3]float32{vel.X(), vel.Y(), vel.Z()},
			Yaw:      entity.Yaw(),
		}
	}
	return snapshot.ServerTick(), states
}

func entityIDs(states []protocol.EntityState) []uint64 {
	ids := make([]uint64, len(states))
	for i, state := range states {
		ids[i] = state.EntityID
	}
	return ids
}

func horizontalDistance(a, b [3]float32) float64 {
	return math.Hypot(float64(a[0]-b[0]), float64(a[2]-b[2]))
}

// ---------------------------------------------------------------------------
// Gravity, ground and ledges
// ---------------------------------------------------------------------------

func TestAPlayerFallsToTheSurfaceAndStaysOnIt(t *testing.T) {
	t.Parallel()

	// The surface's top face is at groundTop+1, which is where feet come to rest.
	const groundTop = 63
	h := newHarness(t, flatWorld{groundTop: groundTop})
	player, _ := h.join(1, [3]float32{0.5, 70, 0.5})

	landed := h.settle(player)
	if math.Abs(float64(landed.Pos[1])-(groundTop+1)) > tolerance {
		t.Fatalf("landed at y = %v, want the surface at %d", landed.Pos[1], groundTop+1)
	}
	if landed.Vel[1] != 0 {
		t.Errorf("vertical velocity after landing is %v, want 0", landed.Vel[1])
	}

	// And it stays there: gravity keeps pulling every tick, and every tick the ground
	// keeps answering. A resting player that drifts is the shape of a collision that
	// resolves by a fraction each time.
	h.advance(100)
	resting := player.State()
	if math.Abs(float64(resting.Pos[1])-float64(landed.Pos[1])) > tolerance {
		t.Errorf("a resting player drifted from %v to %v over 100 ticks", landed.Pos[1], resting.Pos[1])
	}
	if !resting.OnGround {
		t.Error("a player standing on the ground does not report being on it")
	}
}

// A long fall reaches terminal velocity, which is three blocks per tick at 20 Hz. A
// collision that only tested the destination of a whole tick's movement would step
// clean over a floor one block thick — so this is the tunnelling test, and it fails if
// the per-axis sub-stepping is removed.
//
// Swept over sub-block starting heights rather than dropped from one, and that is what
// makes it a test rather than a coincidence. Once terminal velocity is reached the
// position advances by exactly three blocks a tick, so the fractional part of y is
// fixed for the rest of the fall — and a single drop either happens to land a sample
// inside the floor or happens not to. Twenty offsets cover the whole cycle, so a
// version that jumps 3 blocks per test has to miss at least one of them.
func TestALongFallDoesNotTunnelThroughAThinFloor(t *testing.T) {
	t.Parallel()

	const floorY = 63

	// The premise: the naive version's step really is long enough to skip a
	// one-block floor, so there is something for the sweep to catch.
	perTick := game.TerminalFallSpeed / game.DefaultTickRate
	if gap := perTick - game.PlayerHeight; gap < 1 {
		t.Fatalf("a tick's fall is %v blocks and the player is %v tall, leaving a %v-block "+
			"gap: a whole-tick step could not skip a one-block floor, so this test proves nothing",
			perTick, game.PlayerHeight, gap)
	}

	for offset := range 20 {
		from := 400 + float32(offset)/20
		t.Run(fmt.Sprintf("from y=%v", from), func(t *testing.T) {
			t.Parallel()

			h := newHarness(t, thinFloor{y: floorY})
			player, _ := h.join(1, [3]float32{0.5, from, 0.5})

			landed := h.settle(player)
			if math.Abs(float64(landed.Pos[1])-(floorY+1)) > tolerance {
				t.Fatalf("a fall from %v ended at y = %v, want the floor at %d", from, landed.Pos[1], floorY+1)
			}
		})
	}
}

func TestAnUnimpededFallReachesTerminalVelocityAndNoMore(t *testing.T) {
	t.Parallel()

	h := newHarness(t, emptyWorld{})
	player, _ := h.join(1, [3]float32{0.5, 0, 0.5})

	h.advance(500)
	state := player.State()

	if got := float64(state.Vel[1]); math.Abs(got+game.TerminalFallSpeed) > tolerance {
		t.Errorf("fall speed settled at %v, want -%v", got, game.TerminalFallSpeed)
	}
	if state.OnGround {
		t.Error("a player falling through empty space reports being on the ground")
	}
}

func TestWalkingOffALedgeFallsAndLandsOnTheGroundBelow(t *testing.T) {
	t.Parallel()

	// High ground north of z = 0, an eight-block drop south of it.
	const (
		highTop = 63
		lowTop  = 55
	)
	h := newHarness(t, ledgeWorld{edgeZ: 0, highTop: highTop, lowTop: lowTop})

	player, _ := h.join(1, [3]float32{0.5, highTop + 3, -4.5})
	onTop := h.settle(player)
	if math.Abs(float64(onTop.Pos[1])-(highTop+1)) > tolerance {
		t.Fatalf("did not start on the high ground: y = %v", onTop.Pos[1])
	}

	// Walk south, over the edge and down.
	h.hold(player, walking(yawSouth), 120)
	landed := player.State()

	if landed.Pos[2] <= onTop.Pos[2] {
		t.Fatalf("the player did not walk south: z went from %v to %v", onTop.Pos[2], landed.Pos[2])
	}
	if math.Abs(float64(landed.Pos[1])-(lowTop+1)) > tolerance {
		t.Fatalf("landed at y = %v, want the low ground at %d", landed.Pos[1], lowTop+1)
	}
	if !landed.OnGround {
		t.Error("the player did not land")
	}
}

// ---------------------------------------------------------------------------
// Walls
// ---------------------------------------------------------------------------

func TestWalkingIntoAWallStopsAtItsFaceWithoutPenetrating(t *testing.T) {
	t.Parallel()

	const wallX = 5
	h := newHarness(t, wallWorld{floor: flatWorld{groundTop: 63}, wallX: wallX})
	player, _ := h.join(1, [3]float32{0.5, 67, 0.5})
	h.settle(player)

	// Long enough to cross the four blocks to the wall several times over.
	h.hold(player, walking(yawEast), 200)
	state := player.State()

	// The player is a box, not a point: the face that meets the wall is half a width
	// ahead of the position.
	leadingEdge := float64(state.Pos[0]) + game.PlayerWidth/2
	if leadingEdge > wallX {
		t.Errorf("the player's leading edge is at %v, inside the wall at %d", leadingEdge, wallX)
	}
	if wallX-leadingEdge > 0.01 {
		t.Errorf("the player stopped %v blocks short of the wall at %d", wallX-leadingEdge, wallX)
	}
	if state.Vel[0] != 0 {
		t.Errorf("horizontal velocity into a wall is %v, want 0", state.Vel[0])
	}
	if !state.OnGround {
		t.Error("a player pressed against a wall fell off the floor")
	}
}

// Walking diagonally into a wall must slide along it rather than stop dead. That is
// what resolving one axis at a time buys, and it is the reason the axes are not
// resolved together.
func TestWalkingDiagonallyIntoAWallSlidesAlongIt(t *testing.T) {
	t.Parallel()

	const wallX = 5
	h := newHarness(t, wallWorld{floor: flatWorld{groundTop: 63}, wallX: wallX})
	player, _ := h.join(1, [3]float32{0.5, 67, 0.5})
	start := h.settle(player)

	// Facing east, strafing south: into the wall and along it at the same time.
	h.hold(player, protocol.PlayerInput{MoveZ: forward, MoveX: forward, Yaw: float32(yawEast)}, 60)
	state := player.State()

	if leadingEdge := float64(state.Pos[0]) + game.PlayerWidth/2; leadingEdge > wallX {
		t.Errorf("the player's leading edge is at %v, inside the wall at %d", leadingEdge, wallX)
	}
	if state.Pos[2] <= start.Pos[2]+1 {
		t.Errorf("the player did not slide along the wall: z went from %v to %v", start.Pos[2], state.Pos[2])
	}
}

// ---------------------------------------------------------------------------
// Jumping
// ---------------------------------------------------------------------------

// jumpApex is the height a jump would reach with continuous integration: v²/2g.
//
// Derived from the constants rather than restated, because it is the *relationship*
// that has to hold. The discrete integrator at a finite tick rate reaches somewhat
// less than this, so it is the ceiling; one block is the floor, because a player who
// cannot step onto terrain cannot cross it.
func jumpApex() float64 {
	return game.JumpImpulse * game.JumpImpulse / (2 * game.Gravity)
}

func TestAJumpClearsOneBlockAndComesBackDown(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 67, 0.5})
	ground := h.settle(player)

	// One tick with the control held, then released — a hop rather than a hold, so the
	// apex measured is one jump's and not a bunny-hop's.
	h.submit(player, protocol.PlayerInput{Jump: true})
	h.step()
	if player.State().OnGround {
		t.Fatal("the jump did not leave the ground")
	}

	peak := float64(player.State().Pos[1])
	airborne := 1
	for range 200 {
		h.submit(player, protocol.PlayerInput{Jump: false})
		h.step()
		airborne++

		state := player.State()
		peak = max(peak, float64(state.Pos[1]))
		if state.OnGround {
			break
		}
	}

	state := player.State()
	if !state.OnGround {
		t.Fatalf("the player never came down; it is at y = %v", state.Pos[1])
	}

	height := peak - float64(ground.Pos[1])
	if height <= 1 {
		t.Errorf("the jump reached %v blocks, which cannot step onto a one-block rise", height)
	}
	if ceiling := jumpApex(); height > ceiling {
		t.Errorf("the jump reached %v blocks, above the %v the impulse and gravity allow", height, ceiling)
	}
	// A jump is a hop, not a flight. 2·v/g seconds in the air is 13 ticks at 20 Hz;
	// the bound is loose because the tick rate is a flag, and the point is the order
	// of magnitude.
	if airborne > 30 {
		t.Errorf("the jump lasted %d ticks, which is a flight rather than a hop", airborne)
	}
	if math.Abs(float64(state.Pos[1])-float64(ground.Pos[1])) > tolerance {
		t.Errorf("landed at y = %v, having taken off from %v", state.Pos[1], ground.Pos[1])
	}
}

// Whether a jump *happens* is the server's decision, and ground contact is the part
// of it a client cannot know. A client that holds jump in mid-air must not climb.
func TestJumpingInMidAirDoesNothing(t *testing.T) {
	t.Parallel()

	h := newHarness(t, emptyWorld{})
	player, _ := h.join(1, [3]float32{0.5, 200, 0.5})

	h.advance(5)
	before := player.State()

	h.hold(player, protocol.PlayerInput{Jump: true}, 40)
	after := player.State()

	if after.Pos[1] >= before.Pos[1] {
		t.Errorf("holding jump in mid-air moved the player from y = %v to %v", before.Pos[1], after.Pos[1])
	}
	if after.Vel[1] > 0 {
		t.Errorf("holding jump in mid-air produced an upward velocity of %v", after.Vel[1])
	}
}

// ---------------------------------------------------------------------------
// The speed clamp
// ---------------------------------------------------------------------------

// The acceptance criterion, and the reason the clamp is on the vector's magnitude
// rather than on each axis: forged axes far above 1 must produce exactly the
// displacement a legitimate maximum produces, in the same direction.
func TestForgedAxesMoveNoFurtherThanALegitimateMaximum(t *testing.T) {
	t.Parallel()

	const ticks = 60
	diagonal := float32(math.Sqrt2 / 2) // the longest legal (x, z) pair

	walk := func(moveX, moveZ float32) game.PlayerState {
		h := newHarness(t, flatWorld{groundTop: 63})
		player, _ := h.join(1, [3]float32{0.5, 67, 0.5})
		h.settle(player)
		h.hold(player, protocol.PlayerInput{MoveX: moveX, MoveZ: moveZ, Yaw: yawNorth}, ticks)
		return player.State()
	}

	legitimate := walk(diagonal, diagonal)
	forged := walk(1e6, 1e6)
	if forged.Pos != legitimate.Pos {
		t.Errorf("forged axes reached %v, a legitimate maximum reaches %v", forged.Pos, legitimate.Pos)
	}

	// And the length of that displacement is one tick's walk per tick, not √2 of it:
	// a per-axis clamp would let (1, 1) through as a vector of length 1.41.
	straight := walk(0, forward)
	origin := [3]float32{0.5, legitimate.Pos[1], 0.5}
	wantDistance := game.WalkSpeed * ticks / game.DefaultTickRate

	for name, state := range map[string]game.PlayerState{
		"straight ahead":           straight,
		"a legitimate diagonal":    legitimate,
		"axes forged at a million": forged,
	} {
		if got := horizontalDistance(state.Pos, origin); math.Abs(got-wantDistance) > 0.05 {
			t.Errorf("%s travelled %v blocks in %d ticks, want %v", name, got, ticks, wantDistance)
		}
	}
}

// The physics timestep is derived from the tick rate, so a faster server simulates the
// same world at a finer resolution rather than a faster one.
func TestTheTickRateChangesTheResolutionNotTheSpeed(t *testing.T) {
	t.Parallel()

	distanceAfterOneSecond := func(tickRate uint8) float64 {
		h := newHarnessAt(t, flatWorld{groundTop: 63}, tickRate, 8)
		player, _ := h.join(1, [3]float32{0.5, 67, 0.5})
		start := h.settle(player)
		h.hold(player, walking(yawNorth), int(tickRate))
		return horizontalDistance(player.State().Pos, start.Pos)
	}

	slow := distanceAfterOneSecond(20)
	fast := distanceAfterOneSecond(40)

	if math.Abs(slow-game.WalkSpeed) > 0.05 {
		t.Errorf("a 20 Hz server walked %v blocks in a second, want %v", slow, game.WalkSpeed)
	}
	if math.Abs(fast-slow) > 0.05 {
		t.Errorf("a 40 Hz server walked %v blocks in a second and a 20 Hz one walked %v", fast, slow)
	}
}

// ---------------------------------------------------------------------------
// Input the simulation refuses
// ---------------------------------------------------------------------------

// The acceptance criterion that a range clamp cannot satisfy. NaN compares false
// against every bound, so `if v > 1 { v = 1 }` passes it straight into the integrator
// and the position stays NaN for the rest of the session.
func TestNonFiniteInputIsRefusedAndLeavesThePositionFiniteAndUnchanged(t *testing.T) {
	t.Parallel()

	broken := map[string]protocol.PlayerInput{
		"NaN strafe":         {MoveX: float32(math.NaN())},
		"NaN forward":        {MoveZ: float32(math.NaN())},
		"NaN yaw":            {Yaw: float32(math.NaN())},
		"NaN pitch":          {Pitch: float32(math.NaN())},
		"+Inf forward":       {MoveZ: float32(math.Inf(1))},
		"-Inf forward":       {MoveZ: float32(math.Inf(-1))},
		"+Inf yaw":           {Yaw: float32(math.Inf(1))},
		"-Inf pitch":         {Pitch: float32(math.Inf(-1))},
		"NaN among the good": {MoveX: 0.5, MoveZ: float32(math.NaN()), Yaw: 1.0},
	}

	for name, input := range broken {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			h := newHarness(t, flatWorld{groundTop: 63})
			player, _ := h.join(1, [3]float32{0.5, 67, 0.5})
			before := h.settle(player)

			input.ClientTick = 1
			if err := player.Submit(input); err == nil {
				t.Fatal("the simulation accepted a non-finite input")
			}

			h.advance(20)
			after := player.State()

			for axis, value := range after.Pos {
				if math.IsNaN(float64(value)) || math.IsInf(float64(value), 0) {
					t.Fatalf("position axis %d is %v after a refused input", axis, value)
				}
			}
			if horizontalDistance(after.Pos, before.Pos) > tolerance {
				t.Errorf("a refused input moved the player from %v to %v", before.Pos, after.Pos)
			}
			if math.IsNaN(float64(after.Yaw)) || math.IsInf(float64(after.Yaw), 0) {
				t.Errorf("yaw is %v after a refused input", after.Yaw)
			}
		})
	}
}

// A refused input must not disturb the intent that was already accepted: rejection
// happens before anything is written, so a client with one bad frame keeps walking on
// the last good one rather than stopping.
func TestARefusedInputLeavesTheAcceptedIntentAlone(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 67, 0.5})
	h.settle(player)

	h.hold(player, walking(yawNorth), 5)
	moving := player.State()

	h.clientTick++
	if err := player.Submit(protocol.PlayerInput{
		ClientTick: h.clientTick,
		MoveZ:      float32(math.NaN()),
		Yaw:        yawNorth,
	}); err == nil {
		t.Fatal("the simulation accepted a NaN")
	}

	h.step()
	after := player.State()
	if after.Pos[2] >= moving.Pos[2] {
		t.Errorf("the player stopped walking north after a refused frame: z %v then %v", moving.Pos[2], after.Pos[2])
	}
}

func TestStaleInputIsDiscarded(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 67, 0.5})
	h.settle(player)

	// Walk north on tick 10.
	if err := player.Submit(protocol.PlayerInput{ClientTick: 10, MoveZ: forward, Yaw: yawNorth}); err != nil {
		t.Fatalf("Submit: %v", err)
	}

	for name, tick := range map[string]uint32{"the same tick again": 10, "an older tick": 9, "the first tick": 0} {
		if err := player.Submit(protocol.PlayerInput{ClientTick: tick, MoveZ: forward, Yaw: yawSouth}); err == nil {
			t.Errorf("%s (%d) was accepted after tick 10", name, tick)
		}
	}

	// The intent from tick 10 is the one that stands: south would have been the
	// opposite direction.
	h.advance(10)
	if player.State().Pos[2] >= 0.5 {
		t.Errorf("the player moved south, so a stale input was applied: z = %v", player.State().Pos[2])
	}

	if err := player.Submit(protocol.PlayerInput{ClientTick: 11, MoveZ: forward, Yaw: yawSouth}); err != nil {
		t.Errorf("tick 11 was refused after tick 10: %v", err)
	}
}

// client_tick is a uint32 and a session can outlive it: at 20 Hz it wraps after about
// seven years. Comparing the difference as a signed value is what puts 0 immediately
// after 0xFFFFFFFF instead of four billion ticks before it — without which a session
// alive across the wrap would discard every input for ever.
func TestTheClientTickCounterMayWrap(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 67, 0.5})

	if err := player.Submit(protocol.PlayerInput{ClientTick: math.MaxUint32}); err != nil {
		t.Fatalf("Submit: %v", err)
	}
	if err := player.Submit(protocol.PlayerInput{ClientTick: 0}); err != nil {
		t.Errorf("the tick after 0xFFFFFFFF was refused as stale: %v", err)
	}
	if err := player.Submit(protocol.PlayerInput{ClientTick: math.MaxUint32}); err == nil {
		t.Error("0xFFFFFFFF was accepted again after the wrap")
	}
}

// A client that stops sending stops moving. PlayerInput describes the state of the
// controls, so it persists across a dropped frame — but "still held" stops being a
// fair reading of silence, and a client that closed its send loop must not walk to the
// horizon.
func TestAnIntentNobodyRefreshesStopsBeingApplied(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 67, 0.5})
	h.settle(player)

	h.hold(player, walking(yawNorth), 5)

	// One tick of silence must not stop them: a dropped frame is not a released key.
	moving := player.State()
	h.advance(1)
	if player.State().Pos[2] >= moving.Pos[2] {
		t.Error("one missing frame stopped the player mid-stride")
	}

	// Half a second of it does.
	h.advance(int(game.DefaultTickRate))
	stopped := player.State()
	h.advance(20)
	if horizontalDistance(player.State().Pos, stopped.Pos) > tolerance {
		t.Errorf("a silent client kept walking: %v then %v", stopped.Pos, player.State().Pos)
	}
	if got := player.State().Yaw; math.Abs(float64(got)-yawNorth) > tolerance {
		t.Errorf("a silent client was turned to face %v; facing is not a control that decays", got)
	}
}

// ---------------------------------------------------------------------------
// Terrain that has not arrived
// ---------------------------------------------------------------------------

// A tick may not wait for terrain, so a chunk that is not resident has to answer one
// of "solid" or "air" — and "air" drops the player out of a world that is merely still
// loading. This is that rule end to end, through the real cache.
func TestAPlayerOverANonResidentChunkDoesNotFallThroughTheWorld(t *testing.T) {
	t.Parallel()

	const seed = 424242
	chunks := world.NewCache(seed, 2, 256)
	h := newHarness(t, game.NewCacheTerrain(chunks))

	spawn := world.SpawnAt(seed)
	player, _ := h.join(1, spawn)

	// Nothing has been generated: Peek answers "not resident" for every voxel.
	h.advance(200)
	waiting := player.State()

	if waiting.Pos != spawn {
		t.Errorf("the player moved to %v while its terrain had not arrived; it spawned at %v", waiting.Pos, spawn)
	}
	if waiting.Vel != [3]float32{} {
		t.Errorf("a player waiting for terrain accumulated velocity %v", waiting.Vel)
	}
	if !waiting.OnGround {
		t.Error("a player waiting for terrain is not on the ground, so it will arrive with ten seconds of fall speed")
	}

	// Now the terrain arrives, the way streaming delivers it.
	center := world.ContainingChunk(spawn[0], spawn[1], spawn[2])
	for y := center.Y - 1; y <= center.Y+1; y++ {
		for z := center.Z - 1; z <= center.Z+1; z++ {
			for x := center.X - 1; x <= center.X+1; x++ {
				if _, _, err := chunks.Get(context.Background(), world.Coord{X: x, Y: y, Z: z}); err != nil {
					t.Fatalf("generate chunk %d,%d,%d: %v", x, y, z, err)
				}
			}
		}
	}

	// Ticked rather than settled: a frozen player already reports being on the ground,
	// so "wait until OnGround" would return before it had moved at all.
	h.advance(60)
	landed := player.State()

	surface := world.HeightAt(seed, 0, 0)
	if math.Abs(float64(landed.Pos[1])-float64(surface+1)) > tolerance {
		t.Errorf("landed at y = %v, want the surface at %d", landed.Pos[1], surface+1)
	}
	if landed.Pos[1] >= waiting.Pos[1] {
		t.Errorf("the player did not fall once its terrain arrived: %v then %v", waiting.Pos[1], landed.Pos[1])
	}
	if !landed.OnGround {
		t.Error("the player is not resting on the terrain that arrived")
	}
}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

func TestASnapshotIsDeliveredOncePerTick(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 67, 0.5})

	if got := out.count(); got != 0 {
		t.Fatalf("joining delivered %d snapshots; a tick is what produces one", got)
	}

	h.advance(5)
	if got := out.count(); got != 5 {
		t.Fatalf("five ticks delivered %d snapshots, want 5", got)
	}

	tick, states := decodeSnapshot(t, out.last())
	if tick != 5 {
		t.Errorf("the fifth snapshot says tick %d", tick)
	}
	if len(states) != 1 {
		t.Fatalf("a lone player sees %d entities, want just itself", len(states))
	}

	state := player.State()
	if states[0].EntityID != player.EntityID() {
		t.Errorf("the snapshot names entity %d, want %d", states[0].EntityID, player.EntityID())
	}
	if states[0].Pos != state.Pos {
		t.Errorf("the snapshot carries %v, the simulation says %v", states[0].Pos, state.Pos)
	}
	for axis, value := range states[0].Pos {
		if math.IsNaN(float64(value)) || math.IsInf(float64(value), 0) {
			t.Errorf("the snapshot's position axis %d is %v; the server must never emit one", axis, value)
		}
	}
}

func TestASnapshotReachesOnlyTheSessionsThatCanSeeTheEntity(t *testing.T) {
	t.Parallel()

	// Radius 1, so two players three chunks apart are certainly out of view.
	h := newHarnessAt(t, flatWorld{groundTop: 63}, game.DefaultTickRate, 1)

	near, nearOut := h.join(1, [3]float32{0.5, 67, 0.5})
	alsoNear, alsoNearOut := h.join(2, [3]float32{4.5, 67, 4.5})
	far, farOut := h.join(3, [3]float32{world.ChunkSize*3 + 0.5, 67, 0.5})

	h.advance(1)

	for _, tc := range []struct {
		name string
		out  *sink
		want []uint64
	}{
		{"the first neighbour", nearOut, []uint64{near.EntityID(), alsoNear.EntityID()}},
		{"the second neighbour", alsoNearOut, []uint64{near.EntityID(), alsoNear.EntityID()}},
		{"the distant player", farOut, []uint64{far.EntityID()}},
	} {
		_, states := decodeSnapshot(t, tc.out.last())
		got := entityIDs(states)
		if len(got) != len(tc.want) {
			t.Errorf("%s sees %v, want %v", tc.name, got, tc.want)
			continue
		}
		for i := range got {
			if got[i] != tc.want[i] {
				t.Errorf("%s sees %v, want %v", tc.name, got, tc.want)
				break
			}
		}
	}
}

// Two players in view of each other must each see the other move, which is the whole
// point of the fan-out: the second player's capsule is driven by the first player's
// authoritative position and by nothing the first client claimed.
func TestOnePlayerSeesAnotherWalk(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})
	watcher, watcherOut := h.join(1, [3]float32{0.5, 67, 0.5})
	walker, _ := h.join(2, [3]float32{2.5, 67, 0.5})
	h.settle(watcher)

	positionOf := func(id uint64) [3]float32 {
		t.Helper()
		_, states := decodeSnapshot(t, watcherOut.last())
		for _, state := range states {
			if state.EntityID == id {
				return state.Pos
			}
		}
		t.Fatalf("entity %d is missing from the watcher's snapshot", id)
		return [3]float32{}
	}

	before := positionOf(walker.EntityID())
	h.hold(walker, walking(yawNorth), 40)
	after := positionOf(walker.EntityID())

	if after[2] >= before[2] {
		t.Errorf("the watcher did not see the walker move north: z %v then %v", before[2], after[2])
	}
	if watcherStill := watcher.State().Pos; watcherStill[2] != 0.5 {
		t.Errorf("the watcher moved to %v on somebody else's input", watcherStill)
	}
}

func TestLeavingStopsTheSnapshots(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 67, 0.5})

	h.advance(3)
	delivered := out.count()

	h.sim.Leave(player)
	if got := h.sim.Count(); got != 0 {
		t.Errorf("the simulation still holds %d players after the only one left", got)
	}

	h.advance(3)
	if got := out.count(); got != delivered {
		t.Errorf("a session that left received %d more snapshots", got-delivered)
	}

	// Idempotent, because teardown races a shutdown that may already have run.
	h.sim.Leave(player)
	h.sim.Leave(nil)
}

// A session whose queue is full loses the snapshot rather than stalling the tick: a
// snapshot describes one tick and is worthless by the time a full queue drains.
func TestAFullQueueDropsTheSnapshotAndTheTickCarriesOn(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})
	slow, slowOut := h.join(1, [3]float32{0.5, 67, 0.5})
	_, fastOut := h.join(2, [3]float32{1.5, 67, 0.5})

	slowOut.mu.Lock()
	slowOut.full = true
	slowOut.mu.Unlock()

	h.advance(5)

	if got := slowOut.count(); got != 0 {
		t.Errorf("a full queue accepted %d snapshots", got)
	}
	if got := fastOut.count(); got != 5 {
		t.Errorf("the other session received %d snapshots, want 5 — one slow client must not cost everyone a tick", got)
	}
	// A session that cannot be told about the world is still in it: dropping a frame
	// must not drop the player out of the simulation.
	if state := slow.State(); state.Pos[1] >= 67 {
		t.Errorf("the slow client's player was not advanced; it is still at y = %v", state.Pos[1])
	}
}

func TestJoinRefusesWhatItCannotSimulate(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})

	if _, err := h.sim.Join(1, testPlayerID(1), [3]float32{0, 64, 0}, nil, nil); err == nil {
		t.Error("a nil deliver was accepted; there would be nowhere to put a snapshot")
	}
	if _, err := h.sim.Join(1, testPlayerID(1), [3]float32{0, float32(math.NaN()), 0}, nil, func([]byte) bool { return true }); err == nil {
		t.Error("a NaN spawn was accepted")
	}
	// The zero player id is the digest of nothing and names nobody, so a simulation
	// that accepted it would hold two players who are the same person the moment
	// anything keys a record on them.
	if _, err := h.sim.Join(1, identity.PlayerID{}, [3]float32{0, 64, 0}, nil, func([]byte) bool { return true }); err == nil {
		t.Error("a player joined under no identity at all")
	}

	joined, err := h.sim.Join(7, testPlayerID(7), [3]float32{0, 64, 0}, nil, func([]byte) bool { return true })
	if err != nil {
		t.Fatalf("Join: %v", err)
	}
	// Both names, side by side, because they answer different questions: the entity id
	// names this session and the player id names the person across all of them.
	if got := joined.PlayerID(); got != testPlayerID(7) {
		t.Errorf("PlayerID = %s, want the identity it joined under", got)
	}
	if got := joined.EntityID(); got != 7 {
		t.Errorf("EntityID = %d, want 7", got)
	}
	if _, err := h.sim.Join(7, testPlayerID(7), [3]float32{0, 64, 0}, nil, func([]byte) bool { return true }); err == nil {
		t.Error("the same entity id joined twice; the first session's handle would be orphaned")
	}
}

func TestNewSimRejectsBadArguments(t *testing.T) {
	t.Parallel()

	if _, err := game.NewSim(0, 3, testWorldSeed, flatWorld{}, refusingEditor{}, testEntityIDs(), discard()); err == nil {
		t.Error("a tick rate of 0 was accepted, and the timestep is derived from it")
	}
	if _, err := game.NewSim(20, 3, testWorldSeed, nil, refusingEditor{}, testEntityIDs(), discard()); err == nil {
		t.Error("a nil terrain was accepted")
	}
	if _, err := game.NewSim(20, 3, testWorldSeed, flatWorld{}, nil, testEntityIDs(), discard()); err == nil {
		t.Error("a nil editor was accepted; a simulation that cannot change its world looks exactly like one whose edit rules are broken")
	}
	if _, err := game.NewSim(20, 3, testWorldSeed, flatWorld{}, refusingEditor{}, nil, discard()); err == nil {
		t.Error("a nil identity source was accepted; a simulation counting for itself would name a drop with a live player's id")
	}
	if _, err := game.NewSim(20, 3, testWorldSeed, flatWorld{}, refusingEditor{}, testEntityIDs(), nil); err == nil {
		t.Error("a nil logger was accepted")
	}
}

// ---------------------------------------------------------------------------
// The streaming seam
// ---------------------------------------------------------------------------

func TestTheChunkFeedPublishesTheSpawnAndThenOnlyBorderCrossings(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})
	spawn := [3]float32{0.5, 67, 0.5}
	player, _ := h.join(1, spawn)

	// The spawn coordinate is waiting before the streaming goroutine ever starts, so
	// the join view goes out without a tick having to happen first.
	first, err := player.NextChunk(context.Background())
	if err != nil {
		t.Fatalf("NextChunk: %v", err)
	}
	if want := world.ContainingChunk(spawn[0], spawn[1], spawn[2]); first != want {
		t.Fatalf("the first coordinate is %+v, want the spawn chunk %+v", first, want)
	}

	// Standing still publishes nothing. Twenty times a second of "the view has not
	// moved" would have the streamer re-diff a 343-chunk set for nothing.
	h.settle(player)
	h.advance(40)
	if coord, err := nextChunkWithin(player, 50*time.Millisecond); err == nil {
		t.Fatalf("standing still published %+v", coord)
	}

	// Walking across a border publishes the chunk the server put the player in.
	h.hold(player, walking(yawSouth), 400)
	crossed, err := nextChunkWithin(player, 2*time.Second)
	if err != nil {
		t.Fatalf("crossing a chunk border published nothing: %v", err)
	}

	state := player.State()
	if want := world.ContainingChunk(state.Pos[0], state.Pos[1], state.Pos[2]); crossed != want {
		t.Errorf("published %+v, but the player is in %+v", crossed, want)
	}
	if crossed == first {
		t.Errorf("the player walked %v blocks south and is still in chunk %+v", state.Pos[2], crossed)
	}
}

// The invariant from server/AGENTS.md, and the reason it is stated there: a select
// whose cases are both ready picks one at random, so a cancelled context with a
// coordinate waiting would be honoured about half the time. Run enough times that
// "about half" cannot pass.
func TestNextChunkHonoursAnAlreadyCancelledContext(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})

	for i := range 500 {
		player, _ := h.join(uint64(i+1), [3]float32{0.5, 67, 0.5})

		ctx, cancel := context.WithCancel(context.Background())
		cancel()

		if _, err := player.NextChunk(ctx); err == nil {
			t.Fatalf("attempt %d handed out a coordinate on a cancelled context", i)
		}
		h.sim.Leave(player)
	}
}

// nextChunkWithin is NextChunk with a deadline, so a test can assert that nothing was
// published without hanging when nothing is.
func nextChunkWithin(player *game.Player, patience time.Duration) (world.Coord, error) {
	ctx, cancel := context.WithTimeout(context.Background(), patience)
	defer cancel()
	return player.NextChunk(ctx)
}

// ---------------------------------------------------------------------------
// Concurrency
// ---------------------------------------------------------------------------

// What `go test -race` is for. Two clients submit input from their own goroutines
// while the tick loop steps and a third reader asks for state — which is exactly the
// arrangement the server runs: one goroutine per session, one for the loop.
func TestInputAndTicksFromDifferentGoroutinesAreRaceFree(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})
	first, firstOut := h.join(1, [3]float32{0.5, 67, 0.5})
	second, secondOut := h.join(2, [3]float32{2.5, 67, 0.5})

	stop := make(chan struct{})
	var senders sync.WaitGroup

	for i, player := range []*game.Player{first, second} {
		senders.Add(1)
		go func() {
			defer senders.Done()

			yaw := float32(yawNorth)
			if i == 1 {
				yaw = float32(yawSouth)
			}
			for tick := uint32(1); ; tick++ {
				select {
				case <-stop:
					return
				default:
				}
				// Refusals are fine and expected — the counters race the tick loop.
				_ = player.Submit(protocol.PlayerInput{ClientTick: tick, MoveZ: forward, Yaw: yaw, Jump: tick%20 == 0})
				_ = player.State()
			}
		}()
	}

	for range 400 {
		h.step()
	}
	close(stop)
	senders.Wait()

	if firstOut.count() != 400 || secondOut.count() != 400 {
		t.Errorf("snapshots delivered: %d and %d, want 400 each", firstOut.count(), secondOut.count())
	}
	for _, player := range []*game.Player{first, second} {
		state := player.State()
		for axis, value := range state.Pos {
			if math.IsNaN(float64(value)) || math.IsInf(float64(value), 0) {
				t.Errorf("entity %d position axis %d is %v", state.EntityID, axis, value)
			}
		}
	}
}
