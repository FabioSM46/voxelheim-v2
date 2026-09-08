package persist

import (
	"encoding/binary"
	"errors"
	"math"
	"os"
	"reflect"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func rewardFixture() RewardJournal {
	owner := SessionCharacter{PlayerID: testID(1), CharacterID: 1}
	xpOwner := SessionCharacter{PlayerID: testID(2), CharacterID: 2}
	bound := SessionCharacter{PlayerID: testID(3), CharacterID: 3}
	return RewardJournal{Revision: 1, NextGeneration: 2, Runs: []RewardRun{{Generation: 1, ContentVersion: world.WorldgenVersion,
		Session: SessionRecord{ID: 7, Seed: 19, Ruin: [2]int64{2, 3}, ExpiresUnix: 100, Bound: []SessionCharacter{bound}, DefeatedBosses: []vnet.MobKind{vnet.MobKindVargrGuardian}},
		Defeats: []RewardDefeat{{Kind: vnet.MobKindVargrGuardian, Personal: []PersonalReward{{Owner: owner, Entries: []protocol.InventoryStack{{ItemID: 1, Count: 2}}, Silver: 30}}, Experience: []BossExperienceReward{{Owner: xpOwner, Amount: 90}}}},
	}}, Intents: []RewardIntent{{RewardReference: RewardReference{Generation: 1, Boss: vnet.MobKindVargrGuardian, Owner: owner, Entries: 1, Silver: true}, Postimage: Record{Character: 1, Owner: owner.PlayerID, Name: "Eivor", Appearance: testAppearance(), Health: 100, BossRewardEpoch: 1, Silver: 30}}}}
}

func TestRewardJournalRoundTripKeepsSeparateRecipientPopulations(t *testing.T) {
	want := rewardFixture()
	b, err := encodeRewardJournal(want)
	if err != nil {
		t.Fatal(err)
	}
	got, err := decodeRewardJournal(b)
	if err != nil || !reflect.DeepEqual(got, want) {
		t.Fatalf("journal roundtrip: %+v %v", got, err)
	}
	dir := t.TempDir()
	s, err := OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	if err := s.commit(0, want); err != nil {
		t.Fatal(err)
	}
	reopened, err := OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(reopened.journal, want) {
		t.Fatal("disk journal changed exact rewards")
	}
	if !errors.Is(reopened.CheckInactive(), ErrRewardReplayRequired) {
		t.Fatal("format-only reader ignored pending rewards")
	}
}

func TestRewardJournalRejectsInvalidReferencesAndEpochs(t *testing.T) {
	cases := map[string]func(*RewardJournal){
		"zero generation":         func(j *RewardJournal) { j.Runs[0].Generation = 0 },
		"generation beyond floor": func(j *RewardJournal) { j.NextGeneration = 1 },
		"zero high water":         func(j *RewardJournal) { j.NextGeneration = 0 },
		"missing run":             func(j *RewardJournal) { j.Intents[0].Generation = 9 },
		"missing boss":            func(j *RewardJournal) { j.Intents[0].Boss = vnet.MobKindDraugrKing },
		"wrong owner":             func(j *RewardJournal) { j.Intents[0].Owner.PlayerID = testID(4) },
		"missing entry":           func(j *RewardJournal) { j.Intents[0].Entries = 2 },
		"consumed entry":          func(j *RewardJournal) { j.Runs[0].Defeats[0].Personal[0].Taken = 1 },
		"consumed silver":         func(j *RewardJournal) { j.Runs[0].Defeats[0].Personal[0].SilverTaken = true },
		"xp population differs":   func(j *RewardJournal) { j.Intents[0].Experience = true },
		"epoch mismatch":          func(j *RewardJournal) { j.Intents[0].Postimage.BossRewardEpoch = 2 },
		"epoch overflow": func(j *RewardJournal) {
			j.Intents[0].PreviousEpoch = math.MaxUint64
			j.Intents[0].Postimage.BossRewardEpoch = 0
		},
		"duplicate intent":         func(j *RewardJournal) { j.Intents = append(j.Intents, j.Intents[0]) },
		"duplicate generation":     func(j *RewardJournal) { j.Runs = append(j.Runs, j.Runs[0]) },
		"duplicate recipient":      func(j *RewardJournal) { d := &j.Runs[0].Defeats[0]; d.Personal = append(d.Personal, d.Personal[0]) },
		"inconsistent progress":    func(j *RewardJournal) { j.Runs[0].Session.DefeatedBosses = nil },
		"postimage oversized name": func(j *RewardJournal) { j.Intents[0].Postimage.Name = string(make([]byte, MaxNameBytes+1)) },
	}
	for name, mutate := range cases {
		t.Run(name, func(t *testing.T) {
			j := rewardFixture()
			mutate(&j)
			if _, err := encodeRewardJournal(j); err == nil {
				t.Fatal("invalid journal encoded")
			}
		})
	}
}

func TestRewardJournalPreflightRefusesMalformedLengthsBeforeAllocation(t *testing.T) {
	j := rewardFixture()
	valid, err := encodeRewardJournal(j)
	if err != nil {
		t.Fatal(err)
	}
	cases := map[string]func([]byte) []byte{
		"run count uint32 overflow": func(b []byte) []byte {
			binary.LittleEndian.PutUint32(b[world.HeaderSize+16:], math.MaxUint32)
			return b
		},
		"binding count": func(b []byte) []byte {
			binary.LittleEndian.PutUint32(b[world.HeaderSize+20+12+40:], MaxRewardBindings+1)
			return b
		},
		"postimage length": func(b []byte) []byte {
			rec := encodeRecord(j.Intents[0].Postimage)
			at := len(b) - world.ChecksumSize - len(rec) - 4
			binary.LittleEndian.PutUint32(b[at:], math.MaxUint32)
			return b
		},
		"postimage name": func(b []byte) []byte {
			rec := encodeRecord(j.Intents[0].Postimage)
			at := len(b) - world.ChecksumSize - len(rec) + offNameLen
			binary.LittleEndian.PutUint16(b[at:], MaxNameBytes+1)
			return b
		},
		"trailing byte": func(b []byte) []byte { return append(b, 0) },
		"truncated":     func(b []byte) []byte { return b[:len(b)-3] },
	}
	for name, mutate := range cases {
		t.Run(name, func(t *testing.T) {
			b := mutate(append([]byte(nil), valid...))
			world.PutChecksum(b)
			if _, err := decodeRewardJournal(b); err == nil {
				t.Fatal("malformed lengths accepted")
			}
		})
	}
	valid[len(valid)-1] ^= 1
	if _, err := decodeRewardJournal(valid); err == nil {
		t.Fatal("bad checksum accepted")
	}
}

func TestRewardJournalCumulativeBounds(t *testing.T) {
	j := rewardFixture()
	j.Intents = nil
	// Each roster is independently below its limit; their combined size is not.
	owners := make([]PersonalReward, MaxRewardRecipients/2+1)
	for i := range owners {
		owners[i] = PersonalReward{Owner: SessionCharacter{PlayerID: testID(1), CharacterID: uint64(i + 1)}}
	}
	j.Runs[0].Defeats[0].Personal = owners
	j.Runs[0].Defeats = append(j.Runs[0].Defeats, RewardDefeat{Kind: vnet.MobKindDraugrKing, Personal: owners})
	j.Runs[0].Session.DefeatedBosses = append(j.Runs[0].Session.DefeatedBosses, vnet.MobKindDraugrKing)
	if _, err := encodeRewardJournal(j); err == nil {
		t.Fatal("cumulative recipients exceeded")
	}
	j = rewardFixture()
	j.Intents = nil
	rows := make([]PersonalReward, MaxRewardEntries/MaxPersonalRewardEntries+1)
	for i := range rows {
		rows[i].Owner = SessionCharacter{PlayerID: testID(1), CharacterID: uint64(i + 1)}
		rows[i].Entries = make([]protocol.InventoryStack, MaxPersonalRewardEntries)
		for k := range rows[i].Entries {
			rows[i].Entries[k] = protocol.InventoryStack{ItemID: 1, Count: 1}
		}
	}
	j.Runs[0].Defeats[0].Personal = rows
	if _, err := encodeRewardJournal(j); err == nil {
		t.Fatal("cumulative entries exceeded")
	}
}

func TestRewardJournalUncertainWriteRetainsExactIntentAndBlocksStaleSnapshots(t *testing.T) {
	dir := t.TempDir()
	s, err := OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	j := rewardFixture()
	failure := errors.New("synthetic sync failure")
	s.writeAtomic = func(path string, b []byte) error {
		if err := world.WriteAtomic(path, b); err != nil {
			return err
		}
		return failure
	}
	if err := s.commit(0, j); !errors.Is(err, failure) {
		t.Fatal(err)
	}
	changed := rewardFixture()
	changed.Intents[0].Postimage.Silver++
	if err := s.commit(0, changed); !errors.Is(err, ErrRewardJournalConflict) {
		t.Fatal("uncertain intent was replaced")
	}
	reopened, err := OpenRewardStore(dir)
	if err != nil || !reflect.DeepEqual(reopened.journal, j) {
		t.Fatal("fixture did not survive rename")
	}
	s.writeAtomic = world.WriteAtomic
	if err := s.commit(0, j); err != nil {
		t.Fatal(err)
	}
	if err := s.commit(0, j); !errors.Is(err, ErrRewardJournalConflict) {
		t.Fatal("stale journal accepted")
	}
	next := rewardFixture()
	next.Revision = 2
	next.Intents = nil
	if err := s.commit(1, next); !errors.Is(err, ErrRewardJournalConflict) {
		t.Fatal("unresolved intent was deleted at expiry")
	}
}

func TestRewardJournalHighWaterSurvivesEmptyStateAndWireIDReuse(t *testing.T) {
	dir := t.TempDir()
	s, err := OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	if err := s.commit(0, RewardJournal{Revision: 1, NextGeneration: 40}); err != nil {
		t.Fatal(err)
	}
	s, err = OpenRewardStore(dir)
	if err != nil || s.CheckInactive() != nil || s.journal.NextGeneration != 40 {
		t.Fatal("empty journal forgot high water")
	}
	j := rewardFixture()
	j.Revision = 2
	j.NextGeneration = 41
	if err := s.commit(1, j); !errors.Is(err, ErrRewardJournalConflict) {
		t.Fatal("old generation reused after GC/cold start")
	}
	j.Runs[0].Generation = 40
	j.Intents[0].Generation = 40
	if err := s.commit(1, j); err != nil {
		t.Fatal(err)
	}
	j = rewardFixture()
	j.NextGeneration = 3
	j.Intents = nil
	old := j.Runs[0]
	old.Generation = 2
	j.Runs = append(j.Runs, old)
	if _, err := encodeRewardJournal(j); err != nil {
		t.Fatalf("expired retained generation reserved a transient wire ID: %v", err)
	}
}

func TestRewardJournalRefusesOversizeAndFutureFiles(t *testing.T) {
	dir := t.TempDir()
	s, _ := OpenRewardStore(dir)
	file, err := os.Create(s.path)
	if err != nil {
		t.Fatal(err)
	}
	if err := file.Truncate(MaxRewardJournalBytes + 1); err != nil {
		t.Fatal(err)
	}
	if err := file.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := OpenRewardStore(dir); !errors.Is(err, world.ErrCorruptStore) {
		t.Fatal("oversized journal read")
	}
	b, _ := encodeRewardJournal(RewardJournal{NextGeneration: 1})
	binary.LittleEndian.PutUint32(b[4:8], BossRewardsVersion+1)
	world.PutChecksum(b)
	if err := os.WriteFile(s.path, b, 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := OpenRewardStore(dir); err == nil {
		t.Fatal("future journal accepted")
	}
}
