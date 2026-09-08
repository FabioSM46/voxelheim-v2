package persist

import (
	"math"
	"slices"

	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func validRewardOwner(o SessionCharacter) bool {
	return o.CharacterID != 0 && o.PlayerID != (identity.PlayerID{})
}
func rewardMask(n int) uint64 {
	if n == 64 {
		return math.MaxUint64
	}
	return (uint64(1) << n) - 1
}

func validateRewardJournal(j RewardJournal) error {
	bad := world.ErrCorruptStore
	if j.NextGeneration == 0 || len(j.Runs) > MaxRewardRuns || len(j.Intents) > MaxRewardIntents {
		return bad
	}
	generations := make(map[uint64]*RewardRun, len(j.Runs))
	bindings, recipients, entries := 0, 0, 0
	for i := range j.Runs {
		run := &j.Runs[i]
		s := run.Session
		if run.Generation == 0 || run.Generation >= j.NextGeneration || generations[run.Generation] != nil || s.ID == 0 || run.ContentVersion == 0 || s.ExpiresUnix <= 0 || len(s.Bound) > MaxBoundCharacters || len(run.Defeats) > MaxDefeatedBosses || len(s.DefeatedBosses) != len(run.Defeats) {
			return bad
		}
		generations[run.Generation] = run
		if len(s.Bound) > MaxRewardBindings-bindings {
			return bad
		}
		bindings += len(s.Bound)
		owners := make(map[SessionCharacter]bool, len(s.Bound))
		for _, o := range s.Bound {
			if !validRewardOwner(o) || owners[o] {
				return bad
			}
			owners[o] = true
		}
		for index, d := range run.Defeats {
			if d.Kind == 0 || s.DefeatedBosses[index] != d.Kind || slices.Contains(s.DefeatedBosses[:index], d.Kind) {
				return bad
			}
			if len(d.Personal) > MaxRewardRecipients-recipients {
				return bad
			}
			recipients += len(d.Personal)
			if len(d.Experience) > MaxRewardRecipients-recipients {
				return bad
			}
			recipients += len(d.Experience)
			personal := make(map[SessionCharacter]bool, len(d.Personal))
			for _, p := range d.Personal {
				if !validRewardOwner(p.Owner) || personal[p.Owner] || len(p.Entries) > MaxPersonalRewardEntries || p.Taken & ^rewardMask(len(p.Entries)) != 0 || (p.SilverTaken && p.Silver == 0) {
					return bad
				}
				personal[p.Owner] = true
				if len(p.Entries) > MaxRewardEntries-entries {
					return bad
				}
				entries += len(p.Entries)
				for _, item := range p.Entries {
					if item.ItemID == 0 || item.Count == 0 || item.Durability > item.MaxDurability {
						return bad
					}
				}
			}
			xpOwners := make(map[SessionCharacter]bool, len(d.Experience))
			for _, xp := range d.Experience {
				if !validRewardOwner(xp.Owner) || xpOwners[xp.Owner] || xp.Amount == 0 {
					return bad
				}
				xpOwners[xp.Owner] = true
			}
		}
	}
	claimed := make(map[CharacterID]bool, len(j.Intents))
	for _, in := range j.Intents {
		rec := in.Postimage
		run := generations[in.Generation]
		if run == nil || !validRewardOwner(in.Owner) || rec.Character != CharacterID(in.Owner.CharacterID) || rec.Owner != in.Owner.PlayerID || claimed[rec.Character] || in.PreviousEpoch == math.MaxUint64 || rec.BossRewardEpoch != in.PreviousEpoch+1 || len(rec.Name) > MaxNameBytes {
			return bad
		}
		if validateRewardPostimage(rec) != nil {
			return bad
		}
		claimed[rec.Character] = true
		if in.Entries == 0 && !in.Silver && !in.Experience {
			return bad
		}
		found := false
		for _, d := range run.Defeats {
			if d.Kind != in.Boss {
				continue
			}
			found = true
			if in.Entries != 0 || in.Silver {
				matched := false
				for _, p := range d.Personal {
					if p.Owner != in.Owner {
						continue
					}
					matched = true
					if in.Entries & ^rewardMask(len(p.Entries)) != 0 || in.Entries&p.Taken != 0 || (in.Silver && (p.Silver == 0 || p.SilverTaken)) {
						return bad
					}
				}
				if !matched {
					return bad
				}
			}
			if in.Experience {
				matched := false
				for _, xp := range d.Experience {
					if xp.Owner == in.Owner {
						matched = true
						if xp.Taken {
							return bad
						}
					}
				}
				if !matched {
					return bad
				}
			}
		}
		if !found {
			return bad
		}
	}
	return nil
}

// Structural postimage checks shared by the journal and the pre-intent barrier.
// Game must additionally validate registry/balance semantics before reservation.
func validateRewardPostimage(rec Record) error {
	bad := world.ErrCorruptStore
	if rec.Character == 0 || rec.Owner == (identity.PlayerID{}) || len(rec.Name) > MaxNameBytes {
		return bad
	}
	name, _, err := AcceptName(rec.Name)
	if err != nil || name != rec.Name || rec.Appearance.Validate() != nil {
		return bad
	}
	for _, v := range rec.Pos {
		if math.IsNaN(v) || math.IsInf(v, 0) || math.Abs(v) > math.MaxFloat32 {
			return bad
		}
	}
	if math.IsNaN(rec.Yaw) || math.IsInf(rec.Yaw, 0) || math.Abs(rec.Yaw) > math.Pi {
		return bad
	}
	for _, item := range rec.Slots {
		if item.ItemID == 0 {
			if item.Count != 0 || item.Durability != 0 || item.MaxDurability != 0 {
				return bad
			}
		} else if item.Count == 0 || item.Durability > item.MaxDurability {
			return bad
		}
	}
	return nil
}
