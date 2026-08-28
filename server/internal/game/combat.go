package game

import (
	"errors"
	"fmt"
	"math"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// A swing is a request; the verdict is the server's.
//
// The wire carries an inventory slot and an ordering tick and nothing else — no target,
// no position, no aim, no damage. Everything that decides whether a blow lands is read
// here from state the simulation already owns: where the player is after this tick's
// movement, where they were last looking, what is actually in the slot they named, and
// where the mobs are. A forged client can send any slot at any rate and gain nothing by
// it, because none of those answers came from the client.
//
// There is no acknowledgement. The next snapshot carries the draugr's health and the
// next inventory state carries the blade's condition, and between them they are the
// complete reply.

// swordConeCosine is the cosine of the half-angle of the attack arc.
//
// Precomputed because it is compared against a dot product on every swing, and because
// the comparison is the one place a degree could be confused for a radian.
var swordConeCosine = math.Cos(SwordConeDegrees / 2 * math.Pi / 180)

// pendingSwing is one accepted attack intent waiting for the tick to judge it.
//
// A one-shot rather than a held control: PlayerInput describes the state of the movement
// keys and persists, while a swing is an event and happens once. It carries only the
// slot — the aim is whatever the player's latest accepted input said, and re-reading it
// from a later request would let a client choose its own aim after seeing the world move.
type pendingSwing struct {
	slot uint8
}

// Attack records one swing for the tick to resolve.
//
// Runs on the session's read goroutine, so it does no work beyond admission: the tick is
// what resolves the swing against the positions that tick produced, which is what stops
// network scheduling from choosing an in-between position to be judged at.
//
// Every refusal is an error the session logs at debug. Missing launcher ammunition also
// carries the actionable NoAmmunition answer; stale ticks, invalid slots and cooldown
// refusals retain silence. None is a protocol failure.
func (p *Player) Attack(req protocol.AttackRequest) (vnet.RefusalReason, error) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if err := p.cannotActLocked(); err != nil {
		return vnet.RefusalReasonUnknown, err
	}
	if p.blocking {
		// A shield up silently drops every swing before it creates pending state.
		return vnet.RefusalReasonUnknown, nil
	}

	// Its own ordering guard, beside movement's and mining's rather than shared with
	// them: the three arrive on different messages at different cadences, and one
	// counter would let a fast stream of one silence the others.
	if p.haveAttackTick && !newerTick(req.ClientTick, p.lastAttackTick) {
		return vnet.RefusalReasonUnknown, fmt.Errorf("stale attack client tick %d; the newest accepted is %d", req.ClientTick, p.lastAttackTick)
	}
	p.haveAttackTick, p.lastAttackTick = true, req.ClientTick

	if req.Slot >= protocol.InventorySlots {
		return vnet.RefusalReasonUnknown, fmt.Errorf("attack slot %d is outside %d slots", req.Slot, protocol.InventorySlots)
	}
	if p.pendingSwing != nil {
		// Two clicks inside one tick. The first is already waiting to be judged and the
		// second would either replace it or queue behind it; neither is a thing the
		// player asked for, and both would let a client raise its own attack rate.
		return vnet.RefusalReasonUnknown, errors.New("an attack is already waiting for the tick")
	}
	if p.attackCooldown > 0 {
		return vnet.RefusalReasonUnknown, fmt.Errorf("the weapon is recovering for %d more ticks", p.attackCooldown)
	}

	// A launcher with no ammunition gets the one actionable refusal this attack path
	// carries. This is a session-goroutine admission check, so waiting for this player's
	// inventory is safe; the tick repeats it with TryLock before spending anything.
	p.inventory.mu.Lock()
	missingAmmunition := !p.launcherHasAmmunitionLocked(req.Slot)
	p.inventory.mu.Unlock()
	if missingAmmunition {
		return vnet.RefusalReasonNoAmmunition, errors.New("the launcher has no ammunition")
	}

	p.pendingSwing = &pendingSwing{slot: req.Slot}
	return vnet.RefusalReasonUnknown, nil
}

// Block silently accepts only a live player's usable off-hand shield.
func (p *Player) Block(active bool) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	p.blocking = active && p.alive() && !p.leaving && p.wornShield.fraction > 0
	if p.blocking {
		p.pendingSwing = nil
	}
}

