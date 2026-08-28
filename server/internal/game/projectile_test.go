package game

import (
	"math"
	"testing"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

type projectileTerrain struct {
	wallZ  *int64
	absent bool
}

type projectileBoundaryTerrain struct{}

func (projectileBoundaryTerrain) Block(x, _, _ int64) (world.Block, bool) {
	if x >= 1 {
		return world.Air, false
	}
	return world.Air, true
}
func (w projectileBoundaryTerrain) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

func (w projectileBoundaryTerrain) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || block != world.Air
}

func (w projectileTerrain) Block(_, y, z int64) (world.Block, bool) {
	if w.absent {
		return world.Air, false
	}
	if y <= 63 || (w.wallZ != nil && z <= *w.wallZ) {
		return world.Stone, true
	}
	return world.Air, true
}
func (w projectileTerrain) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

func (w projectileTerrain) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || block != world.Air
}

func spawnTestProjectile(t *testing.T, h *vitalsHarness, kind vnet.ProjectileKind, owner *Player, direction [3]float64, speed float64) uint64 {
	t.Helper()
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	id, ok := h.sim.spawnProjectileLocked(kind, owner, projectileOriginLocked(owner), direction, speed)
	if !ok {
		t.Fatalf("spawnProjectileLocked(%s) was refused", kind)
	}
	return id
}

func advanceTestProjectiles(h *vitalsHarness) []*projectile {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	return h.sim.advanceProjectilesLocked(h.sim.sortedPlayersLocked())
}

func projectileState(h *vitalsHarness, id uint64) (projectile, bool) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	proj, ok := h.sim.projectiles[id]
	if !ok {
		return projectile{}, false
	}
	return *proj, true
}

func TestProjectileSpawnUsesTheSharedIdentityAndStartsOutsideItsOwner(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, projectileTerrain{})
	owner, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	id := spawnTestProjectile(t, h, vnet.ProjectileKindArrow, owner, [3]float64{0, 0, -1}, ArrowSpeed)
	proj, ok := projectileState(h, id)
	if !ok {
		t.Fatal("spawned projectile is absent")
	}
	if id <= 100 || proj.owner != owner.entityID || proj.kind != vnet.ProjectileKindArrow {
		t.Fatalf("projectile identity = %#v, owner=%d kind=%s", proj.entityID, proj.owner, proj.kind)
	}
	if proj.vel != [3]float64{0, 0, -ArrowSpeed} {
		t.Errorf("velocity = %v, want [0 0 %v]", proj.vel, -ArrowSpeed)
	}
	if boxesIntersect(projectileBody.boxAt(proj.pos), playerBox(owner.pos)) {
		t.Fatalf("projectile at %v still intersects its owner's box", proj.pos)
	}
}

func TestProjectileSpawnNormalizesDirectionAndBoundsLaunchSpeed(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, projectileTerrain{})
	owner, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	id := spawnTestProjectile(t, h, vnet.ProjectileKindEnergyOrb, owner, [3]float64{0, 0, -10}, OrbSpeed)
	proj, _ := projectileState(h, id)
	if proj.vel != [3]float64{0, 0, -OrbSpeed} {
		t.Errorf("normalized velocity = %v, want [0 0 %v]", proj.vel, -OrbSpeed)
	}

	h.sim.mu.Lock()
	_, ok := h.sim.spawnProjectileLocked(vnet.ProjectileKindArrow, owner, projectileOriginLocked(owner), [3]float64{1, 0, 0}, ProjectileMaxLaunchSpeed+1)
	h.sim.mu.Unlock()
	if ok {
		t.Fatal("spawn accepted a launch speed above ProjectileMaxLaunchSpeed")
	}
}

func TestAnArrowHitsADraugrWithoutTunnelling(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, projectileTerrain{})
	owner, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	mobID := h.spawnDraugrAt([3]float32{0.5, 64, -5.5})
	spawnTestProjectile(t, h, vnet.ProjectileKindArrow, owner, [3]float64{0, 0, -1}, ArrowSpeed)

	for range 5 {
		advanceTestProjectiles(h)
		if got := h.mobHealth(mobID); got != draugrRow.maxHealth {
			if got != draugrRow.maxHealth-ArrowDamage {
				t.Fatalf("draugr health = %d, want %d", got, draugrRow.maxHealth-ArrowDamage)
			}
			return
		}
	}
	t.Fatal("30 block/s arrow passed the draugr without hitting it")
}

