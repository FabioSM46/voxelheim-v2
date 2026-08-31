// Command voxelheimd is the authoritative Voxelheim server.
//
// It accepts framed connections, admits the clients whose protocol version
// matches, and runs the simulation at a fixed rate. Every gameplay decision is
// made here; the client renders what this process says is true.
package main

import (
	"context"
	"crypto/tls"
	"errors"
	"flag"
	"fmt"
	"log/slog"
	"math"
	"os"
	"os/signal"
	"strings"
	"sync"
	"syscall"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/certs"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
	"github.com/FabioSM46/voxelheim-v2/server/internal/transport"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Accept-failure backoff. A transient listener error — the process is out of file
// descriptors, a peer vanished between SYN and accept — must not end the accept
// loop: a server that has stopped accepting while it keeps ticking is worse than
// one that exited, because nothing about it looks broken. The backoff keeps a
// persistent failure from becoming a hot loop that floods the log.
const (
	minAcceptBackoff = 50 * time.Millisecond
	maxAcceptBackoff = time.Second

	stormPollInterval  = 10 * time.Second
	missedStormWarning = time.Minute
	stormWarningTenMin = 10 * time.Minute
	stormWarningOneMin = time.Minute
	stormWarningFinal  = 10 * time.Second
)

func main() {
	opts := parseFlags()

	log, err := newLogger(opts.logLevel, opts.logFormat)
	if err != nil {
		fmt.Fprintf(os.Stderr, "voxelheimd: %v\n", err)
		os.Exit(2)
	}

	// NotifyContext turns the first SIGINT/SIGTERM into a cancelled context and
	// leaves a second one lethal — a shutdown that hangs must still be killable.
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	if err := run(ctx, opts, log); err != nil {
		log.Error("server stopped with an error", "error", err)
		os.Exit(1)
	}
}

type options struct {
	listen         string
	seed           int64
	worldDir       string
	worldName      string
	accountService string
	// accountServiceFingerprint is the SHA-256 of the certificate the account service
	// presents, copied out of that service's own startup line. Required whenever
	// accountService is given and meaningless without it — see validateTicketKeySource.
	accountServiceFingerprint string
	ticketKey                 string
	// Where this server tells the account service it can be reached, and the file its
	// registration key is read from. The key itself is never a flag — see announce.go.
	announceAddress     string
	registrationKeyFile string
	tickRate            uint
	viewDistance        uint
	maxPlayers          uint
	terrainMemoryMiB    uint64
	handshakeTimeout    time.Duration
	characterTimeout    time.Duration
	idleTimeout         time.Duration
	stormPeriod         time.Duration
	nextStorm           string

	logLevel  string
	logFormat string
}

func parseFlags() options {
	var opts options

	flag.StringVar(&opts.listen, "listen", "127.0.0.1:7777", "address to listen on; a :0 port binds a free one")
	flag.Int64Var(&opts.seed, "seed", 1, "world seed; the same seed regenerates the same world")
	registerWorldDirFlag(flag.CommandLine, &opts)
	flag.StringVar(&opts.worldName, "world-name", "",
		"the name this world is registered under with the account service, in lowercase letters, digits and "+
			"hyphens. A session ticket names one world and is useless at any other, so this is what the tickets "+
			"presented here must name. Required: a server that does not know which world it is cannot tell a "+
			"ticket for it from a ticket for somebody else's")
	flag.StringVar(&opts.accountService, "account-service", "",
		"https base URL of the account service, whose signing key is read once from "+ticketKeyPath+" at startup "+
			"and then kept. Nothing is asked of it again: admitting a player is a signature check, so the service "+
			"being down costs nobody a game. Mutually exclusive with -ticket-key, and requires "+
			"-account-service-fingerprint beside it")
	flag.StringVar(&opts.accountServiceFingerprint, "account-service-fingerprint", "",
		"the SHA-256 of the certificate the account service presents, in hex, as that service prints it at "+
			"startup under certificate_sha256. Required with -account-service and refused without it. There is "+
			"no trust on first use and no way to skip the check: a fetch that cannot tell which service "+
			"answered is one an attacker can answer, and the key it returns is kept for the life of this server")
	flag.StringVar(&opts.ticketKey, "ticket-key", "",
		"the account service's Ed25519 public key in hex, as GET "+ticketKeyPath+" publishes it and as that "+
			"service prints it at startup. Use it instead of -account-service when the key is copied by hand "+
			"rather than fetched. Exactly one of the two is required")
	flag.StringVar(&opts.announceAddress, "announce-address", "",
		"the address players dial this server at, as host:port, announced to -account-service so it appears in the "+
			"list they choose from. **Separate from -listen**: a server bound to 0.0.0.0 has to announce something a "+
			"player can actually reach, and only its operator knows what that is. Announcing is off unless this, "+
			"-account-service and a registration key are all given, and a failed announce is never fatal")
	flag.StringVar(&opts.registrationKeyFile, "registration-key-file", "",
		"a file holding the registration key this server announces with. The key itself is never a flag — a flag "+
			"value is visible in `ps` to every user on this machine and lands in shell history — so it comes from "+
			"this file or from "+registrationKeyEnv+", and never from both")
	flag.UintVar(&opts.tickRate, "tick-rate", game.DefaultTickRate, "authoritative simulation ticks per second (1..255)")
	flag.UintVar(&opts.viewDistance, "view-distance", game.DefaultViewDistance, "chunk streaming radius in chunks (0..16)")
	flag.UintVar(&opts.maxPlayers, "max-players", session.DefaultConcurrentSessions,
		"maximum concurrent sessions (100..1000); a connection past it receives SERVER_FULL")
	flag.Uint64Var(&opts.terrainMemoryMiB, "terrain-memory-mib", world.DefaultTerrainMemoryMiB,
		"memory budget for resident terrain chunks, in MiB; one chunk is budgeted as 96 KiB")
	flag.DurationVar(&opts.handshakeTimeout, "handshake-timeout", session.DefaultHandshakeTimeout,
		"how long a new connection may say nothing before it is closed; it gets no reply, having sent nothing to reply to")
	flag.DurationVar(&opts.characterTimeout, "character-timeout", session.DefaultCharacterTimeout,
		"how long an admitted account may take to choose a character before its connection is closed. Minutes "+
			"rather than seconds because this is the one phase of a connection a person is inside — reading a "+
			"character list, picking colours, typing a name — and neither of the other two windows describes "+
			"that. What bounds it at all is that the account's single live session is already claimed while it "+
			"waits. Must be at least the handshake timeout")
	flag.DurationVar(&opts.idleTimeout, "idle-timeout", session.DefaultIdleTimeout,

		"how long an admitted session may say nothing before it is closed; seconds are safe because the client sends "+
			"PlayerInput every tick — standing still and dead included — so a healthy client is never silent for "+
			"longer than one tick interval. Must be at least the handshake timeout")
	flag.DurationVar(&opts.stormPeriod, "storm-period", game.DefaultStormPeriod,
		"real-wall-clock time between Fimbulvetr storms; 0 disables storms")
	flag.StringVar(&opts.nextStorm, "next-storm", "",
		"one-time RFC3339 override for the next Fimbulvetr deadline; persisted at startup")
	flag.StringVar(&opts.logLevel, "log-level", "info", "log level: debug, info, warn or error")
	flag.StringVar(&opts.logFormat, "log-format", "text", "log format: text or json")
	flag.Parse()

	return opts
}

func registerWorldDirFlag(flags *flag.FlagSet, opts *options) {
	flags.StringVar(&opts.worldDir, "world-dir", world.DefaultWorldDir(),
		"directory the world's edits and the players' records are stored in; the default follows the terrain "+
			"generator version. Empty runs an ephemeral world where nothing is kept — edits are lost on exit, no "+
			"life is written, and a reconnect is a new player at the spawn even within the same process")
}

// validate checks the flags against the ranges they will be narrowed into.
//
// Checking the raw values is the whole point. Clamping first and validating the
// clamped result is how `-tick-rate 1000` becomes a silent 255 Hz server, and how
// an error message ends up quoting a number the operator never typed.
func (o options) validate() error {
	switch {
	case o.tickRate < 1 || o.tickRate > math.MaxUint8:
		return fmt.Errorf("tick rate must be in 1..%d, got %d", math.MaxUint8, o.tickRate)
	case o.viewDistance > protocol.MaxViewDistance:
		return fmt.Errorf("view distance must be at most %d, got %d", protocol.MaxViewDistance, o.viewDistance)
	case o.maxPlayers < session.MinConcurrentSessions || o.maxPlayers > session.MaxConcurrentSessions:
		return fmt.Errorf("max players must be in %d..%d, got %d",
			session.MinConcurrentSessions, session.MaxConcurrentSessions, o.maxPlayers)
	case o.terrainMemoryMiB == 0:
		return errors.New("terrain memory must be greater than zero MiB")
	case o.stormPeriod < 0:
		return fmt.Errorf("storm period must not be negative, got %s", o.stormPeriod)
	}
	if o.nextStorm != "" {
		if _, err := time.Parse(time.RFC3339, o.nextStorm); err != nil {
			return fmt.Errorf("next storm must be RFC3339, got %q: %w", o.nextStorm, err)
		}
	}

	// Asked of the type that enforces it at runtime rather than restated here. Two
	// copies of a rule are two rules the moment one of them is edited, and this one
	// is a range the operator can type — exactly the kind that gets widened in the
	// place nobody was reading.
	if err := o.timeouts().Validate(); err != nil {
		return err
	}

	// The same discipline for the door. Asked of `ticket.WorldIDFor`, which is the one
	// place a world name's vocabulary is decided — `registry.Server.Validate` asks the
	// same function rather than restating it, so a name this server will run under is
	// always one a ticket can be minted for.
	//
	// **Refused here rather than defaulted**, because there is no default that could be
	// right: a world id is what stops a ticket minted for somebody else's server being
	// presented at this one, and a server guessing at its own name would be a server
	// admitting the wrong people or nobody at all. It is checked before the key is
	// fetched, so an operator who typed the name wrong is told that instead of watching
	// an HTTP request fail.
	if _, err := ticket.WorldIDFor(o.worldName); err != nil {
		return fmt.Errorf("invalid -world-name: %w", err)
	}

	workingSet := world.CacheWorkingSetFor(int(o.viewDistance))
	capacity := world.CacheCapacityFor(int(o.viewDistance), int(o.maxPlayers), o.terrainMemoryMiB)
	if capacity < workingSet {
		return fmt.Errorf(
			"max players %d with a terrain memory budget of %d MiB gives a residency of %d chunks, "+
				"but one view distance %d session needs %d chunks (%d in view plus headroom), about %d MiB; "+
				"the largest view distance this budget can hold is %d",
			o.maxPlayers, o.terrainMemoryMiB, capacity, o.viewDistance, workingSet,
			world.ChunksInView(int(o.viewDistance)), world.MemoryMiBFor(uint64(workingSet)),
			world.LargestViewDistanceHeld(o.terrainMemoryMiB))
	}
	return o.validateTicketKeySource()
}

// timeouts is the read-deadline policy these flags describe.
func (o options) timeouts() session.Timeouts {
	return session.Timeouts{
		Handshake: o.handshakeTimeout,
		Character: o.characterTimeout,
		Idle:      o.idleTimeout,
		Leave:     session.DefaultLeaveLinger,
	}

}

func newLogger(level, format string) (*slog.Logger, error) {
	var lvl slog.Level
	if err := lvl.UnmarshalText([]byte(level)); err != nil {
		return nil, fmt.Errorf("unknown log level %q", level)
	}

	handlerOpts := &slog.HandlerOptions{Level: lvl}
	switch strings.ToLower(format) {
	case "text":
		return slog.New(slog.NewTextHandler(os.Stderr, handlerOpts)), nil
	case "json":
		return slog.New(slog.NewJSONHandler(os.Stderr, handlerOpts)), nil
	default:
		return nil, fmt.Errorf("unknown log format %q", format)
	}
}

func run(ctx context.Context, opts options, log *slog.Logger) error {
	if err := opts.validate(); err != nil {
		return fmt.Errorf("invalid flags: %w", err)
	}

	cfg := session.Config{
		WorldSeed: opts.seed,
		// Narrowing is safe here: validate has already refused anything that would
		// not survive the conversion.
		TickRate:     uint8(opts.tickRate),
		ChunkSize:    world.ChunkSize,
		ViewDistance: uint8(opts.viewDistance),
		// Derived from the world, never hardcoded: since #519 this is the square
		// outside the capital's castle gate, and the capital is a function of the seed.
		Spawn: world.SpawnAt(opts.seed),
	}
	// options.validate covers what the operator can type; this covers the contract
	// invariants from schemas/handshake.fbs for the fields that are not flag-derived.
	if err := cfg.Validate(); err != nil {
		return fmt.Errorf("invalid configuration: %w", err)
	}

	// **Before the world, before the port, before anything.** A server that cannot check
	// a session ticket cannot admit anybody, so there is nothing else worth doing if this
	// fails — and it is also the one step that can wait on somebody else's machine, which
	// makes it the step to fail before this process has taken a directory or a port. From
	// here on nothing on the admission path touches the network again.
	verifier, err := openVerifier(ctx, opts, log)
	if err != nil {
		return err
	}

	// One cache per server, seeded once: every session streams from the same chunks,
	// so a chunk two players can both see is generated once.
	//
	// Before the listener, deliberately. Opening the world is the last thing that can
	// refuse this configuration — a directory recorded under another seed is a refusal to
	// start, because loading its edits onto this seed's terrain would not fail, it would
	// quietly serve half of one world and half of another's digging. A server that has
	// already bound a port and accepted a client is a worse place to find that out.
	chunks, err := openWorld(opts, cfg.WorldSeed, log)
	if err != nil {
		return err
	}

	// After openWorld and never before it: the seed and worldgen checks are what
	// refuse a directory this server did not write, and a player record must not be
	// created inside one that is about to be rejected.
	players, err := openPlayers(opts, log)
	if err != nil {
		return err
	}

	camp, err := openStructures(opts, log)
	if err != nil {
		return err
	}

	clock, err := openClock(opts, log)
	if err != nil {
		return err
	}

	explored, err := openExploration(opts, log)
	if err != nil {
		return err
	}

	marks, err := openMarkers(opts, log)
	if err != nil {
		return err
	}

	tr, fingerprint, err := listen(opts, log)
	if err != nil {
		return err
	}

	// Telling the list where home is, if an operator asked for it. Built here because this
	// is where the fingerprint above is in hand, and it is the same number the startup line
	// carries and a client will demand — there is one source for it, never two.
	//
	// **This can refuse to announce and can never refuse to start.** A nil announcer is a
	// server that keeps every player it has and simply does not appear in the list; see
	// newAnnouncer, which says so in one startup line.
	announce := newAnnouncer(opts, fingerprint, log)

	// Built before the simulation because the simulation is handed its identity source:
	// players are named by the registry as connections arrive, and every other entity —
	// an item on the ground, and in time whatever else the world owns — is named by the
	// same counter, so no id ever means two things at once.
	registry := session.NewRegistry(int(opts.maxPlayers))

	// Who is live, one level up from the connections: entity ids name a session,
	// players outlive one. Built here rather than inside Serve because it is shared by
	// every session — being the one place that knows an account is already playing is
	// the whole of what it does, and it holds the verifier for the same reason: one
	// key, read once, shared by every door.
	identities, err := session.NewIdentities(players, explored, marks, verifier, log)
	if err != nil {
		return err
	}

	// The simulation collides against exactly the chunks the sessions stream, read
	// through Peek so that a tick never generates terrain — and edits the same chunks
	// through the cache directly, because resolving an edit is allowed to wait for one.
	// Two seams over one world: the reader that must never block, and the writer that may.
	//
	// The seed goes in beside them and generates nothing: it seeds the spawn director's
	// one generator, so where the dark puts creatures is a property of this world rather
	// than of this process. See game.NewSim.
	sim, err := game.NewSim(cfg.TickRate, cfg.ViewDistance, cfg.WorldSeed, game.NewCacheTerrain(chunks), chunks, registry.NextID, log)
	if err != nil {
		return fmt.Errorf("invalid simulation: %w", err)
	}
	if err := sim.ConfigureChunkRegeneration(chunks, registry.ResendChunk); err != nil {
		return fmt.Errorf("configure chunk regeneration: %w", err)
	}
	if err := sim.ConfigureWater(chunks); err != nil {
		return fmt.Errorf("configure water: %w", err)
	}

	// Before the listener is served and therefore before any session can be admitted,
	// which is what puts the camp in the first snapshot a returning player receives
	// rather than in the one after the first autosave.
	restoreStructures(sim, camp, log)

	// The world's time of day, in the same window and for the same reason: a returning
	// player's *first* snapshot should carry the evening they logged off in, not the
	// dawn a default would have handed them for one tick.
	restoreClock(sim, clock, log)
	stormDeadlineChanged := false
	if opts.stormPeriod == 0 {
		stormDeadlineChanged = sim.NextStorm() != 0
		sim.DisableStorm()
	} else if opts.nextStorm != "" {
		next, err := time.Parse(time.RFC3339, opts.nextStorm)
		if err != nil {
			return fmt.Errorf("parse validated next storm %q: %w", opts.nextStorm, err)
		}
		sim.ScheduleStorm(next.Unix())
		stormDeadlineChanged = true
	}

	// **Nothing places a mob here, and that absence is the feature.** This used to put
	// one draugr at a seed-derived anchor, where it stood for as long as the server ran
	// and where its replacement stood ten seconds after anyone killed it. The world is
	// populated by the spawn director now, from inside the tick, around the players who
	// are actually connected — so a server nobody has joined holds no creatures at all.

	srv := &server{
		tr:          tr,
		registry:    registry,
		identities:  identities,
		cfg:         cfg,
		timeouts:    opts.timeouts(),
		chunks:      chunks,
		structures:  camp,
		clock:       clock,
		sim:         sim,
		saveEvery:   world.DefaultSaveInterval,
		stormPeriod: opts.stormPeriod,
		wallClock:   game.SystemClock{},
		announce:    announce,
		log:         log,
	}
	if stormDeadlineChanged {
		// The override is a startup action rather than an in-memory suggestion. Writing
		// it now means a crash before the first autosave still restarts from that choice.
		srv.flushClock()
	}

	log.Info("voxelheimd listening",
		"addr", tr.Addr(),
		"world_name", opts.worldName,
		"tick_rate", cfg.TickRate,
		"chunk_size", cfg.ChunkSize,
		"view_distance", cfg.ViewDistance,
		"max_players", opts.maxPlayers,
		"terrain_memory_mib", opts.terrainMemoryMiB,
		"resident_chunks", chunks.Capacity(),
		"world_seed", cfg.WorldSeed,
		"world_dir", opts.worldDir,
		"handshake_timeout", opts.handshakeTimeout.String(),
		"idle_timeout", opts.idleTimeout.String(),
		"storm_period", opts.stormPeriod.String(),
		"next_storm_unix", sim.NextStorm(),
	)

	srv.run(ctx)
	return nil
}

// openWorld builds the chunk cache, backed by the world directory unless the operator
// asked for an ephemeral world.
//
// **Only the edits are stored, never the generated terrain.** The base is a pure function
// of the seed, so writing it would be spending disk on something this process can always
// recompute — and it would erase the one distinction the GDD's Fimbulvetr storm is built
// on, which is knowing which voxels a player put there.
func openWorld(opts options, seed int64, log *slog.Logger) (*world.Cache, error) {
	if opts.worldDir == "" {
		// Loud, because it is the mode in which an evening's digging disappears. Chosen
		// explicitly by an empty -world-dir; the flag's default is a real directory.
		log.Warn("no world directory; this world is ephemeral and every edit will be lost on exit")
		return world.NewCache(seed, world.DefaultWorkers,
			world.CacheCapacityFor(int(opts.viewDistance), int(opts.maxPlayers), opts.terrainMemoryMiB)), nil
	}

	store, err := world.OpenStore(opts.worldDir, seed)
	if err != nil {
		return nil, fmt.Errorf("opening the world directory: %w", err)
	}
	log.Info("world directory opened", "world_dir", store.Dir(), "format_version", world.StoreVersion)

	return world.NewPersistentCache(store, world.DefaultWorkers,
		world.CacheCapacityFor(int(opts.viewDistance), int(opts.maxPlayers), opts.terrainMemoryMiB)), nil
}

// listen starts the server's transport, which is encrypted and has no alternative.
//
// **There is no flag here, and that is the design rather than an omission.** An
// identity token is a bearer credential: whatever can read one off the wire can come
// back as that player. A switch that turned the encryption off would make that exposure
// a configuration mistake somebody could make once and never notice — and the failure
// mode of a plaintext session is silent, because nothing about it looks wrong from
// either end. The only setting nobody can get wrong is the one that does not exist.
//
// The cost is stated where it lands: an operator keeps server-key.pem, and an ephemeral
// world cannot, so every returning client is refused by its own pin until it clears it.
// That is a refusal a person can see and act on, which is the whole trade — a visible
// failure instead of an invisible exposure.
//
// **The fingerprint is logged at Info, on every start.** It is the number a player
// compares against a refusal, and an operator who cannot produce it on demand cannot
// answer the one question a refused client asks. It gives nothing away — it is a hash of
// a certificate the server hands to everyone who connects.
//
// **It is returned as well as logged**, because the announcer sends it to the account
// service and a client now takes its expectation from that list rather than from a pinned
// file. One certificate, one digest, one function that computes it: the number an operator
// reads in the line below, the number in the list, and the number a client demands are the
// same string or the whole chain is a server nobody can join.
func listen(opts options, log *slog.Logger) (transport.Transport, string, error) {
	var (
		cert tls.Certificate
		err  error
	)
	if opts.worldDir == "" {
		cert, err = certs.Ephemeral()
		// The second warning an ephemeral world earns, and it is not a repeat of
		// openWorld's: that one is about edits, this one is about every returning
		// player being refused by a pin they cannot match.
		log.Warn("an ephemeral world cannot keep its TLS key, so this server presents a new certificate every start; " +
			"clients that pinned an earlier one will refuse to reconnect until they clear the pin")
	} else {
		cert, err = certs.LoadOrCreate(opts.worldDir)
	}
	if err != nil {
		return nil, "", fmt.Errorf("preparing the server certificate: %w", err)
	}

	fingerprint, err := certs.Fingerprint(cert)
	if err != nil {
		return nil, "", fmt.Errorf("reading the server certificate: %w", err)
	}
	log.Info("listening with an encrypted session", "certificate_sha256", fingerprint)

	tr, err := transport.ListenTLS(opts.listen, cert)
	if err != nil {
		return nil, "", err
	}
	return tr, fingerprint, nil
}

// openPlayers opens the player store under the same -world-dir, or answers nil for
// an ephemeral world.
//
// Nil rather than a store that writes nowhere: every persistence path in this server
// is a no-op against a nil store instead of a branch at each call site, and this is
// the same shape openWorld above uses for the chunk cache. An ephemeral world still
// mints tokens and still refuses a second session on one identity — those need no
// disk — so the difference the operator chose is exactly the one they get.
func openPlayers(opts options, log *slog.Logger) (*persist.Store, error) {
	if opts.worldDir == "" {
		// openWorld has already warned that this world is ephemeral; saying it twice
		// would be a second warning about the same decision.
		return nil, nil
	}

	store, err := persist.OpenStore(opts.worldDir)
	if err != nil {
		return nil, fmt.Errorf("opening the player store: %w", err)
	}
	log.Info("player store opened",
		"players_dir", store.Dir(), "format_version", persist.StoreVersion,
		"characters", store.Count(), "max_per_account", persist.MaxCharactersPerAccount)

	// **The one event a format change gets, and it is a warning rather than a line
	// nobody reads.** A directory written before characters cannot be read by this
	// build and is not migrated — see persist.StoreVersion — so it is moved aside
	// whole and a fresh one is opened. Nothing was deleted; an operator who wants
	// those bytes will find them at kept_at, and this is the only time anything says so.
	if aside := store.SetAside(); aside != "" {
		log.Warn("this world's players directory predates characters; it has been kept and a fresh one opened",
			"kept_at", aside, "format_version", persist.StoreVersion)
	}
	for _, kept := range store.Unreadable() {
		log.Error("a character record could not be indexed; it has been kept and that character is not in this world",
			"kept_at", kept)
	}

	return store, nil
}

// openStructures opens the structures file under the same -world-dir, or answers nil
// for an ephemeral world.
//
// Nil rather than a store that writes nowhere, the shape openWorld and openPlayers
// above both use. An ephemeral world still lets a player pitch a tent and respawn at
// it; what it does not do is remember either after the process ends, which is exactly
// the difference the operator chose.
func openStructures(opts options, log *slog.Logger) (*persist.StructureStore, error) {
	if opts.worldDir == "" {
		// openWorld has already warned that this world is ephemeral.
		return nil, nil
	}

	store, err := persist.OpenStructureStore(opts.worldDir)
	if err != nil {
		return nil, fmt.Errorf("opening the structure store: %w", err)
	}
	log.Info("structure store opened", "structures_file", store.Path(), "format_version", persist.StructuresVersion)

	return store, nil
}

// openExploration opens the per-character map ledgers under the same -world-dir, or
// answers nil for an ephemeral world.
//
// Nil rather than a store that writes nowhere, the shape openWorld, openPlayers,
// openStructures and openClock all use. An ephemeral world's characters still explore
// and are still told what they have explored; what it does not do is remember any of it
// after the process ends, which is exactly the difference the operator chose.
//
// **No scan and no index**, unlike openPlayers. A ledger is opened by the id of the
// character playing it, one file at a time, and there is no question about the
// directory as a whole for a startup pass to answer. A file this build cannot read is
// therefore found at the login that needs it and set aside there — see
// session.Identities.recallExploration, which is also where the reason it does not
// refuse that login is written down.
func openExploration(opts options, log *slog.Logger) (*persist.ExplorationStore, error) {
	if opts.worldDir == "" {
		// openWorld has already warned that this world is ephemeral.
		return nil, nil
	}

	store, err := persist.OpenExplorationStore(opts.worldDir)
	if err != nil {
		return nil, fmt.Errorf("opening the exploration store: %w", err)
	}
	log.Info("exploration store opened",
		"exploration_dir", store.Dir(), "format_version", persist.ExplorationVersion,
		"max_columns_per_character", persist.MaxExploredColumns)

	return store, nil
}

// openMarkers opens the per-character marker files under the same -world-dir, or answers
// nil for an ephemeral world.
//
// Nil rather than a store that writes nowhere, the shape openWorld, openPlayers,
// openStructures, openClock and openExploration all use. An ephemeral world's characters
// still put marks on the map and are still answered with the whole list; what it does not
// do is remember any of it after the process ends, which is exactly the difference the
// operator chose.
//
// **No scan and no index**, for the reason openExploration gives: a character's marks are
// opened by the id of the character playing them, one file at a time, and there is no
// question about the directory as a whole for a startup pass to answer. A file this build
// cannot read is therefore found at the login that needs it and set aside there — see
// session.Identities.recallMarkers, which is also where the reason it does not refuse that
// login is written down.
func openMarkers(opts options, log *slog.Logger) (*persist.MarkerStore, error) {
	if opts.worldDir == "" {
		// openWorld has already warned that this world is ephemeral.
		return nil, nil
	}

	store, err := persist.OpenMarkerStore(opts.worldDir)
	if err != nil {
		return nil, fmt.Errorf("opening the marker store: %w", err)
	}
	log.Info("marker store opened",
		"markers_dir", store.Dir(), "format_version", persist.MarkersVersion,
		"max_marks_per_character", persist.MaxMarkers, "max_note_bytes", persist.MaxMarkerNote)

	return store, nil
}

// restoreStructures puts the stored camp back, or starts the world without one.
//
// **A failure here is logged and survived, never returned**, and the asymmetry with
// openStructures above is deliberate. Not being able to *open* the world directory is a
// configuration mistake and the server should refuse to start; a file inside it that
// this build cannot read is a world that has lost its camp, and refusing to start would
// take everything else — the terrain, every player record, the ability to log in at all
// — hostage to it. The player store makes the same call one identity at a time.
//
// The unreadable file is left exactly where it is. Nothing rewrites it until something
// dirties the camp, because a restore deliberately does not mark it dirty, so the
// evidence outlives the start that could not use it.
func restoreStructures(sim *game.Sim, store *persist.StructureStore, log *slog.Logger) {
	stored, found, err := store.Load()
	if err != nil {
		log.Error("the stored structures could not be read; this world starts with none, and the file is kept",
			"structures_file", store.Path(), "error", err)
		return
	}
	if !found {
		return
	}

	camp := make([]game.Structure, len(stored))
	for i, rec := range stored {
		// The four fields, one at a time. game and persist do not import each other, so
		// this loop is the mapping — the same job session does between persist.Record
		// and game.Life, and here for the same reason.
		camp[i] = game.Structure{
			Kind:   rec.Kind,
			Anchor: rec.Anchor,
			Facing: rec.Facing,
			Owner:  rec.Owner,
		}
	}

	if err := sim.RestoreStructures(camp); err != nil {
		log.Error("the stored structures were refused whole; this world starts with none, and the file is kept",
			"structures_file", store.Path(), "structures", len(camp), "error", err)
		return
	}
	log.Info("structures restored", "structures_file", store.Path(), "structures", len(camp))
}

// openClock opens the clock file under the same -world-dir, or answers nil for an
// ephemeral world.
//
// Nil rather than a store that writes nowhere, the shape openWorld, openPlayers and
// openStructures above all use. An ephemeral world still has a day and a night, and
// they still arrive on time; what it does not do is remember where in the day it was,
// which is exactly the difference the operator chose.
func openClock(opts options, log *slog.Logger) (*persist.ClockStore, error) {
	if opts.worldDir == "" {
		// openWorld has already warned that this world is ephemeral.
		return nil, nil
	}

	store, err := persist.OpenClockStore(opts.worldDir)
	if err != nil {
		return nil, fmt.Errorf("opening the clock store: %w", err)
	}
	log.Info("clock store opened", "clock_file", store.Path(), "format_version", persist.ClockVersion)

	return store, nil
}

// restoreClock puts the stored clock back, or starts the world at first light.
//
// **restoreStructures' discipline, three numbers instead of a camp**, and the same three
// answers. A file that is not there is a world that has not been played in, and that is
// silence rather than a log line. A file that cannot be *read* — unreachable, wrong
// magic, a version this build does not speak, a flipped byte under the checksum — and
// values that cannot be *true* — a tick of day at or beyond the day length, or a world
// tick that disagrees with it — are both logged at error and both survived: the world
// starts at tick 0 with no storm scheduled and everything else about it still works.
// Refusing to start over a clock would take the terrain, every player record and the
// ability to log in at all hostage to thirty-two bytes.
//
// **The storm deadline is restored even though nothing schedules one yet**: the field is
// written by every save from now on, so a build that read it only once the scheduler
// existed would spend the intervening releases dropping a value it had stored.
//
// **The unreadable file is left exactly where it is**, and unlike the structures file it
// does not stay there long: the clock is rewritten on the next autosave pass, because
// there is no dirty flag to hold it back — see saveClockLoop. The evidence therefore
// survives the start that could not use it and no longer than that, which is the trade
// a value that changes every tick forces. A corrupt clock costs a player one evening's
// position in the day; a corrupt player record costs them a life, which is why that one
// is quarantined under a timestamped name instead.
func restoreClock(sim *game.Sim, store *persist.ClockStore, log *slog.Logger) {
	stored, found, err := store.Load()
	if err != nil {
		log.Error("the stored clock could not be read; this world starts at first light",
			"clock_file", store.Path(), "error", err)
		return
	}
	if !found {
		return
	}

	// The checks belong to game, which owns the day length; persist judges only what a
	// file can be wrong about. A clock that cannot exist is refused rather than repaired
	// — see game.Sim.RestoreClock.
	if err := sim.RestoreClock(stored.TickOfDay, stored.WorldTick); err != nil {
		log.Error("the stored clock was refused; this world starts at first light",
			"clock_file", store.Path(), "tick_of_day", stored.TickOfDay,
			"world_tick", stored.WorldTick, "error", err)
		return
	}
	// Only after the pair is accepted. A file whose clock was refused is one this build
	// does not believe, and taking one field out of it would be believing part of it.
	sim.ScheduleStorm(stored.NextStormUnix)
	log.Info("clock restored", "clock_file", store.Path(), "tick_of_day", stored.TickOfDay,
		"world_tick", stored.WorldTick, "next_storm_unix", stored.NextStormUnix)
}

// server wires the transport, the session registry and the simulation loop
// together. It is a type so that the shutdown ordering below can be tested with a
// fake transport, instead of only through a signal and a real socket.
type server struct {
	tr         transport.Transport
	registry   *session.Registry
	identities *session.Identities
	cfg        session.Config
	timeouts   session.Timeouts
	chunks     *world.Cache
	structures *persist.StructureStore
	clock      *persist.ClockStore
	sim        *game.Sim
	clockMu    sync.Mutex

	// saveEvery is the autosave interval. Zero means world.DefaultSaveInterval; tests
	// shorten it so the loop can be observed without waiting on the real one.
	saveEvery time.Duration

	// stormPeriod is zero when the event is disabled. wallClock is shared with no
	// simulation state: it drives only the ten-second scheduler and is injectable so
	// tests can cross a real week without waiting for one.
	stormPeriod time.Duration
	stormEvery  time.Duration
	wallClock   game.Clock
	stormCycle  stormCycle

	// announce tells the account service where this server is, or is nil when nobody asked
	// it to. Nil is the ordinary state — a LAN game, a test, an operator who has registered
	// nothing — and its loop returns at once, which is the shape a nil player store already
	// uses here.
	announce *announcer

	log *slog.Logger
}

// run serves until ctx ends, then shuts down and returns.
func (s *server) run(ctx context.Context) {
	// Two wait groups, because shutdown must wait on them in order: the accept
	// loop first, everything it spawned second. See shutdown for why.
	var (
		accepting sync.WaitGroup
		workers   sync.WaitGroup
	)

	workers.Add(1)
	go func() {
		defer workers.Done()
		if err := s.waterScanLoop(ctx); err != nil && !errors.Is(err, context.Canceled) {
			s.log.Error("the water composition scanner stopped", "error", err)
		}
	}()

	// One heartbeat per simulated minute, at debug level: enough to see that the
	// loop is alive without turning the log into a metronome.
	heartbeatEvery := uint64(s.cfg.TickRate) * 60

	loop, err := game.NewLoop(s.cfg.TickRate, game.SystemClock{}, s.log, func(tick uint64) {
		// The whole simulation, and it runs whether or not anyone is connected: time
		// in the world does not depend on who is watching. Every player is advanced
		// from their intent and every session is handed what it can see — nothing here
		// blocks, and nothing here generates terrain.
		s.broadcastWaterChanges(s.sim.Step(tick))

		if tick%heartbeatEvery == 0 {
			// Peek-only, never Get: the tick loop must not generate terrain, because a
			// tick that waits on a chunk is a tick every connected player misses.
			s.log.Debug("simulation heartbeat",
				"tick", tick,
				"sessions", s.registry.Count(),
				"players", s.sim.Count(),
				"drops", s.sim.DropCount(),
				"chunks_resident", s.chunks.Len(),
			)
		}
	})
	if err != nil {
		// Unreachable in practice: cfg.TickRate is validated before we get here.
		// Logged rather than returned so a future refactor cannot turn a config
		// mistake into a server that silently does not tick.
		s.log.Error("tick loop not started", "error", err)
	} else {
		workers.Add(1)
		go func() {
			defer workers.Done()
			if err := loop.Run(ctx); err != nil && !errors.Is(err, context.Canceled) {
				s.log.Error("tick loop failed", "error", err)
			}
		}()
	}

	// The autosave loop is a worker like the tick loop, and being one is what puts it in
	// the shutdown ordering: workers.Wait() is what makes the final flush the only writer
	// left. An ephemeral world's SaveLoop returns immediately.
	workers.Add(1)
	go func() {
		defer workers.Done()
		if err := s.chunks.SaveLoop(ctx, s.saveEvery, s.log); err != nil && !errors.Is(err, context.Canceled) {
			s.log.Error("the world autosave loop stopped", "error", err)
		}
	}()

	// The Fimbulvetr's wall-clock worker. It never performs I/O under Sim.mu: listing
	// candidates happens here, and the simulation consumes the resulting pass in bounded
	// slices on later authoritative ticks.
	workers.Add(1)
	go func() {
		defer workers.Done()
		if err := s.stormLoop(ctx); err != nil && !errors.Is(err, context.Canceled) {
			s.log.Error("the storm scheduler stopped", "error", err)
		}
	}()

	// The players' own autosave, beside the world's and on the same interval. A worker
	// for the same reason, and with the same consequence: it stops before shutdown's
	// final write, and every session's teardown has already written that session's
	// record by then — which is why there is no final flush for players.
	workers.Add(1)
	go func() {
		defer workers.Done()
		if err := s.savePlayersLoop(ctx); err != nil && !errors.Is(err, context.Canceled) {
			s.log.Error("the player autosave loop stopped", "error", err)
		}
	}()

	// The camp's own autosave, beside the world's and the players', on the same
	// interval and a worker for the same reason. Unlike the players', this one *does*
	// have a final flush in shutdown: a session's teardown writes that session's record,
	// but nothing writes the camp when a player leaves — a structure outlives the
	// session that placed it, which is the whole point of the file.
	workers.Add(1)
	go func() {
		defer workers.Done()
		if err := s.saveStructuresLoop(ctx); err != nil && !errors.Is(err, context.Canceled) {
			s.log.Error("the structure autosave loop stopped", "error", err)
		}
	}()

	// The clock's own autosave, on the same interval and a worker for the same reason.
	// It has a final flush in shutdown like the camp's, and for a stronger version of
	// the same reason: nothing a session does writes the clock, because nothing a
	// session does moves it — the tick loop is the only thing that ever has.
	workers.Add(1)
	go func() {
		defer workers.Done()
		if err := s.saveClockLoop(ctx); err != nil && !errors.Is(err, context.Canceled) {
			s.log.Error("the clock autosave loop stopped", "error", err)
		}
	}()

	// Telling the account service where this server is, on the same worker discipline as the
	// autosaves above: it ends on ctx, it is waited for in shutdown, and a pass that fails is
	// logged and retried rather than escalated. **A nil announcer's loop returns at once**,
	// which is what makes "nobody configured this" cost one function call rather than a
	// branch at this call site.
	workers.Add(1)
	go func() {
		defer workers.Done()
		if err := s.announce.loop(ctx); err != nil && !errors.Is(err, context.Canceled) {
			s.log.Error("the announce loop stopped", "error", err)
		}
	}()

	accepting.Add(1)
	go func() {
		defer accepting.Done()
		s.acceptLoop(ctx, &workers)
	}()

	<-ctx.Done()
	s.shutdown(&accepting, &workers)
}

// savePlayersLoop writes every connected player's life, every interval, until ctx ends.
//
// The players' counterpart to world.Cache.SaveLoop and deliberately the same shape,
// including what it does about failure: a full disk is a reason to shout, not a reason
// for a server to stop saving for the rest of its life, so a failed write is logged and
// the next pass tries again. It returns ctx.Err() on cancellation.
//
// **What it exists for is the crash**, not the disconnect. Every clean end of a session
// — a quit, an idle timeout, a shutdown — writes that session's record in Serve's
// teardown, so this loop changes nothing about any of them. What it bounds is how much
// of an evening a kill -9 can take: at most one interval.
//
// The capture and the write are separate, which is the discipline this file already
// keeps for chunks. Sim.Records takes the simulation's lock, copies, and releases it;
// every byte reaches the disk out here, with nothing held.
func (s *server) savePlayersLoop(ctx context.Context) error {
	every := s.saveEvery
	if every <= 0 {
		every = world.DefaultSaveInterval
	}

	ticker := time.NewTicker(every)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
			if err := s.identities.RememberAll(s.sim.Records()); err != nil {
				s.log.Error("saving the connected players failed; they will be retried", "error", err)
			}
			s.flushExperienceAwards()
		}
	}
}

