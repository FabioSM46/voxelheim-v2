package game

import (
	"fmt"
	"math"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The dungeon's lesser creatures as encounters: placed once, woken by zones, and the
// cave's spiders coming out of the walls in waves.
//
// # Placed once
//
// Every minor-spawn slot the layout declares is filled when the instance is built, on
// the same construction path as the two bosses (see [Sim.placeDungeonEncounters]),
// each creature leashed to its group's zone. Nothing ever fills one again: the
// open-world director never runs here (see [Sim.directMobsLocked]), so a creature
// killed stays killed until the instance itself resets, and a cleared hall stays
// cleared. The cave's burrows are the one group not filled at build: they are where
// the waves come out of.
//
// Which species a group holds is the room's: the first hall is the draugr's, the
// second the vargr's, and the sand hall's scorpions lie buried. How many of a group's
// slots a smaller party meets is the balance issue's number; every slot is filled.
//
// # Triggers
//
// A trigger volume fires once per instance, on the first tick a live player's feet
// stand inside it, and never again. The cavern's starts the spider waves; the sand
// hall's is recorded and does nothing more, since its scorpions wake by proximity.
//
// # Spider waves
//
// Three waves, each from the burrows in turn, [spiderWaveInterval] apart — or
// [spiderWaveBreather] after the previous wave's last spider dies, when that is
// sooner, so a party that clears a wave quickly is not left waiting but always gets a
// breath. A wipe — no live player inside — stops the waves for good; what comes back
// after one is the persistence issue's wipe rule, not this file's.
//
// The layout makes the cavern's trigger the first thing a party reaches, ahead of the
// web curtain across the neck beyond it, so the trigger is the one start the waves
// need (world.TestTheCaveTriggerComesBeforeTheWebCurtain pins the order).

// spiderWaveSizes is how many spiders each wave brings, in order.
var spiderWaveSizes = [...]int{4, 5, 6}

const (
	// spiderWaveInterval is the longest a wave waits after the previous one began.
	spiderWaveInterval = 25 * time.Second
	// spiderWaveBreather is how long after a wave's last spider dies the next begins,
	// when that is sooner than the interval.
	spiderWaveBreather = 5 * time.Second
)

// dungeonMobCeiling is the most creatures one dungeon instance can ever hold at once:
// the two bosses, every placed slot and every spider of every wave alive together. The
// bound is structural rather than a cap applied at run time — nothing else ever
// creates a creature here — and TestTheDungeonHoldsItsMobAndSnapshotBudget pins it.
const dungeonMobCeiling = 2 + 16 + 8 + 4 + 5 + 6

// dungeonGroupSpecies is which species stands in each placed group's slots.
var dungeonGroupSpecies = map[int]struct {
	kind   vnet.MobKind
	buried bool
}{
	0:                     {vnet.MobKindDraugr, false},
	1:                     {vnet.MobKindDraugr, false},
	2:                     {vnet.MobKindVargr, false},
	3:                     {vnet.MobKindVargr, false},
	world.SandBuriedGroup: {vnet.MobKindScorpion, true},
}

// dungeonDescent is the minor encounters' state in one instance.
type dungeonDescent struct {
	// zones is every group's leash zone, indexed by group.
	zones []box
	// groups is every placed creature's identity, by group. A spider wave's are
	// appended to world.CaveBurrowGroup as the wave comes out.
	groups   map[int][]uint64
	triggers []dungeonTrigger
	waves    spiderWaves
}

// dungeonTrigger is one trigger volume and whether it has fired.
type dungeonTrigger struct {
	index  int
	volume box
	fired  bool
}

// spiderWaves is the cave's wave schedule.
type spiderWaves struct {
	burrows []world.PlacedAnchor
	// started is set when the cavern's trigger fires; stopped by a wipe.
	started, stopped bool
	// next is the wave to come, and due the tick it comes on.
	next int
	due  uint64
	// current is the latest wave's spiders; cleared is set once all of them are dead.
	current []uint64
	cleared bool
}

// placeDungeonMinorsLocked fills every placed group's slots and reads the triggers and
// burrows, at construction. A slot that cannot be filled refuses the instance, as a
// boss that cannot be placed does: a dungeon missing part of itself is not published.
func (s *Sim) placeDungeonMinorsLocked(seed int64, d *dungeonEncounters) error {
	desc := &d.descent
	desc.groups = make(map[int][]uint64)
	for _, z := range world.InstanceDungeonZones(seed) {
		desc.zones = append(desc.zones, box{
			min: [3]float64{float64(z.Min[0]), float64(z.Min[1]), float64(z.Min[2])},
			max: [3]float64{float64(z.Max[0] + 1), float64(z.Max[1] + 1), float64(z.Max[2] + 1)},
		})
	}
	corners := make(map[int][]world.PlacedAnchor)
	var triggerOrder []int
	for _, a := range world.InstanceDungeonAnchors(seed) {
		switch a.Kind {
		case world.AnchorInstanceMinorSpawn:
			if a.Index == world.CaveBurrowGroup {
				desc.waves.burrows = append(desc.waves.burrows, a)
				continue
			}
			species, known := dungeonGroupSpecies[a.Index]
			if !known || a.Index >= len(desc.zones) {
				return fmt.Errorf("game: dungeon minor group %d has no species or zone", a.Index)
			}
			id, made := s.placeMinorMobLocked(species.kind, anchorStanding(a), desc.zones[a.Index], species.buried)
			if !made {
				return fmt.Errorf("game: could not place dungeon minor %s in group %d", species.kind, a.Index)
			}
			desc.groups[a.Index] = append(desc.groups[a.Index], id)
		case world.AnchorInstanceTrigger:
			if corners[a.Index] == nil {
				triggerOrder = append(triggerOrder, a.Index)
			}
			corners[a.Index] = append(corners[a.Index], a)
		}
	}
	for _, index := range triggerOrder {
		c := corners[index]
		if len(c) != 2 {
			return fmt.Errorf("game: dungeon trigger %d has %d corners", index, len(c))
		}
		desc.triggers = append(desc.triggers, dungeonTrigger{index: index, volume: box{
			min: [3]float64{float64(min(c[0].X, c[1].X)), float64(min(c[0].Y, c[1].Y)), float64(min(c[0].Z, c[1].Z))},
			max: [3]float64{float64(max(c[0].X, c[1].X) + 1), float64(max(c[0].Y, c[1].Y) + 1), float64(max(c[0].Z, c[1].Z) + 1)},
		}})
	}
	return nil
}

// anchorStanding is where a body placed on a slot stands: centred in the slot's cell,
// feet on its floor.
func anchorStanding(a world.PlacedAnchor) [3]float64 {
	return [3]float64{float64(a.X) + .5, float64(a.Y), float64(a.Z) + .5}
}

// advanceDungeonDescentLocked is one tick of the triggers and the waves, and reports
// whether it created a creature. It runs in the director's place, after the mobs have
// stepped, so a trigger fires on the positions this tick produced.
//
// The caller holds Sim.mu.
func (s *Sim) advanceDungeonDescentLocked(tick uint64, players []*Player) bool {
	desc := &s.dungeon.descent
	anyAlive := false
	for _, p := range players {
		if p.alive() {
			anyAlive = true
			break
		}
	}
	w := &desc.waves
	if w.started && !anyAlive {
		w.stopped = true
	}
	for i := range desc.triggers {
		t := &desc.triggers[i]
		if t.fired {
			continue
		}
		for _, p := range players {
			if p.alive() && t.volume.holds(p.pos) {
				t.fired = true
				if t.index == world.CaveTrigger && !w.started {
					w.started, w.due = true, tick
				}
				break
			}
		}
	}
	if !w.started || w.stopped || w.next >= len(spiderWaveSizes) {
		return false
	}
	rate := uint8(math.Round(1 / s.dt))
	if w.next > 0 && !w.cleared && s.allDeadLocked(w.current) {
		w.cleared = true
		w.due = min(w.due, tick+uint64(ticksFor(spiderWaveBreather, rate)))
	}
	if tick < w.due {
		return false
	}
	w.current = s.releaseSpiderWaveLocked(w.next)
	w.cleared = false
	w.next++
	w.due = tick + uint64(ticksFor(spiderWaveInterval, rate))
	return true
}

// releaseSpiderWaveLocked brings wave n out of the burrows, each spider from the next
// burrow in turn after where the previous wave left off, so the waves come from every
// wall rather than always the same holes.
func (s *Sim) releaseSpiderWaveLocked(n int) []uint64 {
	desc := &s.dungeon.descent
	w := &desc.waves
	if len(w.burrows) == 0 {
		return nil
	}
	first := 0
	for _, size := range spiderWaveSizes[:n] {
		first += size
	}
	zone := desc.zones[world.CaveBurrowGroup]
	wave := make([]uint64, 0, spiderWaveSizes[n])
	for i := range spiderWaveSizes[n] {
		burrow := w.burrows[(first+i)%len(w.burrows)]
		id, made := s.placeMinorMobLocked(vnet.MobKindCaveSpider, anchorStanding(burrow), zone, false)
		if !made {
			s.log.Error("could not release a cave spider", "wave", n)
			continue
		}
		wave = append(wave, id)
	}
	desc.groups[world.CaveBurrowGroup] = append(desc.groups[world.CaveBurrowGroup], wave...)
	return wave
}

// allDeadLocked reports whether none of ids is alive any more. A killed creature
// leaves Sim.mobs on the tick the blow lands, so absence is death here: nothing else
// takes one of the dungeon's creatures away.
func (s *Sim) allDeadLocked(ids []uint64) bool {
	for _, id := range ids {
		if s.mobs[id] != nil {
			return false
		}
	}
	return true
}

// dungeonGroupClearedLocked reports whether every creature of a minor-spawn group is
// dead — for the cave's burrows, only once all three waves have come out. The seam the
// persistence issue records cleared groups from.
func (s *Sim) dungeonGroupClearedLocked(group int) bool {
	if s.dungeon == nil {
		return false
	}
	desc := &s.dungeon.descent
	if group == world.CaveBurrowGroup && desc.waves.next < len(spiderWaveSizes) {
		return false
	}
	return s.allDeadLocked(desc.groups[group])
}

// dungeonTriggerFiredLocked reports whether a trigger volume has fired in this
// instance.
func (s *Sim) dungeonTriggerFiredLocked(index int) bool {
	if s.dungeon == nil {
		return false
	}
	for _, t := range s.dungeon.descent.triggers {
		if t.index == index {
			return t.fired
		}
	}
	return false
}

// holds reports whether a standing position lies inside a trigger volume: half-open,
// as every collision box is, so feet standing on the floor course's top are inside
// and a body on the course above the volume is not.
func (b box) holds(pos [3]float64) bool {
	for axis := range 3 {
		if pos[axis] < b.min[axis] || pos[axis] >= b.max[axis] {
			return false
		}
	}
	return true
}
