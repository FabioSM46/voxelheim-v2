package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"log/slog"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/certs"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
	"github.com/FabioSM46/voxelheim-v2/server/internal/transport"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func discard() *slog.Logger { return slog.New(slog.DiscardHandler) }

// The account service every test in this package is admitted by: one signing pair and
// one world, built once for the whole binary.
//
// Shared for the reason internal/session's tests share theirs — a pair is a key and a
// world id is a digest of a name, so neither carries state between tests — and built
// here rather than fetched, because a game server's key comes from a fetch exactly once
// and the tests about *that* are the ones that stand an HTTP server up.
var (
	testPair  *ticket.Pair
	testWorld ticket.WorldID
)

// testWorldName is the world these tests' server is registered under.
const testWorldName = "midgard"

func TestMain(m *testing.M) {
	dir, err := os.MkdirTemp("", "voxelheimd-tickets")
	if err != nil {
		panic("voxelheimd test: making a directory for the signing pair: " + err.Error())
	}
	defer func() { _ = os.RemoveAll(dir) }()

	if testPair, err = ticket.LoadOrCreate(dir); err != nil {
		panic("voxelheimd test: minting the signing pair: " + err.Error())
	}
	if testWorld, err = ticket.WorldIDFor(testWorldName); err != nil {
		panic("voxelheimd test: naming the world: " + err.Error())
	}

	// os.Exit skips deferred functions, so the cleanup above is deferred *after* the
	// exit is, and therefore runs before it.
	code := m.Run()
	defer os.Exit(code)
}

// testIdentities is a claim set over store and explored — nil for the ephemeral world
// — admitting tickets from the package's own account service.
func testIdentities(t *testing.T, store *persist.Store, explored *persist.ExplorationStore) *session.Identities {
	t.Helper()

	verifier, err := session.NewVerifier(testPair.Public(), testWorld, nil)
	if err != nil {
		t.Fatalf("NewVerifier: %v", err)
	}
	identities, err := session.NewIdentities(store, explored, nil, verifier, discard())
	if err != nil {
		t.Fatalf("NewIdentities: %v", err)
	}
	return identities
}

// testAccount is a distinct account per seed, chosen rather than random so a failing
// test names the same player on every run.
func testAccount(seed byte) ticket.AccountID {
	var account ticket.AccountID
	for i := range account {
		account[i] = seed*17 + byte(i)
	}
	return account
}

// testPlayerID is the player id an account resolves to: what the store keys on, what a
// structure records as its owner, and what a log line carries.
func testPlayerID(account ticket.AccountID) identity.PlayerID {
	return identity.IDOf(identity.Account(account))
}

// helloFor is the frame a client holding a valid ticket for account sends.
func helloFor(t *testing.T, account ticket.AccountID) []byte {
	t.Helper()

	return helloAsking(t, account, "Eivor")
}

// helloAsking is helloFor under a chosen display name, which is what a hello says about
// a character until the character phase lands on the wire: an account with several
// characters plays the one wearing that name.
func helloAsking(t *testing.T, account ticket.AccountID, name string) []byte {
	t.Helper()

	minted, _, err := testPair.Mint(account, testWorld, time.Now())
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}
	return protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, name, minted[:])
}

// testTicketKey is the pair's public half in the hex the -ticket-key flag takes.
func testTicketKey() string { return testPair.PublicHex() }

func testConfig() session.Config {
	return session.Config{
		WorldSeed:    1,
		TickRate:     20,
		ChunkSize:    32,
		ViewDistance: 3,
		Spawn:        [3]float32{0.5, 80, 0.5},
	}
}

// testServer builds a server around a fake transport, with the same world and
// simulation wiring run() produces.
//
// Real ones rather than zero values: srv.run ticks the simulation, so a nil *Sim
// would make every one of these tests a race between the first tick and the
// cancellation that ends it.
//
// The world is ephemeral, so no test leaves a directory behind in the package's own;
// testWorldServer is the persistent counterpart.
func testServer(t *testing.T, tr transport.Transport) *server {
	t.Helper()
	return newTestServer(t, tr, world.NewCache(testConfig().WorldSeed, 1, 64), nil)
}

// testWorldServer is testServer over a world directory, wired the same way round: the
// simulation collides against the cache that is being persisted, not a second one.
func testWorldServer(t *testing.T, tr transport.Transport, dir string) *server {
	t.Helper()
	return newTestServer(t, tr, world.NewPersistentCache(openStore(t, dir, testConfig().WorldSeed), 1, 64), nil)
}

func newTestServer(t *testing.T, tr transport.Transport, chunks *world.Cache, players *persist.Store) *server {
	return newTestServerWithLimit(t, tr, chunks, players, session.DefaultConcurrentSessions)
}

