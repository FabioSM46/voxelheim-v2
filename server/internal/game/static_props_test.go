package game

import (
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
	"math"
	"testing"
)

func testStaticPropIndex(t testing.TB) *staticPropIndex {
	t.Helper()
	index, err := newStaticPropIndex([]world.PlacedStaticProp{{ID: 1, Kind: world.PropBanquetTable, Facing: 1, Origin: [3]int64{31, 0, 31}}})
	if err != nil {
		t.Fatal(err)
	}
	return index
}
func TestStaticPropCollisionDoesNotWaitForMaterialisation(t *testing.T) {
	index := testStaticPropIndex(t)
	if len(index.visible(world.Coord{}, 1, nil)) != 0 {
		t.Fatal("unmaterialised snapshot")
	}
	if !index.overlaps(box{min: [3]float64{31, .9, 31}, max: [3]float64{32, 1.5, 32}}) {
		t.Fatal("unmaterialised tabletop is not authoritative")
	}
	// Through empty space below the tabletop, away from its four legs.
	if index.blocksRay([3]float64{29, .5, 31.5}, [3]float64{34, .5, 31.5}) {
		t.Fatal("filled real gap between table legs")
	}
	if !index.blocksRay([3]float64{29, .9, 31.5}, [3]float64{34, .9, 31.5}) {
		t.Fatal("ray passed through tabletop")
	}
	index.materialise(world.Coord{X: 1, Z: 1})
	index.materialise(world.Coord{X: 1, Z: 1})
	if got := index.visible(world.Coord{X: 1, Z: 1}, 0, nil); len(got) != 1 || got[0].PropID != 1 {
		t.Fatal("overhanging prop missing or duplicated")
	}
	if len(index.visible(world.Coord{X: 10}, 0, nil)) != 0 {
		t.Fatal("prop remains after view exit")
	}
}
func TestStaticPropQueriesAllocateNothing(t *testing.T) {
	index := testStaticPropIndex(t)
	outside := box{min: [3]float64{100, 0, 100}, max: [3]float64{101, 2, 101}}
	inside := box{min: [3]float64{31, .9, 31}, max: [3]float64{32, 1.5, 32}}
	if got := testing.AllocsPerRun(1000, func() {
		index.overlaps(outside)
		index.overlaps(inside)
		index.blocksRay([3]float64{29, .5, 31.5}, [3]float64{34, .5, 31.5})
	}); got != 0 {
		t.Fatalf("per-probe allocations: %g", got)
	}
}
func BenchmarkStaticPropQueries(b *testing.B) {
	index := testStaticPropIndex(b)
	for _, scenario := range []struct {
		name string
		body box
	}{
		{"outside-capital", box{min: [3]float64{100, 0, 100}, max: [3]float64{101, 2, 101}}},
		{"furnished-room", box{min: [3]float64{31, .9, 31}, max: [3]float64{32, 1.5, 32}}},
	} {
		b.Run(scenario.name, func(b *testing.B) {
			b.ReportAllocs()
			for b.Loop() {
				index.overlaps(scenario.body)
			}
		})
	}
	b.Run("table-gap-ray", func(b *testing.B) {
		b.ReportAllocs()
		for b.Loop() {
			index.blocksRay([3]float64{29, .5, 31.5}, [3]float64{34, .5, 31.5})
		}
	})
}

type furnishedTestTerrain struct {
	scriptedTerrain
	props *staticPropIndex
}

func (t furnishedTestTerrain) staticPropOverlap(b box) bool { return t.props.overlaps(b) }
func (t furnishedTestTerrain) staticPropRay(from, to [3]float64) bool {
	return t.props.blocksRay(from, to)
}
func TestStaticPropTerrainHooksPreserveVoxelAirAndTableGap(t *testing.T) {
	terrain := furnishedTestTerrain{props: testStaticPropIndex(t)}
	if terrain.Solid(31, 0, 31) {
		t.Fatal("prop polluted voxel solidity")
	}
	from, to := [3]float64{29, .9, 31.5}, [3]float64{34, .9, 31.5}
	if clearLineOfSight(terrain, from, to) {
		t.Fatal("LOS ignored tabletop")
	}
	if !clearLineOfSight(scriptedTerrain{}, from, to) {
		t.Fatal("no-prop LOS changed")
	}
	from[1], to[1] = .5, .5
	if !clearLineOfSight(terrain, from, to) {
		t.Fatal("LOS filled table-leg gap")
	}
	start := [3]float64{29, 0, 31.5}
	moved, blocked := moveAndCollideWithStep(terrain, playerBody, start, [3]float64{5, 0, 0}, playerStepHeight)
	if !blocked[0] || moved[0] >= 31 {
		t.Fatalf("body passed through tabletop: %v %v", moved, blocked)
	}
	moved, blocked = moveAndCollideWithStep(scriptedTerrain{}, playerBody, start, [3]float64{5, 0, 0}, playerStepHeight)
	if blocked[0] || moved[0] != 34 {
		t.Fatal("no-prop movement changed")
	}
}