func TestArrowGravityMatchesTheAuthoritativeAcceleration(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, emptyProjectileTerrain{})
	owner, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	id := spawnTestProjectile(t, h, vnet.ProjectileKindArrow, owner, [3]float64{1, 0, 0}, ArrowSpeed)
	start, _ := projectileState(h, id)

	const ticks = 17
	for range ticks {
		advanceTestProjectiles(h)
	}
	got, ok := projectileState(h, id)
	if !ok {
		t.Fatal("arrow expired before the gravity measurement")
	}
	elapsed := float64(ticks) / DefaultTickRate
	wantDrop := Gravity * elapsed * elapsed / 2
	gotDrop := start.pos[1] - got.pos[1]
	if math.Abs(gotDrop-wantDrop) > ProjectileMaxStep {
		t.Errorf("vertical drop = %.4f, want %.4f ± %.2f", gotDrop, wantDrop, ProjectileMaxStep)
	}
	if want := -Gravity * elapsed; math.Abs(got.vel[1]-want) > 1e-9 {
		t.Errorf("vertical velocity = %.4f, want %.4f", got.vel[1], want)
	}
}

// emptyProjectileTerrain is resident air everywhere. It is deliberately separate from
// movement_test's external-package fixture so this package-level test can use it.
type emptyProjectileTerrain struct{}

func (emptyProjectileTerrain) Solid(_, _, _ int64) bool { return false }
func (emptyProjectileTerrain) Block(_, _, _ int64) (world.Block, bool) {
	return world.Air, true
}
func (w emptyProjectileTerrain) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

func TestAOneBlockWallStopsEveryArrowOffset(t *testing.T) {
	t.Parallel()
	wallZ := int64(-3)
	for sample := range 100 {
		h := newVitalsHarness(t, DefaultTickRate, projectileTerrain{wallZ: &wallZ})
		x := float32(sample)/100 + 0.01
		owner, _ := h.join(1, [3]float32{x, 64, 0.5})
		id := spawnTestProjectile(t, h, vnet.ProjectileKindArrow, owner, [3]float64{0, 0, -1}, ArrowSpeed)
		for range 4 {
			advanceTestProjectiles(h)
		}
		proj, ok := projectileState(h, id)
		if !ok || !proj.stuck {
			t.Fatalf("offset %d: wall did not leave a stuck arrow", sample)
		}
		if proj.pos[2] <= -2 {
			t.Fatalf("offset %d: arrow crossed wall face to z=%v", sample, proj.pos[2])
		}
	}
}

func TestAVargrBodyCannotBeTunnelledThrough(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, emptyProjectileTerrain{})
	owner, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	mobID := h.spawnMobAt(vnet.MobKindVargr, [3]float32{0.5, 64, -3.5})
	origin := projectileOriginLocked(owner)
	target := boxCentre(vargrRow.body.boxAt([3]float64{0.5, 64, -3.5}))
	direction := [3]float64{target[0] - origin[0], target[1] - origin[1], target[2] - origin[2]}
	length := vectorLength(direction)
	for axis := range 3 {
		direction[axis] /= length
	}
	spawnTestProjectile(t, h, vnet.ProjectileKindArrow, owner, direction, ArrowSpeed)

	for range 4 {
		advanceTestProjectiles(h)
	}
	if got := h.mobHealth(mobID); got != vargrRow.maxHealth-ArrowDamage {
		t.Errorf("vargr health = %d, want %d", got, vargrRow.maxHealth-ArrowDamage)
	}
}

func TestAnArrowNeverHitsItsOwnerEvenWhenFiredStraightDown(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, projectileTerrain{})
	owner, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	id := spawnTestProjectile(t, h, vnet.ProjectileKindArrow, owner, [3]float64{0, -1, 0}, ArrowSpeed)
	advanceTestProjectiles(h)

	if got := h.vitals(owner).Health; got != PlayerMaxHealth {
		t.Errorf("owner health = %d after its own arrow, want %d", got, PlayerMaxHealth)
	}
	if proj, ok := projectileState(h, id); !ok || !proj.stuck {
		t.Fatal("downward arrow neither survived stuck nor remained inspectable")
	}
}

