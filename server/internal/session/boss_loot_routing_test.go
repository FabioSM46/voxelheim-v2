package session_test

import (
	"context"
	"path/filepath"
	"slices"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

type consumedBossLoot struct {
	indices []uint8
	silver  uint32
}

// heldBossCorpse stands in for a killed dungeon boss's corpse behind one player, the one part
// of this path a session test cannot build. Every take against it must become a reward claim,
// and a claim that lands consumes from it. Everything else is the player's own.
type heldBossCorpse struct {
	*game.Player
	selection game.BossLootSelection
	consumed  chan consumedBossLoot
}

func (h heldBossCorpse) TakeLoot(protocol.LootTakeRequest) (vnet.RefusalReason, error) {
	return vnet.RefusalReasonUnknown, game.ErrBossRewardClaimRequired
}

func (h heldBossCorpse) TakeAllLoot(protocol.LootTakeAllRequest) (vnet.RefusalReason, error) {
	return vnet.RefusalReasonUnknown, game.ErrBossRewardClaimRequired
}

func (h heldBossCorpse) BossLootToClaim(uint64, uint32, uint64) (game.BossLootSelection, vnet.RefusalReason, error) {
	return h.selection, vnet.RefusalReasonUnknown, nil
}

func (h heldBossCorpse) ConsumeClaimedBossLoot(_ uint64, indices []uint8, silver uint32) bool {
	h.consumed <- consumedBossLoot{slices.Clone(indices), silver}
	return true
}

// A loot take that must be a boss reward claim becomes one only on the portal visit into
// that boss's run. The same take is refused before the character enters and again after it
// has left, through the frames a client sends and the session's own idea of where it stands.
func TestALootTakeBecomesABossRewardClaimOnlyInsideItsDungeonVisit(t *testing.T) {
	cfg, chunks, open, peers, _ := portalSession(t, 2)
	identities, store := knownIdentities(t)
	var fallback [3]float64
	for axis, value := range cfg.Spawn {
		fallback[axis] = float64(value)
	}
	character := seedAt(t, store, testAccount(93), "Looter", fallback)

	// The run this character already owes, restored rather than killed for, with its journal
	// generation: a real portal crossing then lands in it.
	ruin, found := world.RuinAt(cfg.WorldSeed, 0, 0)
	if !found {
		t.Fatal("fixture ruin missing")
	}
	const runID = uint64(1) << 40
	guardian := []vnet.MobKind{vnet.MobKindVargrGuardian}
	expires := time.Now().Add(time.Hour).Unix()
	owner := game.InstanceCharacter{PlayerID: character.Owner, CharacterID: uint64(character.ID)}
	run := game.SavedSession{ID: runID, Seed: int64(runID ^ 0x49a3d758c1e260bf), Ruin: game.InstanceRuin{CellX: ruin.CellX, CellZ: ruin.CellZ},
		ExpiresUnix: expires, DefeatedBosses: guardian, Bound: []game.InstanceCharacter{owner}, Generation: 1}
	if _, _, err := cfg.Instances.RestoreSessions([]game.SavedSession{run}); err != nil {
		t.Fatal(err)
	}
	journal, err := persist.OpenRewardStore(filepath.Dir(store.Dir()))
	if err != nil {
		t.Fatal(err)
	}
	pelt := protocol.InventoryStack{ItemID: uint16(game.ItemVargrPelt), Count: 3}
	bound := persist.SessionCharacter{PlayerID: character.Owner, CharacterID: uint64(character.ID)}
	record := persist.SessionRecord{ID: runID, Seed: run.Seed, Ruin: [2]int64{ruin.CellX, ruin.CellZ}, ExpiresUnix: expires,
		DefeatedBosses: guardian, Bound: []persist.SessionCharacter{bound}}
	defeat := persist.RewardDefeat{Kind: vnet.MobKindVargrGuardian, Personal: []persist.PersonalReward{{Owner: bound, Entries: []protocol.InventoryStack{pelt}, Silver: 30}}}
	if err := journal.AllocateRun(store, 1, record, world.WorldgenVersion, defeat); err != nil {
		t.Fatal(err)
	}
	if err := identities.EnableRewards(journal, cfg.Instances); err != nil {
		t.Fatal(err)
	}
	const corpseID = uint64(1) << 50
	consumed := make(chan consumedBossLoot, 4)
	session.PutBossLootBehind(identities, func(p *game.Player) session.SessionLoot {
		return heldBossCorpse{Player: p, consumed: consumed, selection: game.BossLootSelection{
			CorpseID: corpseID, Kind: vnet.MobKindVargrGuardian, Entries: []protocol.InventoryStack{pelt}, EntryIndices: []uint8{0}, Silver: 30,
		}}
	})

	conn := newFakeConn()
	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, cfg, noTimeouts(), chunks, open, peers, identities, peers.NextID(), discard())
	}()
	t.Cleanup(func() {
		_ = conn.Close()
		endsCleanly(t, done)
	})
	conn.in <- hello(93)
	characterList(t, nextFrame(t, conn))
	conn.in <- protocol.EncodeSelectCharacterRequest(protocol.SelectCharacterRequest{CharacterID: uint64(character.ID)})
	welcomeFrom(t, vnet.GetRootAsEnvelope(nextFrame(t, conn), 0))
	frames := collect(t, conn)

	// A refusal is sent through the session's blocking queue, so counting them waits on a
	// frame that is always delivered.
	refused := func(action vnet.RefusedAction) int {
		n := 0
		for _, r := range frames.actionRefusals() {
			if r.Action == action && r.Reason == vnet.RefusalReasonCorpseUnavailable {
				n++
			}
		}
		return n
	}
	take := protocol.EncodeLootTakeRequest(protocol.LootTakeRequest{CorpseID: corpseID, EntryID: 1, Revision: 1, ClientTick: 1})
	noClaim := func(when string) {
		t.Helper()
		select {
		case got := <-consumed:
			t.Fatalf("a take %s was claimed: %+v", when, got)
		default:
		}
	}

	conn.in <- take
	waitUntil(t, "the take before entering to be refused", func() bool { return refused(vnet.RefusedActionTakeLoot) == 1 })
	noClaim("before entering")

	walkIntoVeil(t, conn, open, "entry", func() bool { return len(frames.transitions()) == 1 })
	change := frames.transitions()[0]
	if change.WorldID != runID {
		t.Fatalf("entered world %d, want the owed run %d", change.WorldID, runID)
	}
	conn.in <- take
	select {
	case got := <-consumed:
		if !slices.Equal(got.indices, []uint8{0}) || got.silver != 30 {
			t.Fatalf("claimed = %+v, want roll index 0 and 30 silver", got)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("a take inside the dungeon visit was never claimed")
	}
	snapshot, err := journal.Snapshot()
	if err != nil {
		t.Fatal(err)
	}
	if personal := snapshot.Runs[0].Defeats[0].Personal[0]; personal.Taken != 1 || !personal.SilverTaken {
		t.Fatalf("journal personal = %+v, want the pelt and silver taken", personal)
	}

	// Leave by walking into the exit arch.
	walkToExit(t, cfg, conn, change, "exit", func() bool { return len(frames.transitions()) == 2 })
	if frames.transitions()[1].WorldID != 0 {
		t.Fatal("the exit did not return to the open world")
	}

	conn.in <- take
	waitUntil(t, "the take after leaving to be refused", func() bool { return refused(vnet.RefusedActionTakeLoot) == 2 })
	noClaim("after leaving")
}