func newTestServerWithLimit(t *testing.T, tr transport.Transport, chunks *world.Cache, players *persist.Store, limit int) *server {
	t.Helper()

	cfg := testConfig()
	registry := session.NewRegistry(limit)
	sim, err := game.NewSim(cfg.TickRate, cfg.ViewDistance, cfg.WorldSeed, game.NewCacheTerrain(chunks), chunks, registry.NextID, discard())
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	if err := sim.ConfigureChunkRegeneration(chunks, registry.ResendChunk); err != nil {
		t.Fatalf("ConfigureChunkRegeneration: %v", err)
	}
	if err := sim.ConfigureWater(chunks); err != nil {
		t.Fatalf("ConfigureWater: %v", err)
	}

	return &server{
		tr:       tr,
		registry: registry,
		// A nil player store is the ephemeral world, which is how most of these tests
		// run: tickets are still verified and accounts are still exclusive, and nothing
		// is written.
		identities: testIdentities(t, players, nil),
		cfg:        cfg,
		// Left zero on purpose: these tests are about shutdown ordering and accept-loop
		// behaviour, and a read deadline would end their connections on a schedule they
		// did not ask for. The flags cannot produce this, which is what validate is for.
		timeouts: session.Timeouts{},
		chunks:   chunks,
		sim:      sim,
		log:      discard(),
	}
}

// validOptions is a configuration every field of which passes validate.
//
// The cases below mutate the single field under test rather than building a literal
// each time. With validated flags, a literal that omits one is a case that
// passes for a reason it did not mean — an omitted duration is zero, and zero is now
// a refusal of its own.
func validOptions() options {
	return options{
		listen:           "127.0.0.1:0",
		worldName:        testWorldName,
		ticketKey:        testTicketKey(),
		tickRate:         20,
		viewDistance:     3,
		maxPlayers:       session.DefaultConcurrentSessions,
		terrainMemoryMiB: world.DefaultTerrainMemoryMiB,
		handshakeTimeout: session.DefaultHandshakeTimeout,
		characterTimeout: session.DefaultCharacterTimeout,
		idleTimeout:      session.DefaultIdleTimeout,
		stormPeriod:      game.DefaultStormPeriod,
	}
}

func TestTheWorldDirectoryFlagPreservesAllThreeModes(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name string
		args []string
		want string
	}{
		{name: "omitted", want: world.DefaultWorldDir()},
		{name: "explicit persistent", args: []string{"-world-dir", "named-world"}, want: "named-world"},
		{name: "explicit ephemeral", args: []string{"-world-dir", ""}, want: ""},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()

			var opts options
			flags := flag.NewFlagSet("voxelheimd", flag.ContinueOnError)
			registerWorldDirFlag(flags, &opts)
			if err := flags.Parse(test.args); err != nil {
				t.Fatalf("Parse: %v", err)
			}
			if opts.worldDir != test.want {
				t.Errorf("world directory = %q, want %q", opts.worldDir, test.want)
			}
		})
	}
}

func TestWorldDirectoryHelpNamesTheConcreteDefault(t *testing.T) {
	t.Parallel()

	var opts options
	var help strings.Builder
	flags := flag.NewFlagSet("voxelheimd", flag.ContinueOnError)
	flags.SetOutput(&help)
	registerWorldDirFlag(flags, &opts)
	if err := flags.Parse([]string{"-h"}); !errors.Is(err, flag.ErrHelp) {
		t.Fatalf("Parse(-h) = %v, want flag.ErrHelp", err)
	}
	if want := `default "` + world.DefaultWorldDir() + `"`; !strings.Contains(help.String(), want) {
		t.Errorf("-h does not name the concrete world directory %q:\n%s", world.DefaultWorldDir(), help.String())
	}
}

func TestOpeningTheDefaultWorldLogsItsConcreteDirectory(t *testing.T) {
	t.Parallel()

	dir := filepath.Join(t.TempDir(), world.DefaultWorldDir())
	var logged strings.Builder
	log := slog.New(slog.NewTextHandler(&logged, &slog.HandlerOptions{Level: slog.LevelInfo}))
	if _, err := openWorld(options{worldDir: dir}, 1, log); err != nil {
		t.Fatalf("openWorld: %v", err)
	}
	if !strings.Contains(logged.String(), dir) {
		t.Errorf("startup log does not name the concrete world directory %q:\n%s", dir, logged.String())
	}
}