// flushExperienceAwards writes every mob award earned after its tap owner left the
// simulation. Each write is an absolute lifetime total and is acknowledged
// independently: one damaged record cannot discard another character's reward, and a
// failed write remains queued for the next autosave.
func (s *server) flushExperienceAwards() {
	for _, award := range s.sim.PendingExperienceAwards() {
		persisted, err := s.identities.RememberExperience(award)
		if err != nil {
			s.log.Error("saving offline mob experience failed; it will be retried",
				"player_id", award.PlayerID.Short(), "error", err)
			continue
		}
		if persisted {
			s.sim.AcknowledgeExperienceAward(award)
		}
	}
}

// saveStructuresLoop writes the camp whenever it has changed, until ctx ends.
//
// world.Cache.SaveLoop's shape, including what it does about failure: a full disk is a
// reason to shout, not a reason for a server to stop saving for the rest of its life,
// so a failed write puts the camp back in the queue and the next pass — and the final
// flush at shutdown — tries again. It returns ctx.Err() on cancellation.
//
// A pass over an unchanged camp costs one mutex and no I/O, which is what the dirty
// flag buys: a world nobody is building in is not rewriting a byte-identical file every
// five seconds for the life of the process.
//
// The capture and the write are separate here as everywhere else in this file:
// Sim.TakeDirtyStructures takes the simulation's lock, copies, and releases it; every
// byte reaches the disk out here with nothing held.
func (s *server) saveStructuresLoop(ctx context.Context) error {
	every := s.saveEvery
	if every <= 0 {
		every = world.DefaultSaveInterval
	}

	ticker := time.NewTicker(every)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
			s.flushStructures()
		}
	}
}

