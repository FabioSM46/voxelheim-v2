package game

import (
	"math"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The sky, as the simulation publishes it: one sample per connected player per tick, at
// that player's own column, put on that player's own snapshot.
//
// **The decision is not made here.** [world.WeatherAt] is the whole of what the weather
// is — a pure function of the world seed, the absolute tick and a column — and this file
// only asks it, converts its vocabulary to the wire's and hands the answer to the
// encoder. Nothing about a player, a fire or a block changes because of it; that is #465.
//
// **The one thing this file does decide is the override**, and it decides it by not
// deciding: while [Sim.weatherOverride] is set, every player is handed exactly that
// value and the field is never sampled. The Fimbulvetr storm (#469) is what sets it —
// a blizzard is scheduled and announced, so it cannot be something a climate produces
// by chance, and "the same weather for everyone, everywhere" is a statement a field
// sampled per column cannot make.

// The weather vocabulary is one list kept in two packages, and this is where they are
// held to each other — internal/session/mapsurface.go's arrangement for the surface
// vocabulary, for the same reason and by the same mechanism.
//
// internal/world must not know that a wire exists, so [world.WeatherKind] and
// `WeatherKind` are declared apart and numbered by hand. This package imports both, so
// it is the only place both lists are visible at once. Each member is converted in both
// directions and a conversion of a negative constant to uint8 is a compile error, so a
// member that moves on either side fails the build here rather than putting snow in a
// desert.
const (
	_ = uint8(world.WeatherUnknown - world.WeatherKind(vnet.WeatherKindUnknown))
	_ = uint8(vnet.WeatherKindUnknown - vnet.WeatherKind(world.WeatherUnknown))
	_ = uint8(world.WeatherClear - world.WeatherKind(vnet.WeatherKindClear))
	_ = uint8(vnet.WeatherKindClear - vnet.WeatherKind(world.WeatherClear))
	_ = uint8(world.WeatherRain - world.WeatherKind(vnet.WeatherKindRain))
	_ = uint8(vnet.WeatherKindRain - vnet.WeatherKind(world.WeatherRain))
	_ = uint8(world.WeatherSnow - world.WeatherKind(vnet.WeatherKindSnow))
	_ = uint8(vnet.WeatherKindSnow - vnet.WeatherKind(world.WeatherSnow))
	_ = uint8(world.WeatherSandstorm - world.WeatherKind(vnet.WeatherKindSandstorm))
	_ = uint8(vnet.WeatherKindSandstorm - vnet.WeatherKind(world.WeatherSandstorm))
	_ = uint8(world.WeatherBlizzard - world.WeatherKind(vnet.WeatherKindBlizzard))
	_ = uint8(vnet.WeatherKindBlizzard - vnet.WeatherKind(world.WeatherBlizzard))
)

// The weather field's period is DayLengthTicks × 2, and this is where that is stated.
//
// It cannot be stated in internal/world: that package owns the constant a front's length
// is expressed in and knows nothing about a day, because a day is a property of the
// simulation's clock. So the number is declared there and pinned here, in both
// directions, exactly as the vocabulary above is. Retune DayLengthTicks and this build
// stops compiling until [world.WeatherPeriodTicks] has been retuned with it.
const (
	_ = uint64(world.WeatherPeriodTicks - DayLengthTicks*2)
	_ = uint64(DayLengthTicks*2 - world.WeatherPeriodTicks)
)

// weatherKindOf converts one [world.WeatherKind] to the wire's `WeatherKind`. It is a
// conversion rather than a translation, and the constants above are why.
func weatherKindOf(kind world.WeatherKind) vnet.WeatherKind {
	return vnet.WeatherKind(kind)
}

// weatherAtLocked is what the sky is doing over one player, this tick.
//
// worldTick is passed in rather than read from s, because [Sim.stepWorld] reads it once
// for the whole tick: every player in one tick stands under one instant of the world's
// weather, and re-reading the field per player would be a second read of a value that
// cannot change under a lock this goroutine is holding.
//
// **The override wins outright and skips the sample.** It is not blended, not clamped
// and not compared with what the column would otherwise have said — the storm is a fact
// about the world rather than a modifier on the local sky, so while one is imposed every
// player is handed the same kind and the same intensity wherever they are standing.
//
// The column is the floor of the player's position, exactly as [chunkAt] takes it: the
// weather over a player is the weather over the voxel they are standing in, and the two
// have to agree about which voxel that is.
//
// The caller holds Sim.mu.
func (s *Sim) weatherAtLocked(worldTick uint64, pos [3]float64) protocol.WeatherState {
	if s.weatherOverride != nil {
		return *s.weatherOverride
	}

	kind, intensity := world.WeatherAt(s.worldSeed, worldTick, int64(math.Floor(pos[0])), int64(math.Floor(pos[2])))
	return protocol.WeatherState{Kind: weatherKindOf(kind), Intensity: intensity}
}

// ---------------------------------------------------------------------------
// The weather that bites
// ---------------------------------------------------------------------------

// What heavy weather does, and the three predicates that decide when it does it.
//
// **Every effect is a scale or a skip at a place the server already decides**, which is
// why this file gains three one-line predicates and one accessor rather than a system.
// The reach was already checked, the walking speed was already computed, the station
// scan already ran: nothing new is being simulated, and a sandstorm is a different
// argument to an answer that was being computed anyway.
//
// **The threshold is a step and not a ramp**, and [WeatherHeavy] says why at length. The
// three predicates below are therefore the whole of "is it bad out": each pairs the one
// intensity threshold with the one kind its rule belongs to, so a heavy sandstorm cannot
// slow a walker and deep snow cannot shorten an arm.
//
// **Nothing here invents a refusal.** A voxel outside the shortened reach is refused as
// `OutOfReach`, exactly as a voxel outside the full one is, and a cook beside a doused
// fire is refused because no station stands there. A client is told the same sentences
// it was told before; what changed is where the line is.

// sandstormBites reports whether this sky shortens the arm of the player standing under
// it.
func sandstormBites(w protocol.WeatherState) bool {
	return w.Kind == vnet.WeatherKindSandstorm && w.Intensity >= WeatherHeavy
}

// snowBites reports whether this sky is deep enough to walk through slowly.
//
// **A blizzard counts, and that is the storm's whole relationship with this file.** The
// Fimbulvetr (#469) imposes [vnet.WeatherKindBlizzard] through [Sim.weatherOverride], and
// a storm that left everybody walking at full speed would be a storm in name. It is not
// a fourth rule: a blizzard is snow that was scheduled rather than sampled, so it reads
// as snow here and as nothing at all in the two predicates around it.
func snowBites(w protocol.WeatherState) bool {
	return (w.Kind == vnet.WeatherKindSnow || w.Kind == vnet.WeatherKindBlizzard) &&
		w.Intensity >= WeatherHeavy
}

// rainDouses reports whether this sky puts a fire out.
//
// Rain and not a blizzard, which is the asymmetry [snowBites] does not have: snow lying
// deep and snow being driven are the same thing to walk through, and a fire that a
// blizzard extinguished would be a fire nobody could keep alight during the one event
// the game asks them to survive.
func rainDouses(w protocol.WeatherState) bool {
	return w.Kind == vnet.WeatherKindRain && w.Intensity >= WeatherHeavy
}

// reachLocked is how far this player may reach right now, in blocks.
//
// **The one implementation of the reach, and every site that used to name [EditReach]
// now names this instead.** That is enforced rather than asserted:
// TestNoCallSiteReadsTheReachConstantDirectly walks the package's own source and fails on
// a site that reads the constant, because a reach rule that applies at four of five
// call sites is not a rule — it is a shortcut through whichever one was forgotten, and
// the forgotten one would be discovered by a player rather than by a reviewer.
//
// It reads [Player.weather], which is the sky the tick loop sampled at this player's own
// column: two players a chunk apart can be under different weather and therefore have
// different arms, and a player indoors is no exception because there is no roof in this
// rule — the sandstorm is over the column, not over the head.
//
// The caller holds Sim.mu.
func (p *Player) reachLocked() float64 {
	if sandstormBites(p.weather) {
		return EditReach * SandstormReachScale
	}
	return EditReach
}

// douseFiresLocked settles, for every campfire standing, whether it is burning this tick.
//
// **A fire's weather is its own column's, not its owner's**, which is the reason this is
// a pass over the registry rather than a flag somebody sets when they place one: a camp
// on the edge of the tundra can be dry while the player who built it is standing in the
// rain fifty blocks away, and the answer has to be about the fire.
//
// **It is recomputed from scratch every tick and never remembered, and that is the whole
// of "it relights by itself".** There is no relighting mechanic and nothing to persist:
// a doused fire is a fire the rain is currently on, so when the rain eases the next tick
// computes `false` and the fire is burning again. It also means the field is safe to
// leave out of [Structure] — a restored camp comes back lit and is corrected on the first
// tick, which is the same direction the wire's `lit` default already fails in.
//
// Only campfires are asked. Nothing else has a fire to put out, and asking the field
// about a tent would be paying for an answer that is discarded.
//
// The caller holds Sim.mu.
func (s *Sim) douseFiresLocked(worldTick uint64) {
	for _, held := range s.structures {
		if held.kind != vnet.StructureKindCampfire {
			continue
		}
		anchor := held.anchorVoxel()
		over := [3]float64{float64(anchor[0]), float64(anchor[1]), float64(anchor[2])}
		held.doused = rainDouses(s.weatherAtLocked(worldTick, over))
	}
}