func TestOptionsValidate(t *testing.T) {
	t.Parallel()

	if err := validOptions().validate(); err != nil {
		t.Fatalf("valid flags rejected: %v", err)
	}

	// The raw value is what gets validated. A clamped check would accept every one
	// of these and start a server the operator did not ask for.
	cases := map[string]func(*options){
		"tick rate 0":                func(o *options) { o.tickRate = 0 },
		"tick rate past a byte":      func(o *options) { o.tickRate = 256 },
		"tick rate far past a byte":  func(o *options) { o.tickRate = 1000 },
		"view distance past the cap": func(o *options) { o.viewDistance = protocol.MaxViewDistance + 1 },
		"view distance far past it":  func(o *options) { o.viewDistance = 1000 },
		"player limit below the intended scale": func(o *options) {
			o.maxPlayers = session.MinConcurrentSessions - 1
		},
		"player limit above the intended scale": func(o *options) {
			o.maxPlayers = session.MaxConcurrentSessions + 1
		},
		"terrain memory 0": func(o *options) { o.terrainMemoryMiB = 0 },
		// The new refusal: inside the contract's ceiling and outside this server's.
		"view distance the cache cannot hold": func(o *options) {
			o.viewDistance = uint(world.LargestViewDistanceHeld(o.terrainMemoryMiB)) + 1
		},
		"one session does not fit the terrain budget": func(o *options) {
			o.terrainMemoryMiB = world.MemoryMiBFor(
				uint64(world.CacheWorkingSetFor(int(o.viewDistance)))) - 1
		},

		// A zero deadline is not "no deadline" for a server: it is the flag the whole
		// issue exists to remove, spelled as a number.
		"idle timeout 0":        func(o *options) { o.idleTimeout = 0 },
		"handshake timeout 0":   func(o *options) { o.handshakeTimeout = 0 },
		"character timeout 0":   func(o *options) { o.characterTimeout = 0 },
		"negative idle timeout": func(o *options) { o.idleTimeout = -time.Second },
		"negative storm period": func(o *options) { o.stormPeriod = -time.Second },
		"non-RFC3339 storm":     func(o *options) { o.nextStorm = "next Thursday" },
		// A first read allowed to outlive the budget every later read is held to.
		"handshake beyond idle": func(o *options) {
			o.handshakeTimeout = 21 * time.Second
			o.idleTimeout = 20 * time.Second
		},
		// And the same rule for the phase a person sits inside: a peer that has
		// presented a ticket this server accepted must not be held to a stricter budget
		// than one that has presented nothing.
		"character below handshake": func(o *options) {
			o.handshakeTimeout = 5 * time.Second
			o.characterTimeout = time.Second
		},
	}

	for name, mutate := range cases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			opts := validOptions()
			mutate(&opts)
			if err := opts.validate(); err == nil {
				t.Fatalf("validate accepted %s: %+v", name, opts)
			}
		})
	}

	// Boundaries are accepted, so the check is a range and not an accident.
	for _, mutate := range []func(*options){
		func(o *options) {
			o.tickRate = 1
			o.viewDistance = 0
			o.maxPlayers = session.MinConcurrentSessions
		},
		func(o *options) { o.maxPlayers = session.MaxConcurrentSessions },
		// **Not protocol.MaxViewDistance any more, and the change is the point of #666.**
		// The contract's ceiling is 16, which asks for 33³ = 35937 chunks; no residency
		// this server sizes itself to holds that, so the flag is refused at startup
		// rather than accepted into a server that thrashes. The largest a boundary case
		// may use is therefore the largest the cache can hold, which
		// [world.LargestViewDistanceHeld] answers rather than this test restating.
		func(o *options) {
			o.tickRate = 255
			o.viewDistance = uint(world.LargestViewDistanceHeld(o.terrainMemoryMiB))
		},
		func(o *options) { o.handshakeTimeout = o.idleTimeout },
		func(o *options) { o.stormPeriod = 0 },
		func(o *options) { o.nextStorm = "2030-01-02T03:04:05Z" },
		func(o *options) {
			o.handshakeTimeout = time.Nanosecond
			o.characterTimeout = time.Nanosecond
			o.idleTimeout = time.Nanosecond
		},
		// A character window far past the idle one is the expected shape rather than a
		// mistake: a character screen is not an idle session.
		func(o *options) { o.characterTimeout = time.Hour },
	} {

		opts := validOptions()
		mutate(&opts)
		if err := opts.validate(); err != nil {
			t.Errorf("validate rejected the boundary %+v: %v", opts, err)
		}
	}
}

// The error must quote what the operator typed. Reporting a clamped 255 for
// `-tick-rate 1000` sends them looking for a limit they never hit.
func TestOptionsValidateReportsTheValueGiven(t *testing.T) {
	t.Parallel()

	opts := validOptions()
	opts.tickRate = 1000
	err := opts.validate()
	if err == nil {
		t.Fatal("validate accepted a tick rate of 1000")
	}
	if !strings.Contains(err.Error(), "1000") {
		t.Errorf("error %q does not mention the value the operator gave", err)
	}

	// The same rule for a duration: an operator who typed 45s must read 45s back.
	opts = validOptions()
	opts.handshakeTimeout = 45 * time.Second
	opts.idleTimeout = 20 * time.Second
	err = opts.validate()
	if err == nil {
		t.Fatal("validate accepted a handshake timeout longer than the idle timeout")
	}
	if !strings.Contains(err.Error(), "45s") {
		t.Errorf("error %q does not mention the value the operator gave", err)
	}

	opts = validOptions()
	opts.terrainMemoryMiB = world.MemoryMiBFor(
		uint64(world.CacheWorkingSetFor(int(opts.viewDistance)))) - 1
	err = opts.validate()
	if err == nil {
		t.Fatal("validate accepted a budget smaller than one working set")
	}
	for _, want := range []string{
		"max players 100", "48 MiB", "0 chunks", "distance 3", "514 chunks",
		"343 in view", "49 MiB",
	} {
		if !strings.Contains(err.Error(), want) {
			t.Errorf("memory refusal %q does not contain %q", err, want)
		}
	}
}