func TestStaticPropVendorCanTradeOverTableAndClosesBehindBookcase(t *testing.T) {
	index, err := newStaticPropIndex([]world.PlacedStaticProp{
		{ID: 1, Kind: world.PropBanquetTable, Facing: 1, Origin: [3]int64{0, 64, 0}},
		{ID: 2, Kind: world.PropBookcase, Facing: 1, Origin: [3]int64{0, 64, 1}},
	})
	if err != nil {
		t.Fatal(err)
	}
	terrain := furnishedTestTerrain{scriptedTerrain: scriptedTerrain{want: func(_, y, _ int64) bool { return y < 64 }}, props: index}
	h := newVitalsHarness(t, DefaultTickRate, terrain)
	player, _ := h.join(1, [3]float32{-1.5, 64, .5})
	resident := h.standResidentAt(vnet.ResidentRoleSmith, [3]float64{2.5, 64, .5}, 0)
	if _, err := player.InteractNPC(protocol.NpcInteractRequest{EntityID: resident.entityID, ClientTick: 1}); err != nil {
		t.Fatalf("over-table interaction refused: %v", err)
	}
	h.step()
	if h.openStall(player) == 0 {
		t.Fatal("over-table stall closed")
	}
	h.fund(player, 50)
	if _, err := player.Trade(tradeFor(resident, ItemPickaxe, 1, true, 1, 2)); err != nil {
		t.Fatalf("over-table trade refused: %v", err)
	}
	if h.carrying(player, ItemPickaxe) != 1 {
		t.Fatal("over-table purchase missing")
	}
	h.standAt(player, [3]float64{-.5, 64, 2.5})
	h.step()
	if h.openStall(player) != 0 {
		t.Fatal("stall remained open behind bookcase")
	}
	if _, err := player.InteractNPC(protocol.NpcInteractRequest{EntityID: resident.entityID, ClientTick: 2}); err == nil {
		t.Fatal("opened through bookcase")
	}
	terrain.props = nil
	h.sim.terrain = terrain
	if _, err := player.InteractNPC(protocol.NpcInteractRequest{EntityID: resident.entityID, ClientTick: 3}); err != nil {
		t.Fatalf("no-prop interaction changed: %v", err)
	}
}

func TestStaticPropSweepStopsSmallFastBodiesAtThinLegs(t *testing.T) {
	index, err := newStaticPropIndex([]world.PlacedStaticProp{{ID: 1, Kind: world.PropChair, Facing: 1, Origin: [3]int64{0, 0, 0}}})
	if err != nil {
		t.Fatal(err)
	}
	terrain := furnishedTestTerrain{props: index}
	for _, direction := range []float64{-1, 1} {
		for phase := range 50 {
			start := [3]float64{.5 - direction*(2+float64(phase)*.005), .1, .24}
			moved, hit := moveAndCollide(terrain, projectileBody, start, [3]float64{direction * 4, 0, 0})
			want := .145 - collisionSkin
			if direction < 0 {
				want = .855 + collisionSkin
			}
			if !hit[0] || math.Abs(moved[0]-want) > 1e-5 {
				t.Fatalf("projectile skipped first chair leg (direction %g phase %d): %v %v", direction, phase, moved, hit)
			}
			start[2] = .5
			_, hit = moveAndCollide(terrain, projectileBody, start, [3]float64{direction * 4, 0, 0})
			if hit[0] {
				t.Fatal("projectile blocked in actual leg gap")
			}
		}
	}
}