// flushStructures writes the camp if it has changed, and puts it back in the queue if
// the write fails.
//
// The re-marking is the contract Sim.TakeDirtyStructures states and the one
// world.Cache.Flush keeps for a chunk: taking the camp clears the flag, so a caller
// that dropped a failed write would lose the change for good.
func (s *server) flushStructures() bool {
	camp, dirty := s.sim.TakeDirtyStructures()
	if !dirty {
		return true
	}

	// The other direction of restoreStructures' loop, and the only other place the two
	// four-field types meet. Both mappings live in this file because this is the one
	// package that imports game and persist together.
	records := make([]persist.StructureRecord, len(camp))
	for i, held := range camp {
		records[i] = persist.StructureRecord{
			Kind:   held.Kind,
			Anchor: held.Anchor,
			Facing: held.Facing,
			Owner:  held.Owner,
		}
	}

	if err := s.structures.Save(records); err != nil {
		s.sim.MarkStructuresDirty()
		s.log.Error("saving the structures failed; they will be retried",
			"structures_file", s.structures.Path(), "structures", len(camp), "error", err)
		return false
	}
	return true
}

// saveClockLoop writes the world's time of day, every interval, until ctx ends.
//
// The camp's loop and the players', on the same interval, with the same answer to
// failure: a full disk is a reason to shout, not a reason for a server to stop saving
// for the rest of its life. It returns ctx.Err() on cancellation.
//
// **There is no dirty flag here, and the absence is the point.** The camp has one
// because a world nobody is building in should cost no I/O at all; there is no such
// thing as a world in which time is not passing, so a flag would be set on every pass
// and would buy a comparison instead of a write. What the clock costs unconditionally
// is thirty-two bytes and a rename every five seconds.
//
// **A failed write is not re-marked either, for the same reason it needs no flag.** The
// next pass reads the live clock, which is newer than the one that failed — so unlike a
// camp, where the change is gone from the registry the moment it is taken, nothing here
// is lost by dropping a failure on the floor. What a failed write costs is the ticks
// between it and the next success, which is what the interval already bounds.
func (s *server) saveClockLoop(ctx context.Context) error {
	every := s.saveEvery
	if every <= 0 {
		every = world.DefaultSaveInterval
	}

	ticker := time.NewTicker(every)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
			s.flushClock()
		}
	}
}

