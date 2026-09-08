package persist

import (
	"bytes"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"math"
	"os"
	"path/filepath"
	"slices"
	"sync"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// V1 layout (little endian): magic/version, revision:u64, next_generation:u64,
// run_count:u32, runs, intent_count:u32, intents, CRC32. Each run stores generation,
// worldgen content version, session/seed/ruin/expiry, bounded bindings and defeats.
// Defeats contain separate personal and XP lists. A personal row carries its stable
// owner, taken mask, silver flag/purse and fixed eight-byte inventory entries.
// An intent names generation/boss/owner/selection, prior epoch and a length-delimited
// complete v11 character record (including that record's own checksum).
// Cumulative bounds include bindings, BOTH recipient lists, all entries and complete
// intent postimages; name lengths are checked before any string allocation.
const (
	BossRewardsVersion       uint32 = 1
	MaxRewardJournalBytes           = 16 << 20
	MaxRewardRuns                   = 1024
	MaxRewardRecipients             = 8192
	MaxRewardBindings               = 8192
	MaxRewardEntries                = 65536
	MaxRewardIntents                = 1024
	MaxPersonalRewardEntries        = 64
	rewardFileName                  = "boss-rewards.bin"
)

var (
	rewardMagic              = [4]byte{'V', 'X', 'H', 'R'}
	ErrRewardReplayRequired  = errors.New("persist: boss rewards require replay support before startup")
	ErrRewardJournalConflict = errors.New("persist: stale or uncertain reward journal revision")
)

// RewardJournal is independent of the whole-session autosave. Its generations do
// not use the wire entity counter. NextGeneration survives even an empty journal;
// zero and overflow are refusals, never an opportunity to reuse a receipt identity.
// No gameplay producer is enabled in this format-only delivery.
type RewardJournal struct {
	Revision       uint64
	NextGeneration uint64
	Runs           []RewardRun
	Intents        []RewardIntent
}

type RewardRun struct {
	Generation     uint64
	ContentVersion uint32
	Session        SessionRecord
	Defeats        []RewardDefeat
}

// Personal, Experience and Session.Bound are deliberately different populations.
// Entries retain their original indices after claims; Taken selects consumed rows.
type RewardDefeat struct {
	Kind       vnet.MobKind
	Personal   []PersonalReward
	Experience []BossExperienceReward
}

type PersonalReward struct {
	Owner       SessionCharacter
	Entries     []protocol.InventoryStack
	Silver      uint32
	Taken       uint64
	SilverTaken bool
}

type BossExperienceReward struct {
	Owner  SessionCharacter
	Amount uint32
	Taken  bool
}

type RewardReference struct {
	Generation uint64
	Boss       vnet.MobKind
	Owner      SessionCharacter
	Entries    uint64
	Silver     bool
	Experience bool
}

// The character receipt is atomically stored with the complete postimage. Epochs
// serialize that character's claims across runs; generation identifies the run.
// Prepared intents must survive expiry until replay or acknowledgement resolves them.
type RewardIntent struct {
	RewardReference
	PreviousEpoch uint64
	Postimage     Record
}

// RewardStore owns a serialized journal image. commit is deliberately private:
// future mutation APIs must enforce defeat/claim/GC transitions, and there are no
// ordinary whole-journal autosave writers. The only production reader currently
// accepts empty/high-water-only state and refuses state that needs replay.
type RewardStore struct {
	mu          sync.Mutex
	path        string
	journal     RewardJournal
	uncertain   []byte
	writeAtomic func(string, []byte) error
}

func OpenRewardStore(dir string) (*RewardStore, error) {
	if dir == "" {
		return nil, nil
	}
	s := &RewardStore{path: filepath.Join(dir, rewardFileName), journal: RewardJournal{NextGeneration: 1}, writeAtomic: world.WriteAtomic}
	f, err := os.Open(s.path)
	if errors.Is(err, fs.ErrNotExist) {
		return s, nil
	}
	if err != nil {
		return nil, err
	}
	defer func() { _ = f.Close() }()
	info, err := f.Stat()
	if err != nil {
		return nil, err
	}
	if info.Size() > MaxRewardJournalBytes {
		return nil, fmt.Errorf("%w: reward journal size", world.ErrCorruptStore)
	}
	// The same descriptor is bounded even if the inode grows after Stat. An extra
	// byte distinguishes a file exactly at the limit from a truncated oversized read.
	data, err := io.ReadAll(io.LimitReader(f, MaxRewardJournalBytes+1))
	if err != nil {
		return nil, err
	}
	if len(data) > MaxRewardJournalBytes {
		return nil, fmt.Errorf("%w: reward journal size", world.ErrCorruptStore)
	}

	s.journal, err = decodeRewardJournal(data)
	if err != nil {
		return nil, err
	}
	return s, nil
}

// CheckInactive prevents a format-only build from silently ignoring earned rewards
// or starting ordinary character writers before unresolved postimages are replayed.
func (s *RewardStore) CheckInactive() error {
	if s == nil {
		return nil
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if len(s.journal.Runs) > 0 || len(s.journal.Intents) > 0 {
		return ErrRewardReplayRequired
	}
	return nil
}

// commit is a storage primitive, not a gameplay/GC API. Callers supply the revision
// they read. An uncertain write locks the exact bytes for retry; even generation
// allocation cannot move past it until directory durability has been confirmed.
func (s *RewardStore) commit(expected uint64, next RewardJournal) error {
	if s == nil {
		return ErrRewardJournalConflict
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if expected != s.journal.Revision || expected == math.MaxUint64 || next.Revision != expected+1 || next.NextGeneration < s.journal.NextGeneration {
		return ErrRewardJournalConflict
	}

	// No acknowledgement/GC API is active in this part. Unresolved intents cannot
	// disappear or be recaptured through the storage primitive, even at expiry.
	for _, intent := range s.journal.Intents {
		if !slices.Contains(next.Intents, intent) {
			return ErrRewardJournalConflict
		}
	}
	for _, run := range next.Runs {
		found := false
		for _, old := range s.journal.Runs {
			if old.Generation == run.Generation {
				found = true
				break
			}
		}
		if !found && run.Generation < s.journal.NextGeneration {
			return ErrRewardJournalConflict
		}
	}
	data, err := encodeRewardJournal(next)
	if err != nil {
		return err
	}
	if s.uncertain != nil && !bytes.Equal(data, s.uncertain) {
		return ErrRewardJournalConflict
	}
	s.uncertain = data
	if err := s.writeAtomic(s.path, data); err != nil {
		return err
	}
	// Decode makes an owned copy; a caller cannot mutate a successfully written image.
	s.journal, err = decodeRewardJournal(data)
	s.uncertain = nil
	return err
}

// The decoder first walks every length without allocating slices or strings. Only
// after cumulative counts, complete postimage sizes, checksum and exact EOF pass
// does it allocate the bounded data model and check its cross-references.
type rewardReader struct {
	data []byte
	at   int
	err  error
}

func (r *rewardReader) take(n int) []byte {
	if r.err != nil || n < 0 || n > len(r.data)-r.at {
		r.err = world.ErrCorruptStore
		return nil
	}
	b := r.data[r.at : r.at+n]
	r.at += n
	return b
}
func (r *rewardReader) number(n int) uint64 {
	b := r.take(n)
	if b == nil {
		return 0
	}
	var v uint64
	for i := n - 1; i >= 0; i-- {
		v = v<<8 | uint64(b[i])
	}
	return v
}
func (r *rewardReader) count(limit int, total *int) int {
	raw := r.number(4)
	if raw > uint64(limit) {
		r.err = world.ErrCorruptStore
		return 0
	}
	n := int(raw)
	if total != nil && n > limit-*total {
		r.err = world.ErrCorruptStore
		return 0
	}
	if total != nil {
		*total += n
	}
	return n
}
func rewardPut(b *[]byte, n uint64, size int) {
	for range size {
		*b = append(*b, byte(n))
		n >>= 8
	}
}
func rewardOwner(b *[]byte, o SessionCharacter) {
	*b = append(*b, o.PlayerID[:]...)
	rewardPut(b, o.CharacterID, 8)
}
func readRewardOwner(r *rewardReader) SessionCharacter {
	var o SessionCharacter
	copy(o.PlayerID[:], r.take(32))
	o.CharacterID = r.number(8)
	return o
}
func rewardFlag(r *rewardReader) bool {
	v := r.number(1)
	if v > 1 {
		r.err = world.ErrCorruptStore
	}
	return v == 1
}
func putRewardFlag(b *[]byte, v bool) {
	if v {
		*b = append(*b, 1)
	} else {
		*b = append(*b, 0)
	}
}

func encodeRewardJournal(j RewardJournal) ([]byte, error) {
	if err := validateRewardJournal(j); err != nil {
		return nil, err
	}
	b := world.NewRecord(world.HeaderSize, 0, rewardMagic, BossRewardsVersion)
	// NewRecord includes checksum space; append the payload before its final checksum.
	b = b[:world.HeaderSize]
	rewardPut(&b, j.Revision, 8)
	rewardPut(&b, j.NextGeneration, 8)
	rewardPut(&b, uint64(len(j.Runs)), 4)
	for _, run := range j.Runs {
		rewardPut(&b, run.Generation, 8)
		rewardPut(&b, uint64(run.ContentVersion), 4)
		s := run.Session
		rewardPut(&b, s.ID, 8)
		rewardPut(&b, uint64(s.Seed), 8)
		rewardPut(&b, uint64(s.Ruin[0]), 8)
		rewardPut(&b, uint64(s.Ruin[1]), 8)
		rewardPut(&b, uint64(s.ExpiresUnix), 8)
		rewardPut(&b, uint64(len(s.Bound)), 4)
		for _, owner := range s.Bound {
			rewardOwner(&b, owner)
		}
		rewardPut(&b, uint64(len(run.Defeats)), 4)
		for _, d := range run.Defeats {
			rewardPut(&b, uint64(d.Kind), 1)
			rewardPut(&b, uint64(len(d.Personal)), 4)
			for _, p := range d.Personal {
				rewardOwner(&b, p.Owner)
				rewardPut(&b, p.Taken, 8)
				putRewardFlag(&b, p.SilverTaken)
				rewardPut(&b, uint64(p.Silver), 4)
				rewardPut(&b, uint64(len(p.Entries)), 4)
				for _, item := range p.Entries {
					rewardPut(&b, uint64(item.ItemID), 2)
					rewardPut(&b, uint64(item.Count), 2)
					rewardPut(&b, uint64(item.Durability), 2)
					rewardPut(&b, uint64(item.MaxDurability), 2)
				}
			}
			rewardPut(&b, uint64(len(d.Experience)), 4)
			for _, xp := range d.Experience {
				rewardOwner(&b, xp.Owner)
				rewardPut(&b, uint64(xp.Amount), 4)
				putRewardFlag(&b, xp.Taken)
			}
		}
	}
	rewardPut(&b, uint64(len(j.Intents)), 4)
	for _, in := range j.Intents {
		rewardPut(&b, in.Generation, 8)
		rewardPut(&b, uint64(in.Boss), 1)
		rewardOwner(&b, in.Owner)
		rewardPut(&b, in.Entries, 8)
		putRewardFlag(&b, in.Silver)
		putRewardFlag(&b, in.Experience)
		rewardPut(&b, in.PreviousEpoch, 8)
		record := encodeRecord(in.Postimage)
		rewardPut(&b, uint64(len(record)), 4)
		b = append(b, record...)
	}
	b = append(b, make([]byte, world.ChecksumSize)...)
	if len(b) > MaxRewardJournalBytes {
		return nil, world.ErrCorruptStore
	}
	world.PutChecksum(b)
	return b, nil
}

func decodeRewardJournal(b []byte) (RewardJournal, error) {
	if len(b) > MaxRewardJournalBytes || len(b) < world.HeaderSize+24+world.ChecksumSize {
		return RewardJournal{}, world.ErrCorruptStore
	}
	if err := world.CheckHeader(b, rewardMagic, BossRewardsVersion); err != nil {
		return RewardJournal{}, err
	}
	if err := world.CheckChecksum(b); err != nil {
		return RewardJournal{}, err
	}
	if _, err := walkRewardJournal(b, false); err != nil {
		return RewardJournal{}, err
	}
	j, err := walkRewardJournal(b, true)
	if err != nil {
		return RewardJournal{}, err
	}
	return j, validateRewardJournal(j)
}

func walkRewardJournal(b []byte, allocate bool) (RewardJournal, error) {
	r := rewardReader{data: b[world.HeaderSize : len(b)-world.ChecksumSize]}
	j := RewardJournal{Revision: r.number(8), NextGeneration: r.number(8)}
	runs := r.count(MaxRewardRuns, nil)
	bindings, recipients, entries, postimages := 0, 0, 0, 0
	for range runs {
		run := RewardRun{Generation: r.number(8), ContentVersion: uint32(r.number(4))}
		s := &run.Session
		s.ID = r.number(8)
		s.Seed = int64(r.number(8))
		s.Ruin = [2]int64{int64(r.number(8)), int64(r.number(8))}
		s.ExpiresUnix = int64(r.number(8))
		bound := r.count(MaxRewardBindings, &bindings)
		if bound > MaxBoundCharacters {
			r.err = world.ErrCorruptStore
		}
		for range bound {
			o := readRewardOwner(&r)
			if allocate {
				s.Bound = append(s.Bound, o)
			}
		}
		defeats := r.count(MaxDefeatedBosses, nil)
		for range defeats {
			d := RewardDefeat{Kind: vnet.MobKind(r.number(1))}
			owners := r.count(MaxRewardRecipients, &recipients)
			for range owners {
				p := PersonalReward{Owner: readRewardOwner(&r), Taken: r.number(8), SilverTaken: rewardFlag(&r), Silver: uint32(r.number(4))}
				count := r.count(MaxRewardEntries, &entries)
				if count > MaxPersonalRewardEntries {
					r.err = world.ErrCorruptStore
				}
				for range count {
					item := protocol.InventoryStack{ItemID: uint16(r.number(2)), Count: uint16(r.number(2)), Durability: uint16(r.number(2)), MaxDurability: uint16(r.number(2))}
					if allocate {
						p.Entries = append(p.Entries, item)
					}
				}
				if allocate {
					d.Personal = append(d.Personal, p)
				}
			}
			xp := r.count(MaxRewardRecipients, &recipients)
			for range xp {
				entry := BossExperienceReward{Owner: readRewardOwner(&r), Amount: uint32(r.number(4)), Taken: rewardFlag(&r)}
				if allocate {
					d.Experience = append(d.Experience, entry)
				}
			}
			if allocate {
				run.Defeats = append(run.Defeats, d)
				s.DefeatedBosses = append(s.DefeatedBosses, d.Kind)
			}
		}
		if allocate {
			j.Runs = append(j.Runs, run)
		}
	}
	intents := r.count(MaxRewardIntents, nil)
	for range intents {
		in := RewardIntent{}
		in.Generation = r.number(8)
		in.Boss = vnet.MobKind(r.number(1))
		in.Owner = readRewardOwner(&r)
		in.Entries = r.number(8)
		in.Silver = rewardFlag(&r)
		in.Experience = rewardFlag(&r)
		in.PreviousEpoch = r.number(8)
		size := r.count(MaxRewardIntents*maxRecordSize, &postimages)
		if size > maxRecordSize || size < recordHeaderSize+world.ChecksumSize {
			r.err = world.ErrCorruptStore
			size = 0
		}
		data := r.take(size)
		// A fixed-width preflight verifies the name before the allocating decoder runs.
		if len(data) >= recordHeaderSize {
			name := int(binary.LittleEndian.Uint16(data[offNameLen : offNameLen+2]))
			if name > MaxNameBytes || recordHeaderSize+name+world.ChecksumSize != len(data) {
				r.err = world.ErrCorruptStore
			}
		}
		if allocate && r.err == nil {
			rec, err := decodeRecord(data)
			if err != nil {
				r.err = err
			} else {
				in.Postimage = rec
				j.Intents = append(j.Intents, in)
			}
		}
	}
	if r.err != nil {
		return RewardJournal{}, r.err
	}
	if r.at != len(r.data) {
		return RewardJournal{}, world.ErrCorruptStore
	}
	return j, nil
}
