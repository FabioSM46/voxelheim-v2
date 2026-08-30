package world

// Weather: what the sky is doing over one column, at one instant of the world's life.
//
// **The sky is a field, not an event.** There is no weather state kept anywhere, no
// scheduler and nothing to persist: [WeatherAt] is a pure function of the world seed,
// the absolute tick and a column, exactly as the terrain under it is a pure function of
// the seed and a column. Two players a hundred blocks apart therefore see the same
// front because they compute the same number, not because one of them was told; and a
// restart does not reset the sky, because the tick it is sampled at is the world's own
// (game.Sim.WorldTick) rather than the process's.
//
// **What it publishes and what it does not.** This file states the *fact* of the
// weather. Nothing here changes a player, a fire or a block — those are #465 — and
// nothing here draws anything. It is also not the storm: [WeatherBlizzard] is a member
// of the vocabulary because the wire has one, and it is never a value this function
// returns. The blizzard is scheduled, announced by a StormWarning and imposed by
// game.Sim.weatherOverride; a climate cannot produce one by chance.
//
// The arithmetic is integer-only, in Q16.16, for the reason noise.go gives at length:
// a float expression may round differently after a compiler upgrade, and two players
// who disagree about the rain are two players in different worlds.

const (
	// weatherScaleBlocks is how many blocks span one lattice cell of the weather
	// field — how wide a front is.
	//
	// Twice climateScaleBlocks, and the doubling is the point: a front has to be
	// something that crosses climates rather than something each climate keeps. At
	// four thousand blocks the first octave alone spans two deserts, so walking out
	// from under the rain is a walk rather than a step over a boundary.
	weatherScaleBlocks = 4096

	// WeatherPeriodTicks is how many ticks span one lattice cell of the field's time
	// axis — how long a front takes to pass.
	//
	// Two whole days, which at DefaultTickRate is about forty minutes of real time.
	// Long enough that the sky is a condition you are under rather than a flicker, and
	// short enough that a session sees it change.
	//
	// **Exported so that internal/game can pin it to DayLengthTicks × 2**, which is
	// where the number comes from. It cannot be derived here: game imports world and
	// never the reverse, so the two constants are declared apart and held to each other
	// from the side that can see both — the arrangement internal/session/mapsurface.go
	// uses for the surface vocabulary, and for the same reason.
	WeatherPeriodTicks = 48000
)

// weatherSeedOffset decorrelates the weather field from every other field derived from
// this world's seed, in the style of temperatureSeedOffset and the rest. Sampling any
// of them again would make the rain a property of the terrain: every storm would sit
// over the same ridge forever.
const weatherSeedOffset int64 = 0x38D01377

// Where a clear sky stops and weather begins, and where weather is as hard as it gets.
//
// **Both are read off the field's measured distribution rather than off its range, and
// generate.go's ore thresholds are why that distinction is written down here too.**
// fbm3D averages four octaves, so its values are concentrated around the middle and
// almost nothing reaches either end: over 884,736 samples on a decorrelated lattice
// (48³ points at 4999/5303 blocks and 51001 ticks apart, across eight seeds) the field
// spans 0.08 to 0.93 and its percentiles run
//
//	p50    p90    p99    p99.9   p99.99   max
//	0.500  0.649  0.753  0.819   0.863    0.935
//
// so a threshold named as "the calmest 45 percent of the range" selects nothing like
// forty-five percent of the columns.
//
//   - weatherClearThreshold is one*453/1000, which selects **34.9%** of samples pooled
//     and 34.7–35.2% per seed. That is the "roughly 35% clear" the design asks for, and
//     it is a measurement rather than a reading of the constant.
//   - weatherFullField is the p99.9 of the same sample, so intensity 255 means "harder
//     than all but one instant in a thousand" instead of naming a value the field never
//     reaches. Ramping to `one` instead would have capped the strongest front anybody
//     ever stands under at about 224 of 255, and the top eighth of the scale would have
//     been dead range on the wire.
//
// Anything above weatherFullField clamps. Retuning either number is retuning how often
// it rains, and TestTheSkyIsClearAboutAThirdOfTheTime is what measures the result.
const (
	weatherClearThreshold = one * 453 / 1000
	weatherFullField      = one * 82 / 100
)

// WeatherKind is what kind of weather is falling. Its members and their values are
// `WeatherKind`'s on the wire, member for member.
//
// **Declared here and not imported, for the reason SurfaceKind is**: internal/world
// must not know that a wire exists. The two lists are numbered by hand and held to each
// other in internal/game, which imports both — see weatherKindOf there.
//
// WeatherUnknown is the zero value and is never returned by anything in this package. It
// exists because the wire has it and the two lists have to line up; on the wire it is a
// protocol error rather than a kind of sky, which is what makes an accidental zero a
// loud failure instead of a quiet clear day.
type WeatherKind uint8

// The weather vocabulary. Values are wire values by agreement: append, never renumber.
const (
	WeatherUnknown   WeatherKind = 0
	WeatherClear     WeatherKind = 1
	WeatherRain      WeatherKind = 2
	WeatherSnow      WeatherKind = 3
	WeatherSandstorm WeatherKind = 4

	// WeatherBlizzard is the Fimbulvetr storm's own kind. It is never returned by
	// [WeatherAt]: a blizzard is scheduled rather than sampled, and the member is here
	// so that the storm and the ordinary sky speak one vocabulary.
	WeatherBlizzard WeatherKind = 5
)

