package persist

import "slices"

// OwedLoot is every defeat of the run with this generation whose held personal loot the journal
// still owes, in death order (#1036). It is what a restart offers again, rebuilt from the frozen
// rolls.
//
// **Only what each owner has not taken.** Following the owner decision recorded on #1036, partly
// taken loot is offered again:
//   - Each owner keeps its entries outside Taken, at their original indices, and its silver
//     unless SilverTaken.
//   - An owner who has taken everything is left out, and so is an owner a prepared intent for
//     this defeat names.
//   - A defeat with no owner left is left out.
//
// The journal's per-owner taken record is the only source, so a rebuilt corpse never offers
// anything twice. Experience is not held by a corpse and is not returned: it is claimed from the
// journal directly.
func (j RewardJournal) OwedLoot(generation uint64) []RewardDefeat {
	var out []RewardDefeat
	for _, run := range j.Runs {
		if run.Generation != generation {
			continue
		}
		for _, d := range run.Defeats {
			owed := RewardDefeat{Kind: d.Kind}
			for _, p := range d.Personal {
				pending := slices.ContainsFunc(j.Intents, func(in RewardIntent) bool {
					return in.Generation == generation && in.Boss == d.Kind && in.Owner == p.Owner
				})
				if owesLoot(p) && !pending {
					owed.Personal = append(owed.Personal, p)
				}
			}
			if len(owed.Personal) > 0 {
				out = append(out, owed)
			}
		}
	}
	return out
}

// owesLoot reports whether an owner has an entry or silver it has not taken.
func owesLoot(p PersonalReward) bool {
	return p.Taken != rewardMask(len(p.Entries)) || (p.Silver != 0 && !p.SilverTaken)
}
