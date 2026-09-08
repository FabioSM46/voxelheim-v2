package persist

import (
	"errors"
	"math"
)

var (
	ErrRewardPending     = errors.New("persist: character has a pending boss reward")
	ErrRewardEpoch       = errors.New("persist: stale or unauthorized boss reward epoch")
	ErrRewardReservation = errors.New("persist: invalid boss reward reservation")
)

// RewardReservation is an opaque, single-character write barrier. The future reward
// coordinator must establish it under the identity writer lock BEFORE capturing the
// live postimage. Neither this token nor any disk operation holds a simulation lock.
// The barrier outlives record durability: ReleaseReward follows live publication.
// No gameplay caller uses these APIs until durable intent replay is wired.
type RewardReservation struct {
	character CharacterID
	epoch     uint64
	intent    bool
	written   bool
	postimage Record
}

func (s *Store) rewardFloorLocked(id CharacterID) (uint64, error) {
	if floor, ok := s.rewardFloors[id]; ok {
		return floor, nil
	}
	rec, _, err := s.Load(id)
	if err != nil {
		return 0, err
	}
	if s.rewardFloors == nil {
		s.rewardFloors = make(map[CharacterID]uint64)
	}
	s.rewardFloors[id] = rec.BossRewardEpoch
	return rec.BossRewardEpoch, nil
}

func (s *Store) checkRewardSaveLocked(id CharacterID, rec Record) error {
	if s.rewardFences[id] != nil {
		return ErrRewardPending
	}
	if s.strictRewards.Load() {
		if _, found, err := s.Load(id); err != nil {
			return err
		} else if !found {
			return ErrRewardRecoveryRequired
		}
	}
	floor, err := s.rewardFloorLocked(id)
	if err != nil {
		return err
	}
	if rec.BossRewardEpoch != floor {
		return ErrRewardEpoch
	}
	return nil
}

// ReserveReward waits for any already-started Store.Save, then returns its latest
// durable result and installs the barrier in the same critical section. The caller
// must merge this baseline (including offline XP) before preparing a live postimage.
// Ephemeral stores refuse durable reservations; their gameplay remains in-memory.
func (s *Store) ReserveReward(id CharacterID) (*RewardReservation, Record, error) {
	if s == nil || s.dir == "" {
		return nil, Record{}, ErrRewardReservation
	}
	s.recordMu.Lock()
	defer s.recordMu.Unlock()
	if s.rewardFences[id] != nil {
		return nil, Record{}, ErrRewardPending
	}
	if _, known := s.Character(id); !known {
		return nil, Record{}, ErrUnknownCharacter
	}
	rec, found, err := s.Load(id)
	if err != nil {
		return nil, Record{}, err
	}
	if !found {
		return nil, Record{}, ErrRewardReservation
	}
	if rec.BossRewardEpoch == math.MaxUint64 {
		return nil, Record{}, ErrRewardEpoch
	}
	floor, err := s.rewardFloorLocked(id)
	if err != nil {
		return nil, Record{}, err
	}
	if rec.BossRewardEpoch != floor {
		return nil, Record{}, ErrRewardEpoch
	}
	token := &RewardReservation{character: id, epoch: floor + 1}
	if s.rewardFences == nil {
		s.rewardFences = make(map[CharacterID]*RewardReservation)
	}
	s.rewardFences[id] = token
	return token, rec, nil
}

func (s *Store) ownsRewardLocked(r *RewardReservation) bool {
	return r != nil && s.rewardFences[r.character] == r
}

// BeginRewardIntent seals the exact postimage BEFORE attempting the journal write.
// Validate the complete journal and entitlement references before calling this.
// Once sealed, even a failed/uncertain journal write cannot release the reservation.
// Retry the same immutable intent; do not recapture inventory or allocate a new epoch.
func (s *Store) BeginRewardIntent(r *RewardReservation, postimage Record) error {
	if s == nil {
		return ErrRewardReservation
	}
	s.recordMu.Lock()
	defer s.recordMu.Unlock()
	if !s.ownsRewardLocked(r) || r.intent {
		return ErrRewardReservation
	}
	character, ok := s.Character(r.character)
	if !ok || postimage.Character != r.character || postimage.Owner != character.Owner ||
		postimage.Name != character.Name || postimage.Appearance != character.Appearance || postimage.BossRewardEpoch != r.epoch {
		return ErrRewardReservation
	}
	if err := validateRewardPostimage(postimage); err != nil {
		return err
	}
	r.postimage, r.intent = postimage, true
	return nil
}

// WritePreparedReward is called ONLY after the exact sealed intent is durable in
// the reward journal. It may be retried after any I/O error, including rename having
// succeeded but directory sync failing. The barrier remains until live publication.
func (s *Store) WritePreparedReward(r *RewardReservation) error {
	if s == nil {
		return ErrRewardReservation
	}
	s.recordMu.Lock()
	defer s.recordMu.Unlock()
	if !s.ownsRewardLocked(r) || !r.intent {
		return ErrRewardReservation
	}
	character, known := s.Character(r.character)
	if !known {
		return ErrUnknownCharacter
	}
	if err := s.writeRecord(character, r.postimage); err != nil {
		return err
	}
	s.rewardFloors[r.character] = r.epoch
	r.written = true
	return nil
}

// ReleaseReward must follow atomic live publication of inventory, silver and epoch.
// A stale capture still contains the old epoch and is refused after this release.
func (s *Store) ReleaseReward(r *RewardReservation) error {
	if s == nil {
		return ErrRewardReservation
	}
	s.recordMu.Lock()
	defer s.recordMu.Unlock()
	if !s.ownsRewardLocked(r) || !r.written {
		return ErrRewardReservation
	}
	delete(s.rewardFences, r.character)
	return nil
}

// AbortReward is available only before the first journal write could have begun.
func (s *Store) AbortReward(r *RewardReservation) error {
	if s == nil {
		return ErrRewardReservation
	}
	s.recordMu.Lock()
	defer s.recordMu.Unlock()
	if !s.ownsRewardLocked(r) || r.intent {
		return ErrRewardReservation
	}
	delete(s.rewardFences, r.character)
	return nil
}
