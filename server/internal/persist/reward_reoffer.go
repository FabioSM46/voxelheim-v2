package persist

import "slices"

// UntouchedLoot is every defeat of the run with this generation whose held personal loot the
// journal still owes whole, in death order (#1036). It is what may be offered again after a
// restart, rebuilt from the frozen rolls exactly as they were frozen.
//
// **A defeat qualifies only while nothing about its loot has been delivered.**
//   - No owner has taken an entry or its silver.
//   - No prepared intent names the defeat.
//
// A defeat somebody has taken from, or that has no loot at all, stays owed in the journal and
// is not rebuilt. Experience is not held by a corpse and is not considered here: it is claimed
// from the journal directly.
func (j RewardJournal) UntouchedLoot(generation uint64) []RewardDefeat {
	var out []RewardDefeat
	for _, run := range j.Runs {
		if run.Generation != generation {
			continue
		}
		for _, d := range run.Defeats {
			if len(d.Personal) == 0 || slices.ContainsFunc(d.Personal, func(p PersonalReward) bool { return p.Taken != 0 || p.SilverTaken }) {
				continue
			}
			if slices.ContainsFunc(j.Intents, func(in RewardIntent) bool { return in.Generation == generation && in.Boss == d.Kind }) {
				continue
			}
			out = append(out, d)
		}
	}
	return out
}