func TestRunRejectsInvalidFlags(t *testing.T) {
	t.Parallel()

	// A listener must never be opened for a configuration that cannot run.
	badTick := validOptions()
	badTick.tickRate = 1000
	badTick.logLevel = "info"
	if err := run(context.Background(), badTick, discard()); err == nil {
		t.Fatal("run accepted a tick rate of 1000")
	}

	// A deadline is refused in the same place and before the same port is bound. A
	// server that started with -idle-timeout 0 would be the one this flag exists to
	// replace, and it would look configured rather than forgotten.
	noIdle := validOptions()
	noIdle.idleTimeout = 0
	noIdle.logLevel = "info"
	if err := run(context.Background(), noIdle, discard()); err == nil {
		t.Fatal("run accepted an idle timeout of 0")
	}
}

// A world directory recorded under another seed is a configuration that cannot run, and
// it is refused in the same place and for the same reason as a bad tick rate: before the
// port is bound. Serving it would not fail — the stored edits would land on the wrong
// terrain and look like somebody's building.
func TestRunRefusesASeedTheStoredWorldDoesNotMatch(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	if _, err := world.OpenStore(dir, 1); err != nil {
		t.Fatalf("OpenStore: %v", err)
	}

	opts := validOptions()
	opts.seed = 2
	opts.worldDir = dir
	opts.logLevel = "info"
	err := run(context.Background(), opts, discard())
	if err == nil {
		t.Fatal("run started against a world recorded under a different seed")
	}
	if !errors.Is(err, world.ErrSeedMismatch) {
		t.Errorf("run returned %v, which is not an ErrSeedMismatch", err)
	}
}

// The final save is part of the shutdown ordering, and this is what says so: the autosave
// interval is set far beyond the test's lifetime, so the only thing that can have written
// the file is the flush at the end of server.shutdown. Move it before workers.Wait() and
// it races the sessions; drop it and this test fails outright.
func TestShutdownSavesTheWorld(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	cfg := testConfig()

	srv := testWorldServer(t, newLateTransport(newBlockingConn()), dir)
	// Long enough that the autosave loop cannot fire during the test, so a pass can only
	// come from the shutdown flush.
	srv.saveEvery = time.Hour

	if err := srv.chunks.Apply(context.Background(), 5, 6, 7, world.Snow, allowAnything); err != nil {
		t.Fatalf("Apply: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		defer close(done)
		srv.run(ctx)
	}()
	cancel()

	select {
	case <-done:
	case <-time.After(10 * time.Second):
		t.Fatal("shutdown hung")
	}

	// A fresh cache over the same directory: nothing in memory, everything from disk.
	reloaded := world.NewPersistentCache(openStore(t, dir, cfg.WorldSeed), 1, 64)
	got, err := reloaded.BlockAt(context.Background(), 5, 6, 7)
	if err != nil {
		t.Fatalf("BlockAt after a restart: %v", err)
	}
	if got != world.Snow {
		t.Errorf("the voxel holds %d after a restart, want Snow: shutdown did not save the world", got)
	}
}

// An ephemeral server has no world directory, and its shutdown must still be a clean one.
func TestShutdownIsCleanWithoutAWorldDirectory(t *testing.T) {
	t.Parallel()

	srv := testServer(t, newLateTransport(newBlockingConn()))

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		defer close(done)
		srv.run(ctx)
	}()
	cancel()

	select {
	case <-done:
	case <-time.After(10 * time.Second):
		t.Fatal("shutdown hung on a server with no world directory")
	}
}

func openStore(t *testing.T, dir string, seed int64) *world.Store {
	t.Helper()

	store, err := world.OpenStore(dir, seed)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	return store
}

// allowAnything is the Apply predicate for a test about wiring rather than about
// legality; the rules live in internal/game.
func allowAnything(world.Block) error { return nil }

// blockingConn is a connection that never says anything: reads block until it is
// closed. That is the client that turns a shutdown-ordering mistake into a hang.
type blockingConn struct {
	closeOnce sync.Once
	done      chan struct{}
}

func newBlockingConn() *blockingConn {
	return &blockingConn{done: make(chan struct{})}
}

func (c *blockingConn) ReadFrame() ([]byte, error) {
	<-c.done
	return nil, net.ErrClosed
}

func (c *blockingConn) WriteFrame([]byte) error { return nil }
func (c *blockingConn) RemoteAddr() string      { return "blocking" }

// SetReadDeadline does nothing, and is never asked to: the servers in this file run
// with the zero session.Timeouts, so Serve clears the deadline rather than setting
// one. A connection whose silence is the point must stay silent for as long as the
// shutdown ordering needs it to.
func (c *blockingConn) SetReadDeadline(time.Time) error { return nil }

func (c *blockingConn) Close() error {
	c.closeOnce.Do(func() { close(c.done) })
	return nil
}

func (c *blockingConn) isClosed() bool {
	select {
	case <-c.done:
		return true
	default:
		return false
	}
}

// lateTransport hands out one connection *after* Close, which is exactly the
// in-flight accept the shutdown ordering has to survive: a connection that exists
// but is not yet registered when the shutdown begins.
//
// Accept is called from a single goroutine (the accept loop), so the delivery flag
// needs no lock.
type lateTransport struct {
	closed    chan struct{}
	closeOnce sync.Once
	late      transport.Conn
	delivered bool
}

func newLateTransport(late transport.Conn) *lateTransport {
	return &lateTransport{closed: make(chan struct{}), late: late}
}