func TestStaticPropSnapshotsUseCompleteViewSets(t *testing.T) {
	index := testStaticPropIndex(t)
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{31.5, 64, 31.5})
	h.sim.staticProps = index
	h.step()
	if newestSnapshot(t, out).StaticPropsLength() != 0 {
		t.Fatal("unmaterialised props published")
	}
	index.materialise(world.Coord{X: 1, Z: 1})
	h.step()
	snapshot := newestSnapshot(t, out)
	if snapshot.StaticPropsLength() != 1 {
		t.Fatal("materialised prop not published")
	}
	var prop vnet.StaticPropState
	if !snapshot.StaticProps(&prop, 0) || prop.PropId() != 1 {
		t.Fatal("wrong descriptor identity")
	}
	h.sim.mu.Lock()
	player.chunk = world.Coord{X: 20, Y: 2, Z: 20}
	player.pos = [3]float64{640, 64, 640}
	h.sim.mu.Unlock()
	h.step()
	if newestSnapshot(t, out).StaticPropsLength() != 0 {
		t.Fatal("view exit retained prop")
	}
}
func TestStaticPropInstancesHaveNoCapitalCatalogue(t *testing.T) {
	manager := instanceTestManager(t, 20, 1)
	instance, err := manager.Create(InstanceRuin{})
	if err != nil {
		t.Fatal(err)
	}
	terrain, ok := instance.Sim.terrain.(*CacheTerrain)
	if !ok || !terrain.cache.Finite() || terrain.staticProps != nil || instance.Sim.staticProps != nil {
		t.Fatal("finite instance acquired capital solids")
	}
}
func TestStaticPropWorldVocabularyMatchesWire(t *testing.T) {
	if world.MaxCapitalProps != protocol.MaxStaticProps {
		t.Fatal("world and snapshot caps differ")
	}
	pairs := []struct {
		world world.StaticPropKind
		wire  vnet.StaticPropKind
	}{
		{world.PropBanquetTable, vnet.StaticPropKindBanquetTable},
		{world.PropChair, vnet.StaticPropKindChair},
		{world.PropBench, vnet.StaticPropKindBench},
		{world.PropThrone, vnet.StaticPropKindThrone},
		{world.PropBookcase, vnet.StaticPropKindBookcase},
		{world.PropDesk, vnet.StaticPropKindDesk},
		{world.PropCounter, vnet.StaticPropKindCounter},
		{world.PropBarrel, vnet.StaticPropKindBarrel},
		{world.PropEquipmentRack, vnet.StaticPropKindEquipmentRack},
		{world.PropCouncilTable, vnet.StaticPropKindCouncilTable},
		{world.PropRug, vnet.StaticPropKindRug},
		{world.PropRunner, vnet.StaticPropKindRunner},
		{world.PropBanner, vnet.StaticPropKindBanner},
		{world.PropShield, vnet.StaticPropKindShield},
		{world.PropTrophy, vnet.StaticPropKindTrophy},
		{world.PropFeastSetting, vnet.StaticPropKindFeastSetting},
		{world.PropWallSconce, vnet.StaticPropKindWallSconce},
		{world.PropFloorCandelabrum, vnet.StaticPropKindFloorCandelabrum},
		{world.PropTableCandelabrum, vnet.StaticPropKindTableCandelabrum},
	}
	for _, pair := range pairs {
		if uint8(pair.world) != uint8(pair.wire) {
			t.Fatalf("worldkind%d differs from wirekind%d", pair.world, pair.wire)
		}
	}
}

func BenchmarkStaticPropOrdinaryMovement(b *testing.B) {
	terrain := scriptedTerrain{want: func(_, y, _ int64) bool { return y < 64 }}
	b.ReportAllocs()
	for b.Loop() {
		moveAndCollideWithStep(terrain, playerBody, [3]float64{.5, 64, .5}, [3]float64{.2, 0, .1}, playerStepHeight)
	}
}

func TestStaticPropArrivalAndRespawnAvoidNewFurniture(t *testing.T) {
	index, err := newStaticPropIndex([]world.PlacedStaticProp{{ID: 1, Kind: world.PropBanquetTable, Facing: 1, Origin: [3]int64{31, 64, 31}}})
	if err != nil {
		t.Fatal(err)
	}
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.sim.staticProps = index
	saved := [3]float64{31.5, 64, 31.5}
	fallback := [3]float32{100.5, 64, 100.5}
	life := Life{Pos: saved, Health: PlayerMaxHealth, Hunger: PlayerMaxHunger}
	player, out := h.joinLife(1, fallback, &life)
	want := [3]float64{100.5, 64, 100.5}
	if player.pos != want || life.Pos != saved {
		t.Fatal("arrival failed to move safely or mutated stored life")
	}
	if h.sim.respawnColumnFitsLocked(playerBody, saved, 63) {
		t.Fatal("respawn accepted furniture overlap")
	}
	h.step()
	snapshot := newestSnapshot(t, out)
	var entity vnet.EntityState
	if !snapshot.Entities(&entity, 0) {
		t.Fatal("missing first position")
	}
	pos := entity.Pos(nil)
	if [3]float32{pos.X(), pos.Y(), pos.Z()} != fallback {
		t.Fatal("first snapshot differs from normalized arrival")
	}
	if _, err := h.sim.SafeStaticPropArrival(saved, saved); err == nil {
		t.Fatal("unsafe fallback admitted")
	}
	h.sim.staticProps = nil
	if got, err := h.sim.SafeStaticPropArrival(saved, want); err != nil || got != saved {
		t.Fatal("no-prop arrival changed")
	}
}
