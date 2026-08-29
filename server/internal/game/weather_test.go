package game

import (
	"go/ast"
	"go/parser"
	"go/token"
	"math"
	"os"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The threshold is one line shared by all three effects. This table pins both sides of
// that line and the kinds each effect belongs to: 159 is still scenery, 160 bites, and a
// blizzard is snow for movement without becoming rain or sand for either other rule.
func TestHeavyWeatherHasOneThresholdAndThreeEffects(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name       string
		weather    protocol.WeatherState
		sand, snow bool
		rain       bool
	}{
		{"light sandstorm", protocol.WeatherState{Kind: vnet.WeatherKindSandstorm, Intensity: WeatherHeavy - 1}, false, false, false},
		{"heavy sandstorm", protocol.WeatherState{Kind: vnet.WeatherKindSandstorm, Intensity: WeatherHeavy}, true, false, false},
		{"light snow", protocol.WeatherState{Kind: vnet.WeatherKindSnow, Intensity: WeatherHeavy - 1}, false, false, false},
		{"heavy snow", protocol.WeatherState{Kind: vnet.WeatherKindSnow, Intensity: WeatherHeavy}, false, true, false},
		{"heavy blizzard", protocol.WeatherState{Kind: vnet.WeatherKindBlizzard, Intensity: WeatherHeavy}, false, true, false},
		{"light rain", protocol.WeatherState{Kind: vnet.WeatherKindRain, Intensity: WeatherHeavy - 1}, false, false, false},
		{"heavy rain", protocol.WeatherState{Kind: vnet.WeatherKindRain, Intensity: WeatherHeavy}, false, false, true},
		{"clear at full intensity", protocol.WeatherState{Kind: vnet.WeatherKindClear, Intensity: 255}, false, false, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := sandstormBites(tc.weather); got != tc.sand {
				t.Errorf("sandstormBites = %t, want %t", got, tc.sand)
			}
			if got := snowBites(tc.weather); got != tc.snow {
				t.Errorf("snowBites = %t, want %t", got, tc.snow)
			}
			if got := rainDouses(tc.weather); got != tc.rain {
				t.Errorf("rainDouses = %t, want %t", got, tc.rain)
			}
		})
	}
}

// Every authoritative reach decision goes through Player.reachLocked. The first half
// exercises the balance rule; the second walks the package syntax so a future call site
// cannot quietly use EditReach and leave one interaction longer than all the others.
func TestEveryReachDecisionUsesTheWeatherAwareReach(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	for _, tc := range []struct {
		name    string
		weather protocol.WeatherState
		want    float64
	}{
		{"light sandstorm", protocol.WeatherState{Kind: vnet.WeatherKindSandstorm, Intensity: WeatherHeavy - 1}, EditReach},
		{"heavy sandstorm", protocol.WeatherState{Kind: vnet.WeatherKindSandstorm, Intensity: WeatherHeavy}, EditReach * SandstormReachScale},
		{"heavy rain", protocol.WeatherState{Kind: vnet.WeatherKindRain, Intensity: 255}, EditReach},
	} {
		player.weather = tc.weather
		if got := player.reachLocked(); got != tc.want {
			t.Errorf("%s reach = %v, want %v", tc.name, got, tc.want)
		}
	}

	wantCallers := map[string]int{
		"edit.go:Edit":                    1,
		"loot.go:accessibleCorpseLocked":  1,
		"loot.go:canOpenCorpseLocked":     1,
		"mining.go:Mine":                  1,
		"mining.go:advanceMining":         1,
		"resident.go:InteractNPC":         1,
		"structure.go:PlaceStructure":     1,
		"structure.go:removeOwnStructure": 1,
		"vendor.go:tradeableLocked":       1,
	}
	gotCallers := make(map[string]int)
	fset := token.NewFileSet()
	entries, err := os.ReadDir(".")
	if err != nil {
		t.Fatalf("read game package: %v", err)
	}
	for _, entry := range entries {
		name := entry.Name()
		if entry.IsDir() || len(name) < 3 || name[len(name)-3:] != ".go" || len(name) >= 8 && name[len(name)-8:] == "_test.go" {
			continue
		}
		file, err := parser.ParseFile(fset, name, nil, 0)
		if err != nil {
			t.Fatalf("parse %s: %v", name, err)
		}
		for _, declaration := range file.Decls {
			function, ok := declaration.(*ast.FuncDecl)
			if !ok || function.Body == nil {
				continue
			}
			ast.Inspect(function.Body, func(node ast.Node) bool {
				identifier, ok := node.(*ast.Ident)
				if ok && identifier.Name == "EditReach" && function.Name.Name != "reachLocked" {
					t.Errorf("%s:%s reads EditReach directly", name, function.Name.Name)
				}
				selector, ok := node.(*ast.SelectorExpr)
				if ok && selector.Sel.Name == "reachLocked" {
					gotCallers[name+":"+function.Name.Name]++
				}
				return true
			})
		}
	}
	if len(gotCallers) != len(wantCallers) {
		t.Errorf("reachLocked has callers %v, want %v", gotCallers, wantCallers)
	}
	for caller, want := range wantCallers {
		if got := gotCallers[caller]; got != want {
			t.Errorf("%s calls reachLocked %d times, want %d", caller, got, want)
		}
	}
}