func (t *lateTransport) Accept() (transport.Conn, error) {
	<-t.closed // no pending connections until the listener is closed

	if !t.delivered {
		t.delivered = true
		return t.late, nil
	}
	return nil, fmt.Errorf("late transport: %w", net.ErrClosed)
}

func (t *lateTransport) Addr() string { return "late" }

func (t *lateTransport) Close() error {
	t.closeOnce.Do(func() { close(t.closed) })
	return nil
}

// TestShutdownClosesAConnectionAcceptedDuringShutdown pins the ordering in
// server.shutdown. With the registry snapshotted before the accept loop has
// exited, the connection delivered below is never closed, its session blocks in
// ReadFrame forever, and the shutdown hangs — so this test times out instead of
// passing. The fix is ordering, and this is what proves the ordering.
func TestShutdownClosesAConnectionAcceptedDuringShutdown(t *testing.T) {
	t.Parallel()

	late := newBlockingConn()
	srv := testServer(t, newLateTransport(late))

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		defer close(done)
		srv.run(ctx)
	}()

	cancel()

	select {
	case <-done:
	case <-time.After(10 * time.Second):
		t.Fatal("shutdown hung: a connection accepted during shutdown was never closed")
	}

	if !late.isClosed() {
		t.Error("the in-flight connection was left open")
	}
	if got := srv.registry.Count(); got != 0 {
		t.Errorf("registry still holds %d sessions after shutdown", got)
	}
}

// flakyTransport fails a few times before yielding a connection: EMFILE, a peer
// that vanished between SYN and accept, anything transient.
type flakyTransport struct {
	mu        sync.Mutex
	failures  int
	conn      transport.Conn
	delivered bool
	closed    chan struct{}
	closeOnce sync.Once
	attempts  int
}

func newFlakyTransport(failures int, conn transport.Conn) *flakyTransport {
	return &flakyTransport{failures: failures, conn: conn, closed: make(chan struct{})}
}

func (t *flakyTransport) Accept() (transport.Conn, error) {
	t.mu.Lock()
	t.attempts++
	attempt := t.attempts
	t.mu.Unlock()

	if attempt <= t.failures {
		return nil, errors.New("flaky transport: too many open files")
	}
	if !t.delivered {
		t.delivered = true
		return t.conn, nil
	}

	<-t.closed
	return nil, fmt.Errorf("flaky transport: %w", net.ErrClosed)
}

func (t *flakyTransport) Addr() string { return "flaky" }

func (t *flakyTransport) Close() error {
	t.closeOnce.Do(func() { close(t.closed) })
	return nil
}

func (t *flakyTransport) attemptCount() int {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.attempts
}

// A transient accept error must not end the accept loop. A server that has stopped
// accepting connections while its tick loop keeps running looks healthy from the
// outside, which is what makes this failure mode worth a test.
func TestAcceptLoopRetriesTransientErrors(t *testing.T) {
	t.Parallel()

	conn := newBlockingConn()
	tr := newFlakyTransport(3, conn)
	srv := testServer(t, tr)

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	var workers sync.WaitGroup
	accepted := make(chan struct{})
	go func() {
		defer close(accepted)
		srv.acceptLoop(ctx, &workers)
	}()

	deadline := time.Now().Add(10 * time.Second)
	for srv.registry.Count() == 0 {
		if time.Now().After(deadline) {
			t.Fatalf("the accept loop gave up after %d attempts instead of retrying", tr.attemptCount())
		}
		time.Sleep(10 * time.Millisecond)
	}

	if got := tr.attemptCount(); got < 4 {
		t.Errorf("accept was attempted %d times, want at least 4 (3 failures then a success)", got)
	}

	cancel()
	if err := tr.Close(); err != nil {
		t.Fatalf("close transport: %v", err)
	}
	select {
	case <-accepted:
	case <-time.After(10 * time.Second):
		t.Fatal("the accept loop did not exit after the context was cancelled")
	}

	srv.registry.CloseAll()
	workers.Wait()
}

// The connection beyond the operator's ceiling receives the contract's SERVER_FULL
// answer. Dropping it would leave the client with no reason it can show, and checking the
// count outside Registry.Add would let two accepts both observe the last slot as free.
func TestAConnectionPastTheSessionLimitIsRefusedWithAReason(t *testing.T) {
	t.Parallel()

	first := newBlockingConn()
	refused := newScriptedConn("past-limit")
	tr := newQueueTransport(first, refused)
	srv := newTestServerWithLimit(t, tr, world.NewCache(testConfig().WorldSeed, 1, 64), nil, 1)

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	var workers sync.WaitGroup
	accepted := make(chan struct{})
	go func() {
		defer close(accepted)
		srv.acceptLoop(ctx, &workers)
	}()

	env := nextReply(t, refused)
	if got := env.PayloadType(); got != vnet.PayloadServerReject {
		t.Fatalf("connection past the limit got %s, want ServerReject", got)
	}
	var reject vnet.ServerReject
	tbl := new(flatbuffers.Table)
	if !env.Payload(tbl) {
		t.Fatal("server-full refusal has no payload")
	}
	reject.Init(tbl.Bytes, tbl.Pos)
	if got := reject.Reason(); got != vnet.RejectReasonSERVER_FULL {
		t.Errorf("reason = %s, want SERVER_FULL", got)
	}
	if detail := string(reject.Detail()); !strings.Contains(detail, "1 concurrent session") {
		t.Errorf("detail %q does not state the configured limit", detail)
	}
	select {
	case <-refused.done:
	case <-time.After(2 * time.Second):
		t.Error("the refused connection was left open")
	}
	if got := srv.registry.Count(); got != 1 {
		t.Errorf("registry holds %d sessions after the refusal, want only the admitted one", got)
	}

	cancel()
	if err := tr.Close(); err != nil {
		t.Fatalf("close transport: %v", err)
	}
	select {
	case <-accepted:
	case <-time.After(10 * time.Second):
		t.Fatal("accept loop did not stop")
	}
	srv.registry.CloseAll()
	workers.Wait()
}