// flushClock writes where the world's day has got to, how long the world has run, and
// when its next storm falls due.
//
// The capture and the write are separate here as everywhere else in this file:
// Sim.Clock takes the simulation's lock, copies three numbers, and releases it; the
// bytes that reach the disk do so with nothing held.
//
// **Sim.Clock and not the three readers beside it**, and this is the call site that
// distinction was written for: the tick loop is free to run between two lock takes, so a
// tick of day from one and a world tick from the next can disagree — and
// game.Sim.RestoreClock refuses a pair that disagrees, so the world would lose its clock
// at the next start with nothing to say why.
func (s *server) flushClock() {
	s.clockMu.Lock()
	defer s.clockMu.Unlock()
	tickOfDay, worldTick, nextStormUnix := s.sim.Clock()
	if err := s.clock.Save(tickOfDay, worldTick, nextStormUnix); err != nil {
		s.log.Error("saving the clock failed; it will be retried",
			"clock_file", s.clock.Path(), "tick_of_day", tickOfDay,
			"world_tick", worldTick, "next_storm_unix", nextStormUnix, "error", err)
	}
}

// shutdown stops the server in the only order that terminates.
//
// Closing the listener unblocks Accept, but an accept-loop iteration can already
// be holding a connection it has not registered yet. Snapshotting the registry at
// that moment would miss that connection: its session would start, block in
// ReadFrame, and never be closed — so the final wait would never return, and a
// client that connected and said nothing at exactly the wrong moment would hang
// the shutdown for good.
//
// Waiting for the accept loop to exit before closing the registered connections
// closes that window by construction: once the loop has returned, every connection
// that exists is registered.
//
// The final save belongs at the end of that same sequence rather than after it. Edits
// arrive on session goroutines and the autosave loop is a worker, so workers.Wait() is the
// first moment at which the delta layer has stopped changing and nothing else is writing
// to the world directory — which makes this one flush the last word on what the world
// holds. Flushing any earlier races an edit into oblivion; flushing outside shutdown, from
// main, would run after the process has already told itself it stopped.
func (s *server) shutdown(accepting, workers *sync.WaitGroup) {
	s.log.Info("shutting down", "sessions", s.registry.Count())

	if err := s.tr.Close(); err != nil {
		s.log.Warn("closing the listener failed", "error", err)
	}
	accepting.Wait()
	s.registry.CloseAll()
	workers.Wait()

	// A mob can die after its tap owner disconnected and after that session wrote its
	// final record. The award is queued by the simulation and this is the last durable
	// write after both the tick and every session have stopped changing it.
	s.flushExperienceAwards()

	if err := s.chunks.Flush(); err != nil {
		s.log.Error("saving the world failed; edits since the last autosave are lost", "error", err)
	}
	// The camp, in the same window and for the same reason: placement and removal arrive
	// on session goroutines and the autosave loop is a worker, so workers.Wait() above is
	// the first moment nothing else can still be changing it.
	s.flushStructures()
	// The clock, in the same window and for a stricter version of the same reason: the
	// tick loop is the only thing that moves it and the tick loop is a worker, so after
	// workers.Wait() the day has genuinely stopped and this write is the last word on
	// where it stopped.
	s.flushClock()

	s.log.Info("voxelheimd stopped")
}