// Snow is a walking scale, composed after hunger and skipped for swimming. A diagonal
// intent makes the assertion independent of either horizontal axis, while the direct
// step keeps client prediction and network timing out of an authoritative speed rule.
func TestHeavySnowSlowsWalkingAndComposesWithStarvation(t *testing.T) {
	t.Parallel()

	velocityAt := func(weather protocol.WeatherState, hunger uint16, terrain Terrain, pos [3]float32) float64 {
		h := newDropHarness(t, terrain)
		player, _ := h.join(1, pos)
		h.sim.mu.Lock()
		defer h.sim.mu.Unlock()
		player.weather = weather
		player.hunger = hunger
		player.current = intent{moveX: 0.6, moveZ: 0.8}
		player.step(1/float64(DefaultTickRate), terrain)
		return math.Hypot(player.vel[0], player.vel[2])
	}

	ground := dropTerrain{groundTop: 63}
	for _, tc := range []struct {
		name    string
		weather protocol.WeatherState
		hunger  uint16
		terrain Terrain
		pos     [3]float32
		want    float64
	}{
		{"light snow", protocol.WeatherState{Kind: vnet.WeatherKindSnow, Intensity: WeatherHeavy - 1}, 1, ground, [3]float32{0.5, 64, 0.5}, WalkSpeed},
		{"heavy snow", protocol.WeatherState{Kind: vnet.WeatherKindSnow, Intensity: WeatherHeavy}, 1, ground, [3]float32{0.5, 64, 0.5}, WalkSpeed * SnowSpeedScale},
		{"blizzard", protocol.WeatherState{Kind: vnet.WeatherKindBlizzard, Intensity: WeatherHeavy}, 1, ground, [3]float32{0.5, 64, 0.5}, WalkSpeed * SnowSpeedScale},
		{"starving in heavy snow", protocol.WeatherState{Kind: vnet.WeatherKindSnow, Intensity: 255}, 0, ground, [3]float32{0.5, 64, 0.5}, WalkSpeed * StarvingSpeedScale * SnowSpeedScale},
		{"swimming in heavy snow", protocol.WeatherState{Kind: vnet.WeatherKindSnow, Intensity: 255}, 1, lakeWorld{bedTop: 57, waterTop: 65}, [3]float32{0.5, 60, 0.5}, SwimSpeed},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := velocityAt(tc.weather, tc.hunger, tc.terrain, tc.pos); math.Abs(got-tc.want) > 1e-12 {
				t.Errorf("horizontal speed = %v, want %v", got, tc.want)
			}
		})
	}
}

// The rain is sampled at the fire before every reader in the tick. While it is heavy the
// station neither cooks nor keeps mobs away and says lit=false on the wire; at 159 the
// same persisted fire relights without an action from its owner.
func TestHeavyRainDousesACampfireAndLightRainRelightsIt(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	fire := h.plantCampfire(player, 0, [3]int32{0, 63, 0})
	h.stockPack(player, ingredient{item: ItemRawMeat, count: 1})

	rain := protocol.WeatherState{Kind: vnet.WeatherKindRain, Intensity: WeatherHeavy}
	h.sim.mu.Lock()
	h.sim.weatherOverride = &rain
	h.sim.mu.Unlock()
	h.step()

	h.sim.mu.Lock()
	doused := fire.doused
	station := h.sim.stationWithinLocked(vnet.StructureKindCampfire, player.pos, CampfireCookRadius)
	safe := h.sim.nearACampfireLocked(player.pos)
	h.sim.mu.Unlock()
	if !doused || station || safe {
		t.Errorf("under heavy rain doused=%t station=%t safe=%t, want true false false", doused, station, safe)
	}
	states := snapshotStructures(t, out)
	if len(states) != 1 || states[0].StructureId() != fire.structureID || states[0].Lit() {
		t.Fatalf("heavy-rain snapshot = %d structures, lit=%t; want the one fire unlit", len(states), len(states) == 1 && states[0].Lit())
	}
	if _, err := h.craft(player, vnet.RecipeIDCookedMeat); err == nil {
		t.Error("raw meat cooked beside a doused campfire")
	}

	rain.Intensity = WeatherHeavy - 1
	h.step()
	h.sim.mu.Lock()
	doused = fire.doused
	station = h.sim.stationWithinLocked(vnet.StructureKindCampfire, player.pos, CampfireCookRadius)
	safe = h.sim.nearACampfireLocked(player.pos)
	h.sim.mu.Unlock()
	if doused || !station || !safe {
		t.Errorf("under light rain doused=%t station=%t safe=%t, want false true true", doused, station, safe)
	}
	states = snapshotStructures(t, out)
	if len(states) != 1 || !states[0].Lit() {
		t.Fatalf("light-rain snapshot carries lit=%t, want true", len(states) == 1 && states[0].Lit())
	}
	if _, err := h.craft(player, vnet.RecipeIDCookedMeat); err != nil {
		t.Errorf("cooking beside the relit campfire: %v", err)
	}
}