// ---------------------------------------------------------------------------
// One session per identity, through the whole server rather than through Serve.
// ---------------------------------------------------------------------------

// scriptedConn is a connection a test speaks through: frames in, frames out.
//
// Writes never block. An admitted session sends a welcome, an inventory state and
// then as much of its view as it can, and a test that only wants the first frame
// must not become the reason the writer goroutine stalls.
type scriptedConn struct {
	name string
	in   chan []byte
	out  chan []byte

	closeOnce sync.Once
	done      chan struct{}
}

func newScriptedConn(name string) *scriptedConn {
	return &scriptedConn{name: name, in: make(chan []byte, 4), out: make(chan []byte, 64), done: make(chan struct{})}
}

func (c *scriptedConn) ReadFrame() ([]byte, error) {
	select {
	case frame := <-c.in:
		return frame, nil
	case <-c.done:
		return nil, net.ErrClosed
	}
}

func (c *scriptedConn) WriteFrame(frame []byte) error {
	select {
	case c.out <- frame:
	default:
		// Dropped rather than blocked. What this test reads is the handshake's answer,
		// which is the first frame either way.
	}
	return nil
}

func (c *scriptedConn) RemoteAddr() string              { return c.name }
func (c *scriptedConn) SetReadDeadline(time.Time) error { return nil }

func (c *scriptedConn) Close() error {
	c.closeOnce.Do(func() { close(c.done) })
	return nil
}

// queueTransport hands out a prepared list of connections and then blocks, the way a
// listener with nobody dialling it does.
type queueTransport struct {
	conns     chan transport.Conn
	closed    chan struct{}
	closeOnce sync.Once
}

func newQueueTransport(conns ...transport.Conn) *queueTransport {
	queued := make(chan transport.Conn, len(conns))
	for _, conn := range conns {
		queued <- conn
	}
	return &queueTransport{conns: queued, closed: make(chan struct{})}
}

func (t *queueTransport) Accept() (transport.Conn, error) {
	select {
	case conn := <-t.conns:
		return conn, nil
	case <-t.closed:
		return nil, net.ErrClosed
	}
}

func (t *queueTransport) Addr() string { return "queue" }

func (t *queueTransport) Close() error {
	t.closeOnce.Do(func() { close(t.closed) })
	return nil
}

func openPlayerStore(t *testing.T, dir string) *persist.Store {
	t.Helper()

	store, err := persist.OpenStore(dir)
	if err != nil {
		t.Fatalf("persist.OpenStore: %v", err)
	}
	return store
}

// seedCharacter mints the character an account plays here and gives it a life to come
// back to, answering the character so a test can read its record afterwards.
//
// The life is a *living* one, because that is the only kind this build resumes: a health
// of zero is what persist.Record.Unplayed reads as "this character has never played", so
// a fixture without one would be admitted with nothing and pass for the wrong reason.
func seedCharacter(t *testing.T, store *persist.Store, account ticket.AccountID, name string) persist.Character {
	t.Helper()

	character, err := store.Create(testPlayerID(account), name, testAppearance())
	if err != nil {
		t.Fatalf("creating the seeded character: %v", err)
	}

	if err := store.Save(character.ID, persist.Record{
		LastSeen: time.Unix(1, 0),
		Pos:      [3]float64{0.5, 64, 0.5},
		Health:   game.PlayerMaxHealth,
	}); err != nil {
		t.Fatalf("seeding the record: %v", err)
	}
	return character
}

// onlyCharacter is the one character an account holds in this world, and a fatal failure
// when it holds none or several.
func onlyCharacter(t *testing.T, store *persist.Store, account ticket.AccountID) persist.Character {
	t.Helper()

	held := store.Characters(testPlayerID(account))
	if len(held) != 1 {
		t.Fatalf("the account holds %d characters, want exactly 1", len(held))
	}
	return held[0]
}

// nextReply is the next frame the server sends on conn.
//
// **It was `firstReply` while a handshake was one exchange.** A hello is answered with
// the account's characters now and the welcome comes one message later, so most callers
// read two — see [enterWorld], which is what they should be using.
func nextReply(t *testing.T, conn *scriptedConn) *vnet.Envelope {
	t.Helper()

	select {
	case frame := <-conn.out:
		return vnet.GetRootAsEnvelope(frame, 0)
	case <-time.After(5 * time.Second):
		t.Fatalf("%s got no answer", conn.name)
		return nil
	}
}

