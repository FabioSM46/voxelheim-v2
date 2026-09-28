package main

import (
	"encoding/binary"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
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

// journalWithLoot writes a reward journal holding one live run whose guardian's loot, one item
// and 30 silver for one owner, nobody has taken from yet.
func journalWithLoot(t *testing.T, item uint16) string {
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
	character, err := players.Create(identity.IDOf(identity.Account{9}), "Looter", testAppearance())
	if err != nil {
		t.Fatal(err)
	}
	owner := persist.SessionCharacter{PlayerID: character.Owner, CharacterID: uint64(character.ID)}
	record := persist.SessionRecord{ID: 7, Seed: 19, Ruin: [2]int64{2, 3}, ExpiresUnix: time.Now().Add(time.Hour).Unix(),
		DefeatedBosses: []vnet.MobKind{vnet.MobKindVargrGuardian}, Bound: []persist.SessionCharacter{owner}}
	defeat := persist.RewardDefeat{Kind: vnet.MobKindVargrGuardian, Personal: []persist.PersonalReward{{Owner: owner, Entries: []protocol.InventoryStack{{ItemID: item, Count: 1}}, Silver: 30}}}
	if err := journal.AllocateRun(players, 1, record, world.WorldgenVersion, defeat); err != nil {
		t.Fatal(err)
	}
	return dir
}

// Startup hands the restore the loot the journal still owes. The proof needs nothing
// but the restore's own verdict: loot a pack could hold comes back with its run, and loot no
// pack could hold refuses the whole startup restore, which it could only do if it was handed.
func TestStartupRebuildsTheBossLootTheJournalStillOwes(t *testing.T) {
	for _, c := range []struct {
		name    string
		item    uint16
		refused bool
	}{
		{"loot a pack could hold", uint16(game.ItemBone), false},
		{"loot no pack could hold", 65535, true},
	} {
		instances, err := restoreOver(t, journalWithLoot(t, c.item))
		if c.refused {
			if !errors.Is(err, game.ErrInvalidSession) || instances.Count() != 0 {
				t.Errorf("%s: restore = %v with %d sessions, want %v and none", c.name, err, instances.Count(), game.ErrInvalidSession)
			}
			continue
		}
		if err != nil || len(instances.SavedSessions()) != 1 {
			t.Errorf("%s: restore = %v with runs %+v, want the run back", c.name, err, instances.SavedSessions())
		}
	}
}

// Unlike manager-only restoration, startup crosses the sessions-v3 codec and the
// reward journal overlay before constructing the terrain. The journal predates the
// ordinary chest openings; only sessions.bin owns those once-per-run flags.
func TestStartupKeepsAllOpenedChestsAcrossRewardOverlay(t *testing.T) {
	dir := journalWithRun(t, world.WorldgenVersion)
	journal, err := persist.OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	if err := journal.AppendDefeat(1, persist.RewardDefeat{Kind: vnet.MobKindDraugrKing}, nil); err != nil {
		t.Fatal(err)
	}
	snapshot, err := journal.Snapshot()
	if err != nil || len(snapshot.Runs) != 1 {
		t.Fatalf("journal fixture: %v", err)
	}
	record := snapshot.Runs[0].Session
	if len(record.OpenedChests) != 0 {
		t.Fatal("fixture journal already knows ordinary chest openings")
	}
	record.DefeatedBosses = []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing}
	record.Checkpoints, record.SolvedPuzzles = 3, []uint8{1, 3}
	record.OpenedChests = []uint8{0, 1, 2}
	if err := openSessionStore(t, dir).Save([]persist.SessionRecord{record}); err != nil {
		t.Fatal(err)
	}

	instances, err := restoreOver(t, dir)
	if err != nil {
		t.Fatal(err)
	}
	live, ok := instances.Lookup(record.ID)
	if !ok {
		t.Fatal("journaled run did not restore")
	}
	if got := live.Sim.DungeonRoute().OpenedChests; !slices.Equal(got, record.OpenedChests) {
		t.Fatalf("startup lost opened chests: %v", got)
	}
	checked := 0
	for _, anchor := range world.InstanceDungeonAnchors(live.Seed) {
		if anchor.Kind != world.AnchorInstanceChest {
			continue
		}
		coord := world.ChunkOf(anchor.X, anchor.Y, anchor.Z)
		chunk, _, err := live.Chunks.Get(live.Context, coord)
		if err != nil {
			t.Fatal(err)
		}
		ox, oy, oz := coord.Origin()
		if block := chunk.At(int(anchor.X-ox), int(anchor.Y-oy), int(anchor.Z-oz)); block != world.ChestOpen {
			t.Fatalf("restored chest %d block = %d, want ChestOpen", anchor.Index, block)
		}
		lootOffered := false
		player, err := live.Sim.JoinCharacter(uint64(1000+checked), identity.PlayerID{byte(checked + 1)}, 1,
			[]string{"Asta", "Bryn", "Cora"}[checked],
			[3]float32{float32(anchor.X) - .5, float32(anchor.Y), float32(anchor.Z) + .5}, testAppearance(), nil,
			func(frame []byte) bool {
				if vnet.GetRootAsEnvelope(frame, 0).PayloadType() == vnet.PayloadLootState {
					lootOffered = true
				}
				return true
			})
		if err != nil {
			t.Fatal(err)
		}
		before := player.InventoryState()
		reason, useErr := player.UseMechanism([3]int32{int32(anchor.X), int32(anchor.Y), int32(anchor.Z)})
		if useErr == nil || reason != vnet.RefusalReasonChestAlreadyOpened {
			t.Fatalf("restored chest %d use = %s, %v", anchor.Index, reason, useErr)
		}
		if lootOffered || !reflect.DeepEqual(before, player.InventoryState()) {
			t.Fatalf("restored chest %d offered ordinary loot or changed inventory", anchor.Index)
		}
		checked++
	}
	if checked != world.InstanceChestCount {
		t.Fatalf("checked %d restored chests, want %d", checked, world.InstanceChestCount)
	}
}
