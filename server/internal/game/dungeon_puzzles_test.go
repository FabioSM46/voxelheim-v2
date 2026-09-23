package game

import (
	"bytes"
	"testing"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// puzzleDungeon is one live dungeon session of seed at rate, every chunk resident,
// with one player who receives every frame.
type puzzleDungeon struct {
	t      *testing.T
	s      *Sim
	p      *Player
	frames [][]byte
}

func newPuzzleDungeon(t *testing.T, seed int64, rate uint8, defeated []vnet.MobKind) *puzzleDungeon {
	t.Helper()
	manager := instanceTestManager(t, rate, 1)
	manager.mu.Lock()
	raw, err := manager.newSessionLocked(100, seed, InstanceRuin{}, defeated)
	manager.mu.Unlock()
	if err != nil {
		t.Fatal(err)
	}
	session := raw.snapshot()
	loadDungeon(t, session)
	d := &puzzleDungeon{t: t, s: session.Sim}
	d.p = &Player{entityID: 999, sim: d.s, lifeState: vnet.LifeStateAlive, health: 100, hunger: 100,
		deliver: func(frame []byte) bool { d.frames = append(d.frames, frame); return true }}
	d.s.players[d.p.entityID] = d.p
	return d
}

// mechanisms is the dungeon's mechanism anchors of one puzzle, in declaration order.
func (d *puzzleDungeon) mechanisms(puzzle int) []world.PlacedAnchor {
	var out []world.PlacedAnchor
	for _, a := range d.s.dungeon.gate.Mechanisms() {
		if a.Index == puzzle {
			out = append(out, a)
		}
	}
	return out
}

// use stands the player just under a cell and uses it.
func (d *puzzleDungeon) use(a world.PlacedAnchor) (vnet.RefusalReason, error) {
	d.p.pos = [3]float64{float64(a.X) + .5, float64(a.Y) - 1, float64(a.Z) + .5}
	return d.p.UseMechanism([3]int32{int32(a.X), int32(a.Y), int32(a.Z)})
}

func (d *puzzleDungeon) mustUse(a world.PlacedAnchor) {
	d.t.Helper()
	if reason, err := d.use(a); err != nil {
		d.t.Fatalf("using %+v refused as %s: %v", a, reason, err)
	}
}

// block is what the simulation's own terrain reads at a cell.
func (d *puzzleDungeon) block(a world.PlacedAnchor) world.Block {
	d.t.Helper()
	b, ok := d.s.terrain.Block(a.X, a.Y, a.Z)
	if !ok {
		d.t.Fatalf("cell %+v is not resident", a)
	}
	return b
}

// doorOpen reports whether every cell of a door reads as air, and fails on a door
// that is neither wholly open nor wholly shut.
func (d *puzzleDungeon) doorOpen(index int) bool {
	d.t.Helper()
	cells := d.s.dungeon.gate.DoorCells(index)
	air := 0
	for _, c := range cells {
		if d.block(c) == world.Air {
			air++
		}
	}
	if air != 0 && air != len(cells) {
		d.t.Fatalf("door %d is %d of %d open", index, air, len(cells))
	}
	return air == len(cells)
}

// updates is how many block updates arrived since the last call. Nothing else may
// have arrived, and the last update of each cell must name what the world holds there
// now.
func (d *puzzleDungeon) updates() int {
	d.t.Helper()
	last := map[world.PlacedAnchor]uint16{}
	for i, frame := range d.frames {
		env := vnet.GetRootAsEnvelope(frame, 0)
		var tab flatbuffers.Table
		if env.PayloadType() != vnet.PayloadBlockUpdate || !env.Payload(&tab) {
			d.t.Fatalf("frame %d is not a block update", i)
		}
		var u vnet.BlockUpdate
		u.Init(tab.Bytes, tab.Pos)
		pos := u.Pos(nil)
		last[world.PlacedAnchor{X: int64(pos.X()), Y: int64(pos.Y()), Z: int64(pos.Z())}] = u.BlockId()
	}
	for cell, block := range last {
		if got := d.block(cell); uint16(got) != block {
			d.t.Fatalf("the last update of %+v says %d, the world holds %d", cell, block, got)
		}
	}
	n := len(d.frames)
	d.frames = nil
	return n
}

func (d *puzzleDungeon) tick(n uint32) {
	for range n {
		d.s.advanceDungeonPuzzlesLocked()
	}
}

// The rune hall at every rotation: the seed's order lights one stone per press, a
// stone out of order darkens every lit one, and the fourth opens the door for good,
// after which every stone is locked.
func TestTheRuneStonesOpenTheirDoorOnlyInTheSeedsOrder(t *testing.T) {
	for _, seed := range []int64{0, 1, 2, 3, -7} {
		d := newPuzzleDungeon(t, seed, DefaultTickRate, nil)
		stones := d.mechanisms(world.RunePuzzle)
		order := world.InstanceRuneOrder(seed)
		lit := func() int {
			n := 0
			for _, s := range stones {
				if d.block(s) == world.RuneStoneLit {
					n++
				}
			}
			return n
		}
		// Two right, then a wrong one: all dark again.
		d.mustUse(stones[order[0]])
		d.mustUse(stones[order[1]])
		if lit() != 2 || d.updates() != 2 {
			t.Fatalf("seed %d: two presses in order lit %d", seed, lit())
		}
		d.mustUse(stones[order[3]])
		if lit() != 0 || d.updates() != 2 {
			t.Fatalf("seed %d: a stone out of order left %d lit", seed, lit())
		}
		// A lit stone pressed again is out of order too.
		d.mustUse(stones[order[0]])
		d.mustUse(stones[order[0]])
		if lit() != 0 {
			t.Fatalf("seed %d: pressing a lit stone again left %d lit", seed, lit())
		}
		d.updates()
		for k := range 3 {
			d.mustUse(stones[order[k]])
		}
		if d.doorOpen(world.RunePuzzle) {
			t.Fatalf("seed %d: the door opened on three stones", seed)
		}
		d.mustUse(stones[order[3]])
		if lit() != 4 || !d.doorOpen(world.RunePuzzle) {
			t.Fatalf("seed %d: four stones in order did not open the door", seed)
		}
		if n := d.updates(); n != 4+25 {
			t.Fatalf("seed %d: the solve sent %d updates, want four runes and 25 door cells", seed, n)
		}
		for _, s := range stones {
			if reason, err := d.use(s); reason != vnet.RefusalReasonMechanismLocked || err == nil {
				t.Fatalf("seed %d: a solved stone answered %s", seed, reason)
			}
		}
		if lit() != 4 || d.updates() != 0 {
			t.Fatalf("seed %d: a locked stone changed something", seed)
		}
	}
}

// The grille at two tick rates: open for exactly GrilleHold, then shut with its lever
// dropped; a second pull while it is open is locked.
func TestTheGrilleLeverHoldsTheGrilleOpenForTwelveSeconds(t *testing.T) {
	for _, rate := range []uint8{DefaultTickRate, 40} {
		d := newPuzzleDungeon(t, 1, rate, nil)
		lever := d.mechanisms(world.GrillePuzzle)[0]
		hold := ticksFor(GrilleHold, rate)
		if hold != 12*uint32(rate) {
			t.Fatalf("rate %d: the grille holds %d ticks", rate, hold)
		}
		d.mustUse(lever)
		if d.block(lever) != world.LeverOn || !d.doorOpen(world.GrillePuzzle) || d.updates() != 9+1 {
			t.Fatalf("rate %d: the pull did not open the grille", rate)
		}
		if reason, _ := d.use(lever); reason != vnet.RefusalReasonMechanismLocked {
			t.Fatalf("rate %d: pulling the raised lever answered %s", rate, reason)
		}
		d.tick(hold - 1)
		if !d.doorOpen(world.GrillePuzzle) || d.updates() != 0 {
			t.Fatalf("rate %d: the grille shut early", rate)
		}
		d.tick(1)
		if d.doorOpen(world.GrillePuzzle) || d.block(lever) != world.LeverOff || d.updates() != 9+1 {
			t.Fatalf("rate %d: the grille did not shut after %d ticks", rate, hold)
		}
		// And it can be pulled again.
		d.mustUse(lever)
		if !d.doorOpen(world.GrillePuzzle) {
			t.Fatalf("rate %d: the second pull did not open the grille", rate)
		}
	}
}

// The grille never shuts on a body: a player or a mob standing in one of its cells
// holds it open past its time, and it shuts on the first tick they are clear.
func TestTheGrilleNeverShutsOnABody(t *testing.T) {
	for _, who := range []string{"player", "mob"} {
		d := newPuzzleDungeon(t, 2, DefaultTickRate, nil)
		lever := d.mechanisms(world.GrillePuzzle)[0]
		cell := d.s.dungeon.gate.DoorCells(world.GrillePuzzle)[4] // the middle column's lowest cell
		d.mustUse(lever)
		standing := [3]float64{float64(cell.X) + .5, float64(cell.Y), float64(cell.Z) + .5}
		clear := func() {}
		switch who {
		case "player":
			other := &Player{entityID: 998, sim: d.s, lifeState: vnet.LifeStateAlive, pos: standing, deliver: func([]byte) bool { return true }}
			d.s.players[other.entityID] = other
			clear = func() { other.pos[1] += 10 }
		case "mob":
			id, made := d.s.spawnMobLocked(vnet.MobKindDraugr, standing)
			if !made {
				t.Fatal("no draugr")
			}
			clear = func() { delete(d.s.mobs, id) }
		}
		d.tick(ticksFor(GrilleHold, DefaultTickRate) + 100)
		if !d.doorOpen(world.GrillePuzzle) || d.block(lever) != world.LeverOn {
			t.Fatalf("%s: the grille shut on a body", who)
		}
		clear()
		d.tick(1)
		if d.doorOpen(world.GrillePuzzle) || d.block(lever) != world.LeverOff {
			t.Fatalf("%s: the grille stayed open once its cells were clear", who)
		}
	}
}

// The twin levers: each stays up for TwinLeverHold; the second pulled while the first
// is up opens the door for good, and one pulled after the other has dropped does not.
func TestTheTwinLeversOpenTheirDoorOnlyTogether(t *testing.T) {
	d := newPuzzleDungeon(t, 3, DefaultTickRate, nil)
	levers := d.mechanisms(world.TwinLeverPuzzle)
	hold := ticksFor(TwinLeverHold, DefaultTickRate)
	if hold != 10*uint32(DefaultTickRate) {
		t.Fatalf("a twin lever holds %d ticks", hold)
	}

	// Too slow: the first drops on its last tick, and the second alone opens nothing.
	d.mustUse(levers[0])
	if reason, _ := d.use(levers[0]); reason != vnet.RefusalReasonMechanismLocked {
		t.Fatalf("pulling a raised twin lever answered %s", reason)
	}
	d.tick(hold)
	if d.block(levers[0]) != world.LeverOff {
		t.Fatal("the first lever stayed up past its time")
	}
	d.mustUse(levers[1])
	if d.doorOpen(world.TwinLeverPuzzle) || d.block(levers[1]) != world.LeverOn {
		t.Fatal("one lever alone opened the door")
	}
	d.tick(hold)
	if d.block(levers[1]) != world.LeverOff {
		t.Fatal("the second lever stayed up past its time")
	}
	d.updates()

	// Together: the second pulled on the first's last up tick.
	d.mustUse(levers[1])
	d.tick(hold - 1)
	d.mustUse(levers[0])
	if !d.doorOpen(world.TwinLeverPuzzle) || d.block(levers[0]) != world.LeverOn || d.block(levers[1]) != world.LeverOn {
		t.Fatal("both levers up did not open the door")
	}
	if n := d.updates(); n != 1+1+9 {
		t.Fatalf("the solve sent %d updates", n)
	}
	d.tick(hold * 3)
	if !d.doorOpen(world.TwinLeverPuzzle) || d.block(levers[0]) != world.LeverOn || d.block(levers[1]) != world.LeverOn || d.updates() != 0 {
		t.Fatal("the solved door or its levers moved")
	}
	for _, l := range levers {
		if reason, _ := d.use(l); reason != vnet.RefusalReasonMechanismLocked {
			t.Fatalf("a solved twin lever answered %s", reason)
		}
	}
}

// Every refusal a use can meet, and none of them changes a block.
func TestAMechanismUseIsRefusedDeadOutOfReachOrOffAMechanism(t *testing.T) {
	d := newPuzzleDungeon(t, 0, DefaultTickRate, nil)
	lever := d.mechanisms(world.GrillePuzzle)[0]
	door := d.s.dungeon.gate.DoorCells(world.ReturnShortcutDoor)[0]
	d.p.pos = [3]float64{float64(lever.X) + .5, float64(lever.Y) - 1, float64(lever.Z) + 10.5}
	if reason, err := d.p.UseMechanism([3]int32{int32(lever.X), int32(lever.Y), int32(lever.Z)}); reason != vnet.RefusalReasonOutOfReach || err == nil {
		t.Fatalf("a lever ten blocks away answered %s", reason)
	}
	for _, cell := range []world.PlacedAnchor{door, d.s.dungeon.gate.DoorCells(world.GrillePuzzle)[0], {X: lever.X, Y: lever.Y - 1, Z: lever.Z}} {
		if reason, err := d.use(cell); reason != vnet.RefusalReasonNotAMechanism || err == nil {
			t.Fatalf("cell %+v answered %s", cell, reason)
		}
	}
	d.p.lifeState = vnet.LifeStateDead
	if reason, err := d.use(lever); reason != vnet.RefusalReasonPlayerIsDead || err == nil {
		t.Fatalf("a dead player's pull answered %s", reason)
	}
	if d.block(lever) != world.LeverOff || d.doorOpen(world.GrillePuzzle) || d.doorOpen(world.ReturnShortcutDoor) || d.updates() != 0 {
		t.Fatal("a refused use changed the world")
	}
}

// The return shortcut is shut until the king dies, whatever the puzzles do, and opens
// for good on his authoritative death; a dungeon restored after his death has it open
// from the start.
func TestTheReturnShortcutOpensOnlyWhenTheKingDies(t *testing.T) {
	d := newPuzzleDungeon(t, 1, DefaultTickRate, nil)
	s := d.s
	// Every mechanism used, and both permanent puzzles solved: the shortcut stays shut.
	order := world.InstanceRuneOrder(1)
	for _, k := range order {
		d.mustUse(d.mechanisms(world.RunePuzzle)[k])
	}
	d.mustUse(d.mechanisms(world.GrillePuzzle)[0])
	d.mustUse(d.mechanisms(world.TwinLeverPuzzle)[0])
	d.mustUse(d.mechanisms(world.TwinLeverPuzzle)[1])
	if d.doorOpen(world.ReturnShortcutDoor) {
		t.Fatal("a puzzle opened the return shortcut")
	}
	beast := s.mobs[s.dungeon.guardianID]
	s.damageMobLocked(beast, beast.health)
	if d.doorOpen(world.ReturnShortcutDoor) {
		t.Fatal("the guardian's death opened the return shortcut")
	}
	d.updates()
	king := s.mobs[s.dungeon.kingID]
	if !s.damageMobLocked(king, king.health) {
		t.Fatal("the king did not die")
	}
	if !d.doorOpen(world.ReturnShortcutDoor) || d.updates() != 12 {
		t.Fatal("the king's death did not open the return shortcut to everybody")
	}
	if reason, _ := d.use(s.dungeon.gate.DoorCells(world.ReturnShortcutDoor)[0]); reason != vnet.RefusalReasonNotAMechanism {
		t.Fatalf("a shortcut door cell answered %s", reason)
	}

	restored := newPuzzleDungeon(t, 1, DefaultTickRate, []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing})
	if !restored.doorOpen(world.ReturnShortcutDoor) {
		t.Fatal("a dungeon restored after the king's death has its shortcut shut")
	}
	if fresh := newPuzzleDungeon(t, 1, DefaultTickRate, []vnet.MobKind{vnet.MobKindVargrGuardian}); fresh.doorOpen(world.ReturnShortcutDoor) {
		t.Fatal("a dungeon restored before the king's death has its shortcut open")
	}
}

