package persist

import (
	"errors"
	"math"
	"os"
	"reflect"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func TestRewardFenceRejectsOldCapturesUntilAndAfterPublication(t *testing.T) {
	s, dir := openStore(t)
	c := newCharacter(t, s, testID(1), "Eivor")
	old, _, err := s.Load(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	r, base, err := s.ReserveReward(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(base, old) {
		t.Fatal("reservation lost durable baseline")
	}
	next := base
	next.BossRewardEpoch++
	next.Silver = 30
	next.Experience = 90
	next.Slots[0] = protocol.InventoryStack{ItemID: 1, Count: 2}
	if err := s.Save(c.ID, old); !errors.Is(err, ErrRewardPending) {
		t.Fatalf("pre-intent save: %v", err)
	}
	if _, err := s.Quarantine(c.ID); !errors.Is(err, ErrRewardPending) {
		t.Fatalf("quarantine crossed barrier: %v", err)
	}
	if err := s.BeginRewardIntent(r, next); err != nil {
		t.Fatal(err)
	}
	next.Silver = 999
	next.Slots[0].Count = 999 // token owns the sealed value
	if err := s.AbortReward(r); !errors.Is(err, ErrRewardReservation) {
		t.Fatal("sealed intent was abortable")
	}
	if err := s.WritePreparedReward(r); err != nil {
		t.Fatal(err)
	}
	if err := s.Save(c.ID, old); !errors.Is(err, ErrRewardPending) {
		t.Fatalf("durable-before-live publication save: %v", err)
	}
	disk, _, err := s.Load(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	if disk.BossRewardEpoch != 1 || disk.Silver != 30 || disk.Slots[0].Count != 2 {
		t.Fatal("prepared postimage changed")
	}
	if err := s.ReleaseReward(r); err != nil {
		t.Fatal(err)
	}
	if err := s.Save(c.ID, old); !errors.Is(err, ErrRewardEpoch) {
		t.Fatalf("old capture after publication: %v", err)
	}
	disk.Silver++
	if err := s.Save(c.ID, disk); err != nil {
		t.Fatal(err)
	}
	disk.BossRewardEpoch++
	if err := s.Save(c.ID, disk); !errors.Is(err, ErrRewardEpoch) {
		t.Fatal("ordinary save minted a receipt")
	}
	reopened, err := OpenStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	if err := reopened.Save(c.ID, old); !errors.Is(err, ErrRewardEpoch) {
		t.Fatalf("cold start lost floor: %v", err)
	}
}

func TestRewardReservationCapturesTheWriterThatWasAlreadyInFlight(t *testing.T) {
	s, _ := openStore(t)
	c := newCharacter(t, s, testID(1), "Eivor")
	rec, _, _ := s.Load(c.ID)
	rec.Experience = 123
	rec.Silver = 456
	entered, release := make(chan struct{}), make(chan struct{})
	s.recordWriter = func(path string, b []byte) error { close(entered); <-release; return world.WriteAtomic(path, b) }
	saved := make(chan error, 1)
	go func() { saved <- s.Save(c.ID, rec) }()
	<-entered
	type result struct {
		r   *RewardReservation
		rec Record
		err error
	}
	reserved := make(chan result, 1)
	go func() { r, base, err := s.ReserveReward(c.ID); reserved <- result{r, base, err} }()
	close(release)
	if err := <-saved; err != nil {
		t.Fatal(err)
	}
	got := <-reserved
	if got.err != nil || got.rec.Experience != 123 || got.rec.Silver != 456 {
		t.Fatalf("lost pre-barrier update: %+v", got)
	}
	if err := s.AbortReward(got.r); err != nil {
		t.Fatal(err)
	}
}

func TestRewardFenceSurvivesUncertainCharacterWrite(t *testing.T) {
	s, _ := openStore(t)
	c := newCharacter(t, s, testID(1), "Eivor")
	r, next, err := s.ReserveReward(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	old := next
	next.BossRewardEpoch++
	next.Silver = 17
	if err := s.BeginRewardIntent(r, next); err != nil {
		t.Fatal(err)
	}
	failure := errors.New("synthetic directory sync failure")
	s.recordWriter = func(path string, b []byte) error {
		if err := world.WriteAtomic(path, b); err != nil {
			return err
		}
		return failure
	}
	if err := s.WritePreparedReward(r); !errors.Is(err, failure) {
		t.Fatal(err)
	}
	disk, _, _ := s.Load(c.ID)
	if disk.Silver != 17 || disk.BossRewardEpoch != 1 {
		t.Fatal("fixture did not cross rename")
	}
	if err := s.ReleaseReward(r); !errors.Is(err, ErrRewardReservation) {
		t.Fatal("uncertain write released")
	}
	if err := s.AbortReward(r); !errors.Is(err, ErrRewardReservation) {
		t.Fatal("uncertain intent aborted")
	}
	if err := s.Save(c.ID, old); !errors.Is(err, ErrRewardPending) {
		t.Fatal("ordinary save erased uncertain receipt")
	}
	s.recordWriter = nil
	if err := s.WritePreparedReward(r); err != nil {
		t.Fatal(err)
	}
	if err := s.ReleaseReward(r); err != nil {
		t.Fatal(err)
	}
}

func TestRewardFenceRejectsInvalidPostimageBeforeSealing(t *testing.T) {
	for _, mutate := range []func(*Record){func(r *Record) { r.Name = "" }, func(r *Record) { r.Pos[0] = math.NaN() }, func(r *Record) { r.Slots[0].Count = 1 }, func(r *Record) { r.Owner = testID(2) }, func(r *Record) { r.BossRewardEpoch++ }} {
		s, _ := openStore(t)
		c := newCharacter(t, s, testID(1), "Eivor")
		r, next, err := s.ReserveReward(c.ID)
		if err != nil {
			t.Fatal(err)
		}
		next.BossRewardEpoch++
		mutate(&next)
		if err := s.BeginRewardIntent(r, next); err == nil {
			t.Fatal("invalid postimage accepted")
		}
		if err := s.AbortReward(r); err != nil {
			t.Fatalf("no-I/O rejection stranded character: %v", err)
		}
	}
}

func TestRewardFenceRefusesEpochOverflowAndEphemeralReservations(t *testing.T) {
	s, dir := openStore(t)
	c := newCharacter(t, s, testID(1), "Eivor")
	rec, _, _ := s.Load(c.ID)
	rec.BossRewardEpoch = math.MaxUint64
	if err := os.WriteFile(s.recordPath(c.ID), encodeRecord(rec), 0600); err != nil {
		t.Fatal(err)
	}
	s, err := OpenStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	if _, _, err := s.ReserveReward(c.ID); !errors.Is(err, ErrRewardEpoch) {
		t.Fatal("overflow reserved")
	}
	for _, ephemeral := range []*Store{nil, NewMemoryStore()} {
		if _, _, err := ephemeral.ReserveReward(c.ID); !errors.Is(err, ErrRewardReservation) {
			t.Fatal("ephemeral claimed durability")
		}
	}
}
