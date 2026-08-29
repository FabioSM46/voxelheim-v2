package game

import (
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The weather vocabulary, read back member by member.
//
// The constants in weather.go already fail the build if either list moves, and this is
// the other half of that pin rather than a duplicate of it: the constants say the
// *numbers* agree, and this says each number is still attached to the name it is
// supposed to be attached to. A pair of members swapped on both sides at once satisfies
// the arithmetic and puts sand in a blizzard, and only a table written out by name
// catches it.
func TestTheWeatherVocabularyIsTheWires(t *testing.T) {
	t.Parallel()

	for _, row := range []struct {
		server world.WeatherKind
		wire   vnet.WeatherKind
		name   string
	}{
		{world.WeatherUnknown, vnet.WeatherKindUnknown, "Unknown"},
		{world.WeatherClear, vnet.WeatherKindClear, "Clear"},
		{world.WeatherRain, vnet.WeatherKindRain, "Rain"},
		{world.WeatherSnow, vnet.WeatherKindSnow, "Snow"},
		{world.WeatherSandstorm, vnet.WeatherKindSandstorm, "Sandstorm"},
		{world.WeatherBlizzard, vnet.WeatherKindBlizzard, "Blizzard"},
	} {
		if got := weatherKindOf(row.server); got != row.wire {
			t.Errorf("%s converts to %s, want %s", row.name, got, row.wire)
		}
		if got := vnet.EnumNamesWeatherKind[row.wire]; got != row.name {
			t.Errorf("wire member %d is named %q, want %q", row.wire, got, row.name)
		}
	}

	// And the list has no member this table has not accounted for — an appended kind
	// that nothing here converts would otherwise be found by a client.
	if got, want := len(vnet.EnumNamesWeatherKind), 6; got != want {
		t.Errorf("the wire knows %d weather kinds and this table pins %d", got, want)
	}
}

// The period the weather field drifts at is two days, and the pin in weather.go is a
// compile-time one. This is the sentence that pin is enforcing, written where somebody
// changing DayLengthTicks will read it.
func TestAFrontTakesTwoDaysToPass(t *testing.T) {
	t.Parallel()

	if got, want := uint64(world.WeatherPeriodTicks), uint64(DayLengthTicks)*2; got != want {
		t.Errorf("the weather period is %d ticks and two days are %d", got, want)
	}
}

// weatherColumns are far enough apart to be under different fronts and different
// climates: the field's lattice cell is 4096 blocks and a climate's is 2048, so twenty
// thousand blocks between neighbours puts each of these in its own weather and its own
// land.
var weatherColumns = [][3]float32{
	{0.5, 64, 0.5},
	{20000.5, 64, -20000.5},
	{-40000.5, 64, 60000.5},
	{75000.5, 64, 15000.5},
}

// Every snapshot carries the sky over its own recipient, and it is the sky
// world.WeatherAt states for that recipient's own column at the tick the snapshot was
// built at.
//
// **Per recipient rather than per tick, which is what makes it worth a test.**
// TickOfDay beside it on the wire is the same number for everybody; this one is not, and
// a snapshot loop that hoisted the sample out of the per-viewer loop would still pass
// every assertion about the field's shape while telling four players in four climates
// the same thing.
func TestEverySnapshotCarriesTheSkyOverItsOwnRecipient(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})

	outs := make([]*dropSink, len(weatherColumns))
	players := make([]*Player, len(weatherColumns))
	for i, pos := range weatherColumns {
		players[i], outs[i] = h.join(uint64(i+1), pos)
	}
	h.advance(3)

	worldTick := h.sim.WorldTick()
	seen := make(map[protocol.WeatherState]int, len(weatherColumns))
	for i := range weatherColumns {
		snapshot := newestSnapshot(t, outs[i])
		carried := snapshot.Weather(nil)
		if carried == nil {
			t.Fatalf("player %d was sent a snapshot with no weather at all, which is this server saying it keeps none", i)
		}

		h.sim.mu.Lock()
		x, z := int64(math.Floor(players[i].pos[0])), int64(math.Floor(players[i].pos[2]))
		h.sim.mu.Unlock()

		kind, intensity := world.WeatherAt(testWorldSeed, worldTick, x, z)
		if got, want := carried.Kind(), weatherKindOf(kind); got != want {
			t.Errorf("player %d at (%d, %d) is told %s, and the field says %s", i, x, z, got, want)
		}
		if got := carried.Intensity(); got != intensity {
			t.Errorf("player %d at (%d, %d) is told intensity %d, and the field says %d", i, x, z, got, intensity)
		}
		if carried.Kind() == vnet.WeatherKindUnknown {
			t.Errorf("player %d is told a present weather of Unknown, which the client closes the session over", i)
		}
		if (carried.Kind() == vnet.WeatherKindClear) != (carried.Intensity() == 0) {
			t.Errorf("player %d is told %s at intensity %d; intensity 0 is Clear's and only Clear's",
				i, carried.Kind(), carried.Intensity())
		}
		if carried.Kind() == vnet.WeatherKindBlizzard {
			t.Errorf("player %d is told a blizzard, and no climate produces one", i)
		}
		seen[protocol.WeatherState{Kind: carried.Kind(), Intensity: carried.Intensity()}]++
	}

	// The claim above is vacuous on a world where every column happens to agree, so
	// this says the four columns really were told different things.
	if len(seen) < 2 {
		t.Errorf("all %d players in four climates were told the same sky, so nothing here distinguishes per-recipient weather from per-tick weather", len(weatherColumns))
	}
}