// Updates reach a player whose queue was full when they were made, in order and once
// each, and a player who joins after a change is sent only what changes after it.
func TestPuzzleUpdatesQueueBehindAFullOutboundQueue(t *testing.T) {
	d := newPuzzleDungeon(t, 0, DefaultTickRate, nil)
	var got [][]byte
	open := false
	d.p.deliver = func(frame []byte) bool {
		if !open {
			return false
		}
		got = append(got, frame)
		return true
	}
	lever := d.mechanisms(world.GrillePuzzle)[0]
	d.mustUse(lever)
	open = true
	late := &Player{entityID: 997, sim: d.s, lifeState: vnet.LifeStateAlive, deliver: func([]byte) bool { t.Fatal("a late joiner was sent an old change"); return true }}
	d.s.players[late.entityID] = late
	d.s.flushDungeonGateLocked()
	if len(got) != 10 {
		t.Fatalf("the backlog delivered %d of 10 updates", len(got))
	}
	want := protocol.EncodeBlockUpdate(protocol.BlockUpdate{Pos: [3]int32{int32(lever.X), int32(lever.Y), int32(lever.Z)}, BlockID: uint16(world.LeverOn)})
	if !bytes.Equal(got[len(got)-1], want) {
		t.Fatal("the lever's update is not the last of its pull")
	}
	if len(d.s.dungeon.pending) != 0 || d.s.dungeon.changes != nil {
		t.Fatal("a delivered backlog was kept")
	}
}
