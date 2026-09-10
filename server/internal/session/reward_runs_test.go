package session

import (
	"log/slog"
	"slices"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
)

// runWorld is a durable world with rewards enabled over an instance manager, holding no
// characters: the runs it syncs are progress and bindings only.
type runWorld struct {
	store   *persist.Store
	journal *persist.RewardStore
	ids     *Identities
	manager *game.InstanceManager
}

func newRunWorld(t *testing.T, store *persist.Store, journal *persist.RewardStore) *runWorld {
	t.Helper()
	ids, _ := internalIdentities(t, store)
	manager, err := game.NewInstanceManager(20, 2, 4, NewRegistry(DefaultConcurrentSessions).NextID, slog.New(slog.DiscardHandler))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(manager.Close)
	if err := ids.EnableRewards(journal, manager); err != nil {
		t.Fatal(err)
	}
	return &runWorld{store, journal, ids, manager}
}

func openRunStores(t *testing.T) (*persist.Store, *persist.RewardStore) {
	t.Helper()
	dir := t.TempDir()
	store, err := persist.OpenStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	journal, err := persist.OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	return store, journal
}

func runCharacter(n byte) game.InstanceCharacter {
	return game.InstanceCharacter{PlayerID: identity.IDOf(identity.Account{n}), CharacterID: uint64(n)}
}

func savedRun(expires int64, defeated []vnet.MobKind, bound ...game.InstanceCharacter) game.SavedSession {
	return game.SavedSession{
		ID: 7, Seed: 0x5EED, Ruin: game.InstanceRuin{CellX: 2, CellZ: 3},
		ExpiresUnix: expires, DefeatedBosses: defeated, Bound: bound,
	}
}

func (w *runWorld) journalNow(t *testing.T) persist.RewardJournal {
	t.Helper()
	journal, err := w.journal.Snapshot()
	if err != nil {
		t.Fatal(err)
	}
	return journal
}

func sessionCharacters(characters ...game.InstanceCharacter) []persist.SessionCharacter {
	out := make([]persist.SessionCharacter, len(characters))
	for i, c := range characters {
		out[i] = persist.SessionCharacter{PlayerID: c.PlayerID, CharacterID: c.CharacterID}
	}
	return out
}

func TestSyncRewardRunsWritesDefeatedProgressAndBindingsOnce(t *testing.T) {
	t.Parallel()
	store, journal := openRunStores(t)
	w := newRunWorld(t, store, journal)
	first := runCharacter(51)
	run := savedRun(time.Now().Add(time.Hour).Unix(), []vnet.MobKind{vnet.MobKindVargrGuardian}, first)
	if _, _, err := w.manager.RestoreSessions([]game.SavedSession{run}); err != nil {
		t.Fatal(err)
	}

	if err := w.ids.SyncRewardRuns(time.Now()); err != nil {
		t.Fatal(err)
	}
	got := w.journalNow(t)
	if got.NextGeneration != 2 || len(got.Runs) != 1 || got.Runs[0].Generation != 1 {
		t.Fatalf("journal = %+v, want one run at generation 1", got)
	}
	stored := got.Runs[0]
	if !slices.Equal(stored.Session.DefeatedBosses, run.DefeatedBosses) || !slices.Equal(stored.Session.Bound, sessionCharacters(first)) {
		t.Fatalf("journal run = %+v", stored.Session)
	}
	if len(stored.Defeats) != 1 || len(stored.Defeats[0].Personal) != 0 || len(stored.Defeats[0].Experience) != 0 {
		t.Fatalf("defeat = %+v, want progress with no entitlement", stored.Defeats)
	}
	if saved := w.manager.SavedSessions(); len(saved) != 1 || saved[0].Generation != 1 {
		t.Fatalf("the manager was not told its run's generation: %+v", saved)
	}

	if err := w.ids.SyncRewardRuns(time.Now()); err != nil {
		t.Fatal(err)
	}
	if again := w.journalNow(t); again.Revision != got.Revision {
		t.Fatalf("an unchanged run was written again: revision %d -> %d", got.Revision, again.Revision)
	}
}