// Two players standing on the same column are told the same sky, which is the user
// story's whole claim — weather is a fact of the world and not a client's mood.
func TestTwoPlayersOnOneColumnAreToldTheSameSky(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	_, first := h.join(1, [3]float32{20000.5, 64, -20000.5})
	_, second := h.join(2, [3]float32{20000.5, 64, -20000.5})
	h.advance(3)

	a, b := newestSnapshot(t, first).Weather(nil), newestSnapshot(t, second).Weather(nil)
	if a == nil || b == nil {
		t.Fatal("a snapshot carries no weather")
	}
	if a.Kind() != b.Kind() || a.Intensity() != b.Intensity() {
		t.Errorf("two players on one column are told %s/%d and %s/%d",
			a.Kind(), a.Intensity(), b.Kind(), b.Intensity())
	}
}

// The sky moves with the world's own clock, not with the process's: a world restored to
// a tick far from where it started reports the weather of *that* tick.
//
// This is what #462's absolute counter buys, and it is checked through RestoreClock
// rather than by stepping for two days, because the point is that the value read is
// worldTick and not currentTick — and those two are equal on a fresh process, which is
// precisely the coincidence that would hide the bug.
func TestTheSkyFollowsTheWorldsOwnTickAndNotTheProcesss(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	_, out := h.join(1, [3]float32{20000.5, 64, -20000.5})

	// Half a period into a much later day, which is where the field is furthest from
	// where a fresh world starts.
	const restored = DayLengthTicks * 41
	if err := h.sim.RestoreClock(restored%DayLengthTicks, restored); err != nil {
		t.Fatalf("RestoreClock: %v", err)
	}
	h.step()

	carried := newestSnapshot(t, out).Weather(nil)
	if carried == nil {
		t.Fatal("the snapshot carries no weather")
	}
	kind, intensity := world.WeatherAt(testWorldSeed, restored+1, 20000, -20001)
	if carried.Kind() != weatherKindOf(kind) || carried.Intensity() != intensity {
		t.Errorf("at world tick %d the player is told %s/%d, and the field says %s/%d",
			restored+1, carried.Kind(), carried.Intensity(), weatherKindOf(kind), intensity)
	}
}

// The override replaces every player's weather while it is set, and the field is not
// consulted at all: a blizzard is the same everywhere, which is the one thing a function
// of the column cannot say.
//
// The kind chosen is Blizzard deliberately — no climate produces one, so a snapshot
// carrying it can only have come from here.
func TestAnOverrideReplacesEveryPlayersWeather(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	outs := make([]*dropSink, len(weatherColumns))
	for i, pos := range weatherColumns {
		_, outs[i] = h.join(uint64(i+1), pos)
	}

	h.advance(2)
	storm := protocol.WeatherState{Kind: vnet.WeatherKindBlizzard, Intensity: 220}
	h.sim.mu.Lock()
	h.sim.weatherOverride = &storm
	h.sim.mu.Unlock()
	h.advance(2)

	for i := range weatherColumns {
		carried := newestSnapshot(t, outs[i]).Weather(nil)
		if carried == nil {
			t.Fatalf("player %d was sent a snapshot with no weather while a storm was imposed", i)
		}
		if carried.Kind() != storm.Kind || carried.Intensity() != storm.Intensity {
			t.Errorf("player %d is told %s/%d under the storm, want %s/%d",
				i, carried.Kind(), carried.Intensity(), storm.Kind, storm.Intensity)
		}
	}

	// And clearing it hands the world back to the field rather than leaving the last
	// storm frozen over everybody.
	h.sim.mu.Lock()
	h.sim.weatherOverride = nil
	h.sim.mu.Unlock()
	h.advance(2)

	worldTick := h.sim.WorldTick()
	for i, pos := range weatherColumns {
		carried := newestSnapshot(t, outs[i]).Weather(nil)
		if carried == nil {
			t.Fatalf("player %d was sent a snapshot with no weather after the storm passed", i)
		}
		kind, intensity := world.WeatherAt(testWorldSeed, worldTick,
			int64(math.Floor(float64(pos[0]))), int64(math.Floor(float64(pos[2]))))
		if carried.Kind() != weatherKindOf(kind) || carried.Intensity() != intensity {
			t.Errorf("player %d is told %s/%d after the storm, and the field says %s/%d",
				i, carried.Kind(), carried.Intensity(), weatherKindOf(kind), intensity)
		}
	}
}