func TestTerrainAndLifetimeEndEachProjectileAtItsOwnRule(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, projectileTerrain{})
	owner, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	arrowID := spawnTestProjectile(t, h, vnet.ProjectileKindArrow, owner, [3]float64{0, -1, 0}, ArrowSpeed)
	orbID := spawnTestProjectile(t, h, vnet.ProjectileKindEnergyOrb, owner, [3]float64{0, -1, 0}, OrbSpeed)
	advanceTestProjectiles(h)

	arrow, arrowLive := projectileState(h, arrowID)
	if !arrowLive || !arrow.stuck || arrow.vel != [3]float64{} {
		t.Fatalf("terrain arrow = %+v live=%v, want stuck with zero velocity", arrow, arrowLive)
	}
	if _, orbLive := projectileState(h, orbID); orbLive {
		t.Fatal("orb survived its terrain collision")
	}
	for range int(h.sim.arrowStuckTicks) - 1 {
		advanceTestProjectiles(h)
	}
	if _, live := projectileState(h, arrowID); !live {
		t.Fatal("stuck arrow despawned before three seconds")
	}
	advanceTestProjectiles(h)
	if _, live := projectileState(h, arrowID); live {
		t.Fatal("stuck arrow survived beyond three seconds")
	}

	empty := newVitalsHarness(t, DefaultTickRate, emptyProjectileTerrain{})
	shooter, _ := empty.join(1, [3]float32{0.5, 64, 0.5})
	expiring := spawnTestProjectile(t, empty, vnet.ProjectileKindEnergyOrb, shooter, [3]float64{1, 0, 0}, OrbSpeed)
	for range int(empty.sim.orbLifetimeTicks) {
		advanceTestProjectiles(empty)
	}
	if _, live := projectileState(empty, expiring); live {
		t.Fatal("orb survived its configured lifetime")
	}
}

func TestArrowExpiryWinsOverStickingOnItsFinalTick(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, projectileTerrain{})
	owner, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	id := spawnTestProjectile(t, h, vnet.ProjectileKindArrow, owner, [3]float64{0, -1, 0}, ArrowSpeed)
	h.sim.mu.Lock()
	h.sim.projectiles[id].ticksLeft = 1
	h.sim.mu.Unlock()

	advanceTestProjectiles(h)
	if _, live := projectileState(h, id); live {
		t.Fatal("arrow became stuck instead of expiring on its final flight tick")
	}
}

func TestAProjectileHoldsOverANonResidentChunk(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, projectileTerrain{absent: true})
	owner, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	id := spawnTestProjectile(t, h, vnet.ProjectileKindEnergyOrb, owner, [3]float64{1, 0, 0}, OrbSpeed)
	before, _ := projectileState(h, id)
	advanceTestProjectiles(h)
	after, ok := projectileState(h, id)
	if !ok || after.pos != before.pos || after.vel != before.vel {
		t.Fatalf("held projectile changed from pos=%v vel=%v to pos=%v vel=%v", before.pos, before.vel, after.pos, after.vel)
	}
}

func TestAProjectileHoldsBeforeEnteringANonResidentChunk(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, projectileBoundaryTerrain{})
	owner, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	id := spawnTestProjectile(t, h, vnet.ProjectileKindEnergyOrb, owner, [3]float64{1, 0, 0}, OrbSpeed)
	before, _ := projectileState(h, id)

	advanceTestProjectiles(h)
	after, ok := projectileState(h, id)
	if !ok || after.pos != before.pos || after.vel != before.vel {
		t.Fatalf("boundary hold changed pos=%v vel=%v to pos=%v vel=%v", before.pos, before.vel, after.pos, after.vel)
	}
}

func TestAnOrbHealsAPlayerButAnArrowPassesThrough(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, emptyProjectileTerrain{})
	owner, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	target, _ := h.join(2, [3]float32{4.5, 64, 0.5})
	h.hurt(target, 20)

	orbID := spawnTestProjectile(t, h, vnet.ProjectileKindEnergyOrb, owner, [3]float64{1, 0, 0}, OrbSpeed)
	for range 6 {
		advanceTestProjectiles(h)
	}
	if _, live := projectileState(h, orbID); live {
		t.Fatal("orb survived after healing its first player target")
	}
	if got := h.vitals(target).Health; got != PlayerMaxHealth-20+OrbHeal {
		t.Errorf("healed health = %d, want %d", got, PlayerMaxHealth-20+OrbHeal)
	}

	arrowID := spawnTestProjectile(t, h, vnet.ProjectileKindArrow, owner, [3]float64{1, 0, 0}, ArrowSpeed)
	for range 4 {
		advanceTestProjectiles(h)
	}
	if _, live := projectileState(h, arrowID); !live {
		t.Fatal("arrow treated another player as solid")
	}
	if got := h.vitals(target).Health; got != PlayerMaxHealth-20+OrbHeal {
		t.Errorf("arrow changed player health to %d", got)
	}
}

