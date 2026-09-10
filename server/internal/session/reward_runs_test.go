package session

import (
	"log/slog"
	"os"
	"slices"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
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

// overlaidRuns restores what a restart would restore from the journal and a sessions file
// holding records, as the manager receives it.
func overlaidRuns(t *testing.T, journal *persist.RewardStore, records []persist.SessionRecord) []game.SavedSession {
	t.Helper()
	overlaid, err := journal.OverlaySessions(records, time.Now().Unix(), world.WorldgenVersion)
	if err != nil {
		t.Fatalf("OverlaySessions: %v", err)
	}
	runs := make([]game.SavedSession, len(overlaid))
	for i, run := range overlaid {
		bound := make([]game.InstanceCharacter, len(run.Session.Bound))
		for k, who := range run.Session.Bound {
			bound[k] = game.InstanceCharacter{PlayerID: who.PlayerID, CharacterID: who.CharacterID}
		}
		runs[i] = game.SavedSession{
			ID: run.Session.ID, Seed: run.Session.Seed, Ruin: game.InstanceRuin{CellX: run.Session.Ruin[0], CellZ: run.Session.Ruin[1]},
			ExpiresUnix: run.Session.ExpiresUnix, DefeatedBosses: run.Session.DefeatedBosses, Bound: bound, Generation: run.Generation,
		}
	}
	return runs
}

func sessionRecordOf(run game.SavedSession) persist.SessionRecord {
	return persist.SessionRecord{
		ID: run.ID, Seed: run.Seed, Ruin: [2]int64{run.Ruin.CellX, run.Ruin.CellZ}, ExpiresUnix: run.ExpiresUnix,
		DefeatedBosses: run.DefeatedBosses, Bound: sessionCharacters(run.Bound...),
	}
}

// Identity, progress and bindings reach the journal in one write, so there is no window
// in which a freshly allocated run holds fewer defeats than the manager had.
func TestSyncRewardRunsAllocatesProgressAndBindingsWithTheRun(t *testing.T) {
	t.Parallel()
	store, journal := openRunStores(t)
	w := newRunWorld(t, store, journal)
	first, second := runCharacter(51), runCharacter(52)
	run := savedRun(time.Now().Add(time.Hour).Unix(), []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing}, first, second)
	if _, _, err := w.manager.RestoreSessions([]game.SavedSession{run}); err != nil {
		t.Fatal(err)
	}
	before := w.journalNow(t).Revision
	if err := w.ids.SyncRewardRuns(time.Now()); err != nil {
		t.Fatal(err)
	}
	got := w.journalNow(t)
	if got.Revision != before+1 {
		t.Fatalf("allocating a run took %d journal writes, want 1", got.Revision-before)
	}
	stored := got.Runs[0].Session
	// The manager orders bindings by identity; the allocation carries that order unchanged.
	current := w.manager.SavedSessions()[0]
	if !slices.Equal(stored.DefeatedBosses, run.DefeatedBosses) || len(stored.Bound) != 2 ||
		!slices.Equal(stored.Bound, sessionCharacters(current.Bound...)) {
		t.Fatalf("allocated run = %+v, want its defeats and both bindings", stored)
	}
}

// The overlay does treat journal defeats as authoritative. A defeat the sessions file saw
// but the journal never received is restored undefeated, and nothing is written for it
// until it happens again: the journal only claims progress it made durable.
func TestJournalRunWithFewerDefeatsThanTheSessionsFileRestoresTheJournalsProgress(t *testing.T) {
	t.Parallel()
	store, journal := openRunStores(t)
	first := runCharacter(51)
	w := newRunWorld(t, store, journal)
	run := savedRun(time.Now().Add(time.Hour).Unix(), []vnet.MobKind{vnet.MobKindVargrGuardian}, first)
	if _, _, err := w.manager.RestoreSessions([]game.SavedSession{run}); err != nil {
		t.Fatal(err)
	}
	if err := w.ids.SyncRewardRuns(time.Now()); err != nil {
		t.Fatal(err)
	}

	ahead := w.manager.SavedSessions()[0]
	ahead.DefeatedBosses = append(slices.Clone(ahead.DefeatedBosses), vnet.MobKindDraugrKing)
	restored := overlaidRuns(t, journal, []persist.SessionRecord{sessionRecordOf(ahead)})
	if len(restored) != 1 || restored[0].Generation != 1 ||
		!slices.Equal(restored[0].DefeatedBosses, []vnet.MobKind{vnet.MobKindVargrGuardian}) {
		t.Fatalf("restored = %+v, want the journal's single defeat under generation 1", restored)
	}

	after := newRunWorld(t, store, journal)
	if _, _, err := after.manager.RestoreSessions(restored); err != nil {
		t.Fatal(err)
	}
	revision := after.journalNow(t).Revision
	if err := after.ids.SyncRewardRuns(time.Now()); err != nil {
		t.Fatal(err)
	}
	if got := after.journalNow(t); got.Revision != revision || len(got.Runs) != 1 {
		t.Fatalf("a restored run was written again: %+v", got)
	}
}

