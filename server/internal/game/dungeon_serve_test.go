package game_test

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"net"
	"slices"
	"sync"
	"testing"
	"time"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

const serveSeed = 0x5EED

// serveTestLog is where the servers these tests boot log. It discards unless a test overrides it.
var serveTestLog = slog.New(slog.DiscardHandler)

// serveConn is a connection whose frames the test writes and reads.
type serveConn struct {
	in        chan []byte
	out       chan []byte
	done      chan struct{}
	closeOnce sync.Once
}

func newServeConn() *serveConn {
	return &serveConn{in: make(chan []byte, 8), out: make(chan []byte, 256), done: make(chan struct{})}
}

func (c *serveConn) ReadFrame() ([]byte, error) {
	select {
	case frame := <-c.in:
		return frame, nil
	case <-c.done:
		return nil, io.EOF
	}
}

func (c *serveConn) WriteFrame(frame []byte) error {
	select {
	case c.out <- frame:
		return nil
	case <-c.done:
		return net.ErrClosed
	}
}

func (c *serveConn) RemoteAddr() string              { return "serve-test" }
func (c *serveConn) SetReadDeadline(time.Time) error { return nil }
func (c *serveConn) Close() error                    { c.closeOnce.Do(func() { close(c.done) }); return nil }
func (c *serveConn) send(frame []byte)               { c.in <- frame }

// serveFrames is what one session has been sent, decoded as it arrives.
type serveFrames struct {
	mu          sync.Mutex
	characters  []uint64
	listed      bool
	welcomed    bool
	worlds      []uint64
	loot        map[uint64]protocol.LootState
	refusals    []protocol.ActionRefused
	inventories int
}

