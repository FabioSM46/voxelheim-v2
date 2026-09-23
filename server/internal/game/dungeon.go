package game

import (
	"fmt"
	"math"
	"slices"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The first dungeon uses the two append-only boss species as stable encounter
// identities. Entity ids are minted anew, never used to interpret saved progress.
// This input is deliberately only defeated encounters; binding storage and the
// first-kill roster stay with their existing owners. The route beside them is the
// rest of a restored run's progress: checkpoints, solved puzzles and cleared groups
// (dungeon_route.go).
type dungeonProgress struct {
	guardian, king bool
	route          DungeonRoute
}

func dungeonProgressFrom(defeated []vnet.MobKind) dungeonProgress {
	return dungeonProgress{
		guardian: slices.Contains(defeated, vnet.MobKindVargrGuardian),
		king:     slices.Contains(defeated, vnet.MobKindDraugrKing),
	}
}

type dungeonEncounters struct {
	gate               *world.InstanceGate
	guardianID, kingID uint64
	progress           dungeonProgress
	puzzles            *dungeonPuzzles
	changes            [][]byte
	pending            map[*Player]int
	// descent is the minor encounters: placed groups, triggers and the spider waves
	// (dungeon_waves.go).
	descent dungeonDescent
	// checkpoints is the route's checkpoints and how many are reached (dungeon_route.go).
	checkpoints dungeonCheckpoints
}

// Construction only, before the manager publishes this session. Both live
// encounters use the ordinary spawn path exactly once; the open-world director
// never owns this simulation, and no missing mob is interpreted as a respawn. The
// minor-spawn groups are placed here too, on the same terms (dungeon_waves.go).
func (s *Sim) placeDungeonEncounters(seed int64, gate *world.InstanceGate, progress dungeonProgress) error {
	d := &dungeonEncounters{gate: gate, progress: progress, pending: make(map[*Player]int)}
	guardian, king, _ := world.InstanceEncounterAnchors(seed)
	place := func(kind vnet.MobKind, a world.PlacedAnchor) (uint64, error) {
		id, made := s.spawnMobLocked(kind, [3]float64{float64(a.X) + .5, float64(a.Y), float64(a.Z) + .5})
		if !made {
			return 0, fmt.Errorf("game: could not place dungeon encounter %s", kind)
		}
		return id, nil
	}
	var err error
	if !progress.guardian {
		d.guardianID, err = place(vnet.MobKindVargrGuardian, guardian)
		if err != nil {
			return err
		}
	}
	if !progress.king {
		d.kingID, err = place(vnet.MobKindDraugrKing, king)
		if err != nil {
			return err
		}
	}
	if err := s.placeDungeonMinorsLocked(seed, d); err != nil {
		return err
	}
	s.placeDungeonCheckpointsLocked(seed, d)
	s.dungeon = d
	s.deathTicks = ticksFor(10*time.Second, uint8(math.Round(1/s.dt)))
	return nil
}

func (s *Sim) dungeonBossLocked(m *mob) bool {
	return s.dungeon != nil && m.entityID == s.dungeon.kingID && !s.dungeon.progress.guardian
}

// Called only by the killed-mob transition, never by disappearance, reset or a
// client request. Only the placed guardian opens the gate: another boss of the
// same species manually injected by a test or future tool cannot advance it.
func (s *Sim) dungeonDefeatLocked(m *mob) {
	d := s.dungeon
	if d == nil {
		return
	}
	if m.entityID == d.kingID && !d.progress.king {
		d.progress.king = true
		s.openReturnShortcutLocked()
	}
	if m.entityID != d.guardianID || d.progress.guardian {
		return
	}
	d.progress.guardian = true
	var cells []world.InstanceCell
	for _, p := range d.gate.Open() {
		cells = append(cells, world.InstanceCell{X: p.X, Y: p.Y, Z: p.Z, Block: world.Air})
	}
	s.announceDungeonCellsLocked(cells)
}

// A full outbound queue retains the unsent suffix. Late arrivals read the open
// cache; disconnected players are removed from this bounded per-session backlog.
func (s *Sim) flushDungeonGateLocked() {
	d := s.dungeon
	if d == nil {
		return
	}
	for p, next := range d.pending {
		if s.players[p.entityID] != p {
			delete(d.pending, p)
			continue
		}
		for next < len(d.changes) && p.deliver(d.changes[next]) {
			next++
		}
		if next == len(d.changes) {
			delete(d.pending, p)
		} else {
			d.pending[p] = next
		}
	}
	if len(d.pending) == 0 {
		d.changes = nil
	}
}