func (s *server) acceptLoop(ctx context.Context, workers *sync.WaitGroup) {
	backoff := minAcceptBackoff

	for {
		conn, err := s.tr.Accept()
		if err != nil {
			if ctx.Err() != nil || transport.IsClosed(err) {
				return
			}

			s.log.Warn("accept failed; retrying", "error", err, "retry_in", backoff.String())
			select {
			case <-ctx.Done():
				return
			case <-time.After(backoff):
			}
			backoff = min(backoff*2, maxAcceptBackoff)
			continue
		}
		backoff = minAcceptBackoff

		entityID, admitted := s.registry.Add(conn)
		if !admitted {
			detail := fmt.Sprintf("server holds at most %d concurrent sessions", s.registry.Limit())
			connectionLog := s.log.With("remote_addr", conn.RemoteAddr())
			if wErr := conn.WriteFrame(protocol.EncodeServerReject(vnet.RejectReasonSERVER_FULL, detail)); wErr != nil {
				connectionLog.Debug("sending the server-full refusal failed", "error", wErr)
			} else {
				connectionLog.Info("connection refused", "reason", vnet.RejectReasonSERVER_FULL.String(), "detail", detail)
			}
			if cErr := conn.Close(); cErr != nil {
				connectionLog.Debug("closing a refused connection failed", "error", cErr)
			}
			continue
		}
		sessionLog := s.log.With("entity_id", entityID, "remote_addr", conn.RemoteAddr())
		sessionLog.Info("connection accepted")

		workers.Add(1)
		go func() {
			defer workers.Done()
			defer s.registry.Remove(entityID)
			defer func() {
				if err := conn.Close(); err != nil {
					sessionLog.Debug("closing connection failed", "error", err)
				}
			}()

			if err := session.Serve(ctx, conn, s.cfg, s.timeouts, s.chunks, s.sim, s.registry, s.identities, entityID, sessionLog); err != nil {
				sessionLog.Warn("session ended with an error", "error", err)
			}
		}()
	}
}