// launcherHasAmmunitionLocked reports whether admission may queue this slot. Non-launchers,
// worn-through launchers and unreadable rows return true: they retain the existing silent
// attack refusal, while only a usable launcher missing its declared ammunition is explained.
// The caller holds inventory.mu.
func (p *Player) launcherHasAmmunitionLocked(slot uint8) bool {
	stack, held := p.inventory.stackAtLocked(slot)
	if !held {
		return true
	}
	definition, registered := itemByID(stack.item)
	if !registered || definition.launches == vnet.ProjectileKindUnknown ||
		(stack.durable() && stack.durability == 0) {
		return true
	}
	if definition.ammunition == ItemNone {
		return true
	}
	for _, candidate := range p.inventory.slots[:equipmentFirst] {
		if candidate.item == definition.ammunition && candidate.count > 0 {
			return true
		}
	}
	return false
}

// resolveAttackLocked judges this player's pending melee swing or launch, if there is one.
//
// Called from Step after every player has moved and before the mobs act, which is the
// whole of the ordering guarantee: a swing is judged against the positions this tick
// produced, and a draugr killed by one cannot land an attack later in the same tick.
//
// **It returns nothing, and it used to return the loot.** A blow that kills produces the
// owned corpse and rolls its container inside Sim.damageMobLocked, under the authoritative
// simulation lock and on this same tick; the ground-drop path is not involved.
//
// The caller holds Sim.mu.
func (p *Player) resolveAttackLocked() {
	if p.attackCooldown > 0 {
		p.attackCooldown--
	}

	pending := p.pendingSwing
	if pending == nil {
		return
	}
	if !p.alive() {
		// Died between the click and the tick. Nothing lands, and the swing is dropped
		// rather than held for the respawn.
		p.pendingSwing = nil
		return
	}

	armed, sampled := p.armedForAttackLocked(pending.slot)
	if !sampled {
		// A session goroutine holds the inventory. The tick never waits for it, and the
		// swing is *kept* rather than dropped — a click the player made does not stop
		// having been made because another one of their own messages was in flight.
		// Nothing else advances here, so the retry judges the next tick's positions.
		return
	}

	// Consumed exactly once, whatever the verdict below. Held any longer it would be a
	// second swing; dropped before the check it would be no swing at all.
	p.pendingSwing = nil
	if armed.meleeDamage == 0 && armed.launches == vnet.ProjectileKindUnknown {
		// An empty slot, a stack of stone, or a blade worn through. Silence is the whole
		// of the answer: the client is told nothing and sees nothing happen.
		return
	}

	// Paid before the search, so a miss costs exactly what a hit does. A client that
	// swings at nothing to find out whether anything is there pays attack cadence for
	// the question.
	if armed.launches != vnet.ProjectileKindUnknown {
		cooldown, speed := p.sim.launchParameters(armed.launches)
		if cooldown == 0 || speed == 0 {
			return
		}
		p.attackCooldown = cooldown
		p.sim.spawnProjectileLocked(
			armed.launches,
			p,
			projectileOriginLocked(p),
			lookDirection(p.current.yaw, p.current.pitch),
			speed,
		)
		return
	}

	p.attackCooldown = p.sim.attackCooldown
	if target := p.sim.swingTargetLocked(p); target != nil {
		p.sim.creditMobDamageLocked(p, target, armed.meleeDamage)
	}
}