// testAppearance is a face the contract allows. Every path that stores one or plays one
// checks it, so a fixture that skipped it would be testing the refusal.
func testAppearance() protocol.Appearance {
	return protocol.Appearance{
		SkinColor:     0x00D9A066,
		ShirtColor:    0x00394F3B,
		TrousersColor: 0x00241E1A,
		ShoesColor:    0x00080808,
		HairModel:     vnet.HairModelLoose,
		HairColor:     0x00734022,
	}
}

// creationOf and selectionOf are the two frames the character phase accepts: make one
// and play it, or play one this account already holds.
func creationOf(name string) []byte {
	return protocol.EncodeCreateCharacterRequest(protocol.CreateCharacterRequest{
		Name: name, Appearance: testAppearance(), HasAppearance: true,
	})
}

func selectionOf(id persist.CharacterID) []byte {
	return protocol.EncodeSelectCharacterRequest(protocol.SelectCharacterRequest{CharacterID: uint64(id)})
}

// enterWorld drives a whole handshake on conn and answers with the welcome.
//
// **Both client frames are queued before either reply is read**, which is legal and is
// what keeps these tests from having to interleave: a client that already knows which
// character it wants may answer the list before it arrives, and the server reads the
// choice when it reaches the phase that is waiting for one. What is asserted on the way
// through is that the *first* answer to a hello is the character list — the phase this
// contract added, at the level a whole server produces it.
func enterWorld(t *testing.T, conn *scriptedConn, hello, choice []byte) *vnet.Envelope {
	t.Helper()

	conn.in <- hello
	conn.in <- choice

	if got := nextReply(t, conn).PayloadType(); got != vnet.PayloadServerCharacterList {
		t.Fatalf("%s got %s in answer to its hello, want a character list", conn.name, got)
	}
	return nextReply(t, conn)
}

// TestTwoConnectionsOnOneAccountDoNotBothGetIn is the wiring test for the whole
// identity path: the store main opens, the claim set it builds, and the two sessions
// the accept loop starts.
//
// It is at this level rather than at Serve's because the thing being checked is that
// the server hands *one* claim set to every session. A per-session one would pass every
// test in internal/session and refuse nobody here.
//
// **The two connections present two different tickets**, which is what the claim moving
// to the account means: same person, two machines, two sign-ins, one live session.
//
// **What each of them is answered with moved one message earlier.** The claim is taken
// when a ticket verifies — before the character list is sent, and long before a person
// has finished choosing — so the winner's first frame is that list and the loser's is
// the refusal. Waiting for a welcome to tell them apart would mean holding a claim open
// for as long as somebody stared at a screen.
func TestTwoConnectionsOnOneAccountDoNotBothGetIn(t *testing.T) {

	t.Parallel()

	dir := t.TempDir()
	players := openPlayerStore(t, dir)
	account := testAccount(1)

	seedCharacter(t, players, account, "Eivor")

	first, second := newScriptedConn("first"), newScriptedConn("second")
	tr := newQueueTransport(first, second)
	srv := newTestServer(t, tr, world.NewPersistentCache(openStore(t, dir, testConfig().WorldSeed), 1, 64), players)

	ctx, cancel := context.WithCancel(context.Background())
	stopped := make(chan struct{})
	go func() {
		defer close(stopped)
		srv.run(ctx)
	}()

	first.in <- helloFor(t, account)
	second.in <- helloFor(t, account)

	// Which connection wins is a race between two session goroutines, and the rule is
	// about the pair rather than about either one: exactly one gets as far as choosing a
	// character, and the other is refused with the reason that says why.
	admitted, rejections := 0, 0
	for _, conn := range []*scriptedConn{first, second} {
		env := nextReply(t, conn)
		switch env.PayloadType() {
		case vnet.PayloadServerCharacterList:
			admitted++

		case vnet.PayloadServerReject:
			rejections++
			var reject vnet.ServerReject
			tbl := new(flatbuffers.Table)
			if !env.Payload(tbl) {
				t.Fatal("the rejection has no payload")
			}
			reject.Init(tbl.Bytes, tbl.Pos)
			if got := reject.Reason(); got != vnet.RejectReasonALREADY_CONNECTED {
				t.Errorf("%s was refused with %s, want ALREADY_CONNECTED", conn.name, got)
			}
		default:
			t.Fatalf("%s got %s, want a character list or a rejection", conn.name, env.PayloadType())
		}
	}

	if admitted != 1 || rejections != 1 {
		t.Errorf("%d accounts admitted and %d rejections; one account admits exactly one session", admitted, rejections)
	}

	cancel()
	_ = first.Close()
	_ = second.Close()
	select {
	case <-stopped:
	case <-time.After(10 * time.Second):
		t.Fatal("the server did not shut down")
	}
}

// ---------------------------------------------------------------------------
// The transport, which is not a choice
// ---------------------------------------------------------------------------