func (f *serveFrames) absorb(frame []byte) {
	env := vnet.GetRootAsEnvelope(frame, 0)
	var table flatbuffers.Table
	if !env.Payload(&table) {
		return
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	switch env.PayloadType() {
	case vnet.PayloadServerCharacterList:
		list := new(vnet.ServerCharacterList)
		list.Init(table.Bytes, table.Pos)
		f.characters = f.characters[:0]
		for i := range list.CharactersLength() {
			var summary vnet.CharacterSummary
			if list.Characters(&summary, i) {
				f.characters = append(f.characters, summary.CharacterId())
			}
		}
		f.listed = true
	case vnet.PayloadServerWelcome:
		f.welcomed = true
	case vnet.PayloadWorldChange:
		change := new(vnet.WorldChange)
		change.Init(table.Bytes, table.Pos)
		f.worlds = append(f.worlds, change.WorldId())
	case vnet.PayloadLootState:
		state := new(vnet.LootState)
		state.Init(table.Bytes, table.Pos)
		decoded := protocol.LootState{CorpseID: state.CorpseId(), Revision: state.Revision(), Silver: state.Silver()}
		for i := range state.EntriesLength() {
			var entry vnet.LootEntry
			if state.Entries(&entry, i) {
				decoded.Entries = append(decoded.Entries, protocol.LootEntry{EntryID: entry.EntryId(), ItemID: entry.ItemId(), Count: entry.Count()})
			}
		}
		if f.loot == nil {
			f.loot = make(map[uint64]protocol.LootState)
		}
		f.loot[decoded.CorpseID] = decoded
	case vnet.PayloadActionRefused:
		refused := new(vnet.ActionRefused)
		refused.Init(table.Bytes, table.Pos)
		f.refusals = append(f.refusals, protocol.ActionRefused{Action: refused.Action(), Reason: refused.Reason()})
	case vnet.PayloadInventoryState:
		f.inventories++
	}
}

func (f *serveFrames) read(view func(*serveFrames) bool) bool {
	f.mu.Lock()
	defer f.mu.Unlock()
	return view(f)
}

// serveWorld is one server process over one world directory: the stores opened with startup
// recovery, an instance manager with durable boss rewards, and the runs the journal holds
// restored with the loot it still owes.
type serveWorld struct {
	dir        string
	pair       *ticket.Pair
	worldID    ticket.WorldID
	players    *persist.Store
	journal    *persist.RewardStore
	chunks     *world.Cache
	open       *game.Sim
	peers      *session.Registry
	manager    *game.InstanceManager
	identities *session.Identities
	cfg        session.Config
	request    protocol.PortalRequest
}

func bootServeWorld(t *testing.T, dir string, pair *ticket.Pair, worldID ticket.WorldID) *serveWorld {
	t.Helper()
	log := serveTestLog
	journal, err := persist.OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	players, err := persist.OpenStoreWithRewardRecovery(dir, journal, session.ValidateRewardRecord)
	if err != nil {
		t.Fatalf("startup recovery: %v", err)
	}
	ruin, found := world.RuinAt(serveSeed, 0, 0)
	if !found {
		t.Fatal("fixture ruin missing")
	}
	group := game.NewWorldGroup()
	chunks := world.NewCache(serveSeed, 4, 512)
	peers := session.NewRegistry(session.DefaultConcurrentSessions)
	open, err := game.NewSim(20, 1, serveSeed, game.NewCacheTerrain(chunks), chunks, peers.NextID, log, game.WithWorldGroup(group))
	if err != nil {
		t.Fatal(err)
	}
	manager, err := game.NewInstanceManager(20, 1, 4, peers.NextID, log, game.WithWorldGroup(group), game.WithDurableBossRewards(true))
	if err != nil {
		t.Fatal(err)
	}
	verifier, err := session.NewVerifier(pair.Public(), worldID, nil)
	if err != nil {
		t.Fatal(err)
	}
	identities, err := session.NewIdentities(players, nil, nil, verifier, log)
	if err != nil {
		t.Fatal(err)
	}
	if err := identities.EnableRewards(journal, manager); err != nil {
		t.Fatal(err)
	}

	// The restore voxelheimd performs over the journal, with the loot it still owes.
	overlaid, err := journal.OverlaySessions(nil, time.Now().Unix(), world.WorldgenVersion)
	if err != nil {
		t.Fatal(err)
	}
	snapshot, err := journal.Snapshot()
	if err != nil {
		t.Fatal(err)
	}
	var saved []game.SavedSession
	for _, run := range overlaid {
		restored := game.SavedSession{ID: run.Session.ID, Seed: run.Session.Seed, Ruin: game.InstanceRuin{CellX: run.Session.Ruin[0], CellZ: run.Session.Ruin[1]},
			ExpiresUnix: run.Session.ExpiresUnix, DefeatedBosses: run.Session.DefeatedBosses, Generation: run.Generation}
		for _, who := range run.Session.Bound {
			restored.Bound = append(restored.Bound, game.InstanceCharacter{PlayerID: who.PlayerID, CharacterID: who.CharacterID})
		}
		for _, d := range snapshot.OwedLoot(run.Generation) {
			defeat := game.BossRewardDefeat{Kind: d.Kind}
			for _, p := range d.Personal {
				defeat.Personal = append(defeat.Personal, game.BossPersonalReward{
					Owner:   game.InstanceCharacter{PlayerID: p.Owner.PlayerID, CharacterID: p.Owner.CharacterID},
					Entries: p.Entries, Silver: p.Silver, Taken: p.Taken, SilverTaken: p.SilverTaken,
				})
			}
			restored.HeldRewards = append(restored.HeldRewards, defeat)
		}
		saved = append(saved, restored)
	}
	if len(saved) != 0 {
		if _, _, err := manager.RestoreSessions(saved); err != nil {
			t.Fatalf("restoring the journal's runs: %v", err)
		}
	}

	w := &serveWorld{dir: dir, pair: pair, worldID: worldID, players: players, journal: journal, chunks: chunks, open: open, peers: peers,
		manager: manager, identities: identities,
		cfg: session.Config{Instances: manager, WorldSeed: serveSeed, TickRate: 20, ChunkSize: 32, ViewDistance: 1,
			Spawn: [3]float32{float32(ruin.Arch.X) + 1.5, float32(ruin.Arch.Y) - 1, float32(ruin.Arch.Z) + .5}, VoiceRange: game.VoiceRangeDefault},
		request: protocol.PortalRequest{HasArch: true, Arch: [3]int32{int32(ruin.Arch.X), int32(ruin.Arch.Y), int32(ruin.Arch.Z)}},
	}
	return w
}

// serveClient is one connected session.
type serveClient struct {
	conn     *serveConn
	frames   *serveFrames
	entityID uint64
	served   chan error
	drained  chan struct{}
}

// awaitFrames steps the dungeons until the frames show what is wanted. Every frame waited on here
// is one the simulation offers again on each tick until the session accepts it, so it arrives.
func (w *serveWorld) awaitFrames(t *testing.T, c *serveClient, what string, done func(*serveFrames) bool) {
	t.Helper()
	deadline := time.Now().Add(20 * time.Second)
	for !c.frames.read(done) {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s", what)
		}
		w.manager.Step()
		time.Sleep(time.Millisecond)
	}
}