// A refused assignment leaves a journal run the manager does not hold. The next pass adopts
// it by identity rather than allocating the run a second generation, and a restart still
// starts.
func TestSyncRewardRunsAdoptsARunWhoseGenerationTheManagerRefused(t *testing.T) {
	t.Parallel()
	store, journal := openRunStores(t)
	w := newRunWorld(t, store, journal)
	run := savedRun(time.Now().Add(time.Hour).Unix(), []vnet.MobKind{vnet.MobKindVargrGuardian}, runCharacter(51))
	if _, _, err := w.manager.RestoreSessions([]game.SavedSession{run}); err != nil {
		t.Fatal(err)
	}
	refusals := 0
	w.ids.rewards.assignRun = func(game.SavedSession, uint64) bool {
		refusals++
		return false
	}
	if err := w.ids.SyncRewardRuns(time.Now()); err != nil {
		t.Fatal(err)
	}
	if refusals != 1 || w.manager.SavedSessions()[0].Generation != 0 || len(w.journalNow(t).Runs) != 1 {
		t.Fatalf("the forced refusal did not leave a journal run the manager lacks (refusals %d)", refusals)
	}

	w.ids.rewards.assignRun = nil
	if err := w.ids.SyncRewardRuns(time.Now()); err != nil {
		t.Fatal(err)
	}
	got := w.journalNow(t)
	if got.NextGeneration != 2 || len(got.Runs) != 1 {
		t.Fatalf("journal = %+v, want the one run adopted rather than allocated again", got)
	}
	current := w.manager.SavedSessions()[0]
	if current.Generation != 1 {
		t.Fatalf("the manager's run = %+v, want the adopted generation 1", current)
	}

	restarted := newRunWorld(t, store, journal)
	if _, _, err := restarted.manager.RestoreSessions(overlaidRuns(t, journal, []persist.SessionRecord{sessionRecordOf(current)})); err != nil {
		t.Fatalf("a restart over the adopted run was refused: %v", err)
	}
}

// A failed journal write is retried with the same bytes before anything else is written.
func TestSyncRewardRunsRetriesAFailedJournalWriteVerbatim(t *testing.T) {
	t.Parallel()
	if os.Geteuid() == 0 {
		t.Skip("directory permissions do not bind root")
	}
	dir := t.TempDir()
	store, err := persist.OpenStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	journal, err := persist.OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	w := newRunWorld(t, store, journal)
	run := savedRun(time.Now().Add(time.Hour).Unix(), []vnet.MobKind{vnet.MobKindVargrGuardian}, runCharacter(51))
	if _, _, err := w.manager.RestoreSessions([]game.SavedSession{run}); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(dir, 0o500); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.Chmod(dir, 0o700) })
	if err := w.ids.SyncRewardRuns(time.Now()); err == nil {
		t.Fatal("a journal write that could not land reported success")
	}
	if w.ids.rewards.pendingRunWrite == nil {
		t.Fatal("the failed write was not kept for a verbatim retry")
	}
	if err := os.Chmod(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := w.ids.SyncRewardRuns(time.Now()); err != nil {
		t.Fatalf("the verbatim retry failed: %v", err)
	}
	reopened, err := persist.OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	durable, err := reopened.Snapshot()
	if err != nil || durable.NextGeneration != 2 || len(durable.Runs) != 1 {
		t.Fatalf("durable journal = %+v, %v; want one run", durable, err)
	}
	if w.manager.SavedSessions()[0].Generation != 1 {
		t.Fatal("the manager was not told the generation after the retry")
	}
}