// creditMobDamageLocked is the one path player-authored damage takes against a mob.
// Melee and projectiles share tap ownership, threat, death and party/offline experience
// exactly; a new delivery mechanism therefore cannot grow its own kill-credit rules.
//
// p may have left the live player map after firing a projectile. Its immutable character
// identity still establishes the tap, damage still lands, and a kill follows the existing
// offline-award path. Threat and boss participation remain live-session concerns.
// The caller holds Sim.mu.
func (s *Sim) creditMobDamageLocked(p *Player, target *mob, damage uint16) {
	if target == nil || damage == 0 || target.health == 0 {
		return
	}

	if p != nil {
		if s.onlineLocked(p) {
			// A valid hit can be the pull before the boss has had a tick in which to
			// acquire a target. Freeze eligibility before damage can make it lethal.
			s.startBossEncounterLocked(target, p)
		}
		if target.firstHit == nil {
			target.firstHit = newMobTap(p)
		}
		dealt := min(damage, target.health)
		if s.onlineLocked(p) {
			s.creditDamageThreatLocked(target, p, dealt)
		}
	}

	if !s.damageMobLocked(target, damage) || target.firstHit == nil {
		return
	}

	owner := s.currentTapOwnerLocked(target.firstHit)
	amount := uint32(target.species().experience)
	if owner == nil {
		award := s.awardOfflineExperienceLocked(target.firstHit, amount)
		s.log.Debug("experience awarded",
			"player_id", award.PlayerID.Short(), "source", "mob kill (offline tap)",
			"amount", amount, "mob_kind", target.kind.String(), "share_count", 1)
		return
	}

	recipients := []*Player{owner}
	source := "mob kill"
	if owner.partyID != 0 {
		recipients = owner.membersNearLocked(target.pos, PartyShareRadius)
		source = "mob kill (shared)"
	}

	shareCount := uint32(len(recipients))
	share, remainder := amount/shareCount, amount%shareCount
	for _, recipient := range recipients {
		received := share
		if recipient == owner {
			received += remainder
		}
		s.awardExperienceLocked(recipient, received)
		s.log.Debug("experience awarded",
			"entity_id", recipient.entityID, "source", source, "amount", received,
			"mob_kind", target.kind.String(), "share_count", shareCount)
	}
}

type armedAttack struct {
	meleeDamage uint16
	launches    vnet.ProjectileKind
}

// launchParameters is the authoritative cadence and initial speed for each projectile
// kind a registry row may launch. Unknown kinds fail closed.
func (s *Sim) launchParameters(kind vnet.ProjectileKind) (uint32, float64) {
	switch kind {
	case vnet.ProjectileKindArrow:
		return s.bowCooldownTicks, ArrowSpeed
	case vnet.ProjectileKindEnergyOrb:
		return s.sceptreCooldownTicks, OrbSpeed
	default:
		return 0, 0
	}
}

// armedForAttackLocked is what the named slot's contents do, and whether the inventory
// could be read at all. A launch consumes its first non-equipment ammunition and one point
// of launcher durability inside this same TryLock window.
//
// Two answers rather than one, because "nothing" and "could not ask" have to be told
// apart: the first ends the swing and the second postpones it. The tick takes the
// inventory lock only if it is free, exactly as a pickup and an inventory delivery do.
//
// **The damage comes from the registry rather than from a comparison against an item
// id.** It used to ask whether the slot held `ItemRustySword`, which was one weapon's name
// spelled inside the combat code; it now asks what the slot is worth, and a zero — every
// resource, every structure, the empty slot — is the same refusal it always was. The
// second blade is therefore a registry entry, and the third will be too.
func (p *Player) armedForAttackLocked(slot uint8) (armedAttack, bool) {
	if !p.inventory.mu.TryLock() {
		return armedAttack{}, false
	}
	defer p.inventory.mu.Unlock()

	stack, ok := p.inventory.stackAtLocked(slot)
	if !ok {
		return armedAttack{}, true
	}
	definition, registered := itemByID(stack.item)
	if !registered || (definition.meleeDamage == 0 && definition.launches == vnet.ProjectileKindUnknown) {
		return armedAttack{}, true
	}
	// Zero durability *under a non-zero maximum* is a blade that is worn through: still
	// carried, still in its slot, and no longer a weapon. A sharpening stone is what
	// brings one back — see `Player.Repair` — and an ordinary hit still does not wear one
	// further.
	//
	// The maximum is what the test asks about, never the current value alone. A weapon
	// that does not wear out carries `(0, 0)` like every resource does, and reading that
	// pair as "worn through" would make it permanently unusable the moment somebody
	// registered one.
	if stack.durable() && stack.durability == 0 {
		return armedAttack{}, true
	}

	if definition.launches != vnet.ProjectileKindUnknown {
		if definition.ammunition != ItemNone {
			ammunitionSlot := -1
			for candidate := range p.inventory.slots[:equipmentFirst] {
				stack := p.inventory.slots[candidate]
				if stack.item == definition.ammunition && stack.count > 0 {
					ammunitionSlot = candidate
					break
				}
			}
			if ammunitionSlot < 0 {
				// The session-side check already explained the ordinary case. This is the
				// race where the last arrow moved before the tick, so the queued launch simply
				// disappears and spends nothing.
				return armedAttack{}, true
			}
			ammunition := &p.inventory.slots[ammunitionSlot]
			ammunition.count--
			if ammunition.count == 0 {
				*ammunition = inventoryStack{}
			}
		}
		launcher := &p.inventory.slots[slot]
		if launcher.durable() {
			launcher.durability--
		}
		p.inventoryDirty = true
		return armedAttack{launches: definition.launches}, true
	}
	return armedAttack{meleeDamage: definition.meleeDamage}, true
}

