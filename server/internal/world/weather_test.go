package world

import "testing"

// The weather field's own lattice, and it is deliberately not climate_test.go's.
//
// **The step has to be wider than the feature being sampled or the samples are not
// samples.** weatherScaleBlocks is 4096 and WeatherPeriodTicks is 48000, so a lattice
// stepping 1024 blocks would take sixteen readings inside one cell of a smooth field
// and report them as sixteen observations. Measured that way the clear share moved
// between 0.30 and 0.42 from seed to seed; stepped past one cell on every axis it sits
// inside 0.347–0.352 across the same eight seeds, which is the difference between a
// measurement and a coincidence.
//
// The steps are coprime with the scales rather than multiples of them, so the lattice
// does not land on the same phase of the field every time.
const (
	weatherLatticeSteps = 32
	weatherStepX        = 4999
	weatherStepZ        = 5303
	weatherStepTicks    = 51001
)

// weatherSeeds are the worlds the measurements below are taken over. More than one,
// because a share measured on a single seed is a property of that seed's field until
// a second one agrees.
var weatherSeeds = []int64{climateSeed, 1234, 0, -7777}

// walkWeatherLattice calls f at every point of the lattice above, for one seed.
func walkWeatherLattice(seed int64, f func(x, z int64, tick uint64, kind WeatherKind, intensity uint8)) {
	for i := range weatherLatticeSteps {
		for j := range weatherLatticeSteps {
			for k := range weatherLatticeSteps {
				x := int64(i-weatherLatticeSteps/2) * weatherStepX
				z := int64(j-weatherLatticeSteps/2) * weatherStepZ
				tick := uint64(k) * weatherStepTicks
				kind, intensity := WeatherAt(seed, tick, x, z)
				f(x, z, tick, kind, intensity)
			}
		}
	}
}

// The claim the whole feature rests on: the sky is a function and not a state, so the
// same world, tick and column answer the same way however many times they are asked and
// whatever else the process has done in between.
//
// Weak on its own — a function that returned a constant would pass — which is why it
// runs beside the shape tests below rather than instead of them.
func TestWeatherAtIsTheSameEveryTimeItIsAsked(t *testing.T) {
	t.Parallel()

	for _, seed := range weatherSeeds {
		walkWeatherLattice(seed, func(x, z int64, tick uint64, kind WeatherKind, intensity uint8) {
			gotKind, gotIntensity := WeatherAt(seed, tick, x, z)
			if gotKind != kind || gotIntensity != intensity {
				t.Fatalf("seed %d at (%d, %d) tick %d answered %v/%d and then %v/%d",
					seed, x, z, tick, kind, intensity, gotKind, gotIntensity)
			}
		})
	}
}

// Two players standing on the same column at the same tick are handed the same sky, and
// two *worlds* are not. The first half is the acceptance criterion; the second is what
// says the seed is actually reaching the field, rather than every world having the same
// weather for the same reason the first half passes.
func TestTwoWorldsDoNotShareOneSky(t *testing.T) {
	t.Parallel()

	differences := 0
	total := 0
	walkWeatherLattice(weatherSeeds[0], func(x, z int64, tick uint64, kind WeatherKind, intensity uint8) {
		_, other := WeatherAt(weatherSeeds[1], tick, x, z)
		total++
		if other != intensity {
			differences++
		}
	})
	if differences*4 < total {
		t.Errorf("two seeds disagree about the sky at only %d of %d lattice points; the seed is barely reaching the field", differences, total)
	}
}

// Roughly a third of the sky is clear, and the criterion's band is 30–40%.
//
// **Measured rather than asserted from the constant**, which is the lesson generate.go's
// ore thresholds record: a threshold on fbm3D is not the share of the field it selects,
// and the only way to know the share is to count it. Per seed rather than pooled, so a
// seed that rained without pause could not be averaged away by three that did not.
func TestTheSkyIsClearAboutAThirdOfTheTime(t *testing.T) {
	t.Parallel()

	for _, seed := range weatherSeeds {
		clear, total := 0, 0
		walkWeatherLattice(seed, func(_, _ int64, _ uint64, kind WeatherKind, intensity uint8) {
			total++
			if kind == WeatherClear {
				clear++
				if intensity != 0 {
					t.Fatalf("seed %d reported a clear sky at intensity %d", seed, intensity)
				}
			} else if intensity == 0 {
				t.Fatalf("seed %d reported %v at intensity 0, which is Clear's value and only Clear's", seed, kind)
			}
		})

		share := float64(clear) / float64(total)
		if share < 0.30 || share > 0.40 {
			t.Errorf("seed %d is clear on %.1f%% of the lattice, want 30–40%%", seed, share*100)
		}
	}
}

