package game

import (
	"math"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// A life the server owns: health, the transition to dead, the countdown, and the
// respawn that ends it.
//
// Everything here runs on the fixed tick under sim.mu, and every duration is a tick
// count derived from the configured rate. `time.Now` does not appear: a player who dies
// on a stalled server must not respawn early because wall time kept moving, and a test
// must be able to cover three seconds instantly.
//
// **Health, hunger and experience are persisted; the clocks around them are not.** A
// life is written to the player store on leave, on the autosave and at shutdown, so both
// reserves, the lifetime progression total and the pack come back with the player — see
// [Life] and [Player.Record]. What is deliberately left behind is
// everything that only means something inside one session: the respawn countdown, the
// protection window, the mining and swing guards. A record always describes a *living*
// player, which is why none of those has anything to say across a disconnect.
//
// That is also what makes quitting mid-death neither an escape nor a double charge. A
// player who is dead when their record is written is written as this file's respawn
// would have left them — alive, at full health, at respawnPositionLocked — with
// chargeDeathPenaltyLocked spending the durability exactly once, whether it is the tick
// that gets there first or the teardown.

// deathDurationTicks converts DeathDuration into the ticks Step counts in.
//
// Never zero: at 1 Hz the arithmetic is exact, but a rate that rounded a duration away
// would respawn a player on the tick they died, and a death nobody can see is not a
// death. Same shape as dropLifetimeTicks and idleLimitTicks.
func deathDurationTicks(tickRate uint8) uint32 {
	return max(uint32(DeathDuration/time.Second)*uint32(tickRate), 1)
}

// respawnProtectionTicks converts RespawnProtection into ticks, on the same rule.
func respawnProtectionTicks(tickRate uint8) uint32 {
	return max(uint32(RespawnProtection/time.Second)*uint32(tickRate), 1)
}

// alive reports whether the simulation will let this player act. The caller holds sim.mu.
func (p *Player) alive() bool { return p.lifeState == vnet.LifeStateAlive }

// vitalsLocked is the wire form of this player's own vitals. The caller holds sim.mu.
func (p *Player) vitalsLocked() protocol.PlayerVitals {
	level := levelFor(p.experience)
	experience := experienceIntoLevel(p.experience)
	experienceToNext := experienceToNext(level)
	if level == MaxLevel {
		// A full final bar preserves the wire invariants without asking every client for
		// a special case for a level that has no successor.
		experience = experienceToNext
	}

	return protocol.PlayerVitals{
		Health:           p.health,
		MaxHealth:        p.maxHealthLocked(),
		LifeState:        p.lifeState,
		RespawnTicks:     p.respawnTicks,
		Invulnerable:     p.protectionTicks > 0,
		Hunger:           p.hunger,
		MaxHunger:        PlayerMaxHunger,
		Level:            level,
		Experience:       experience,
		ExperienceToNext: experienceToNext,
	}
}

// damageLocked takes health away and kills the player if it runs out.
//
// **The only path damage takes.** Falling calls it, and so will every weapon: one place
// decides what "hurt" means, so there is one place that clamps at zero, one that refuses
// to hurt somebody already dead, and one that honours respawn protection. A second
// subtraction somewhere else would be a second set of those answers.
//
// Zero is ignored rather than treated as a hit — a swing that connects for nothing is
// not a hit — and negative damage is unrepresentable, which is the point of the type.
//
// The caller holds sim.mu.
func (p *Player) damageLocked(amount uint16) bool {
	if amount == 0 || !p.alive() || p.protectionTicks > 0 {
		return false
	}

	// A landed hit restarts both clocks. After the guards above rather than before them:
	// a swing that connects for nothing is not a hit, and neither is one on a player the
	// protection window is covering, so neither may postpone a recovery it did not
	// interrupt.
	p.sinceDamageTicks = 0
	p.regenTicks = 0

	if amount >= p.health {
		p.health = 0
		p.dieLocked()
		return true
	}
	p.health -= amount
	return true
}

// healLocked restores authoritative health up to this player's current maximum and
// reports the amount actually restored. A direct heal is not regeneration: it neither
// resets nor advances the quiet-time, interval or hunger counters. The caller holds
// Sim.mu.
func (p *Player) healLocked(amount uint16) uint16 {
	if amount == 0 || !p.alive() {
		return 0
	}
	maximum := p.maxHealthLocked()
	if p.health >= maximum {
		return 0
	}
	restored := min(amount, maximum-p.health)
	p.health += restored
	return restored
}

// dieLocked performs the transition to Dead, exactly once.
//
// Everything the player was in the middle of stops here rather than at each of the
// places that would otherwise have to ask whether they are still alive: movement intent,
// both velocities, the active mining target and any completion or reset queued behind
// it. A later combat intent joins this list; the list is the seam.
//
// What it does *not* touch is the inventory's contents. Death costs condition, never
// possessions — the durability penalty is applied by the tick, once, and only when the
// inventory lock is free.
func (p *Player) dieLocked() {
	if !p.alive() {
		return
	}

	p.lifeState = vnet.LifeStateDead
	p.health = 0
	p.respawnTicks = p.sim.deathTicks
	p.penaltyApplied = false

	p.current = intent{yaw: p.current.yaw}
	p.vel = [3]float64{}

	// Through the canonical setter, so the simulation's reverse index of who is mining
	// what is maintained rather than left pointing at a corpse.
	p.setMiningLocked(nil)
	p.mineReset = nil
	p.mineCompleting = false

	// The combat seam this comment promised. A swing accepted before the blow that
	// killed them does not land afterwards.
	p.pendingSwing = nil
	p.sim.removeAllThreatFor(p.entityID)

	p.sim.log.Debug("player died", "entity_id", p.entityID, "respawn_ticks", p.respawnTicks)
}

// advanceVitalsLocked runs one tick of the death countdown and the protection timer.
//
// Ordered before movement in Step, so a player who died on tick N first counts down on
// N+1 and respawns exactly deathTicks ticks later — rather than losing one to the tick
// they died on.
//
// The caller holds sim.mu.
func (p *Player) advanceVitalsLocked() {
	// There is no per-player timer or respawn goroutine. Step calls this only for
	// players still present in Sim.players, under the same lock Sim.Leave takes; once
	// Leave returns an unfinished countdown cannot fire later and cannot reinsert the
	// player. This is especially load-bearing for a body killed during leave linger.
	if p.protectionTicks > 0 {
		p.protectionTicks--
	}

	if p.alive() {
		// A leaving body remains in the simulation for the server-owned linger, but its
		// player can no longer act and may already be disconnected. That is not connected
		// play, so both the ordinary hunger clock and the regeneration it pays for pause
		// with the controls they were charging for.
		if !p.leaving {
			p.drainHungerLocked()
			p.regenerateLocked()
		}
		return
	}

	// Decremented before the respawn is considered, so a death on tick N is over on tick
	// N + deathTicks exactly. Testing the counter *after* spending it is the difference
	// between three seconds and three seconds plus one tick.
	if p.respawnTicks > 0 {
		p.respawnTicks--
	}

	// The legacy PR 92 penalty, retried until the inventory lock is free. It is applied at most
	// once per death: penaltyApplied is cleared by dieLocked and set by the charge
	// itself, so a contended tick defers rather than skipping, and no later tick can
	// spend the durability twice.
	if !p.penaltyApplied {
		if !p.tryApplyDeathPenaltyLocked() {
			// The tick never waits for a session goroutine. The countdown keeps running —
			// it is a clock rather than a queue, and the client has already been told a
			// number — but the respawn below waits, so nobody returns having paid nothing.
			return
		}
	}

	if p.respawnTicks == 0 {
		p.respawnLocked()
	}
}

// drainHungerLocked spends one point after a full interval of connected, living play.
// advanceVitalsLocked is the only caller, on its alive branch, so a corpse's countdown
// never consumes the reserve. At zero the clock is cleared and stopped: eating from
// empty buys a complete interval rather than inheriting a drain that could not land.
func (p *Player) drainHungerLocked() {
	if p.hunger == 0 {
		p.hungerTicks = 0
		return
	}

	p.hungerTicks++
	if p.hungerTicks < p.sim.hungerDrainTicks {
		return
	}
	p.hungerTicks = 0
	p.hunger--
}

// regenerateLocked gives back one point of health when enough quiet has passed.
//
// **It only ever runs for a living player**, because [advanceVitalsLocked] calls it on the
// alive branch and nowhere else. That is what makes "it never resurrects" a property of
// the call site rather than a check that could be forgotten: a dead player's route back is
// the respawn countdown below, unchanged.
//
// The two clocks are separate on purpose. `sinceDamageTicks` is the quiet since the last
// landed hit and stops at the delay; `regenTicks` is progress toward the next point and
// resets each time one is given. Folding them into one counter would mean either losing
// partial progress on every hit — which is what a fight already does — or carrying it
// across a delay it should not survive.
//
// **Regeneration resumes at the delay; the first point arrives one interval later.** Five
// seconds of quiet, then a point at six, then one a second. Stated because it is the kind
// of off-by-one a test should assert deliberately rather than discover.
//
// The caller holds sim.mu.
func (p *Player) regenerateLocked() {
	// Counted to the threshold and no further, so a session that runs for years does not
	// overflow a counter whose only question is "has the delay passed yet".
	if p.sinceDamageTicks < p.sim.regenDelayTicks {
		p.sinceDamageTicks++
		return
	}
	if p.health >= p.maxHealthLocked() {
		return
	}
	if p.hunger == 0 {
		return
	}

	p.regenTicks++
	if p.regenTicks < p.sim.regenIntervalTicks {
		return
	}
	p.regenTicks = 0
	p.health++
	p.regenPoints++
	if p.regenPoints == HealthRegenPointsPerHunger {
		p.regenPoints = 0
		p.hunger--
	}
}

// respawnLocked puts a dead player back in the world at full health.
//
// **The position is the tent they own, and the join spawn only when they own none.**
// That is the choice this function's comment used to promise, and it replaces the
// policy without touching death: dying far from home is a walk back rather than a
// reset, and dying with no camp is what it always was.
//
// Resolved from the live registry every time, deliberately. A cached position would
// still name a tent that collapsed under a broken block or was picked up an hour ago,
// and the player would come back standing in the air where one used to be.
//
// The caller holds sim.mu.
func (p *Player) respawnLocked() {
	p.lifeState = vnet.LifeStateAlive
	p.health = p.maxHealthLocked()
	p.hunger = max(p.hunger, RespawnHungerFloor)
	p.respawnTicks = 0
	p.protectionTicks = p.sim.protectionTicks

	// A respawn is full health, so there is nothing to give back — but the clocks are
	// cleared rather than left running, because a player who died mid-recovery must not
	// arrive with a partial point owed to a life that has ended.
	p.sinceDamageTicks = 0
	p.regenTicks = 0
	p.hungerTicks = 0
	p.regenPoints = 0

	p.pos = p.respawnPositionLocked()
	p.vel = [3]float64{}
	// Not on the ground until a tick says so, exactly as on join: the spawn sits above
	// the surface and the settle is the same landing every other fall is.
	p.onGround = false
	p.current = intent{yaw: p.current.yaw}
	p.idleTicks = 0

	// Both ordering guards, not just movement's. They are separate counters because
	// movement and mining arrive on separate messages with separate idle windows, and
	// resetting one without the other is the asymmetry rather than the safety: a client
	// that restarts its tick counter on a new life would have its walking accepted and
	// its mining refused as stale until the count caught up. A stale frame slipping
	// through the one-request window this opens is a movement intent the tick integrates
	// from the respawn position, or a mining request the tick revalidates — neither is
	// an outcome, so neither is worth the asymmetry.
	p.haveTick, p.lastTick = false, 0
	p.haveMineTick, p.lastMineTick = false, 0
	p.haveAttackTick, p.lastAttackTick = false, 0
	p.haveLootOpenTick, p.lastLootOpenTick = false, 0
	p.haveLootTakeTick, p.lastLootTakeTick = false, 0
	// A new life swings immediately: the cooldown belonged to the blade of a player who
	// is no longer standing there.
	p.attackCooldown = 0

	// The teleport is a chunk change the streaming goroutine has to hear about. Step
	// publishes on a change after stepping, and this runs before that in the same tick,
	// so a respawn across the world wakes the stream on the tick it happens.
	p.chunk = chunkAt(p.pos)
	p.chunks.publish(p.chunk)

	p.sim.log.Debug("player respawned", "entity_id", p.entityID,
		"pos", p.pos, "protection_ticks", p.protectionTicks)
}

// respawnPositionLocked is where this player comes back: their own tent if one stands,
// and otherwise the spawn their join was given.
//
// Their own by *identity*, which is what makes a tent somewhere to come back to rather
// than somewhere to come back to until the connection drops: the same player rejoining
// holds a new entity id and the same identity, so the tent they pitched last week is
// still the answer.
//
// A tent's anchor is the *ground* cell it rests on, so the player is put one block above
// it — standing on that ground, inside the two cells of air the footprint guaranteed
// were clear when it was planted. Centred in the cell for the reason a drop is: the
// anchor names a voxel, and half a block is the middle of one.
//
// It does not check that the headroom is *still* clear. Somebody may have walled the
// tent in since, and the answer to that is the answer to spawning inside any solid —
// moveAndCollide refuses to move a body that starts inside one, so the player stands
// still until it is broken again, exactly as they would anywhere else.
//
// The caller holds sim.mu.
func (p *Player) respawnPositionLocked() [3]float64 {
	home, standing := p.sim.tentOfLocked(p.playerID)
	if !standing {
		return p.spawn
	}

	anchor := home.anchorVoxel()
	return [3]float64{
		float64(anchor[0]) + 0.5,
		float64(anchor[1]) + 1,
		float64(anchor[2]) + 0.5,
	}
}

// fallDamage is what an impact at this speed costs, in health.
//
// One deterministic formula, and integer arithmetic past the threshold: the excess is
// floored to whole blocks per second before it is scaled, so the answer is a step
// function a test can pin at its boundaries rather than a float comparison.
func fallDamage(impact float64) uint16 {
	if !(impact > SafeFallSpeed) {
		// Written as a negated `>` on purpose: a NaN impact compares false against every
		// bound, so `impact <= SafeFallSpeed` would let one through to be scaled into a
		// damage number. Nothing should produce one — the integrator's inputs are all
		// finite — and this is the boundary where it would stop being true silently.
		return 0
	}

	excess := math.Floor(impact - SafeFallSpeed)
	damage := excess * FallDamagePerSpeed
	if damage >= PlayerMaxHealth {
		// Clamped against the level-one base deliberately: a fall that kills a novice is
		// survivable at high level, while this conversion stays bounded and deterministic.
		return PlayerMaxHealth
	}
	return uint16(damage)
}

// landedOnResidentTerrain reports whether the surface a player just stopped on is
// terrain the server has actually generated.
//
// The collision deliberately reads an absent chunk as solid, which is what stops a
// player falling out of a world that is merely still loading — and would just as
// happily present that fiction as a surface to be hurt by. So the layer under the feet
// is re-read without generating anything: every voxel it spans must be resident, and at
// least one of them must be something. Anything else is answered conservatively, which
// here means no damage.
//
// A non-generating read, on the tick, by contract: Terrain.Block never waits.
func landedOnResidentTerrain(t Terrain, pos [3]float64) bool {
	feet := playerBox(pos)
	// The layer immediately below the feet. collisionSkin holds the box a hair above the
	// face it landed on, so the voxel that stopped it is the one under that gap.
	y := int64(math.Floor(feet.min[1] - collisionSkin*2))

	x0, x1 := voxelSpan(feet.min[0], feet.max[0])
	z0, z1 := voxelSpan(feet.min[2], feet.max[2])

	solid := false
	for z := z0; z <= z1; z++ {
		for x := x0; x <= x1; x++ {
			block, resident := t.Block(x, y, z)
			if !resident {
				return false
			}
			if block != world.Air {
				solid = true
			}
		}
	}
	return solid
}
