package game

// MaxLevel is the last level the progression curve can produce.
//
// Thirty levels make the linear steps below accumulate to 21,750 lifetime
// experience: a bounded total that fits comfortably in the persisted uint32 while
// leaving twenty-nine distinct boundaries for later rewards and health growth.
const MaxLevel uint16 = 30

// ExperiencePerLevelStep is how much more experience each successive level costs.
//
// Fifty makes the first boundary cost 50 and the last reachable one cost 1,450,
// twenty-nine times as much. The curve therefore grows without making the foundation
// depend on any source of experience, which belongs to the issues that award it.
const ExperiencePerLevelStep uint32 = 50

// ExperienceCap is the lifetime total at which MaxLevel begins.
//
// It is derived from the arithmetic series of the twenty-nine reachable boundaries:
// 50 * (1 + ... + 29) = 21,750. Keeping the expression beside the two inputs makes a
// level-cap change move the persisted bound and every saturation point with it.
const ExperienceCap uint32 = ExperiencePerLevelStep * uint32(MaxLevel-1) * uint32(MaxLevel) / 2

// maxHealthFor derives the health ceiling for a level. Every caller obtains its level
// from levelFor, so level is always in 1..MaxLevel.
func maxHealthFor(level uint16) uint16 {
	return PlayerMaxHealth + HealthPerLevel*(level-1)
}

// maxHealthLocked is this player's current health ceiling. The caller holds sim.mu.
func (p *Player) maxHealthLocked() uint16 {
	return maxHealthFor(levelFor(p.experience))
}

// levelFor derives the current level from a lifetime total. Totals beyond the cap are
// treated as capped so a defensive caller cannot derive a level this build does not
// have; [Life.Validate] prevents such a total from entering a Player in the first place.
func levelFor(total uint32) uint16 {
	total = min(total, ExperienceCap)
	level := uint16(1)
	for level < MaxLevel && total >= experienceBefore(level+1) {
		level++
	}
	return level
}

// experienceToNext is the width of the named level on the linear curve.
func experienceToNext(level uint16) uint32 {
	return ExperiencePerLevelStep * uint32(level)
}

// experienceIntoLevel is how much of the current level a lifetime total has crossed.
// At and above the cap it is zero because the capped total is exactly the boundary at
// which MaxLevel begins. The wire deliberately represents that terminal state as a
// full bar instead; [Player.vitalsLocked] owns that contract-specific translation.
func experienceIntoLevel(total uint32) uint32 {
	total = min(total, ExperienceCap)
	return total - experienceBefore(levelFor(total))
}

// experienceBefore is the total needed to begin level. It is the one arithmetic-series
// implementation the curve uses; level derivation and progress both read it rather than
// keeping two copies of the boundary calculation.
func experienceBefore(level uint16) uint32 {
	if level <= 1 {
		return 0
	}
	completed := uint32(min(level, MaxLevel) - 1)
	return ExperiencePerLevelStep * completed * (completed + 1) / 2
}

// awardExperienceLocked adds one authoritative award and reports whether it crossed at
// least one level boundary. It saturates before adding, so even MaxUint32 cannot wrap a
// total back below the cap.
//
// The caller holds sim.mu.
func (p *Player) awardExperienceLocked(amount uint32) (leveledUp bool) {
	before := levelFor(p.experience)
	if p.experience >= ExperienceCap || amount >= ExperienceCap-p.experience {
		p.experience = ExperienceCap
	} else {
		p.experience += amount
	}
	after := levelFor(p.experience)
	if after <= before {
		return false
	}

	// Every crossed level grows a living player's current bar with its maximum, so a
	// full player stays full even when one award crosses several boundaries. A mining
	// completion may land after its player died while the world write held no Sim lock;
	// that lifetime experience still counts, but a dead player's zero health is an
	// invariant and only respawn may make it positive again.
	if p.alive() {
		p.health += HealthPerLevel * (after - before)
	}
	return true
}

// awardExperienceLocked applies one authoritative award and makes a crossed level
// visible through the existing appearance path. The next tick re-encodes the subject
// once and offers that frame to every viewer who had already been told about it.
//
// Keeping the invalidation here gives every later experience source one route through
// which to award. A source that called Player.awardExperienceLocked directly would
// update the owner's vitals but leave nearby name plates stale.
//
// The caller holds sim.mu.
func (s *Sim) awardExperienceLocked(p *Player, amount uint32) (leveledUp bool) {
	leveledUp = p.awardExperienceLocked(amount)
	if leveledUp {
		s.forgetDescribedLocked(p.entityID)
	}
	return leveledUp
}
