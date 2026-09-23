package game

import (
	"errors"
	"fmt"
	"math"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The first dungeon's three puzzles and its return shortcut.
//
// **A mechanism is a block, and every state it has is a block too.** A lever up or
// down, a rune dark or lit and a door open or shut are cells the instance gate patches
// into every composition, so a use that succeeds is answered by the `BlockUpdate`s it
// produces and by nothing else, and an eviction or a regeneration shows exactly the
// state this file last set. Nothing here is persisted: a solved puzzle belongs to this
// instance's lifetime (keeping one across a restart is the persistence issue's).
//
//   - The rune hall ([world.RunePuzzle]): four stones pressed in the seed's order,
//     which the inscription over the door draws. The right next stone lights; any other
//     stone darkens every lit one; the fourth lights the door open for good.
//   - The grille ([world.GrillePuzzle]): the lever holds it open for [GrilleHold], and
//     it shuts only once nobody — player or mob — stands in one of its cells. The lever
//     drops as the grille shuts.
//   - The twin levers ([world.TwinLeverPuzzle]): each stays up for [TwinLeverHold]; the
//     second pulled while the first is still up opens the door for good, and both stay
//     up.
//   - The return shortcut ([world.ReturnShortcutDoor]) is no puzzle: no mechanism
//     names it, and only the Draugr king's death opens it.
//
// **A lever that is up and a puzzle that is solved answer MechanismLocked.** A lit rune
// is not locked: pressing one again is a stone out of order, and resets the row.
const (
	// GrilleHold is how long one pull holds the grille open.
	GrilleHold = 12 * time.Second
	// TwinLeverHold is how long each of the twin levers stays up once pulled.
	TwinLeverHold = 10 * time.Second
)

// mechanismRef names one mechanism: its puzzle, and its position among that puzzle's
// mechanism anchors.
type mechanismRef struct{ puzzle, ordinal int }

// dungeonPuzzles is the mechanisms' state. Every field is guarded by Sim.mu; the
// cells themselves are the gate's.
type dungeonPuzzles struct {
	gate       *world.InstanceGate
	mechanisms map[[3]int64]mechanismRef
	cells      map[int][]world.PlacedAnchor // mechanism anchors by puzzle

	order [4]int // the rune stones by the position they are pressed in
	lit   int    // how many of order are lit

	grilleCells []world.PlacedAnchor
	grilleOpen  bool
	grilleLeft  uint32 // ticks until the grille tries to shut

	twinLeft [2]uint32 // ticks each twin lever stays up; zero is down

	grilleHold, twinHold uint32
}

// errMechanismLocked is a use of a lever that is already up or of a solved puzzle.
var errMechanismLocked = errors.New("the mechanism is locked")

// placeDungeonPuzzles takes over the dungeon's mechanisms. Construction only, after
// placeDungeonEncounters and before the manager publishes this session: a king
// already defeated opens the return shortcut here, before anybody can see it shut.
func (s *Sim) placeDungeonPuzzles(seed int64) error {
	d := s.dungeon
	if d == nil {
		return errors.New("game: dungeon puzzles need the dungeon's encounters first")
	}
	rate := uint8(math.Round(1 / s.dt))
	p := &dungeonPuzzles{
		gate:        d.gate,
		mechanisms:  make(map[[3]int64]mechanismRef),
		cells:       make(map[int][]world.PlacedAnchor),
		order:       world.InstanceRuneOrder(seed),
		grilleCells: d.gate.DoorCells(world.GrillePuzzle),
		grilleHold:  ticksFor(GrilleHold, rate),
		twinHold:    ticksFor(TwinLeverHold, rate),
	}
	for _, a := range d.gate.Mechanisms() {
		p.mechanisms[[3]int64{a.X, a.Y, a.Z}] = mechanismRef{puzzle: a.Index, ordinal: len(p.cells[a.Index])}
		p.cells[a.Index] = append(p.cells[a.Index], a)
	}
	for puzzle, want := range map[int]int{world.RunePuzzle: 4, world.GrillePuzzle: 1, world.TwinLeverPuzzle: 2} {
		if got := len(p.cells[puzzle]); got != want {
			return fmt.Errorf("game: dungeon puzzle %d has %d mechanisms, want %d", puzzle, got, want)
		}
	}
	d.puzzles = p
	if d.progress.king {
		d.gate.Update(world.InstanceUpdate{Doors: map[int]bool{world.ReturnShortcutDoor: true}})
	}
	return nil
}

// UseMechanism is a player pulling a lever or touching a rune stone at pos. It answers
// with a refusal reason and an error when the use changes nothing; a use that succeeds
// is answered by the block updates it causes.
//
// Dead first, then whether the cell is a mechanism at all, then reach — the same reach
// and the same measure a station's anchor is held to — and last whether the mechanism
// can move.
func (p *Player) UseMechanism(pos [3]int32) (vnet.RefusalReason, error) {
	s := p.sim
	s.mu.Lock()
	defer s.mu.Unlock()

	if err := p.cannotActLocked(); err != nil {
		return vnet.RefusalReasonPlayerIsDead, err
	}
	cell := [3]int64{int64(pos[0]), int64(pos[1]), int64(pos[2])}
	var ref mechanismRef
	known := false
	if s.dungeon != nil && s.dungeon.puzzles != nil {
		ref, known = s.dungeon.puzzles.mechanisms[cell]
	}
	if !known {
		return vnet.RefusalReasonNotAMechanism, fmt.Errorf("no mechanism stands at %v", pos)
	}
	if reach, distance := p.reachLocked(), distanceToVoxel(p.box(), cell); distance > reach {
		return vnet.RefusalReasonOutOfReach, fmt.Errorf("the mechanism is %.2f blocks from the player, past the reach of %.1f", distance, reach)
	}
	changed, err := s.dungeon.puzzles.use(ref)
	if err != nil {
		return vnet.RefusalReasonMechanismLocked, err
	}
	s.announceDungeonCellsLocked(changed)
	return vnet.RefusalReasonUnknown, nil
}

// use applies one accepted use and returns the cells it changed.
func (p *dungeonPuzzles) use(ref mechanismRef) ([]world.InstanceCell, error) {
	if p.gate.DoorOpen(ref.puzzle) {
		return nil, errMechanismLocked
	}
	switch ref.puzzle {
	case world.RunePuzzle:
		return p.pressRune(ref.ordinal), nil
	case world.GrillePuzzle:
		if p.grilleOpen {
			return nil, errMechanismLocked
		}
		p.grilleOpen, p.grilleLeft = true, p.grilleHold
		return p.gate.Update(world.InstanceUpdate{
			Doors:      map[int]bool{world.GrillePuzzle: true},
			Mechanisms: []world.InstanceCell{p.showing(world.GrillePuzzle, 0, world.LeverOn)},
		}), nil
	case world.TwinLeverPuzzle:
		if p.twinLeft[ref.ordinal] > 0 {
			return nil, errMechanismLocked
		}
		other := 1 - ref.ordinal
		if p.twinLeft[other] > 0 {
			p.twinLeft = [2]uint32{}
			return p.gate.Update(world.InstanceUpdate{
				Doors:      map[int]bool{world.TwinLeverPuzzle: true},
				Mechanisms: []world.InstanceCell{p.showing(world.TwinLeverPuzzle, ref.ordinal, world.LeverOn)},
			}), nil
		}
		p.twinLeft[ref.ordinal] = p.twinHold
		return p.gate.Update(world.InstanceUpdate{
			Mechanisms: []world.InstanceCell{p.showing(world.TwinLeverPuzzle, ref.ordinal, world.LeverOn)},
		}), nil
	}
	return nil, fmt.Errorf("puzzle %d has no rule", ref.puzzle)
}

// pressRune lights the stone when it is the next in the order, opening the door with
// the fourth, and otherwise darkens every stone.
func (p *dungeonPuzzles) pressRune(stone int) []world.InstanceCell {
	if p.order[p.lit] == stone {
		p.lit++
		u := world.InstanceUpdate{Mechanisms: []world.InstanceCell{p.showing(world.RunePuzzle, stone, world.RuneStoneLit)}}
		if p.lit == len(p.order) {
			u.Doors = map[int]bool{world.RunePuzzle: true}
		}
		return p.gate.Update(u)
	}
	p.lit = 0
	var u world.InstanceUpdate
	for ordinal := range p.cells[world.RunePuzzle] {
		u.Mechanisms = append(u.Mechanisms, p.showing(world.RunePuzzle, ordinal, world.RuneStone))
	}
	return p.gate.Update(u)
}

// showing is one mechanism cell of a puzzle showing block.
func (p *dungeonPuzzles) showing(puzzle, ordinal int, block world.Block) world.InstanceCell {
	a := p.cells[puzzle][ordinal]
	return world.InstanceCell{X: a.X, Y: a.Y, Z: a.Z, Block: block}
}

// advance runs one tick of the timed mechanisms and returns the cells it changed.
func (p *dungeonPuzzles) advance(occupied func(cells []world.PlacedAnchor) bool) []world.InstanceCell {
	var changed []world.InstanceCell
	if p.grilleOpen {
		if p.grilleLeft > 0 {
			p.grilleLeft--
		}
		// Past its time the grille waits, tick by tick, for its cells to be clear: it
		// never shuts on a body.
		if p.grilleLeft == 0 && !occupied(p.grilleCells) {
			p.grilleOpen = false
			changed = append(changed, p.gate.Update(world.InstanceUpdate{
				Doors:      map[int]bool{world.GrillePuzzle: false},
				Mechanisms: []world.InstanceCell{p.showing(world.GrillePuzzle, 0, world.LeverOff)},
			})...)
		}
	}
	for k := range p.twinLeft {
		if p.twinLeft[k] == 0 {
			continue
		}
		p.twinLeft[k]--
		if p.twinLeft[k] == 0 {
			changed = append(changed, p.gate.Update(world.InstanceUpdate{
				Mechanisms: []world.InstanceCell{p.showing(world.TwinLeverPuzzle, k, world.LeverOff)},
			})...)
		}
	}
	return changed
}

// advanceDungeonPuzzlesLocked is the tick's half of the puzzles: the grille's and the
// twin levers' timers.
func (s *Sim) advanceDungeonPuzzlesLocked() {
	if s.dungeon == nil || s.dungeon.puzzles == nil {
		return
	}
	s.announceDungeonCellsLocked(s.dungeon.puzzles.advance(s.bodyInCellsLocked))
}

// bodyInCellsLocked reports whether any player's or mob's box overlaps one of cells.
// A dead player's body counts: a door does not close on anybody lying in it either.
func (s *Sim) bodyInCellsLocked(cells []world.PlacedAnchor) bool {
	for _, c := range cells {
		cell := box{
			min: [3]float64{float64(c.X), float64(c.Y), float64(c.Z)},
			max: [3]float64{float64(c.X + 1), float64(c.Y + 1), float64(c.Z + 1)},
		}
		for _, p := range s.players {
			if boxesOverlap(p.box(), cell) {
				return true
			}
		}
		for _, m := range s.mobs {
			if boxesOverlap(m.species().body.boxAt(m.pos), cell) {
				return true
			}
		}
	}
	return false
}

// openReturnShortcutLocked opens the way from the king's arena back to the arrival
// court. Called only by the king's authoritative death.
func (s *Sim) openReturnShortcutLocked() {
	d := s.dungeon
	s.announceDungeonCellsLocked(d.gate.Update(world.InstanceUpdate{Doors: map[int]bool{world.ReturnShortcutDoor: true}}))
}

// announceDungeonCellsLocked queues one change for every player in this dungeon, each
// from where its own backlog stands, and delivers what it can now.
func (s *Sim) announceDungeonCellsLocked(cells []world.InstanceCell) {
	if len(cells) == 0 {
		return
	}
	d := s.dungeon
	for _, p := range s.players {
		if _, waiting := d.pending[p]; !waiting {
			d.pending[p] = len(d.changes)
		}
	}
	for _, c := range cells {
		d.changes = append(d.changes, protocol.EncodeBlockUpdate(protocol.BlockUpdate{
			Pos: [3]int32{int32(c.X), int32(c.Y), int32(c.Z)}, BlockID: uint16(c.Block),
		}))
	}
	s.flushDungeonGateLocked()
}
