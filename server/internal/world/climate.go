package world

// Climate: what kind of land a column is, decided from two slow fields, and how
// hard the ground folds there, decided from a third.
//
// **Climate is a server-side classification, not a wire value.** Nothing sends a
// biome — the client receives block ids and renders them — so this type is free to
// grow a fifth member without a protocol bump. The blocks a climate produces are
// the wire contract, and those are appended in the palette like every other id.
//
// Everything here is the same pure integer function of (seed, x, z) the rest of
// generation is: Q16.16 fixed point, no float anywhere. See noise.go for why.

const (
	// climateScaleBlocks is how many blocks span one lattice cell of the
	// temperature and humidity fields.
	//
	// Two thousand blocks is deliberately much larger than terrainScaleBlocks: a
	// climate has to be something you *walk into* over minutes rather than
	// something that changes with the next ridge. At this scale the first octave
	// alone is about sixteen chunks wide, so a desert is a place rather than a
	// patch.
	climateScaleBlocks = 2048

	// reliefScaleBlocks is how many blocks span one lattice cell of the relief
	// field, which decides how tall the terrain is allowed to be here.
	//
	// Smaller than the climate scale and larger than terrainScaleBlocks, so a
	// mountain range is a few kilometres of high ground crossing whatever climates
	// happen to be there — the shape of the land and what grows on it are two
	// independent questions, which is what lets a taiga have peaks and a desert
	// have dunes and flats.
	reliefScaleBlocks = 768
)

// Each field gets its own offset from the world seed, in the style of
// treeSeedOffset. Sampling one field at three scales would make temperature,
// humidity and relief three views of the same landscape: every desert would sit on
// the same side of every mountain, forever.
const (
	temperatureSeedOffset int64 = 0x85A308D3
	humiditySeedOffset    int64 = 0x03707344
	reliefSeedOffset      int64 = 0xA4093822
)

// Where one climate stops and the next begins.
//
// **Named constants rather than literals in the switch**, for the reason every
// threshold in this package is: these are the numbers a later issue retunes, and a
// retune has to be able to find them. They are fractions of [one], so `one*30/100`
// reads as "the coldest thirty percent of the scale" — which is not the same as
// the coldest thirty percent of *columns*, because fbm2D's sum of octaves is
// concentrated around its midpoint rather than spread flat. See
// TestEveryClimateCoversItsShareOfTheWorld for what that costs each climate.
const (
	// Below this temperature nothing grows: frozen ground, whatever the humidity.
	tundraTemperature = one * 30 / 100

	// Above this temperature the land is hot; hot *and* dry is a desert.
	desertTemperature = one * 70 / 100

	// Below this humidity there is not enough water for anything but sand.
	desertHumidity = one * 40 / 100

	// At or above this humidity the conifers close in.
	taigaHumidity = one * 55 / 100
)

// Climate names the kind of land a column belongs to.
//
// Deliberately not a wire type: it never leaves this package, and the client
// learns about a desert by being sent sand.
type Climate uint8

// The four climates. Plains is the zero value on purpose — an unset Climate is the
// ordinary middle of the map rather than an exotic one, which is the fail-safe
// direction for a value that is passed down through blockAt.
const (
	Plains Climate = iota
	Taiga
	Tundra
	Desert
)

// String names a climate for test failures and diagnostics.
func (c Climate) String() string {
	switch c {
	case Plains:
		return "plains"
	case Taiga:
		return "taiga"
	case Tundra:
		return "tundra"
	case Desert:
		return "desert"
	default:
		return "unknown climate"
	}
}

// temperatureAt is how warm a column is, in [0, one].
func temperatureAt(seed, worldX, worldZ int64) int64 {
	return climateField(seed+temperatureSeedOffset, worldX, worldZ, climateScaleBlocks)
}

// humidityAt is how wet a column is, in [0, one].
func humidityAt(seed, worldX, worldZ int64) int64 {
	return climateField(seed+humiditySeedOffset, worldX, worldZ, climateScaleBlocks)
}

// reliefAt is how hard the land folds at a column, in [0, one]. HeightAt reads it
// to choose an amplitude, which is what puts mountains in every climate rather
// than making "mountain" a climate of its own.
func reliefAt(seed, worldX, worldZ int64) int64 {
	return climateField(seed+reliefSeedOffset, worldX, worldZ, reliefScaleBlocks)
}

// climateField samples one 2D field at a scale. floorDiv rather than a plain
// division for the reason HeightAt uses it: truncation toward zero would mirror
// every field across the origin, so the climate west of spawn would be the mirror
// image of the one east of it.
func climateField(seed, worldX, worldZ, scaleBlocks int64) int64 {
	nx := floorDiv(worldX<<fracBits, scaleBlocks)
	nz := floorDiv(worldZ<<fracBits, scaleBlocks)
	return fbm2D(seed, nx, nz)
}

// ClimateAt classifies one world column.
//
// Exported because it is the seam the rest of the world reads: generation asks it
// once per column, and the tests that describe the shape of the map ask it over a
// lattice. Like HeightAt, it is a pure integer function of (seed, x, z) — the same
// column answers the same way in a build made months from now.
//
// The order of the tests is the classification: cold wins over everything, because
// a frozen desert is a tundra; then hot *and* dry, because that pair is what a
// desert is; then wet, which is where the conifers are. What is left is plains,
// which is most of the world and is deliberately the last answer rather than a
// range of its own.
func ClimateAt(seed, worldX, worldZ int64) Climate {
	temperature := temperatureAt(seed, worldX, worldZ)
	humidity := humidityAt(seed, worldX, worldZ)

	switch {
	case temperature < tundraTemperature:
		return Tundra
	case temperature > desertTemperature && humidity < desertHumidity:
		return Desert
	case humidity >= taigaHumidity:
		return Taiga
	default:
		return Plains
	}
}