func TestAProjectileWhoseOwnerLeftStillCreditsTheStableTap(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, emptyProjectileTerrain{})
	owner, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	mobID := h.spawnDraugrAt([3]float32{0.5, 64, -3.5})
	h.sim.mu.Lock()
	h.sim.mobs[mobID].health = ArrowDamage
	h.sim.mu.Unlock()
	spawnTestProjectile(t, h, vnet.ProjectileKindArrow, owner, [3]float64{0, 0, -1}, ArrowSpeed)
	h.sim.Leave(owner)

	for range 4 {
		advanceTestProjectiles(h)
	}
	if got := h.mobHealth(mobID); got != 0 {
		t.Fatalf("mob health = %d after offline owner's arrow, want 0", got)
	}
	awards := h.sim.PendingExperienceAwards()
	if len(awards) != 1 || awards[0].Experience != uint32(draugrRow.experience) {
		t.Fatalf("offline awards = %+v, want one %d-point award", awards, draugrRow.experience)
	}
}

func TestHealLockedClampsWithoutTouchingRegenerationClocks(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, projectileTerrain{})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	h.sim.mu.Lock()
	player.health = 50
	player.sinceDamageTicks, player.regenTicks, player.hungerTicks, player.regenPoints = 3, 4, 5, 6
	if got := player.healLocked(10); got != 10 || player.health != 60 {
		t.Errorf("50 + 10 restored %d to health %d", got, player.health)
	}
	player.health = 95
	if got := player.healLocked(10); got != 5 || player.health != 100 {
		t.Errorf("95 + 10 restored %d to health %d", got, player.health)
	}
	clocks := [4]uint32{player.sinceDamageTicks, player.regenTicks, player.hungerTicks, uint32(player.regenPoints)}
	player.dieLocked()
	if got := player.healLocked(10); got != 0 || player.health != 0 {
		t.Errorf("dead heal restored %d to health %d", got, player.health)
	}
	h.sim.mu.Unlock()

	if clocks != [4]uint32{3, 4, 5, 6} {
		t.Errorf("heal changed regen clocks to %v", clocks)
	}
}

func (s *dropSink) snapshotProjectiles(t *testing.T) []protocol.ProjectileState {
	t.Helper()
	var newest []protocol.ProjectileState
	found := false
	for _, frame := range s.all() {
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		if envelope.PayloadType() != vnet.PayloadEntitySnapshot {
			continue
		}
		var payload flatbuffers.Table
		if !envelope.Payload(&payload) {
			t.Fatal("EntitySnapshot envelope has no payload")
		}
		var snapshot vnet.EntitySnapshot
		snapshot.Init(payload.Bytes, payload.Pos)
		found = true
		newest = newest[:0]
		for i := range snapshot.ProjectilesLength() {
			var shown vnet.ProjectileState
			if !snapshot.Projectiles(&shown, i) {
				t.Fatalf("projectile %d is missing", i)
			}
			pos, vel := shown.Pos(nil), shown.Vel(nil)
			newest = append(newest, protocol.ProjectileState{
				EntityID: shown.EntityId(), Kind: shown.Kind(),
				Pos: [3]float32{pos.X(), pos.Y(), pos.Z()},
				Vel: [3]float32{vel.X(), vel.Y(), vel.Z()},
			})
		}
	}
	if !found {
		t.Fatal("no EntitySnapshot was delivered")
	}
	return newest
}

func TestSnapshotsCarryProjectileKindPositionVelocityAndDespawn(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, emptyProjectileTerrain{})
	owner, out := h.join(1, [3]float32{0.5, 64, 0.5})
	id := spawnTestProjectile(t, h, vnet.ProjectileKindEnergyOrb, owner, [3]float64{1, 0, 0}, OrbSpeed)
	h.step()

	shown := out.snapshotProjectiles(t)
	if len(shown) != 1 || shown[0].EntityID != id || shown[0].Kind != vnet.ProjectileKindEnergyOrb {
		t.Fatalf("snapshot projectiles = %+v", shown)
	}
	if shown[0].Vel != [3]float32{OrbSpeed, 0, 0} || shown[0].Pos[0] <= 0.5 {
		t.Errorf("snapshot projectile position/velocity = %v / %v", shown[0].Pos, shown[0].Vel)
	}

	h.advance(int(h.sim.orbLifetimeTicks))
	if newest := out.snapshotProjectiles(t); len(newest) != 0 {
		t.Fatalf("expired projectile remains in newest snapshot: %+v", newest)
	}
}