// The pair is generated on first start and kept, so a restart presents the certificate
// every client already pinned. The files are the whole of the ceremony: no CA, no ACME,
// nothing for an operator to install.
func TestTheServerKeepsItsCertificateUnderTheWorldDirectory(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	opts := options{listen: "127.0.0.1:0", worldDir: dir}

	first, _, err := listen(opts, discard())
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	if cErr := first.Close(); cErr != nil {
		t.Fatalf("Close: %v", cErr)
	}

	for _, name := range []string{certs.CertFileName, certs.KeyFileName} {
		if _, sErr := os.Stat(filepath.Join(dir, name)); sErr != nil {
			t.Errorf("the first start left no %s under the world directory: %v", name, sErr)
		}
	}

	// A restart on the same directory. The fingerprint is what a client pins, so it is
	// what has to survive — not merely the fact that some certificate exists.
	before, err := certs.LoadOrCreate(dir)
	if err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}
	second, _, err := listen(opts, discard())
	if err != nil {
		t.Fatalf("second listen: %v", err)
	}
	defer func() { _ = second.Close() }()

	after, err := certs.LoadOrCreate(dir)
	if err != nil {
		t.Fatalf("second LoadOrCreate: %v", err)
	}
	beforePrint, _ := certs.Fingerprint(before)
	afterPrint, _ := certs.Fingerprint(after)
	if beforePrint != afterPrint {
		t.Errorf("a restart changed the fingerprint: %s then %s", beforePrint, afterPrint)
	}
}

// **There is no plaintext server to reach**, and this is what makes that a property of
// the binary rather than a sentence in a doc comment: a peer speaking the framing
// straight at the port the server bound gets nothing. A flag that could turn the
// encryption off would make the exposure a mistake somebody makes once and never
// notices, because a plaintext session looks correct from both ends.
func TestTheServerSpeaksNoPlaintext(t *testing.T) {
	t.Parallel()

	tr, _, err := listen(options{listen: "127.0.0.1:0", worldDir: t.TempDir()}, discard())
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer func() { _ = tr.Close() }()

	plain, err := net.Dial("tcp", tr.Addr())
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer func() { _ = plain.Close() }()

	go func() {
		// A well-formed ClientHello, and complete gibberish as far as a TLS record
		// header is concerned.
		_ = transport.WriteFrame(plain, protocol.EncodeClientHello(vnet.ProtocolVersionCurrent, "Eivor"))
	}()

	conn, err := tr.Accept()
	if err != nil {
		t.Fatalf("Accept: %v", err)
	}
	defer func() { _ = conn.Close() }()

	if _, rErr := conn.ReadFrame(); rErr == nil {
		t.Fatal("a plaintext handshake was read off the server's transport")
	}
}

// An ephemeral world has no directory to keep a key in, so it holds one in memory and
// writes nothing. The consequence — every returning client is refused by its own pin —
// is the operator's to accept, and a startup warning says so.
func TestAnEphemeralWorldEncryptsWithoutKeepingAKey(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	var logged strings.Builder
	log := slog.New(slog.NewTextHandler(&logged, &slog.HandlerOptions{Level: slog.LevelWarn}))

	tr, _, err := listen(options{listen: "127.0.0.1:0", worldDir: ""}, log)
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer func() { _ = tr.Close() }()

	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatalf("ReadDir: %v", err)
	}
	if len(entries) != 0 {
		t.Errorf("an ephemeral encrypted server wrote %d entries to disk", len(entries))
	}
	if !strings.Contains(logged.String(), "new certificate every start") {
		t.Errorf("an ephemeral server did not warn that its pin will not match; it said: %s", logged.String())
	}
}

// The fingerprint is logged so an operator can answer the one question a refused client
// asks. It gives nothing away — it is a hash of what the server hands every connection —
// and the private key must never appear beside it.
func TestTheStartupLogNamesTheFingerprintAndNeverTheKey(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	var logged strings.Builder
	log := slog.New(slog.NewTextHandler(&logged, &slog.HandlerOptions{Level: slog.LevelDebug}))

	tr, announced, err := listen(options{listen: "127.0.0.1:0", worldDir: dir}, log)
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer func() { _ = tr.Close() }()

	cert, err := certs.LoadOrCreate(dir)
	if err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}
	fingerprint, err := certs.Fingerprint(cert)
	if err != nil {
		t.Fatalf("Fingerprint: %v", err)
	}
	if !strings.Contains(logged.String(), fingerprint) {
		t.Error("the startup log does not name the certificate fingerprint")
	}

	// The number listen *returns* is the number it logged, because the announcer sends that
	// one to the account service and a client now takes its expectation from there rather
	// than from a pinned file. Two sources for this string is a server nobody can join.
	if announced != fingerprint {
		t.Errorf("listen logged %s and returned %s; there is one certificate and one digest of it", fingerprint, announced)
	}

	keyPEM, err := os.ReadFile(filepath.Join(dir, certs.KeyFileName))
	if err != nil {
		t.Fatalf("ReadFile: %v", err)
	}
	for _, line := range strings.Split(strings.TrimSpace(string(keyPEM)), "\n") {
		if strings.HasPrefix(line, "-----") {
			continue
		}
		if strings.Contains(logged.String(), line) {
			t.Fatal("the startup log carries private key material")
		}
	}
}
