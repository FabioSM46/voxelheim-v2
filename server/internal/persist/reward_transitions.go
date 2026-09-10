package persist

import (
	"errors"
	"math"
	"path/filepath"
	"reflect"
	"slices"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// Snapshot is an owned, bounded snapshot; mutating it cannot change journal state.
func (s *RewardStore) Snapshot() (RewardJournal, error) {
	if s == nil {
		return RewardJournal{}, ErrRewardJournalConflict
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	return cloneRewardJournal(s.journal)
}
func cloneRewardJournal(j RewardJournal) (RewardJournal, error) {
	b, err := encodeRewardJournal(j)
	if err != nil {
		return RewardJournal{}, err
	}
	return decodeRewardJournal(b)
}

// EnableStrictRewardReceipts is irreversible for this Store. The generation writer
// calls it BEFORE its first I/O attempt, including one whose outcome is uncertain.
// Startup derives the same decision from the never-decreasing generation high-water.
func (s *Store) EnableStrictRewardReceipts() {
	if s == nil {
		return
	}
	s.recordMu.Lock()
	s.strictRewards.Store(true)
	s.recordMu.Unlock()
}

// Store identity is immutable after open. Every cross-store transition must pair
// the journal with the players directory from that same world before sealing it.
func (s *RewardStore) matchesPlayers(players *Store) bool {
	if s == nil || players == nil || players.dir == "" {
		return false
	}
	expected, err := filepath.Abs(filepath.Join(filepath.Dir(players.dir), rewardFileName))
	if err != nil {
		return false
	}
	actual, err := filepath.Abs(s.path)
	return err == nil && expected == actual
}

// AllocateRun uses a generation captured by the caller, not an implicit mint on
// every retry. Repeating the exact allocation is idempotent after uncertain success.
// Baseline defeated bosses from older saved runs acquire no new reward entitlement.
//
// defeats, when given, are the run's defeats with their frozen entitlements, one per
// record.DefeatedBosses entry in the same order, so identity, progress and what each
// defeat owes become durable in one write. Every entitlement in them must be unconsumed.
func (s *RewardStore) AllocateRun(players *Store, generation uint64, record SessionRecord, content uint32, defeats ...RewardDefeat) error {
	if !s.matchesPlayers(players) {
		return ErrRewardJournalConflict
	}
	run := RewardRun{Generation: generation, ContentVersion: content, Session: record}
	if len(defeats) == 0 {
		for _, kind := range record.DefeatedBosses {
			run.Defeats = append(run.Defeats, RewardDefeat{Kind: kind})
		}
	} else {
		if len(defeats) != len(record.DefeatedBosses) {
			return ErrRewardJournalConflict
		}
		for i, defeat := range defeats {
			if defeat.Kind != record.DefeatedBosses[i] || !unconsumedRewardDefeat(defeat) {
				return ErrRewardJournalConflict
			}
		}
		run.Defeats = defeats
	}
	players.EnableStrictRewardReceipts()
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, existing := range s.journal.Runs {
		if existing.Generation == generation {
			if sameRewardRun(existing, run) {
				return nil
			}
			return ErrRewardJournalConflict
		}
	}
	if generation != s.journal.NextGeneration || generation == math.MaxUint64 {
		return ErrRewardJournalConflict
	}
	next, err := cloneRewardJournal(s.journal)
	if err != nil {
		return err
	}
	next.Runs = append(next.Runs, run)
	next.NextGeneration++
	next.Revision++
	return s.commitLocked(s.journal.Revision, next, nil)
}

// AppendDefeat accepts already-rolled personal containers and the independent XP
// recipients. New entitlements must be wholly unconsumed. A retry must be exactly
// the stored defeat: an original retry after consumption conflicts, never resets
// taken flags. Producer publication therefore follows its confirmed append.
// Bindings are unioned so an older whole-session snapshot cannot unbind a character.
func (s *RewardStore) AppendDefeat(generation uint64, defeat RewardDefeat, bound []SessionCharacter) error {
	if s == nil {
		return ErrRewardJournalConflict
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	next, err := cloneRewardJournal(s.journal)
	if err != nil {
		return err
	}
	for i := range next.Runs {
		run := &next.Runs[i]
		if run.Generation != generation {
			continue
		}
		known := false
		for _, old := range run.Defeats {
			if old.Kind == defeat.Kind {
				if !reflect.DeepEqual(old, defeat) {
					return ErrRewardJournalConflict
				}
				known = true
			}
		}
		changed := false
		if !known {
			if !unconsumedRewardDefeat(defeat) {
				return ErrRewardJournalConflict
			}
			run.Defeats = append(run.Defeats, defeat)
			run.Session.DefeatedBosses = append(run.Session.DefeatedBosses, defeat.Kind)
			changed = true
		}
		for _, owner := range bound {
			if !slices.Contains(run.Session.Bound, owner) {
				run.Session.Bound = append(run.Session.Bound, owner)
				changed = true
			}
		}
		if !changed {
			return nil
		}
		next.Revision++
		return s.commitLocked(s.journal.Revision, next, nil)
	}
	return ErrRewardJournalConflict
}

// unconsumedRewardDefeat reports whether no entitlement in a new defeat is already taken.
func unconsumedRewardDefeat(defeat RewardDefeat) bool {
	for _, p := range defeat.Personal {
		if p.Taken != 0 || p.SilverTaken {
			return false
		}
	}
	for _, xp := range defeat.Experience {
		if xp.Taken {
			return false
		}
	}
	return true
}

// sameRewardRun compares a stored run with a candidate as the journal encodes them, so a
// nil list and an empty one are the same run.
func sameRewardRun(stored, candidate RewardRun) bool {
	normalized, err := cloneRewardJournal(RewardJournal{NextGeneration: candidate.Generation + 1, Runs: []RewardRun{candidate}})
	return err == nil && len(normalized.Runs) == 1 && reflect.DeepEqual(stored, normalized.Runs[0])
}

// ValidateClaim is the no-I/O check BEFORE sealing a Store reservation. A second
// check during PrepareClaim closes races with other journal changes.
func (s *RewardStore) ValidateClaim(intent RewardIntent) error {
	if s == nil {
		return ErrRewardJournalConflict
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	next, err := cloneRewardJournal(s.journal)
	if err != nil {
		return err
	}
	next.Intents = append(next.Intents, intent)
	return validateRewardJournal(next)
}

func (s *Store) sealedRewardIntent(r *RewardReservation, ref RewardReference) (RewardIntent, error) {
	if s == nil {
		return RewardIntent{}, ErrRewardReservation
	}
	s.recordMu.Lock()
	defer s.recordMu.Unlock()
	if !s.ownsRewardLocked(r) || !r.intent || ref.Owner.CharacterID != uint64(r.character) || ref.Owner.PlayerID != r.postimage.Owner {
		return RewardIntent{}, ErrRewardReservation
	}
	return RewardIntent{RewardReference: ref, PreviousEpoch: r.epoch - 1, Postimage: r.postimage}, nil
}

// PrepareClaim persists only the immutable postimage sealed by the Store barrier.
// It does not write the character or release ownership. One intent per character
// remains enforced until a verified acknowledgement itself becomes durable.
func (s *RewardStore) PrepareClaim(players *Store, r *RewardReservation, ref RewardReference) error {
	if !s.matchesPlayers(players) {
		return ErrRewardJournalConflict
	}
	intent, err := players.sealedRewardIntent(r, ref)
	if err != nil {
		return err
	}
	if s == nil {
		return ErrRewardJournalConflict
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, old := range s.journal.Intents {
		if old.Postimage.Character == intent.Postimage.Character {
			if old == intent {
				return nil
			}
			return ErrRewardPending
		}
	}
	next, err := cloneRewardJournal(s.journal)
	if err != nil {
		return err
	}
	next.Intents = append(next.Intents, intent)
	next.Revision++
	return s.commitLocked(s.journal.Revision, next, nil)
}

func sameRewardIdentity(a, b Record) bool {
	return a.Character == b.Character && a.Owner == b.Owner && a.Name == b.Name && a.Appearance == b.Appearance
}

func markRewardAcknowledged(j *RewardJournal, intent RewardIntent) error {
	at := slices.Index(j.Intents, intent)
	if at < 0 {
		return ErrRewardJournalConflict
	}
	for i := range j.Runs {
		run := &j.Runs[i]
		if run.Generation != intent.Generation {
			continue
		}
		for k := range run.Defeats {
			d := &run.Defeats[k]
			if d.Kind != intent.Boss {
				continue
			}
			for n := range d.Personal {
				p := &d.Personal[n]
				if p.Owner == intent.Owner {
					p.Taken |= intent.Entries
					if intent.Silver {
						p.SilverTaken = true
					}
				}
			}
			for n := range d.Experience {
				xp := &d.Experience[n]
				if xp.Owner == intent.Owner && intent.Experience {
					xp.Taken = true
				}
			}
		}
	}
	j.Intents = slices.Delete(j.Intents, at, at+1)
	return nil
}

// AcknowledgeClaim verifies the durable write on the owned reservation, not a
// supplied receipt. Call after atomic live publication; keep the reservation until
// this acknowledgement succeeds. An uncertain write never releases the barrier.
func (s *RewardStore) AcknowledgeClaim(players *Store, reservation *RewardReservation, ref RewardReference) error {
	if !s.matchesPlayers(players) {
		return ErrRewardJournalConflict
	}
	proof, proofErr := players.durableRewardIntent(reservation, ref)
	if proofErr != nil {
		return proofErr
	}
	if s == nil || players == nil {
		return ErrRewardJournalConflict
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	var intent *RewardIntent
	for i := range s.journal.Intents {
		in := &s.journal.Intents[i]
		if *in == proof {
			intent = in
			break
		}
	}
	if intent == nil {
		return ErrRewardJournalConflict
	}
	next, err := cloneRewardJournal(s.journal)
	if err != nil {
		return err
	}
	if err := markRewardAcknowledged(&next, *intent); err != nil {
		return err
	}
	next.Revision++
	return s.commitLocked(s.journal.Revision, next, []RewardIntent{*intent})
}

var ErrRewardRecoveryRequired = errors.New("persist: reward receipt cannot be recovered safely")

func rewardDefeatedKinds(run RewardRun) []vnet.MobKind {
	out := make([]vnet.MobKind, len(run.Defeats))
	for i, d := range run.Defeats {
		out[i] = d.Kind
	}
	return out
}

// A readable epoch is not a durability acknowledgement: a directory sync may have
// failed after rename. Only a successful prepared write on the still-owned token
// provides this proof; startup uses a separate pre-index durability confirmation.
func (s *Store) durableRewardIntent(r *RewardReservation, ref RewardReference) (RewardIntent, error) {
	if s == nil {
		return RewardIntent{}, ErrRewardReservation
	}
	s.recordMu.Lock()
	defer s.recordMu.Unlock()
	if !s.ownsRewardLocked(r) || !r.intent || !r.written || ref.Owner.CharacterID != uint64(r.character) || ref.Owner.PlayerID != r.postimage.Owner {
		return RewardIntent{}, ErrRewardReservation
	}
	rec, found, err := s.Load(r.character)
	if err != nil {
		return RewardIntent{}, err
	}
	if !found || !sameRewardIdentity(rec, r.postimage) || rec.BossRewardEpoch < r.epoch {
		return RewardIntent{}, ErrRewardEpoch
	}
	return RewardIntent{RewardReference: ref, PreviousEpoch: r.epoch - 1, Postimage: r.postimage}, nil
}