// After a restart the run keeps its generation, so a later defeat and a later binding
// reach the same durable run rather than a new one.
func TestSyncRewardRunsExtendsARestoredRunUnderItsGeneration(t *testing.T) {
	t.Parallel()
	store, journal := openRunStores(t)
	first, second := runCharacter(51), runCharacter(52)
	before := newRunWorld(t, store, journal)
	run := savedRun(time.Now().Add(time.Hour).Unix(), []vnet.MobKind{vnet.MobKindVargrGuardian}, first)
	if _, _, err := before.manager.RestoreSessions([]game.SavedSession{run}); err != nil {
		t.Fatal(err)
	}
	if err := before.ids.SyncRewardRuns(time.Now()); err != nil {
		t.Fatal(err)
	}

	next := before.manager.SavedSessions()[0]
	next.DefeatedBosses = append(next.DefeatedBosses, vnet.MobKindDraugrKing)
	next.Bound = append(next.Bound, second)
	after := newRunWorld(t, store, journal)
	if _, _, err := after.manager.RestoreSessions([]game.SavedSession{next}); err != nil {
		t.Fatal(err)
	}
	if err := after.ids.SyncRewardRuns(time.Now()); err != nil {
		t.Fatal(err)
	}
	got := after.journalNow(t)
	if got.NextGeneration != 2 || len(got.Runs) != 1 {
		t.Fatalf("journal = %+v, want the same single run", got)
	}
	stored := got.Runs[0].Session
	if !slices.Equal(stored.DefeatedBosses, []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing}) || !slices.Equal(stored.Bound, sessionCharacters(first, second)) {
		t.Fatalf("journal run = %+v", stored)
	}

	// A binding that arrives with no new defeat is unioned onto the run too.
	third := runCharacter(53)
	joined := after.manager.SavedSessions()[0]
	joined.Bound = append(joined.Bound, third)
	later := newRunWorld(t, store, journal)
	if _, _, err := later.manager.RestoreSessions([]game.SavedSession{joined}); err != nil {
		t.Fatal(err)
	}
	if err := later.ids.SyncRewardRuns(time.Now()); err != nil {
		t.Fatal(err)
	}
	if bound := later.journalNow(t).Runs[0].Session.Bound; !slices.Equal(bound, sessionCharacters(first, second, third)) {
		t.Fatalf("bindings = %+v", bound)
	}
}

func TestSyncRewardRunsCollectsResetRunsAndNeverReusesTheirGeneration(t *testing.T) {
	t.Parallel()
	store, journal := openRunStores(t)
	now := time.Now()
	w := newRunWorld(t, store, journal)
	run := savedRun(now.Add(time.Minute).Unix(), []vnet.MobKind{vnet.MobKindVargrGuardian}, runCharacter(51))
	if _, _, err := w.manager.RestoreSessions([]game.SavedSession{run}); err != nil {
		t.Fatal(err)
	}
	if err := w.ids.SyncRewardRuns(now); err != nil {
		t.Fatal(err)
	}
	if err := w.ids.SyncRewardRuns(now.Add(2 * time.Minute)); err != nil {
		t.Fatal(err)
	}
	if got := w.journalNow(t); len(got.Runs) != 0 || got.NextGeneration != 2 {
		t.Fatalf("journal after the reset = %+v", got)
	}

	fresh := newRunWorld(t, store, journal)
	tomorrow := savedRun(now.Add(time.Hour).Unix(), []vnet.MobKind{vnet.MobKindVargrGuardian}, runCharacter(51))
	if _, _, err := fresh.manager.RestoreSessions([]game.SavedSession{tomorrow}); err != nil {
		t.Fatal(err)
	}
	if err := fresh.ids.SyncRewardRuns(now); err != nil {
		t.Fatal(err)
	}
	if got := fresh.journalNow(t); len(got.Runs) != 1 || got.Runs[0].Generation != 2 {
		t.Fatalf("the next run = %+v, want generation 2", got.Runs)
	}
}

func TestSyncRewardRunsIsANoOpWithoutRewards(t *testing.T) {
	t.Parallel()
	ids, _ := internalIdentities(t, persist.NewMemoryStore())
	if err := ids.SyncRewardRuns(time.Now()); err != nil {
		t.Fatalf("a world without rewards = %v", err)
	}
}

func TestValidateRewardRecordJudgesTheLife(t *testing.T) {
	t.Parallel()
	valid := persist.Record{Health: game.PlayerMaxHealth, Hunger: game.PlayerMaxHunger}
	if err := ValidateRewardRecord(valid); err != nil {
		t.Fatalf("a valid life = %v", err)
	}
	invalid := valid
	invalid.Health = 65535
	if err := ValidateRewardRecord(invalid); err == nil {
		t.Fatal("an impossible life was accepted")
	}
}
