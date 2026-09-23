package game

import "math"

// How a boss answers the party that pulled it (#1099).
//
// **The only input is the levels of the characters inside the run at the pull.** Not their
// equipment, not a gear score, not a class, and not anything a player can change between
// the pull and the kill: the repository owner chose levels because a level cannot be taken
// off at the door. Two rules are derived from that one list, and each is argued from what a
// level actually changes in this server rather than from a multiplier somebody picked.
//
//   - **Health grows with the members, and a member's level does not change their share.**
//     A level raises a character's maximum health ([maxHealthFor]) and nothing else: a blow's
//     damage is the blade's (`RustySwordDamage`, `IronSwordDamage`) and the cadence is
//     `SwordCooldown` at every level. A share that grew with level would therefore make the
//     same party with the same blades fight longer for having levelled, which is the one
//     direction a fight-length target cannot survive. So each member brings the species'
//     per-member health — [mobDefinition.maxHealth] — and the boss arrives with that times
//     the members, clamped to three to five ([dungeonMembers]).
//   - **Damage follows the level, through the same curve a level gives a player.** A blow is
//     scaled by the party's mean of `maxHealthFor(level) / PlayerMaxHealth`, so a boss blow
//     costs a member the same share of their health at level 30 as the registry prices it at
//     level 1. That ratio is the whole of what a level buys, which is why it is the whole of
//     what the boss answers it with.
//
// **Computed once, when the encounter is created, and never again.** A member who levels up,
// changes armour, dies, disconnects or returns during the fight changes nothing about it: the
// numbers live on [bossEncounter] and a new encounter is the only thing that recomputes them.
// A wipe replaces the boss with a fresh one (see dungeon_recovery.go), whose next pull is a
// new encounter and a new measurement of who is there.
//
// **Nobody may enter a run while its boss is engaged** (see [Sim.dungeonCombat]), so the
// characters inside at the pull are the party that fights it. A character who had
// disconnected before the pull and returns during it is not counted; the fight they return
// to is the one the others pulled.

// How many members a dungeon is sized for (#1332).
//
// **A dungeon is group content, made for parties of three to five.** The repository owner
// decided it on 2026-09-23, after the #1298 bot could clear the first dungeon solo only
// under `/immortal`: a party of one or two meets the dungeon sized for three, and finishing
// it that way is the balance saying no. Entry is not refused below three — the numbers are
// the refusal, and TestASoloDelverDiesBeforeKillingAnything is where they are held to it.
//
//   - **Three is the floor.** Every boss is sized for at least three members' health, and
//     every lesser group is a pack of at least three (dungeon_balance.go). One player
//     therefore does three members' work against three members' blows; the #1099 stander,
//     which swings as fast as energy allows and never steps aside, dies to either boss and
//     to one pack long before it can finish them.
//   - **Five is the ceiling, and it is [MaxPartySize].** A party cannot be larger, so the
//     scale never has a member it does not count: every member adds their share of health,
//     which is what keeps a fight the same length for three, four and five.
//   - **Every member present still counts toward the damage mean**, one or five: a blow's
//     scale answers the health of whoever it lands on, and the clamp decides only how many
//     shares of health there are.
//
// The bound it keeps is the wire's: [protocol.MobState] carries health as a uint16, and
// `TestBossHealthFitsTheWireAtEveryScale` holds every boss row's per-member health times
// [bossScaleMaxMembers] under its ceiling.
const (
	bossScaleMinMembers = 3
	bossScaleMaxMembers = MaxPartySize
)

// dungeonMembers is how many members a party of n is sized as: n clamped to
// [bossScaleMinMembers, bossScaleMaxMembers]. The boss's health and the lesser creatures'
// pack size both read it, so the two can never disagree about who is in the room.
func dungeonMembers(n int) int {
	return min(max(n, bossScaleMinMembers), bossScaleMaxMembers)
}

// bossScale is what one pull decided about a boss. The zero value is the registry row itself,
// which is what an encounter created by hand in a test reads as.
type bossScale struct {
	// members is how many characters the boss's health was sized for, in
	// bossScaleMinMembers..bossScaleMaxMembers.
	members uint16

	// maxHealth is the species' per-member health times members.
	maxHealth uint16

	// damagePercent is every blow's size as a percentage of the registry's, in
	// 100..bossScaleMaxDamagePercent.
	damagePercent uint16
}

// bossScaleMaxDamagePercent is the heaviest a boss's blows can become: the damage scale of a
// party whose every member is at [MaxLevel].
var bossScaleMaxDamagePercent = levelDamagePercent(MaxLevel)

// levelDamagePercent is how much heavier a blow is against a character of this level than
// against one at level one, as a percentage: that character's maximum health against
// [PlayerMaxHealth].
func levelDamagePercent(level uint16) uint16 {
	return uint16(uint32(maxHealthFor(level)) * 100 / uint32(PlayerMaxHealth))
}

// bossScaleFor is the scale a boss of this species takes from a party with these levels.
//
// Levels outside 1..[MaxLevel] are clamped into it. The member count is [dungeonMembers] of
// the list's length, so an empty list — a caller's defect, since a boss is only ever pulled
// by somebody — is the smallest party the scale knows, at the registry's blow.
//
// Integer arithmetic throughout, so the same party is the same boss on every run.
func bossScaleFor(def mobDefinition, levels []uint16) bossScale {
	members := uint16(dungeonMembers(len(levels)))
	scale := bossScale{
		members:       members,
		maxHealth:     uint16(min(uint32(def.maxHealth)*uint32(members), math.MaxUint16)),
		damagePercent: 100,
	}
	if len(levels) == 0 {
		return scale
	}
	var total uint32
	for _, level := range levels {
		total += uint32(maxHealthFor(min(max(level, 1), MaxLevel)))
	}
	// The mean of maxHealthFor over the party, against PlayerMaxHealth, rounded to the
	// nearest percent.
	denominator := uint32(len(levels)) * uint32(PlayerMaxHealth)
	percent := (total*100 + denominator/2) / denominator
	scale.damagePercent = uint16(min(max(percent, 100), uint32(bossScaleMaxDamagePercent)))
	return scale
}

// blow is a raw registry blow under this scale. A blow worth anything is worth at least one,
// on the rule [moveDamage] already applies; the zero scale leaves the blow as it is.
func (sc bossScale) blow(raw uint16) uint16 {
	if raw == 0 || sc.damagePercent == 0 {
		return raw
	}
	return uint16(min(max(uint32(raw)*uint32(sc.damagePercent)/100, 1), math.MaxUint16))
}

// bossScaleLevelsLocked is the level of every character inside this simulation, alive or
// dead: the party a pull is made by. The order is not meaningful — the scale reads a count
// and a sum. The caller holds Sim.mu.
func (s *Sim) bossScaleLevelsLocked() []uint16 {
	levels := make([]uint16, 0, len(s.players))
	for _, p := range s.players {
		levels = append(levels, levelFor(p.experience))
	}
	return levels
}

// maxHealth is this creature's health ceiling: the pull's scale once a boss is engaged, the
// dungeon's tier over the row for a creature the dungeon placed (dungeon_balance.go), and
// the registry row before that and for every other species.
func (m *mob) maxHealth() uint16 {
	if m.encounter != nil && m.encounter.scale.maxHealth != 0 {
		return m.encounter.scale.maxHealth
	}
	if m.tiered {
		return tierScaled(m.species().maxHealth, dungeonHealthTier)
	}
	return m.species().maxHealth
}