func (w *serveWorld) connect(t *testing.T, account ticket.AccountID) *serveClient {
	t.Helper()
	c := &serveClient{conn: newServeConn(), frames: &serveFrames{}, entityID: w.peers.NextID(), served: make(chan error, 1), drained: make(chan struct{})}
	go func() {
		defer close(c.drained)
		for {
			select {
			case frame := <-c.conn.out:
				c.frames.absorb(frame)
			case <-c.conn.done:
				return
			}
		}
	}()
	go func() {
		c.served <- session.Serve(context.Background(), c.conn, w.cfg, session.Timeouts{}, w.chunks, w.open, w.peers, w.identities, c.entityID, serveTestLog)
	}()
	minted, _, err := w.pair.Mint(account, w.worldID, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	c.conn.send(protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Hunter", minted[:]))
	w.awaitFrames(t, c, "the character list", func(f *serveFrames) bool { return f.listed })
	var existing []uint64
	c.frames.read(func(f *serveFrames) bool { existing = slices.Clone(f.characters); return true })
	if len(existing) == 0 {
		c.conn.send(protocol.EncodeCreateCharacterRequest(protocol.CreateCharacterRequest{Name: "Hunter", HasAppearance: true, Appearance: protocol.Appearance{
			SkinColor: 0x00E3C4A0, ShirtColor: 0x004A5D3B, TrousersColor: 0x002B2118, ShoesColor: 0x00553311, HairModel: vnet.HairModelBraided, HairColor: 0x00B07A32,
		}}))
	} else {
		c.conn.send(protocol.EncodeSelectCharacterRequest(protocol.SelectCharacterRequest{CharacterID: existing[0]}))
	}
	w.awaitFrames(t, c, "the welcome", func(f *serveFrames) bool { return f.welcomed })
	return c
}

// autosave writes every connected character as voxelheimd's autosave loop does. A claim's durable
// baseline is the character's saved life, so a server saves before any reward can be claimed.
func (w *serveWorld) autosave(t *testing.T) {
	t.Helper()
	if err := w.identities.RememberCharacters(w.manager.Records(w.open)); err != nil {
		t.Fatalf("autosave: %v", err)
	}
}

// cross sends the portal request and waits for the session to change worlds into a run.
func (w *serveWorld) cross(t *testing.T, c *serveClient) game.InstanceSession {
	t.Helper()
	c.conn.send(protocol.EncodePortalRequest(w.request))
	w.awaitFrames(t, c, "the crossing", func(f *serveFrames) bool { return len(f.worlds) > 0 })
	var id uint64
	c.frames.read(func(f *serveFrames) bool { id = f.worlds[len(f.worlds)-1]; return true })
	run, found := w.manager.Lookup(id)
	if !found {
		t.Fatalf("crossed into world %d, which the manager does not hold", id)
	}
	w.autosave(t)
	return run
}

// shutdown stops the process as voxelheimd does: sessions end, claims drain, the journal is
// brought up to date, and the manager closes.
func (w *serveWorld) shutdown(t *testing.T, clients ...*serveClient) {
	t.Helper()
	for _, c := range clients {
		_ = c.conn.Close()
		if err := <-c.served; err != nil {
			t.Fatalf("session ended with %v", err)
		}
		<-c.drained
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := w.identities.DrainRewards(ctx); err != nil {
		t.Fatalf("draining rewards at shutdown: %v", err)
	}
	if err := w.identities.SyncRewardRuns(time.Now()); err != nil {
		t.Fatalf("the shutdown sync: %v", err)
	}
	w.manager.Close()
}

func (w *serveWorld) record(t *testing.T, owner identity.PlayerID) persist.Record {
	t.Helper()
	characters := w.players.Characters(owner)
	if len(characters) != 1 {
		t.Fatalf("the account holds %d characters, want the one it created", len(characters))
	}
	rec, found, err := w.players.Load(characters[0].ID)
	if err != nil || !found {
		t.Fatalf("loading the character = %v, %v", found, err)
	}
	return rec
}

// awaitClaim waits for a claim to finish: the character record and the journal both show it,
// and no intent is left prepared. Both are durable state a finished claim always leaves.
func (w *serveWorld) awaitClaim(t *testing.T, c *serveClient, owner identity.PlayerID, what string, done func(persist.Record, persist.RewardDefeat) bool) persist.Record {
	t.Helper()
	deadline := time.Now().Add(20 * time.Second)
	for {
		rec := w.record(t, owner)
		snapshot, err := w.journal.Snapshot()
		if err != nil {
			t.Fatal(err)
		}
		if len(snapshot.Intents) == 0 && done(rec, w.guardianDefeat(t)) {
			return rec
		}
		if time.Now().After(deadline) {
			snapshot, _ := w.journal.Snapshot()
			var refusals []protocol.ActionRefused
			c.frames.read(func(f *serveFrames) bool { refusals = slices.Clone(f.refusals); return true })
			t.Fatalf("timed out waiting for %s: record epoch %d experience %d; refusals %+v; prepared intents %d", what, rec.BossRewardEpoch, rec.Experience, refusals, len(snapshot.Intents))
		}
		// The world keeps ticking while a claim runs, as a server does: a claim that fails later
		// is answered through a refusal the tick delivers.
		w.manager.Step()
		time.Sleep(2 * time.Millisecond)
	}
}

func (w *serveWorld) guardianDefeat(t *testing.T) persist.RewardDefeat {
	t.Helper()
	snapshot, err := w.journal.Snapshot()
	if err != nil {
		t.Fatal(err)
	}
	for _, run := range snapshot.Runs {
		for _, d := range run.Defeats {
			if d.Kind == vnet.MobKindVargrGuardian {
				return d
			}
		}
	}
	t.Fatal("the journal holds no guardian defeat")
	return persist.RewardDefeat{}
}

// openCorpse stands the player beside the boss's corpse and opens it through a frame.
func (w *serveWorld) openCorpse(t *testing.T, c *serveClient, run game.InstanceSession, kind vnet.MobKind, tick uint32) protocol.LootState {
	t.Helper()
	corpseID, pos, found := run.Sim.BossCorpseForTest(kind)
	if !found {
		t.Fatalf("no %s corpse in the run", kind)
	}
	if err := run.Sim.TeleportForTest(c.entityID, [3]int64{int64(pos[0]), int64(pos[1]), int64(pos[2]) + 1}); err != nil {
		t.Fatal(err)
	}
	c.conn.send(protocol.EncodeLootOpenRequest(protocol.LootOpenRequest{CorpseID: corpseID, ClientTick: tick}))
	var state protocol.LootState
	w.awaitFrames(t, c, "the corpse's loot", func(f *serveFrames) bool {
		s, ok := f.loot[corpseID]
		state = s
		return ok
	})
	return state
}

// The first dungeon played end to end through sessions, over one world directory and two
// restarts. Every take and claim goes through Serve frames and the reward coordinator; only the
// killing blows use a test hook, because a session cannot land one deterministically.
//
// 0/2 → 1/2: the guardian dies, its loot is journaled and released, one entry is taken through a
// frame and the kill's experience is claimed. Restart: the run comes back at 1/2 with no guardian,
// nothing is delivered again, and only the untaken entry is offered, at its roll index. It is
// taken. Second restart: nothing is offered. The king dies: 2/2.
func TestTheFirstDungeonProgressesAcrossRestartsWithEveryRewardDeliveredOnce(t *testing.T) {
	dir := t.TempDir()
	pair, err := ticket.LoadOrCreate(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	worldID, err := ticket.WorldIDFor("midgard")
	if err != nil {
		t.Fatal(err)
	}
	account := ticket.AccountID{93}
	owner := identity.IDOf(identity.Account(account))

	// ---- First process: 0/2 → 1/2, a partial take and the experience.
	w := bootServeWorld(t, dir, pair, worldID)
	c := w.connect(t, account)
	run := w.cross(t, c)
	if !run.Sim.DungeonBossAliveForTest(vnet.MobKindVargrGuardian) || !run.Sim.DungeonBossAliveForTest(vnet.MobKindDraugrKing) {
		t.Fatal("a fresh run does not start at 0/2")
	}
	base := w.record(t, owner)
	if err := run.Sim.KillDungeonBossForTest(vnet.MobKindVargrGuardian, c.entityID); err != nil {
		t.Fatal(err)
	}
	w.manager.Step()
	if err := w.identities.SyncRewardRuns(time.Now()); err != nil {
		t.Fatal(err)
	}
	guardian := w.guardianDefeat(t)
	if len(guardian.Personal) != 1 || len(guardian.Personal[0].Entries) != 2 || len(guardian.Experience) != 1 {
		t.Fatalf("the journaled guardian defeat = %+v, want one owner's two-entry roll and one experience share", guardian)
	}
	share := guardian.Experience[0].Amount

	loot := w.openCorpse(t, c, run, vnet.MobKindVargrGuardian, 1)
	if len(loot.Entries) != 2 || loot.Entries[0].EntryID != 1 || loot.Entries[1].EntryID != 2 {
		t.Fatalf("the released corpse offers %+v, want both rolled entries", loot)
	}
	c.conn.send(protocol.EncodeLootTakeRequest(protocol.LootTakeRequest{CorpseID: loot.CorpseID, EntryID: 1, Revision: loot.Revision, ClientTick: 2}))
	afterTake := w.awaitClaim(t, c, owner, "the first entry's claim", func(r persist.Record, d persist.RewardDefeat) bool {
		return r.BossRewardEpoch == base.BossRewardEpoch+1 && d.Personal[0].Taken != 0
	})
	if got := w.guardianDefeat(t).Personal[0]; got.Taken != 0b01 {
		t.Fatalf("after taking entry 1 the journal records taken %b, want only index 0", got.Taken)
	}
	if err := w.identities.DeliverBossExperience(); err != nil {
		t.Fatal(err)
	}
	delivered := w.awaitClaim(t, c, owner, "the experience claim", func(r persist.Record, d persist.RewardDefeat) bool {
		return d.Experience[0].Taken
	})
	if delivered.Experience != afterTake.Experience+share || delivered.BossRewardEpoch != afterTake.BossRewardEpoch+1 {
		t.Fatalf("after the experience claim: experience %d epoch %d, want %d and %d", delivered.Experience, delivered.BossRewardEpoch, afterTake.Experience+share, afterTake.BossRewardEpoch+1)
	}
	w.shutdown(t, c)

	// ---- Second process: still 1/2, nothing twice, only the remainder offered, and taken.
	w = bootServeWorld(t, dir, pair, worldID)
	c = w.connect(t, account)
	if rec := w.record(t, owner); rec.BossRewardEpoch != delivered.BossRewardEpoch || rec.Experience != delivered.Experience || rec.Silver != delivered.Silver {
		t.Fatalf("a restart changed the character: %+v, want %+v", rec, delivered)
	}
	run = w.cross(t, c)
	if run.Sim.DungeonBossAliveForTest(vnet.MobKindVargrGuardian) || !run.Sim.DungeonBossAliveForTest(vnet.MobKindDraugrKing) {
		t.Fatal("the restarted run is not at 1/2: the guardian respawned or the king is missing")
	}
	if saved := w.manager.SavedSessions(); len(saved) != 1 || !slices.Equal(saved[0].DefeatedBosses, []vnet.MobKind{vnet.MobKindVargrGuardian}) {
		t.Fatalf("restored runs = %+v, want one run with the guardian defeated", saved)
	}
	if err := w.identities.DeliverBossExperience(); err != nil {
		t.Fatal(err)
	}
	remainder := w.openCorpse(t, c, run, vnet.MobKindVargrGuardian, 1)
	if len(remainder.Entries) != 1 || remainder.Entries[0].EntryID != 2 || remainder.Silver != 0 {
		t.Fatalf("the rebuilt corpse offers %+v, want only entry 2 at its roll index", remainder)
	}
	c.conn.send(protocol.EncodeLootTakeAllRequest(protocol.LootTakeAllRequest{CorpseID: remainder.CorpseID, Revision: remainder.Revision, ClientTick: 2}))
	final := w.awaitClaim(t, c, owner, "the remainder's claim", func(r persist.Record, d persist.RewardDefeat) bool {
		return r.BossRewardEpoch == delivered.BossRewardEpoch+1 && d.Personal[0].Taken == 0b11
	})
	if final.Experience != delivered.Experience {
		t.Fatalf("the restart re-delivered experience: %d, want %d", final.Experience, delivered.Experience)
	}
	if got := w.guardianDefeat(t); got.Personal[0].Taken != 0b11 || !got.Experience[0].Taken {
		t.Fatalf("the journal after the remainder = %+v, want every entry and the share taken", got)
	}
	w.shutdown(t, c)

	// ---- Third process: nothing is offered again, and the king completes the run.
	w = bootServeWorld(t, dir, pair, worldID)
	c = w.connect(t, account)
	run = w.cross(t, c)
	if _, _, found := run.Sim.BossCorpseForTest(vnet.MobKindVargrGuardian); found {
		t.Fatal("a corpse was rebuilt for loot nothing is owed from")
	}
	if rec := w.record(t, owner); rec.BossRewardEpoch != final.BossRewardEpoch || rec.Experience != final.Experience {
		t.Fatalf("the second restart changed the character: %+v, want %+v", rec, final)
	}
	if err := run.Sim.KillDungeonBossForTest(vnet.MobKindDraugrKing, c.entityID); err != nil {
		t.Fatal(err)
	}
	w.manager.Step()
	if err := w.identities.SyncRewardRuns(time.Now()); err != nil {
		t.Fatal(err)
	}
	snapshot, err := w.journal.Snapshot()
	if err != nil {
		t.Fatal(err)
	}
	both := []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing}
	if len(snapshot.Runs) != 1 || !slices.Equal(snapshot.Runs[0].Session.DefeatedBosses, both) {
		t.Fatalf("the journal after the king = %+v, want the run at 2/2", snapshot.Runs)
	}
	w.shutdown(t, c)
	if errors.Is(err, context.Canceled) {
		t.Fatal("unreachable")
	}
}