// swingTargetLocked is the mob a swing lands on, or nil.
//
// Range is measured body to body and the arc is a dot product against where the player is
// looking — not a per-axis cube, which would reach further diagonally than it claims to
// and make the corner of a swing worth more than its centre.
//
// The direction is taken from the player's body centre rather than from an eye. The
// server measures reach from the body centre everywhere else (see distanceToVoxel), and
// where the *eyes* are is documented on the client as the client's business. A cone this
// wide is forgiving enough that the difference does not decide a swing, and reconciling
// what the two sides measure is the separate issue client/src/player/constants.rs names.
//
// **Nothing here asks what species it is about to hit**, and the body it measures
// against comes from that creature's own registry row. It used to be draugrBody, spelled
// once and correct for exactly one species: a vargr is wider and much shorter, so a
// hardcoded box would have given it a draugr's reach in both directions — reaching a
// vargr that was too far away and missing one standing at its own edge.
//
// **A body is not a target, and it is now structurally not one.** A corpse is not in
// Sim.mobs at all — the killing blow moves it to the corpse collection in the same call —
// so the search below cannot return one. The zero-health guard stays as the invariant
// rather than as a live case: what it protects against is a swing being *spent* on a body,
// since the search returns the nearest candidate and a corpse lying between a player and
// the draugr behind it would otherwise absorb every swing. This used to be the only thing
// standing between a player and that, because a killed creature spent two and a half
// seconds in Sim.mobs before it stopped being one.
//
// O(mobs) per swing, on the same explicit trade the mob's own target selection records.
// The caller holds Sim.mu.
func (s *Sim) swingTargetLocked(p *Player) *mob {
	aim := lookDirection(p.current.yaw, p.current.pitch)
	origin := boxCentre(playerBox(p.pos))
	reach := playerBox(p.pos)

	var best *mob
	bestDistance := math.Inf(1)

	// Sorted, so two mobs at the same distance resolve by identity rather than by
	// whichever the map happened to yield first.
	for _, m := range s.sortedMobsLocked() {
		if m.health == 0 {
			continue
		}
		body := m.species().body.boxAt(m.pos)

		distance := boxDistance(reach, body)
		if distance > SwordReach {
			continue
		}

		toward := [3]float64{
			boxCentre(body)[0] - origin[0],
			boxCentre(body)[1] - origin[1],
			boxCentre(body)[2] - origin[2],
		}
		length := math.Sqrt(toward[0]*toward[0] + toward[1]*toward[1] + toward[2]*toward[2])
		if length > 0 {
			// Normalised before the dot product, which is what makes the comparison an
			// angle rather than a distance in disguise.
			dot := (aim[0]*toward[0] + aim[1]*toward[1] + aim[2]*toward[2]) / length
			if dot < swordConeCosine {
				continue
			}
		}
		// A zero length means the two centres coincide, which is a mob standing inside
		// the player. There is no direction to test and no reading of "in front of" that
		// excludes it, so it stays a candidate.

		if distance < bestDistance {
			best, bestDistance = m, distance
		}
	}
	return best
}

// lookDirection is the unit vector a yaw and pitch point along.
//
// The same basis the movement integrator uses — yaw 0 looks along -Z and +X is to its
// right — extended by a pitch that is positive upwards. It matches what the client's
// camera builds from the same two numbers (`Quat::from_rotation_y(yaw) *
// Quat::from_rotation_x(pitch)` applied to -Z), which is what makes the server's verdict
// agree with what the player was looking at.
func lookDirection(yaw, pitch float64) [3]float64 {
	sinYaw, cosYaw := math.Sincos(yaw)
	sinPitch, cosPitch := math.Sincos(pitch)
	return [3]float64{-sinYaw * cosPitch, sinPitch, -cosYaw * cosPitch}
}

// boxCentre is the middle of a box.
func boxCentre(b box) [3]float64 {
	return [3]float64{
		(b.min[0] + b.max[0]) / 2,
		(b.min[1] + b.max[1]) / 2,
		(b.min[2] + b.max[2]) / 2,
	}
}