// What falls is the land's decision, and this is the whole of the rule read back off the
// terrain rather than off the constants that produced it: a column above snowLine gets
// snow whatever climate it is in, a tundra column gets snow at any height, a desert
// column below the snow line gets a sandstorm, and everything else gets rain.
//
// A blizzard is never a climate's answer. It is the storm's, and the storm does not
// sample this field.
func TestWhatFallsIsDecidedByTheLandUnderIt(t *testing.T) {
	t.Parallel()

	seen := make(map[WeatherKind]int, 5)
	for _, seed := range weatherSeeds {
		walkWeatherLattice(seed, func(x, z int64, tick uint64, kind WeatherKind, intensity uint8) {
			seen[kind]++
			if kind == WeatherClear {
				return
			}
			if kind == WeatherBlizzard {
				t.Fatalf("seed %d produced a blizzard at (%d, %d) tick %d from a climate", seed, x, z, tick)
			}

			climate := ClimateAt(seed, x, z)
			surface, _, _ := shapeAt(seed, x, z, climate)

			want := WeatherRain
			switch {
			case climate == Tundra || surface >= snowLine:
				want = WeatherSnow
			case climate == Desert:
				want = WeatherSandstorm
			}
			if kind != want {
				t.Fatalf("seed %d at (%d, %d) tick %d is %v ground at height %d and the sky is %v, want %v",
					seed, x, z, tick, climate, surface, kind, want)
			}
		})
	}

	// And every kind a climate can produce actually occurs somewhere, so the table
	// above is exercised rather than merely satisfied by a world that only ever rains.
	for _, kind := range []WeatherKind{WeatherClear, WeatherRain, WeatherSnow, WeatherSandstorm} {
		if seen[kind] == 0 {
			t.Errorf("%v never occurred anywhere on the lattice", kind)
		}
	}
}

// Weather drifts; it does not flicker. Adjacent ticks at one column differ by at most 2
// of 255, which is the criterion — and the measured worst case is 1.
//
// **The kind is allowed to change here and the intensity is not**, because the two are
// different quantities: the kind flips between Clear and something the instant the field
// crosses the threshold, and at that crossing the intensity moves between 0 and 1. What
// must never happen is the *amount* jumping, because that is what a client interpolates.
func TestWeatherDriftsRatherThanFlickering(t *testing.T) {
	t.Parallel()

	const worst = 2
	for _, seed := range weatherSeeds {
		for i := range 24 {
			for j := range 24 {
				x := int64(i-12) * weatherStepX
				z := int64(j-12) * weatherStepZ
				for k := range 64 {
					// Spread across a whole period rather than starting at zero, so the
					// walk crosses the field's steep middle and not only its flat ends.
					tick := uint64(k) * (WeatherPeriodTicks / 64)
					_, before := WeatherAt(seed, tick, x, z)
					_, after := WeatherAt(seed, tick+1, x, z)

					delta := int(before) - int(after)
					if delta < 0 {
						delta = -delta
					}
					if delta > worst {
						t.Fatalf("seed %d at (%d, %d) went from intensity %d to %d in one tick", seed, x, z, before, after)
					}
				}
			}
		}
	}
}

// The intensity scale is used, both ends of it. A ramp that never reaches 255 is a wire
// field a third of which is dead range, and a ramp whose bottom is 0 collides with
// Clear's one legal intensity — generate.go's ore thresholds are the precedent for
// measuring this rather than trusting the arithmetic that produced it.
func TestTheIntensityScaleReachesBothOfItsEnds(t *testing.T) {
	t.Parallel()

	lowest, highest := uint8(255), uint8(0)
	for _, seed := range weatherSeeds {
		walkWeatherLattice(seed, func(_, _ int64, _ uint64, kind WeatherKind, intensity uint8) {
			if kind == WeatherClear {
				return
			}
			lowest = min(lowest, intensity)
			highest = max(highest, intensity)
		})
	}

	if lowest != 1 {
		t.Errorf("the weakest weather on the lattice is intensity %d, want 1", lowest)
	}
	if highest != 255 {
		t.Errorf("the hardest weather on the lattice is intensity %d, want the scale's top of 255", highest)
	}
}

// The mapping is monotone across its whole domain and clamps rather than wrapping. A
// uint8 conversion of an unclamped ramp is exactly the arithmetic that would turn the
// hardest front in the world into a clear-looking 3.
func TestTheIntensityRampIsMonotoneAndClamps(t *testing.T) {
	t.Parallel()

	previous := uint8(0)
	for field := int64(weatherClearThreshold); field <= one; field++ {
		got := weatherIntensityOf(field)
		if got < previous {
			t.Fatalf("the ramp fell from %d to %d at field value %d", previous, got, field)
		}
		if got == 0 {
			t.Fatalf("the ramp produced 0 at field value %d, which is Clear's intensity", field)
		}
		previous = got
	}
	if got := weatherIntensityOf(weatherClearThreshold); got != 1 {
		t.Errorf("the field's first non-clear value maps to %d, want 1", got)
	}
	if got := weatherIntensityOf(one); got != 255 {
		t.Errorf("the top of the field's range maps to %d, want 255", got)
	}
}

// The cost criterion: under 2 µs a call.
//
// The coordinate and the tick both sweep, for the reason BenchmarkGenerate sweeps its Y
// and BenchmarkSurfaceAt sweeps its column — one point would measure whichever of the
// two branches that point happens to take, and about a third of them return before the
// land is classified at all. The sweep therefore reports the mixture a server actually
// pays: one fbm3D on every call, plus a ClimateAt and a shapeAt on the roughly two
// thirds that are not clear.
func BenchmarkWeatherAt(b *testing.B) {
	for i := 0; b.Loop(); i++ {
		WeatherAt(climateSeed, uint64(i)*97, int64(i%64)*613, int64(i/64%64)*727)
	}
}
