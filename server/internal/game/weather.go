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
