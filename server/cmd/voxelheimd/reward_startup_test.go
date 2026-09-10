package main

import (
	"encoding/binary"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Independent v1 fixture: an empty high-water journal or one retained run with no
// defeat yet. Even that run is not safe for an inactive reader to silently ignore.
func startupRewardFixture(active bool) []byte {
	b := world.NewRecord(world.HeaderSize, 0, [4]byte{'V', 'X', 'H', 'R'}, persist.BossRewardsVersion)[:world.HeaderSize]
	b = binary.LittleEndian.AppendUint64(b, 1)
	b = binary.LittleEndian.AppendUint64(b, 42)
	count := uint32(0)
	if active {
		count = 1
	}
	b = binary.LittleEndian.AppendUint32(b, count)
	if active {
		b = binary.LittleEndian.AppendUint64(b, 41) // durable generation
		b = binary.LittleEndian.AppendUint32(b, world.WorldgenVersion)
		for _, n := range []uint64{7, 19, 2, 3, 100} {
			b = binary.LittleEndian.AppendUint64(b, n)
		}
		b = binary.LittleEndian.AppendUint32(b, 0) // bindings
		b = binary.LittleEndian.AppendUint32(b, 0) // defeated bosses
	}
	b = binary.LittleEndian.AppendUint32(b, 0) // prepared intents
	b = append(b, make([]byte, world.ChecksumSize)...)
	world.PutChecksum(b)
	return b
}

func TestStartupRefusesACorruptRewardJournalBeforeOpeningPlayers(t *testing.T) {
	dir := t.TempDir()
	b := startupRewardFixture(true)
	b[len(b)-1] ^= 1
	if err := os.WriteFile(filepath.Join(dir, "boss-rewards.bin"), b, 0600); err != nil {
		t.Fatal(err)
	}
	if _, _, err := openPlayers(options{worldDir: dir}, discard()); !errors.Is(err, world.ErrCorruptStore) {
		t.Fatalf("startup did not refuse a corrupt reward journal: %v", err)
	}
	if _, err := os.Stat(filepath.Join(dir, "players")); !errors.Is(err, os.ErrNotExist) {
		t.Fatal("refused startup modified players")
	}
}

// journalWithRun writes a reward journal holding one live run whose guardian is dead, with
// no sessions file beside it, at the given content version.
func journalWithRun(t *testing.T, content uint32) string {
	t.Helper()
	dir := t.TempDir()
	players, err := persist.OpenStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	journal, err := persist.OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	record := persist.SessionRecord{ID: 7, Seed: 19, Ruin: [2]int64{2, 3}, ExpiresUnix: time.Now().Add(time.Hour).Unix()}
	if err := journal.AllocateRun(players, 1, record, content); err != nil {
		t.Fatal(err)
	}
	if err := journal.AppendDefeat(1, persist.RewardDefeat{Kind: vnet.MobKindVargrGuardian}, nil); err != nil {
		t.Fatal(err)
	}
	return dir
}

func restoreOver(t *testing.T, dir string) (*game.InstanceManager, error) {
	t.Helper()
	_, rewards, err := openPlayers(options{worldDir: dir}, discard())
	if err != nil {
		t.Fatalf("an active reward journal was refused at startup: %v", err)
	}
	runs, err := openSessions(options{worldDir: dir}, discard())
	if err != nil {
		t.Fatal(err)
	}
	instances, err := game.NewInstanceManager(game.DefaultTickRate, 1, game.DefaultMaxInstances, session.NewRegistry(session.DefaultConcurrentSessions).NextID, discard())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(instances.Close)
	return instances, restoreSessions(instances, runs, rewards, time.Now(), discard())
}

// An active journal is recovered rather than refused, and the run it holds comes back
// with its generation and its dead guardian even though the sessions file is missing.
func TestStartupRestoresTheRunsTheRewardJournalHolds(t *testing.T) {
	instances, err := restoreOver(t, journalWithRun(t, world.WorldgenVersion))
	if err != nil {
		t.Fatal(err)
	}
	saved := instances.SavedSessions()
	if len(saved) != 1 || saved[0].Generation != 1 || saved[0].Seed != 19 ||
		!slices.Equal(saved[0].DefeatedBosses, []vnet.MobKind{vnet.MobKindVargrGuardian}) {
		t.Fatalf("restored runs = %+v", saved)
	}
}

func TestStartupStopsRatherThanStartFreeOverAJournalRunItCannotRestore(t *testing.T) {
	instances, err := restoreOver(t, journalWithRun(t, world.WorldgenVersion+1))
	if !errors.Is(err, persist.ErrRewardRecoveryRequired) {
		t.Fatalf("a run from another worldgen = %v, want %v", err, persist.ErrRewardRecoveryRequired)
	}
	if instances.Count() != 0 {
		t.Fatalf("a refused restore built %d sessions", instances.Count())
	}
}

func TestStartupAcceptsHighWaterOnlyRewardsAndEphemeralWorld(t *testing.T) {
	dir := t.TempDir()
	b := startupRewardFixture(false)
	if err := os.WriteFile(filepath.Join(dir, "boss-rewards.bin"), b, 0600); err != nil {
		t.Fatal(err)
	}
	if _, _, err := openPlayers(options{worldDir: dir}, discard()); err != nil {
		t.Fatal(err)
	}
	if s, r, err := openPlayers(options{}, discard()); err != nil || s != nil || r != nil {
		t.Fatal("ephemeral startup changed")
	}
}