// Placement can happen after the tick's fire pass. A new fire must therefore sample
// its own weather before it enters the registry, or requests arriving before the next
// tick could cook on it and the spawn director could count its cold ground as safe.
func TestCampfirePlacedAfterTheWeatherPassIsDousedImmediately(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.stockPack(player, ingredient{item: ItemRawMeat, count: 1})

	rain := protocol.WeatherState{Kind: vnet.WeatherKindRain, Intensity: WeatherHeavy}
	h.sim.mu.Lock()
	h.sim.weatherOverride = &rain
	h.sim.mu.Unlock()

	fire := h.plantCampfire(player, 1, [3]int32{0, 63, 0})
	h.sim.mu.Lock()
	doused := fire.doused
	station := h.sim.stationWithinLocked(vnet.StructureKindCampfire, player.pos, CampfireCookRadius)
	safe := h.sim.nearACampfireLocked(player.pos)
	h.sim.mu.Unlock()
	if !doused || station || safe {
		t.Errorf("new fire under heavy rain doused=%t station=%t safe=%t, want true false false", doused, station, safe)
	}
	if _, err := h.craft(player, vnet.RecipeIDCookedMeat); err == nil {
		t.Error("raw meat cooked on a campfire placed under heavy rain before the next tick")
	}
}

// Two fires can disagree in one tick because the sample belongs to each anchor, not to
// an owner or to the simulation as a whole. The search derives stable columns from the
// deterministic field instead of pinning coordinates that would turn a weather retune
// into an unrelated fixture failure.
func TestEachCampfireReadsTheWeatherAtItsOwnColumn(t *testing.T) {
	t.Parallel()

	var rainy, dry [3]int32
	var sampledTick uint64
	var foundPair bool
	for tick := uint64(1); tick < 100_000 && !foundPair; tick += 997 {
		var foundRain, foundDry bool
		for i := int64(0); i < 64; i++ {
			x := i*8192 - 262_144
			z := ((i*37)%64)*8192 - 262_144
			kind, intensity := world.WeatherAt(testWorldSeed, tick, x, z)
			switch {
			case kind == world.WeatherRain && intensity >= WeatherHeavy && !foundRain:
				rainy, foundRain = [3]int32{int32(x), 63, int32(z)}, true
			case (kind != world.WeatherRain || intensity < WeatherHeavy) && !foundDry:
				dry, foundDry = [3]int32{int32(x), 63, int32(z)}, true
			}
		}
		if foundRain && foundDry {
			sampledTick, foundPair = tick, true
		}
	}
	if !foundPair {
		t.Fatal("the deterministic sample found no heavy-rain and dry pair")
	}

	h := newStructureHarness(t)
	if err := h.sim.RestoreStructures([]Structure{
		{Kind: vnet.StructureKindCampfire, Anchor: rainy, Facing: vnet.FacingNorth, Owner: testPlayerID(1)},
		{Kind: vnet.StructureKindCampfire, Anchor: dry, Facing: vnet.FacingNorth, Owner: testPlayerID(1)},
	}); err != nil {
		t.Fatalf("restore fires: %v", err)
	}
	h.sim.mu.Lock()
	h.sim.douseFiresLocked(sampledTick)
	standing := h.sim.sortedStructuresLocked()
	h.sim.mu.Unlock()
	if len(standing) != 2 {
		t.Fatalf("%d fires stand, want 2", len(standing))
	}
	for _, fire := range standing {
		switch fire.anchor {
		case rainy:
			if !fire.doused {
				t.Error("the fire in the heavy-rain column stayed lit")
			}
		case dry:
			if fire.doused {
				t.Error("the fire in the dry column was doused by weather elsewhere")
			}
		default:
			t.Errorf("unexpected fire at %v", fire.anchor)
		}
	}
}

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