// String names a kind for test failures and diagnostics, as Climate.String does.
func (k WeatherKind) String() string {
	switch k {
	case WeatherClear:
		return "clear"
	case WeatherRain:
		return "rain"
	case WeatherSnow:
		return "snow"
	case WeatherSandstorm:
		return "sandstorm"
	case WeatherBlizzard:
		return "blizzard"
	default:
		return "unknown weather"
	}
}

// WeatherAt is what the sky is doing over one column at one tick of the world's life.
//
// Pure in (seed, worldTick, x, z), like everything else in this package, and that is the
// whole of "the same for everyone": the server computes it once per player per tick and
// two players standing together are handed the same answer because the function has no
// other input.
//
// # The two halves are independent, and the order they are asked in is the cost
//
// *How much* weather there is comes from one fbm3D over the column and the tick. *What
// kind* it is comes from the land: [Tundra] and anything above snowLine gets snow,
// [Desert] gets a sandstorm, everything else gets rain — the climate decides what falls
// and the field decides whether anything falls at all.
//
// Intensity is asked first, and a clear sky returns before the land is ever classified.
// That is not a micro-optimisation: a clear sky has no kind, so naming one would be
// inventing a fact. It also means about a third of the calls cost one fbm3D and nothing
// else.
//
// # Cost
//
// One fbm3D always; on the roughly 65% of calls that are not clear, one ClimateAt (two
// fbm2D) and one shapeAt on top. See BenchmarkWeatherAt: the criterion is 2 µs and the
// measured figure is an order of magnitude under it.
func WeatherAt(seed int64, worldTick uint64, x, z int64) (kind WeatherKind, intensity uint8) {
	field := weatherFieldAt(seed, worldTick, x, z)
	if field < weatherClearThreshold {
		// The one place intensity 0 is produced, and it is produced together with the
		// only kind that may carry it: the wire says intensity is 0 for Clear and Clear
		// alone, so the pair is returned from one statement rather than assembled by a
		// caller.
		return WeatherClear, 0
	}
	return weatherKindAt(seed, x, z), weatherIntensityOf(field)
}

// weatherFieldAt samples the one field the weather is made of, in [0, one].
//
// The two space axes are scaled like every other field in this package — floorDiv rather
// than a plain division, because truncation toward zero would mirror the weather across
// the origin exactly as it would mirror the terrain.
//
// **The time axis is the whole feature.** fbm3D's third dimension is not depth here but
// the world's own tick, so a front is a slice through a volume that the world moves
// through at one tick per tick. That is what makes weather drift instead of flicker, and
// what makes it survive a restart: the tick is world time, and a world that was switched
// off for a month resumes the front it was under.
//
// **The shift is deliberately unguarded.** worldTick<<fracBits overflows int64 somewhere
// past 1.4×10¹⁴ ticks — about two hundred thousand years at DefaultTickRate — and Go's
// shift and conversion are both defined, so the result past that point is a discontinuity
// in the sky rather than a panic or a value that varies by platform. A range check would
// be a branch on every sample for a case no world reaches.
func weatherFieldAt(seed int64, worldTick uint64, x, z int64) int64 {
	nx := floorDiv(x<<fracBits, weatherScaleBlocks)
	nz := floorDiv(z<<fracBits, weatherScaleBlocks)
	nt := floorDiv(int64(worldTick)<<fracBits, WeatherPeriodTicks)
	return fbm3D(seed+weatherSeedOffset, nx, nz, nt)
}

// weatherIntensityOf maps a field value at or above weatherClearThreshold onto 1..255.
//
// 1 rather than 0 at the bottom, because 0 is Clear's and only Clear's: a caller that
// received a Rain at intensity 0 would have to decide what that meant, and there is no
// answer. 255 at and above weatherFullField, which is where the measurement above puts
// the top of what the field actually does.
func weatherIntensityOf(field int64) uint8 {
	if field >= weatherFullField {
		return 255
	}
	return uint8(1 + ((field-weatherClearThreshold)*254)/(weatherFullField-weatherClearThreshold))
}

// weatherKindAt is what falls here when something falls: the land's answer, with no
// reference to the field above.
//
// **Altitude overrides climate and climate overrides nothing else**, which is the same
// order blockAt uses for the ground: a mountain in the desert wears snow at its top by
// the rule that puts snow on it, so the sky over that peak has to agree with what the
// player is standing on. Below snowLine the climate decides, and plains and taiga share
// the same rain because the difference between them is what grows, not what falls.
//
// The climate is computed once and handed to shapeAt, for the reason columnAt does the
// same: an exported HeightAt here would sample temperature and humidity a second time
// for an answer this function already holds.
func weatherKindAt(seed, x, z int64) WeatherKind {
	climate := ClimateAt(seed, x, z)
	surface, _, _, _ := shapeAt(seed, x, z, climate)

	switch {
	case climate == Tundra || surface >= snowLine:
		return WeatherSnow
	case climate == Desert:
		return WeatherSandstorm
	default:
		return WeatherRain
	}
}
