package game

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"log/slog"
	"math"
	"math/rand/v2"
	"slices"
	"sync"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Sim is the authoritative world: every connected player, and the tick that
// advances them.
//
// It decides; sessions carry the results. A session hands it intent through a
// *Player and is handed frames back through the deliver callback it supplied — so
// nothing here touches a socket, and nothing here knows a session exists beyond
// "there is somewhere to put a snapshot".
//
// One mutex guards the whole thing, and Step holds it for a whole tick. That is
// deliberate rather than lazy: see Leave for the guarantee it buys, and Step for why
// nothing under the lock is allowed to block.
type Sim struct {
	// dt is the physics timestep, derived from the tick rate rather than measured.
	// The fixed timestep is what makes the simulation reproducible from the same
	// inputs, and what makes -tick-rate 40 a server that moves players at the same
	// speed with twice the resolution instead of twice as fast.
	dt float64

	viewDistance int32
	idleLimit    int

	// terrain is read only while mu is held: Step samples it to advance state and
	// Mine samples it when accepting a new target. CacheTerrain memoises its last
	// lookup and is explicitly not safe for concurrent use, so the serialisation is
	// not an incidental property.
	terrain Terrain

	// editor is the other end of the world, and the opposite discipline: it is used only
	// from session goroutines, never under the lock, because applying an edit can wait on
	// a chunk being generated and a tick that waits on terrain is a tick every connected
	// player misses.
	editor Editor

	// mintEntityID hands out identities for everything the simulation owns that is not a
	// player, and it is deliberately the *same* counter a session mints a player from
	// (session.Registry.NextID). One counter is what makes "an entity id names one thing"
	// true rather than merely likely: two counters would agree for a while and then start
	// naming a drop and a player with the same number, which every consumer reads as one
	// entity changing kind.
	//
	// Injected rather than owned, because identities belong beside the connections —
	// game may not import session, and a second counter here is exactly the bug above.
	mintEntityID func() uint64

	// deathTicks and protectionTicks are DeathDuration and RespawnProtection in the
	// ticks Step counts, derived from the configured rate for the reason dropLifetime
	// is: the simulation's only clock is Step, so a death must last the same three
	// seconds on a 5 Hz server as on a 60 Hz one.
	deathTicks      uint32
	protectionTicks uint32

	// regenDelayTicks and regenIntervalTicks are HealthRegenDelay and
	// HealthRegenInterval in the ticks Step counts, derived for the reason every other
	// duration here is: the tick rate is an operator's flag.
	regenDelayTicks    uint32
	regenIntervalTicks uint32
	hungerDrainTicks   uint32

	// mobTimings is every registered species' windup and recovery in the ticks Step
	// counts, derived from the configured rate for the reason every other duration here
	// is: a telegraph is four hundred milliseconds on a 5 Hz server and on a 60 Hz one,
	// or it is not a telegraph.
	//
	// **Per species rather than per simulation**, because the two numbers stopped being
	// the draugr's the moment a second row in mobRegistry carried different ones. Built
	// once by mobTimingsFor and read-only afterwards, so nothing about it needs mu —
	// but every mob's kind is registered, which is what makes the lookup always hit.
	mobTimings map[vnet.MobKind]mobTicks

	// spawnEvery is how many ticks apart the director's spawn passes are, and
	// mobDespawnTicks is how long a mob may go unwatched. Both derived from the rate,
	// for the reason above: a night refills at the same speed on every server.
	//
	// Only the *spawn* is on that interval. The director's two removals run every tick
	// — see spawn.go, where the split is argued.
	spawnEvery      uint32
	mobDespawnTicks uint32

	// threatDecayTicks is one second and threatForgetTicks is
	// ThreatForgetSeconds in authoritative ticks. Threat is stepped by the same clock
	// as the creature that owns it, never by a goroutine or wall time.
	threatDecayTicks  uint32
	threatForgetTicks uint32

	// corpseLifetimeTicks is CorpseLifetime in authoritative ticks. A corpse records
	// its absolute expiry tick on the tick it is created, which is the tick of the
	// killing blow; opening it never changes this.
	corpseLifetimeTicks uint64

	// attackCooldown is SwordCooldown in the ticks Step counts, so a blade recovers in
	// six hundred milliseconds whatever rate the server is run at.
	attackCooldown uint32
	// bowCooldownTicks is BowCooldown in authoritative ticks. Launchers choose this cadence
	// independently of the sword's.
	bowCooldownTicks     uint32
	sceptreCooldownTicks uint32

	// dropLifetime is DropLifetime expressed in the ticks Step counts, derived from the
	// tick rate for the same reason the physics timestep is.
	dropLifetime int

	arrowLifetimeTicks uint32
	orbLifetimeTicks   uint32
	arrowStuckTicks    uint32

	// hardness is handMiningTimes in the ticks Step counts, for the same reason again: the
	// table is written in seconds because a block should take the same time to break
	// whatever rate the server runs at. A block absent from it is not breakable.
	hardness map[world.Block]int

	log *slog.Logger

	mu      sync.Mutex
	players map[uint64]*Player

	// chatLimiters are keyed by the identity that survives a connection, not by the
	// Player allocated for one session. Keeping the bucket here is what makes a
	// reconnect resume the same allowance instead of manufacturing a fresh burst. A
	// bucket is removed once elapsed time has completely refilled it, because at that
	// point retaining it and creating a new full bucket are equivalent. See chat.go.
	chatLimiters map[identity.PlayerID]*chatLimiter
	chatNow      func() time.Time

	// pendingExperience is every mob award earned after its tap owner left the
	// simulation and not yet confirmed on that character's stored record. The key is
	// both the account identity and the folded character name: an account may own
	// several characters, while names are unique within one world.
	//
	// Values are absolute lifetime totals rather than deltas, so retrying a save can
	// never award twice. Join also reads this map before publishing a returning body,
	// which closes the race between an offline kill and an immediate reconnect.
	pendingExperience map[characterKey]ExperienceAward

	// parties are authoritative membership groups keyed by ids from the same source
	// that names every other entity. byName is the one live display-name lookup and
	// exists only for party Invite and Kick; names were accepted and made globally
	// unique by persistence before Join receives them.
	parties           map[uint64]*party
	partyMemberships  map[partyMemberKey]uint64
	byName            map[string]*Player
	partyInviteTicks  uint64
	partyOfflineTicks uint64
	currentTick       uint64

	// tickOfDay is where the world stands in its day, and it is always less than
	// DayLengthTicks. See clock.go for everything about it; the field is here because
	// this is the struct mu guards.
	tickOfDay uint32

	// worldTick is how many ticks this world has ever run, across every restart, and it
	// never wraps. worldTick % DayLengthTicks == tickOfDay at every instant — see
	// clock.go, which is the only file that writes either of them.
	//
	// Distinct from currentTick above, which is this process's own count and restarts at
	// zero with it: every deadline built from currentTick is a duration measured inside
	// one run, and this is world time that outlives the run.
	worldTick uint64

	// nextStormUnix is when the next storm falls due, as a Unix second, and zero means
	// unscheduled. A wall-clock value rather than a tick because the storm rides a real
	// week, which includes the days this server was switched off.
	nextStormUnix int64

	// weatherOverride replaces every player's weather while it is non-nil, and is nil in
	// the ordinary world.
	//
	// **The one thing about the sky that is state rather than a field.** Everything else
	// is world.WeatherAt, which is a pure function of the seed, the tick and a column —
	// so it can say "rain here, snow a kilometre north" and cannot say "a blizzard,
	// everywhere, starting now". The Fimbulvetr storm is exactly that second sentence:
	// it is scheduled against nextStormUnix above, announced by a StormWarning, and it
	// is the same weather for every player wherever they stand.
	//
	// A pointer rather than a value plus a flag, for the reason nextStormUnix needs no
	// boolean beside it: there is no legal WeatherState that means "no override", since
	// a present one carrying WeatherKindUnknown is a protocol error and a Clear at
	// intensity 0 is a perfectly ordinary sky somebody might genuinely be imposing.
	//
	// The Fimbulvetr controller sets it through BeginStorm and clears it through
	// CompleteStorm, under this lock like every other field here. It is read once per
	// tick by weatherAtLocked, and read there rather than per player so that a storm
	// cannot begin halfway down one tick's player list.
	weatherOverride *protocol.WeatherState

	// stormWarning is the current phase sent to somebody joining between broadcasts.
	// Its zero value means the ordinary world: no warning is owed.
	stormWarning protocol.StormWarning

	// drops is every item lying in the world. Keyed by identity like players, and for
	// the same reasons: a snapshot names entities by id, and a merge or a pickup has to
	// find one without scanning.
	drops map[uint64]*itemDrop

	// projectiles are transient authoritative entities. projectileOwners retains the
	// firing session object only until its last shot disappears, so a shot that lands
	// after Leave can still establish the stable character tap used by offline awards.
	// Neither collection is persisted.
	projectiles      map[uint64]*projectile
	projectileOwners map[uint64]*Player

	// mobs is every live creature, keyed by identity like players and drops and for the
	// same reasons: a snapshot names entities by id, and a hit has to find one without
	// scanning.
	//
	// Nothing here is persisted, deliberately: a restart loses whatever was hunting,
	// because a mob is a moment in a simulation rather than a change to the world. The
	// director puts them back where the players actually are, which is a better answer
	// than a file could give.
	mobs map[uint64]*mob

	// corpses are killed normal mobs, created on the tick the killing blow lands. Kept
	// separately from mobs so they cannot act, collide, acquire a target or count toward
	// spawn ceilings; the snapshot projection merges both collections back into entity-id
	// order.
	corpses map[uint64]*corpse

	// spawns is the director's random source: seeded from the world seed at
	// construction and advanced only here, under this lock, inside Step.
	//
	// **Guarded by mu like everything else in this block, and that is the whole of the
	// determinism claim.** A package-level rand is shared with every other goroutine in
	// the process, so the sequence one simulation sees would depend on what else was
	// running; a generator advanced outside the tick would depend on when it was asked.
	// Owned here and touched only on the tick goroutine, the same world and the same
	// sequence of ticks produce the same creatures in the same places — which is what
	// lets a test assert a spawn position exactly. See spawn.go.
	spawns *rand.Rand

	// loot is what a kill's yield is drawn from, on exactly the terms above: seeded from
	// the world seed at construction, guarded by mu, and advanced only inside Step.
	//
	// **Its own generator rather than a share of spawns, and the separation is the
	// point.** Both are seeded from the same world seed on different PCG streams, so
	// neither draws the other's numbers and neither consumes them: killing a creature
	// cannot shift where the dark puts the next one, and a busy night cannot shift what
	// the next kill leaves behind. See loot.go, where the stream constant is argued.
	loot *rand.Rand

	// worldSeed is the number this world is, kept rather than only spent on the two
	// generators above, because two of the answers below are properties of the *world*
	// rather than of the process: station.go derives a settlement's stations from it,
	// and a respawn with no tent asks [world.NearestSettlement] where the nearest
	// village stands.
	//
	// **Still not a licence to generate terrain**: the simulation does not call
	// world.Generate, cannot, and reads chunks through a seam that carries no seed. Both
	// lattice queries it reads with this — world.SettlementsNear and
	// world.NearestSettlement — are a handful of hashes over the seed that open no chunk
	// and no cache, which is what lets them answer on the tick without going anywhere
	// near that seam. See station.go.
	worldSeed int64

	// structures is every placed tent and forge, keyed by identity for the reason the
	// three maps above are: a snapshot names them by id, and a removal has to find one
	// without scanning. Unlike the three, nothing in the tick advances them — a
	// structure has no state that changes with time — so they are read here and written
	// only by placement, removal, collapse, the restore at startup, and the settlement
	// stations station.go derives from the seed.
	structures map[uint64]*structure

	// wards is which chunk columns are claimed, and by whom.
	//
	// Runestone claims are derived from [Sim.structures]. Settlement claims are a pure
	// function of worldSeed and the column, cached here on first query (including a
	// negative answer) because settlements never move within one world. Rebuilding after
	// a runestone change replaces only the runestone half and preserves those settlement
	// answers.
	//
	// Nothing here is persisted: both halves are functions of state the server already
	// owns, and writing either down would create a second answer to keep in step.
	wards map[world.Column]wardClaim

	// wardsRevision changes whenever the runestone half of wards is rebuilt. Settlement
	// claims are pure in worldSeed and never spend it. Sessions compare this value on the
	// worker that forwards their per-tick snapshot, so a stone raised or removed while a
	// player stands still still replaces that player's WardsNearby list.
	wardsRevision uint64

	// residents is every person a settlement holds, keyed by identity for the reason
	// every other collection here is: a snapshot names them by id, and an interaction has
	// to find one without scanning.
	//
	// **Deliberately not [Sim.mobs], and that is the design rather than a filing
	// decision.** Combat, the projectiles, the spawn director and the corpse maker all
	// read `mobs`; a resident is invulnerable, unlootable, un-aggroable and never
	// despawned because it is in none of the collections those walk. See resident.go.
	//
	// Nothing here is persisted and nothing is ever removed: a resident is derived from
	// the seed like a world-owned station, so a restart re-derives the same people with
	// the same ids the first time anybody looks at their chunk again.
	residents map[uint64]*resident

	// structuresDirty says the camp has changed since it was last written down.
	//
	// The chunk cache's dirty flag, for a store that rewrites one file rather than many:
	// placement, removal and collapse set it, and the autosave loop and the shutdown
	// flush clear it through Sim.TakeDirtyStructures. A flag rather than a write on the
	// placing goroutine because nothing under this lock may touch a disk, and a flag
	// rather than an unconditional periodic save because a world nobody is building in
	// should cost no I/O at all.
	structuresDirty bool

	// chunkRegenerator and resendChunk are the two package seams the Fimbulvetr pass
	// crosses. The cache puts terrain back; the session registry forgets the old view.
	// Kept as interfaces/functions because game must not import session and because the
	// bounded pass is simulation state guarded by mu, not a connection concern.
	chunkRegenerator ChunkRegenerator
	resendChunk      func(world.Coord) int
	regeneration     []chunkRegenerationPass

	waterWorld    WaterWorld
	pendingWater  map[waterVoxel]uint64
	unstableWater chan unstableWaterBatch
	// waterScanCarry is the tail of a composition scan a tick had no budget for.
	// See [WaterScansPerTick]: a scan is spread over ticks rather than taken whole.
	waterScanCarry unstableWaterBatch
	// waterDue is pendingWater in the order it will be examined. See [waterDueQueue].
	waterDue waterDueQueue

	// byIdentity is the live player behind each identity, and it exists for exactly one
	// question: what entity id does this structure's owner hold *right now*.
	//
	// A second index over the same players rather than a scan, because the answer is
	// needed once per structure per tick and the alternative is O(structures × players)
	// inside the lock Step holds for the whole tick. Maintained in Join and Leave beside
	// the players map, which is what keeps the two from disagreeing: an identity is in
	// here exactly while its player is in there.
	//
	// At most one entry per identity, and session.Identities is what makes that true
	// upstream — one live session to an identity, with the claim released after
	// sim.Leave. Join refuses a second one anyway rather than overwriting, because an
	// overwrite would leave the displaced player's structures resolving to a session
	// that had already gone.
	byIdentity map[identity.PlayerID]*Player

	// minersByPos is the reverse edge from one edited voxel to the only players
	// whose mining state that edit can invalidate. It is updated under mu together
	// with Player.mining, so edits never scan every connected player while holding
	// the lock the tick needs.
	minersByPos map[[3]int32]map[*Player]struct{}
}

// NewSim returns a simulation whose timestep matches tickRate hertz.
//
// mintEntityID is the identity source shared with the sessions; see the field.
//
// **worldSeed is here to derive from and to answer questions about the world, not to
// generate anything.** The simulation still knows nothing about terrain: it does not call
// world.Generate, it cannot, and the seam it reads chunks through has no seed on it. What
// the number buys is that the spawn director's choices and a kill's yield are properties
// of the *world* rather than of the process — two runs of the same world, given the same
// ticks, place the same creatures in the same places and roll the same corpse entries —
// and, since #456, that a village smithy's forge is the same forge with the same id on
// every server that runs this world without a byte of it being written down.
//
// It is also what a respawn hands [world.NearestSettlement], which is a pure function of
// the seed and a column: it says where a village *is* without building one, and the
// voxels it names are still read through the terrain seam like every other voxel here.
// Each generator gets its own PCG stream off this one seed, so neither is a function of
// what the other has drawn. Any value is accepted, including zero: a seed is a starting
// point and there is no such thing as a wrong one.
func NewSim(tickRate, viewDistance uint8, worldSeed int64, terrain Terrain, editor Editor, mintEntityID func() uint64, log *slog.Logger) (*Sim, error) {
	if tickRate < 1 {
		return nil, fmt.Errorf("game: tick rate must be at least 1, got %d", tickRate)
	}
	if terrain == nil {
		return nil, errors.New("game: terrain must not be nil")
	}
	if editor == nil {
		// Refused rather than tolerated with an "edits are disabled" mode. A simulation
		// that silently drops every edit looks exactly like one whose reach check is wrong,
		// and there is no configuration in which a server should not be able to change its
		// own world.
		return nil, errors.New("game: editor must not be nil")
	}
	if mintEntityID == nil {
		// Refused rather than replaced with a counter of our own, which is the whole
		// point of the field: a simulation that mints its own ids would eventually name
		// a drop with a live player's identity.
		return nil, errors.New("game: mintEntityID must not be nil")
	}
	if log == nil {
		return nil, errors.New("game: logger must not be nil")
	}

	return &Sim{
		dt:                 1 / float64(tickRate),
		viewDistance:       int32(viewDistance),
		idleLimit:          idleLimitTicks(tickRate),
		terrain:            terrain,
		editor:             editor,
		mintEntityID:       mintEntityID,
		dropLifetime:       dropLifetimeTicks(tickRate),
		arrowLifetimeTicks: ticksFor(ArrowLifetime, tickRate),
		orbLifetimeTicks:   ticksFor(OrbLifetime, tickRate),
		arrowStuckTicks:    ticksFor(ArrowStuckTicks, tickRate),
		hardness:           handMiningTicksFor(tickRate),
		deathTicks:         deathDurationTicks(tickRate),
		protectionTicks:    respawnProtectionTicks(tickRate),
		regenDelayTicks:    ticksFor(HealthRegenDelay, tickRate),

		regenIntervalTicks:   ticksFor(HealthRegenInterval, tickRate),
		hungerDrainTicks:     ticksFor(HungerDrainInterval, tickRate),
		mobTimings:           mobTimingsFor(tickRate),
		spawnEvery:           ticksFor(SpawnDirectorInterval, tickRate),
		mobDespawnTicks:      ticksFor(MobDespawnGrace, tickRate),
		threatDecayTicks:     uint32(tickRate),
		threatForgetTicks:    uint32(ThreatForgetSeconds) * uint32(tickRate),
		corpseLifetimeTicks:  uint64(ticksFor(CorpseLifetime, tickRate)),
		spawns:               newSpawnRNG(worldSeed),
		loot:                 newLootRNG(worldSeed),
		worldSeed:            worldSeed,
		attackCooldown:       ticksFor(SwordCooldown, tickRate),
		bowCooldownTicks:     ticksFor(BowCooldown, tickRate),
		sceptreCooldownTicks: ticksFor(SceptreCooldown, tickRate),
		log:                  log,
		players:              make(map[uint64]*Player),
		chatLimiters:         make(map[identity.PlayerID]*chatLimiter),
		chatNow:              SystemClock{}.Now,
		pendingExperience:    make(map[characterKey]ExperienceAward),
		parties:              make(map[uint64]*party),
		partyMemberships:     make(map[partyMemberKey]uint64),
		byName:               make(map[string]*Player),
		partyInviteTicks:     uint64(ticksFor(PartyInviteTTL, tickRate)),
		partyOfflineTicks:    uint64(ticksFor(PartyOfflineGrace, tickRate)),
		drops:                make(map[uint64]*itemDrop),
		projectiles:          make(map[uint64]*projectile),
		projectileOwners:     make(map[uint64]*Player),
		mobs:                 make(map[uint64]*mob),
		corpses:              make(map[uint64]*corpse),
		structures:           make(map[uint64]*structure),
		residents:            make(map[uint64]*resident),
		byIdentity:           make(map[identity.PlayerID]*Player),
		minersByPos:          make(map[[3]int32]map[*Player]struct{}),
		pendingWater:         make(map[waterVoxel]uint64),
		unstableWater:        make(chan unstableWaterBatch, waterScanQueueDepth),
	}, nil
}

// idleLimitTicks is how many ticks a player keeps moving on an intent their client
// has stopped refreshing: half a second, whatever the tick rate, and never zero.
func idleLimitTicks(tickRate uint8) int {
	return max(int(tickRate)/2, 1)
}

// PlayerState is a copy of one player's authoritative state.
//
// A copy rather than a view: the live state is guarded by the simulation's lock, and
// handing out a pointer to it would be handing out a data race.
type PlayerState struct {
	EntityID uint64
	Pos      [3]float32
	Vel      [3]float32
	Yaw      float32
	OnGround bool
}

// intent is one tick of movement input, after the simulation has accepted it.
//
// Unexported, and constructed only by acceptIntent, which is what makes it a
// *sanitised* value: every field is finite, the movement axes describe a vector of
// length at most 1, and the yaw is wrapped. There is no way to hold one that says
// otherwise, so nothing downstream re-checks.
type intent struct {
	moveX float64
	moveZ float64
	yaw   float64

	// pitch is where the player is looking vertically, in radians, positive upwards.
	// Movement ignores it — the axes are applied in the horizontal plane — and a swing
	// does not, which is why the simulation keeps it now that it has something to aim
	// at. Wrapped and then clamped to straight up and straight down, so no finite value
	// a client can send produces a direction that is upside down.
	pitch float64

	jump bool
}

// Player is one connected player: the state the simulation owns, and the seam a
// session hands it intent through.
//
// Every exported method takes the simulation's lock, so the session's read goroutine
// and the tick goroutine can both hold one of these without knowing about each
// other.
type Player struct {
	sim      *Sim
	entityID uint64
	// characterID is the server-minted identity of the stored character this session
	// is playing. Unlike entityID it survives reconnects, and unlike playerID it does
	// not identify another character owned by the same account.
	characterID uint64

	// playerID is who this player *is*, across connections; entityID is what names
	// them in this one. Two identifiers rather than one because they answer different
	// questions and change at different rates: a snapshot addresses an entity, and
	// what a player carries back from a previous session is addressed by an identity.
	// Nothing about snapshots or indexes reads this — it is here so that the thing
	// being saved knows what it is being saved as.
	playerID identity.PlayerID

	deliver         func(frame []byte) bool
	deliverSnapshot func(frame []byte, center world.Column) bool
	chunks          *chunkFeed
	mineReady       chan MiningCompletion
	inventory       inventory

	// appearance is what this player looks like: the character's own, handed in at Join
	// and never changed afterwards. **It is read from the stored character and from
	// nothing a client said**, which is the whole of the rule schemas/player.fbs states
	// — a face is chosen once, when the character is created, and a session that could
	// restate it could arrive as somebody else.
	//
	// Immutable after Join, so it needs no lock: every reader is the tick, and there is
	// no writer at all.
	appearance protocol.Appearance
	// name has the same lifetime and authority as appearance: the stored character's
	// display text, never a value restated by this live client. It rides in the same
	// once-per-view description because paying for a string in every snapshot would be
	// indefensible.
	name string

	// Everything below is guarded by sim.mu.
	partyID uint64
	invite  *partyInvite

	pos      [3]float64
	vel      [3]float64
	yaw      float64
	onGround bool

	// chunk is the chunk pos falls in. Kept beside the position rather than derived
	// on demand because streaming is driven by the *change*, and comparing against
	// the value already published is how the tick notices one.
	chunk world.Coord

	// weather is what the sky is doing over this player, recomputed once per tick in
	// stepWorld at the position that tick produced and carried by that tick's snapshot.
	//
	// Kept on the player rather than computed inside the per-viewer snapshot loop
	// because the two loops are not the same shape: the sample belongs to where *this*
	// player is standing, and the snapshot loop is already walking every other player
	// from this one's point of view. Storing it also makes it one sample per player per
	// tick by construction, which is the cost the design commits to.
	//
	// Wire-ready rather than a world.WeatherKind, so the vocabulary is converted once at
	// the sample and not once per snapshot. The zero value never reaches a snapshot: the
	// tick loop writes this for every connected player before any snapshot is built.
	weather protocol.WeatherState

	current   intent
	idleTicks int
	haveTick  bool
	lastTick  uint32

	// leaving is the server-owned linger state. The body remains a live
	// simulation entity — gravity, damage, snapshots and interaction all continue —
	// but no client intent may change it. It is guarded by sim.mu with the intents it
	// disables. A live session may clear it through CancelLeaving; a disconnected one
	// has no route to make that request and stays inert until removal.
	leaving bool

	// The life the server owns. See vitals.go and progression.go, which are where every
	// transition between these values happens; nothing outside them writes them.
	//
	// spawn is the position Join was given, kept as the provisional respawn point. Kept
	// rather than recomputed because the world helper that produced it can generate
	// terrain, and the tick may not.
	health       uint16
	lifeState    vnet.LifeState
	respawnTicks uint32

	// worn is the combat summary derived from the four equipment slots. It is not
	// persisted: the slots are, and Join rebuilds this value from them. Guarded by
	// sim.mu and written only by refreshWornLocked while inventory.mu is held too.
	// That lets the tick read armour and threat without taking the inventory lock — a
	// lock a session goroutine may hold across an in-memory world write.
	worn struct {
		armour uint16
		threat uint16
	}
	// Cached so the tick never takes inventory.mu to decide whether a block applies.
	wornShield struct {
		fraction uint16
		slot     uint8
	}
	blocking bool

	// sinceDamageTicks is how long since the last landed hit, regenTicks how far
	// through the current point of regeneration, and hungerTicks how far through the
	// next point of ordinary hunger drain. regenPoints counts the health already bought
	// by the current hunger point. All stop at small thresholds rather than growing
	// without bound over a long session.
	//
	// **None is persisted, and that is the same rule as the respawn countdown.** A
	// record describes a living player and carries nothing that only means something
	// inside one session — see the note at the top of vitals.go. A player who leaves
	// hurt comes back hurt and waits the full delay again, which is the honest answer:
	// this server did not watch them for the time they were away.
	sinceDamageTicks uint32
	regenTicks       uint32
	hungerTicks      uint32
	regenPoints      uint16
	hunger           uint16
	experience       uint32

	// learnedMounts is the character's permanent mount set. Unlike cast and mounted
	// state it outlives this session, so Join restores it and Record writes it back.
	// Guarded by sim.mu with the rest of the authoritative life.
	learnedMounts LearnedMounts

	protectionTicks uint32
	penaltyApplied  bool
	spawn           [3]float64

	// inventoryDirty records that a pickup changed the slots and the client has not
	// been told yet. Guarded by sim.mu rather than by the inventory's own lock: it is
	// the tick that sets it and the tick that clears it, and the inventory lock is only
	// ever taken here without waiting. See offerInventoryLocked.
	inventoryDirty bool

	// One open normal-mob container per session. lootDirty is the complete LootState
	// still owed after an open or accepted take; lootClosures are LootClosed frames
	// still owed after switching, emptying or expiry. Each is cleared only after its
	// own non-blocking delivery succeeds.
	openLootID   uint64
	lootDirty    bool
	lootClosures []uint64

	// One open stall per session, and the same three-part shape for the same three
	// reasons. openVendorID is the resident this session is trading with and is zero
	// when none is; vendorRevision is the list the client is looking at, starting at 1
	// on open and bumped by every accepted trade; vendorDirty is the complete
	// VendorState still owed; vendorClosures are VendorClosed frames still owed after
	// switching stalls, walking away, dying or the resident leaving view.
	//
	// **Unlike loot, none of this lives on the thing being opened.** A corpse holds a
	// container per looter; a vendor holds nothing at all, because stock is unlimited by
	// contract and the price list is a function of the role. So the whole session is
	// here, which is also what makes two players at one smith independent.
	openVendorID   uint64
	vendorRevision uint32
	vendorDirty    bool
	vendorClosures []uint64

	// The newest landed monster blows this session has not been told about yet. Unlike a
	// snapshot, an event is not superseded by the next tick, so a full outbound queue
	// leaves these pending until offerMobHitsLocked gets one through. The bounded queue
	// drops its oldest presentation event under prolonged congestion. Guarded by sim.mu.
	pendingMobHits []protocol.MobHit

	// Open and take have independent client ordering for the same reason attack and
	// mining do: activity on one message must not silence a different intent stream.
	haveLootOpenTick bool
	lastLootOpenTick uint32
	haveLootTakeTick bool
	lastLootTakeTick uint32
	// Take-all is its own stream for the same reason: pressing F is not a click on an
	// entry, and one must never silence the other in the frame they share.
	haveLootTakeAllTick bool
	lastLootTakeAllTick uint32
	// And a trade is its own stream beside them, for the reason take-all is beside take:
	// buying at a stall is not looting a body, and one must never silence the other.
	haveTradeTick bool
	lastTradeTick uint32

	// described is every entity this session has been told the appearance of, against
	// the tick it was last visible on. **This player is the viewer here, not the
	// subject**: it answers "has this session already been sent a PlayerAppearance for
	// that entity, and is that entity still in view".
	//
	// Per session rather than per character, and that is what makes a reconnect work
	// without a rule of its own: a Player is built by Join, so a new session has told
	// nobody anything and starts empty.
	//
	// The tick value is what bounds it. Every entry is refreshed while its entity stays
	// visible and dropped when it does not, so the map is the size of what is in view
	// rather than of everything that ever was — and an entity that leaves and comes back
	// is described again, which is exactly what the client needs: a snapshot is the
	// complete existence set, so an entity that stopped appearing in one was despawned,
	// and its appearance went with it.
	//
	// Guarded by sim.mu, like everything else below the line above.
	described map[uint64]uint64

	// The combat seam. A swing is an event rather than a held control, so it arrives as
	// a one-shot the tick consumes exactly once — see combat.go. Its ordering guard is
	// its own for the reason mining's is: three intents on three messages at three
	// cadences, and one shared counter would let a fast stream of any of them silence
	// the others.
	haveAttackTick bool
	lastAttackTick uint32
	pendingSwing   *pendingSwing
	attackCooldown uint32

	// Mining intent has its own ordering and idle window. It is refreshed by a
	// different message from movement and neither client's cadence may keep the
	// other alive. The state and flags are guarded by sim.mu; mineReady is the
	// non-blocking handoff from Step to the session's off-tick worker.
	mining         *miningState
	haveMineTick   bool
	lastMineTick   uint32
	mineSerial     uint64
	mineCompleting bool
	mineReset      *miningReset
}

// Join admits a player at spawn and returns the handle its session uses.
//
// playerID is the identity the session's handshake resolved, and it is passed in
// rather than minted here for the reason entityID is: the simulation is handed the
// names of things, it does not decide them. Resolving one reads the player store,
// which must never happen under this package's lock.
//
// **resume is the life this identity left behind, or nil for one that has none.** Nil
// rather than a second constructor, the same shape a nil *world.Store is the ephemeral
// world: there is one admission path, and "this player is new" is a value it takes
// rather than a branch every caller repeats. A resumed player is placed at their stored
// position, facing where they faced, with the health, hunger, experience and pack they
// had; a new one gets full health and hunger, zero experience,
// [newStarterInventory] and spawn.
//
// **spawn is the join spawn either way**, because it is also the provisional respawn
// point — where a player with no tent comes back to. Restoring a position is not moving
// somebody's respawn to wherever they happened to log out.
//
// **name and appearance are the chosen character's, read from the store by the
// handshake.** The appearance is checked here for the reason resume is checked here:
// this is the boundary a stored
// value crosses into the simulation, and from here it goes out on the wire in a
// PlayerAppearance every viewer is required to refuse if it breaks the contract. A
// caller that hands in one nobody validated gets an error rather than a session that
// disconnects everybody who can see it.
//
// deliver is how a snapshot reaches that session, and it must not block — Step calls
// it under the simulation's lock. It returns false for a frame it dropped.
func (s *Sim) Join(entityID uint64, playerID identity.PlayerID, name string, spawn [3]float32, appearance protocol.Appearance, resume *Life, deliver func(frame []byte) bool) (*Player, error) {
	return s.JoinCharacter(entityID, playerID, entityID, name, spawn, appearance, resume, deliver)
}

// JoinCharacter admits one stored character under a new live entity id. CharacterID
// is the stable identity minted by persistence; session is the production caller.
// Join remains a compact helper for game tests whose entity ids stand in for it.
func (s *Sim) JoinCharacter(entityID uint64, playerID identity.PlayerID, characterID uint64, name string, spawn [3]float32, appearance protocol.Appearance, resume *Life, deliver func(frame []byte) bool) (*Player, error) {
	return s.joinCharacter(entityID, playerID, characterID, name, spawn, appearance, resume, deliver,
		func(frame []byte, _ world.Column) bool { return deliver(frame) })
}

// JoinCharacterWithSnapshotDelivery admits one stored character while preserving the
// authoritative column beside each snapshot. Session uses that column to hold a snapshot
// until its stream has reached the same centre; game-only callers use JoinCharacter and
// keep the single delivery seam.
func (s *Sim) JoinCharacterWithSnapshotDelivery(
	entityID uint64,
	playerID identity.PlayerID,
	characterID uint64,
	name string,
	spawn [3]float32,
	appearance protocol.Appearance,
	resume *Life,
	deliver func(frame []byte) bool,
	deliverSnapshot func(frame []byte, center world.Column) bool,
) (*Player, error) {
	if deliverSnapshot == nil {
		return nil, errors.New("game: snapshot delivery must not be nil")
	}
	return s.joinCharacter(entityID, playerID, characterID, name, spawn, appearance, resume, deliver, deliverSnapshot)
}

func (s *Sim) joinCharacter(
	entityID uint64,
	playerID identity.PlayerID,
	characterID uint64,
	name string,
	spawn [3]float32,
	appearance protocol.Appearance,
	resume *Life,
	deliver func(frame []byte) bool,
	deliverSnapshot func(frame []byte, center world.Column) bool,
) (*Player, error) {
	if characterID == 0 {
		return nil, errors.New("game: character id 0 is reserved")
	}
	if deliver == nil {
		return nil, errors.New("game: deliver must not be nil")
	}
	if err := appearance.Validate(); err != nil {
		return nil, fmt.Errorf("game: the character's appearance cannot be worn: %w", err)
	}

	if playerID == (identity.PlayerID{}) {
		// Unreachable through session, which resolves an identity before it joins.
		// Refused rather than accepted anyway: the zero id is the digest of nothing and
		// names nobody, so every player admitted without one would be the same player
		// the moment anything keyed a record on it.
		return nil, errors.New("game: a player must join under a resolved identity")
	}
	for axis, value := range spawn {
		if err := requireFinite(fmt.Sprintf("spawn axis %d", axis), value); err != nil {
			return nil, fmt.Errorf("game: %w", err)
		}
	}

	// Asked again here although the session has already asked it, and the repetition is
	// the point: this is the boundary a stored life crosses into the simulation, and the
	// only thing between a file on disk and a player's position is somebody having
	// checked. A caller that forgets gets an error rather than a NaN in the integrator.
	if resume != nil {
		if err := resume.Validate(); err != nil {
			return nil, fmt.Errorf("game: the stored life cannot be restored: %w", err)
		}
	}

	joinSpawn := [3]float64{float64(spawn[0]), float64(spawn[1]), float64(spawn[2])}
	pos, yaw, health, hunger, experience, silver, learnedMounts, slots := joinSpawn, 0.0, uint16(PlayerMaxHealth), uint16(PlayerMaxHunger), uint32(0), uint32(0), LearnedMounts(0), starterSlots()
	if resume != nil {
		pos, yaw, health, hunger, experience, silver, learnedMounts, slots = resume.Pos, resume.Yaw, resume.Health, resume.Hunger, resume.Experience, resume.Silver, resume.LearnedMounts, restoredSlots(resume.Slots)
	}

	p := &Player{
		sim:         s,
		entityID:    entityID,
		characterID: characterID,
		playerID:    playerID,
		name:        name,
		appearance:  appearance,
		// Empty, and that is the reconnect rule rather than an initialisation detail: a
		// new session has described nobody to this client, so everything it can see is
		// described again — including the players it was already looking at a moment ago.
		described:       make(map[uint64]uint64),
		deliver:         deliver,
		deliverSnapshot: deliverSnapshot,
		chunks:          newChunkFeed(),
		mineReady:       make(chan MiningCompletion, 1),

		// A composite literal for the reason newStarterInventory returns one: the struct
		// carries a mutex, which `go vet`'s copylocks check refuses to see assigned from
		// a variable.
		inventory: inventory{slots: slots, silver: silver},
		pos:       pos,
		spawn:     joinSpawn,
		yaw:       yaw,
		// The intent carries the yaw too, because step reads p.current.yaw and writes it
		// back over p.yaw on the first tick. Without this a restored player would snap to
		// facing north before their client's first input arrived.
		current:       intent{yaw: yaw},
		health:        health,
		hunger:        hunger,
		experience:    experience,
		learnedMounts: learnedMounts,
		lifeState:     vnet.LifeStateAlive,
		// Not on the ground until a tick says so — for a restored player exactly as for a
		// new one. The spawn sits a couple of blocks above the surface
		// (world.SpawnClearance) and a stored position was written wherever the player
		// stood, so both settle by falling, which is the same code path as every other
		// landing rather than a special one.
		onGround: false,
	}
	p.chunk = chunkAt(p.pos)
	// Published before the player is reachable by a tick, so the session's streaming
	// goroutine finds the spawn view waiting for it the moment it starts. The feed
	// keeps the coordinate rather than the wake-up, so nothing is lost by publishing
	// before anyone is listening.
	p.chunks.publish(p.chunk)

	s.mu.Lock()
	defer s.mu.Unlock()

	if _, taken := s.players[entityID]; taken {
		// Unreachable through session.Registry, which mints each id once. Refused
		// rather than overwritten anyway: silently replacing a player would drop the
		// live session's handle on the floor and leave its snapshots going nowhere.
		return nil, fmt.Errorf("game: entity %d is already in the simulation", entityID)
	}
	if live, playing := s.byIdentity[playerID]; playing {
		// Unreachable through session.Identities, which admits one live session to an
		// identity and releases the claim only after Leave has returned. Refused for the
		// reason above and one more: byIdentity is what resolves a structure's owner to
		// an entity id, so an overwrite would point every one of the displaced player's
		// structures at a session that had already ended.
		return nil, fmt.Errorf("game: player %s is already in the simulation as entity %d", playerID.Short(), live.entityID)
	}
	if pending, earned := s.pendingExperience[characterKeyOf(playerID, name)]; earned && pending.Experience > p.experience {
		p.awardExperienceLocked(pending.Experience - p.experience)
	}
	p.inventory.mu.Lock()
	p.refreshWornLocked()
	p.inventory.mu.Unlock()
	s.players[entityID] = p
	s.byIdentity[playerID] = p
	s.byName[foldPlayerName(name)] = p
	s.rebindPartyMemberLocked(p)
	return p, nil
}

// Leave removes a player from the simulation.
//
// **When it returns, no tick is part-way through delivering a frame to that player's
// session, and no later tick will start one.** It takes the lock Step holds for a
// whole tick, so the two are mutually exclusive by construction. That is the
// guarantee session teardown is built on: an outbound channel may only be closed once
// nothing can still send to it, because a send on a closed channel is a panic in a
// goroutine and takes the process with it.
func (s *Sim) Leave(p *Player) {
	if p == nil {
		return
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	// Only the player that was handed in, never whatever currently holds that id: a
	// rejoin that already replaced it must not be evicted by the old session's
	// cleanup. Same reasoning as world.Cache.forget.
	if held, ok := s.players[p.entityID]; ok && held == p {
		s.removeAllThreatFor(p.entityID)
		// A mob tap is keyed independently of this session object, but its offline
		// baseline must include everything this session earned before it left.
		s.rememberTapExperienceLocked(characterKeyOf(p.playerID, p.name), p.experience)
		s.markPartyMemberOfflineLocked(p)
		s.clearInvitesFromLocked(p.entityID)
		p.setMiningLocked(nil)
		p.mineCompleting = false
		p.mineReset = nil
		p.blocking = false
		delete(s.players, p.entityID)
		// The same "only the player that was handed in" guard, applied to the second
		// index: a rejoin that already claimed this identity must keep it. The two maps
		// are written together here and in Join, which is what makes "an identity is in
		// one exactly while its player is in the other" a property rather than a hope.
		if current, indexed := s.byIdentity[p.playerID]; indexed && current == p {
			delete(s.byIdentity, p.playerID)
		}
		folded := foldPlayerName(p.name)
		if current, indexed := s.byName[folded]; indexed && current == p {
			delete(s.byName, folded)
		}
	}
}

// Count is how many players the simulation holds. A connected session that has not
// completed its handshake is not one of them.
func (s *Sim) Count() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.players)
}

// EntityID is the identity the server assigned this player.
func (p *Player) EntityID() uint64 { return p.entityID }

// PlayerID is who this player is across connections, as their handshake resolved it.
func (p *Player) PlayerID() identity.PlayerID { return p.playerID }

// InventoryState is a copy of everything this player carries.
//
// The copy is the value a session sends whole on join and after a change. No caller
// receives the live slots, so there is no route around the locked inventory operations.
func (p *Player) InventoryState() protocol.InventoryState { return p.inventory.state() }

// BeginLeaving makes this body inert without removing it from the simulation.
//
// Idempotent because every session ending converges here: a polite LeaveRequest, an
// idle deadline, a dead socket and a writer failure can notice the same end from
// different paths. The transition clears every queued or held action under the tick's
// lock, so once it returns no later tick can apply intent accepted before the leave.
func (p *Player) BeginLeaving() {
	if p == nil {
		return
	}

	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	if p.leaving {
		return
	}

	p.leaving = true
	p.current = intent{yaw: p.yaw}
	p.setMiningLocked(nil)
	p.mineReset = nil
	p.mineCompleting = false
	p.pendingSwing = nil
	p.blocking = false
}

// CancelLeaving makes future client intent live again when this body is still in its
// server-owned linger. It restores no cleared action: only input received after the
// authoritative acknowledgement may act, so pressing cancel cannot replay movement,
// mining or combat intent held before the leave began.
//
// The bool is the decision the session puts on the wire. False leaves every field
// untouched, which is what a refused cancellation promises.
func (p *Player) CancelLeaving() bool {
	if p == nil {
		return false
	}

	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	if !p.leaving {
		return false
	}

	p.leaving = false
	return true
}

// cannotActLocked distinguishes a corpse from a lingering live body while giving every
// request one gate. The caller holds sim.mu.
func (p *Player) cannotActLocked() error {
	if !p.alive() {
		return errors.New("the player is dead")
	}
	if p.leaving {
		return errors.New("the player is leaving")
	}
	return nil
}

// State is a copy of this player's authoritative state.
func (p *Player) State() PlayerState {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	return p.stateLocked()
}

func (p *Player) stateLocked() PlayerState {
	return PlayerState{
		EntityID: p.entityID,
		Pos:      toWire(p.pos),
		Vel:      toWire(p.vel),
		Yaw:      float32(p.yaw),
		OnGround: p.onGround,
	}
}

// Submit hands one decoded PlayerInput to the simulation.
//
// It returns an error for input the simulation refuses, so the caller can log it and
// carry on reading: the frame was well formed and the stream is still trustworthy —
// only a value is wrong. Two refusals, and each is a refusal rather than something to
// repair:
//
//   - **Non-finite.** NaN and ±Inf are malformed rather than extreme, and a range
//     clamp cannot catch them: NaN compares false against every bound, so the usual
//     check passes it through untouched and it then propagates through the integrator
//     into a position that stays NaN for the rest of the session.
//     schemas/player.fbs states this as a decoder invariant; this is where it is
//     enforced, and it runs before any physics.
//   - **Stale.** client_tick is the client's own counter, never trusted as a clock
//     and only ever read as an order. An input that is not newer than the newest one
//     already accepted is a duplicate or a replay.
//
// Out-of-range *axes* are not refused, because the contract says they are clamped —
// "a client that sends out-of-range values gains nothing". See acceptIntent.
func (p *Player) Submit(in protocol.PlayerInput) error {
	// Before the lock, and before anything reads a number: a malformed value must
	// never reach the state the tick integrates.
	if err := checkInputFinite(in); err != nil {
		return err
	}

	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if err := p.cannotActLocked(); err != nil {
		// A refusal, not a protocol error: the frame was well formed and the client is
		// entitled to keep sending while it waits for the respawn it has been told about.
		// The session logs it at debug and sends nothing, exactly as it does for a stale
		// tick — see handlePostHandshake.
		return err
	}

	if p.haveTick && !newerTick(in.ClientTick, p.lastTick) {
		return fmt.Errorf("stale client tick %d; the newest accepted is %d", in.ClientTick, p.lastTick)
	}

	p.haveTick, p.lastTick = true, in.ClientTick
	p.current = acceptIntent(in)
	p.idleTicks = 0
	return nil
}

// NextChunk blocks until the simulation puts the player in a chunk the caller has not
// been told about, and returns it. It returns ctx's error once the session ends.
//
// The consumer side of the seam chunkFeed describes. The first call returns
// immediately with the spawn chunk Join published.
func (p *Player) NextChunk(ctx context.Context) (world.Coord, error) {
	return p.chunks.next(ctx)
}

// WakeStreaming asks for one more view diff at the chunk the player is already in,
// without waiting for them to leave it.
//
// **The gap it exists to close.** NextChunk returns on a chunk *crossing*, so a player
// who stops moving stops producing view diffs entirely — and a view diff is the only
// thing that re-sends a chunk the session has stopped holding. Everything that repairs a
// session by forgetting a chunk (session.View.Forget) therefore needs a way to say "now
// would be a good time", or the repair lands whenever the player next walks somewhere.
//
// It rings the doorbell the tick loop already rings and changes nothing else: the centre
// stays the coordinate the simulation last published, which is what makes the resulting
// diff a diff at the *current* centre rather than at one the caller chose. Rings
// collapse, so a hundred callers cost one diff — and one diff is all any number of them
// needs, because what each has to say is recorded in the view before it calls.
//
// Callable from any goroutine, and it never blocks. The simulation is not consulted and
// nothing about the player changes: this is a request for a *send*, not for a decision,
// which is why it is the one thing a session may ask of a Player without going through
// intent.
func (p *Player) WakeStreaming() { p.chunks.ring() }

// Step advances the authoritative world by one tick. Normal-mob loot remains inside
// Sim.mu because the kill-to-corpse transition only mutates simulation-owned state;
// ordinary world drops retain their separate session-safe spawn path.
func (s *Sim) Step(tick uint64) []WaterChange {
	return s.stepWorld(tick)
}

// stepWorld advances every player, drop, creature, corpse and clock by one tick and
// delivers what every session can see.
//
// Runs on the tick goroutine, holding one lock for the whole tick — which is what
// makes Leave a guarantee rather than a hope.
//
// **Nothing under that lock may block.** The terrain is read with Peek, which never
// generates; the chunk feed is a non-blocking doorbell; a pickup takes the inventory
// lock only if it is free; and delivery is a non-blocking send that drops the frame
// when a session's queue is full. Dropping is right rather than merely convenient: a
// snapshot describes one tick and is worthless by the time a full queue drains, so
// waiting for room would stall every other player's tick in order to deliver something
// already stale. Inventory and loot-container states instead retain dirty flags and retry.
func (s *Sim) stepWorld(tick uint64) []WaterChange {
	s.mu.Lock()
	defer s.mu.Unlock()

	// The clock first, ahead of anything that could ever read it: time passes, and then
	// the world responds to the time it now is. Everything this tick produces — the
	// tick_of_day every snapshot below carries, and in time whatever asks IsNight —
	// therefore sees one value, and there is no ordering question about which half of a
	// tick ran before the day moved.
	s.advanceClockLocked()
	s.currentTick = tick
	s.advancePartyInvitesLocked(tick)
	s.expireCorpsesLocked(tick)
	s.advanceChunkRegenerationLocked()

	players := s.sortedPlayersLocked()

	// The world's own tick, read once for the whole tick and handed to every weather
	// sample below. Under this lock it cannot change between two players anyway; reading
	// it here is what says so, and it is what makes "the same sky for everyone" a
	// property of the code rather than of the lock's scope.
	worldTick := s.worldTick
	waterChanges := s.advanceWaterLocked(worldTick)

	// The fires, before anything that reads one. A campfire the rain has put out cooks
	// nothing and keeps nothing away for the whole of this tick, so the pass that decides
	// which are burning has to run ahead of the craft requests, the director and the
	// snapshot alike rather than somewhere in the middle of them.
	s.douseFiresLocked(worldTick)

	for _, p := range players {
		// Vitals first, and the order is load-bearing. A player who died on tick N has
		// their countdown decremented from N+1, so the three seconds are three seconds
		// rather than one tick short; and a respawn lands before the movement below, so
		// the tick that brings a player back is also the tick that starts them falling
		// to their spawn and publishes the chunk they arrived in.
		p.advanceVitalsLocked()
		p.step(s.dt, s.terrain)
		p.advanceMining(tick, s.terrain)

		if coord := chunkAt(p.pos); coord != p.chunk {
			p.chunk = coord
			// Only on a change. A repeated coordinate would make the streaming
			// goroutine re-diff a view that has not moved, twenty times a second, per
			// session; a crossing is the event that has anything to send.
			p.chunks.publish(coord)
		}

		// The sky, at the position this tick produced rather than the last one's — the
		// same rule the chunk crossing above and every mob's chase below follow. One
		// sample per player per tick, and none at all for a player nobody is simulating,
		// because this loop is the whole of what weather costs.
		p.weather = s.weatherAtLocked(worldTick, p.pos)
	}

	// The drops, in the order the world changed them: fall and age first, then combine
	// what ended up together, then hand over what somebody is standing on. Merging
	// before collecting is what makes a pile arrive as one insertion rather than as
	// several, and both run after the players have moved so a pickup uses the position
	// this tick produced rather than the last one's.
	drops := s.advanceDropsLocked(s.sortedDropsLocked())
	drops = s.mergeDropsLocked(drops)
	drops = s.collectDropsLocked(players, drops)

	// The attacks, after every player has moved and before any mob acts. Both halves of
	// that are the guarantee: a swing is judged against the positions this tick
	// produced, so network scheduling cannot choose an in-between one to be judged at,
	// and a draugr killed here cannot land an attack later in the same tick.
	//
	// **A blow that kills produces the corpse here, in this loop.** There is no countdown
	// between the two: damageMobLocked takes the creature out of Sim.mobs and rolls its
	// container on the spot, without involving the ground-drop path. Everything below —
	// the mobs, the director, the snapshot projection and offerLootLocked — therefore sees
	// the corpse on this same tick, which is what makes the body lootable in the snapshot
	// that first draws it going down.
	for _, p := range players {
		p.resolveAttackLocked()
	}

	// Projectiles use the positions produced above and land before a mob acts, exactly
	// like a swing. A lethal arrow therefore cannot leave its target one later attack in
	// the same tick merely because the damage arrived through a different weapon.
	projectiles := s.advanceProjectilesLocked(players)

	// The mobs, after the players have moved so a chase steers at the position this tick
	// produced rather than the last one's — and after the swings above. Nothing dies here
	// any more: a creature killed above is already a corpse and is already out of Sim.mobs,
	// so this steps the survivors and nothing else.
	mobs := s.advanceMobsLocked(players)

	// And the director last, after the creatures it manages have been advanced: what it
	// spawns is judged against the positions this tick produced, and what it removes it
	// removes knowing the target each mob chose in it. A population that changed is
	// re-read rather than patched, so the snapshot below is the world as this tick left
	// it — a mob removed here is gone from it, and one created here is in it.
	if s.directMobsLocked(tick, players, mobs) {
		mobs = s.sortedMobsLocked()
	}

	// And the residents, who are none of the above: nothing spawns, removes or damages
	// one, so this is a single pass that turns whoever has somebody standing near them.
	// After the players have moved, for the reason the mobs are — a head turns toward the
	// position this tick produced rather than the last one's — and after the director,
	// which has nothing to say about them.
	s.advanceResidentsLocked(players)

	states := make([]protocol.EntityState, len(players))
	for i, p := range players {
		states[i] = protocol.EntityState{
			EntityID: p.entityID,
			Pos:      toWire(p.pos),
			Vel:      toWire(p.vel),
			Yaw:      float32(p.yaw),
		}
	}
	// The structures, read rather than advanced: nothing about a tent changes with a
	// tick, so they are gathered here beside the entities that do move rather than in a
	// step of their own.
	structures := s.sortedStructuresLocked()

	dropped := dropStates(drops)
	projectedProjectiles := projectileStates(projectiles)
	projectedMobs := s.mobSnapshotsLocked(mobs)
	standing := s.structureStatesLocked(structures)

	// Ahead of the snapshots, deliberately. An inventory state is not superseded by the
	// next tick's and a snapshot is, so when a queue has room for one frame it should
	// be this one.
	for _, p := range players {
		p.offerInventoryLocked()
		p.offerLootLocked()
		p.offerVendorLocked()
		p.offerMobHitsLocked()
	}

	// One snapshot per session, carrying only the entities that session can see.
	// O(sessions × entities), knowingly: a spatial index is worth building when the
	// quadratic term matters and not one issue before.
	visible := make([]protocol.EntityState, 0, len(states))
	visibleDrops := make([]protocol.ItemDropState, 0, len(dropped))
	visibleMobs := make([]protocol.MobState, 0, len(projectedMobs))
	visibleLootCorpses := make([]uint64, 0)
	visibleStructures := make([]protocol.StructureState, 0, len(standing))
	visibleProjectiles := make([]protocol.ProjectileState, 0, len(projectedProjectiles))
	// **Filled from the same pass that fills `visible`, and that is what keeps the two
	// agreeing.** The contract says every id here names a player in the same snapshot's
	// entity vector, and a client refuses a frame where one does not; deriving it from a
	// second walk over `players` would be a second visibility decision to keep in step.
	// Nil until somebody in view is dead, so the ordinary tick allocates nothing and the
	// encoder writes no field at all.
	var visibleDead []uint64
	var visibleBlocking []uint64

	// At most one encoded appearance per player per tick, built the first time a viewer
	// turns out not to have been told about them and handed to every viewer after that.
	// It is Registry.BroadcastChunk's "one encode for every recipient", and it is what
	// makes asking this question inside the per-viewer loop cost nothing on the ticks
	// where nobody's view changed — which is almost all of them, because the answer is
	// only ever built when somebody has just come into somebody else's cube.
	faces := make([][]byte, len(players))

	// The same cache for the residents, on the same terms and for the same reason: at
	// most one encoded description per resident per tick, built the first time a viewer
	// turns out not to have been told about them. Keyed rather than indexed because the
	// residents are a map and there is no per-tick slice of them to index into — the
	// snapshot projection below is where they are put in order, and that order is by
	// entity id rather than by anything this loop could count.
	//
	// Nil until somebody walks into a village, so a world nobody has visited allocates
	// nothing at all.
	var residentFaces map[uint64][]byte

	for _, viewer := range players {
		visible = visible[:0]
		visibleDead = visibleDead[:0]
		visibleBlocking = visibleBlocking[:0]
		for i, p := range players {
			inView := withinView(viewer.chunk, p.chunk, s.viewDistance)
			if inView {
				visible = append(visible, states[i])

				// **The viewer's own death goes in this vector like everybody else's**, which
				// is the point of the field rather than an accident of the loop: a session is
				// inside its own view, so the body it is watching go down and the bodies
				// beside it are stated the same way and cannot drift apart. Its vitals still
				// carry the health and the countdown; those are per-recipient and this is not.
				if !p.alive() {
					visibleDead = append(visibleDead, p.entityID)
				}
				if p.blocking {
					visibleBlocking = append(visibleBlocking, p.entityID)
				}
			}

			// Party membership is the narrow exception to appearance visibility: the HUD
			// needs the existing name/level description for a consenting party-mate even
			// when their body is outside the streamed terrain cube. Their EntityState stays
			// view-bound; position and health use PartyMembers below.
			if !inView && !s.samePartyLocked(viewer, p) {
				continue
			}

			// **The appearance, once per entity per time it enters this viewer's cube**,
			// and ahead of the snapshot that first carries the entity so a client usually
			// has the face before it has anywhere to put it. Either order is legal —
			// schemas/player.fbs says so in as many words — and this is the cheap half of
			// making the placeholder rare.
			//
			// The viewer's own entity is in this loop like everybody else's: a session
			// recognises itself by ServerWelcome.entity_id, and its own appearance arrives
			// the same way as the rest.
			if _, told := viewer.described[p.entityID]; told {
				viewer.described[p.entityID] = tick
				continue
			}
			if faces[i] == nil {
				// The tick never waits for a session operation holding the inventory.
				// Leaving the description uncached retries it next tick, exactly as a
				// dropped delivery below does.
				if !p.inventory.mu.TryLock() {
					continue
				}
				wornHead, wornChest, wornLegs, wornOffHand := p.inventory.wornItemsLocked()
				p.inventory.mu.Unlock()
				faces[i] = protocol.EncodePlayerAppearance(protocol.PlayerAppearance{
					EntityID:      p.entityID,
					Appearance:    p.appearance,
					Name:          p.name,
					Level:         levelFor(p.experience),
					WornHead:      wornHead,
					WornChest:     wornChest,
					WornLegs:      wornLegs,
					WornOffHand:   wornOffHand,
					HasAppearance: true,
					HasName:       true,
				})
			}
			if viewer.deliver(faces[i]) {
				// **Recorded only once the frame is in the queue**, which is
				// session.View.MarkLoaded's rule and the same failure it exists to avoid:
				// marking it when it was merely attempted leaves a client permanently
				// drawing a placeholder for somebody the server believes it has described.
				// Unlike a snapshot there is no later frame to supersede a dropped one, and
				// unlike an inventory state this needs no durable flag to say so — an
				// unrecorded entity is described again on the next tick, for as long as it
				// stays in view.
				viewer.described[p.entityID] = tick
			}
		}

		// The same visibility rule, unchanged: a drop lying on a chunk this session
		// holds is one it can draw, and a drop beyond that cube would be an entity
		// standing on terrain the client has never been sent.
		visibleDrops = visibleDrops[:0]
		for i, d := range drops {
			if withinView(viewer.chunk, d.chunk, s.viewDistance) {
				visibleDrops = append(visibleDrops, dropped[i])
			}
		}

		// The same rule a third time. A mob on a chunk this session holds is one it can
		// draw; a mob beyond that cube would be a creature standing on terrain the
		// client has never been sent.
		visibleMobs = visibleMobs[:0]
		visibleLootCorpses = visibleLootCorpses[:0]
		for _, shown := range projectedMobs {
			if withinView(viewer.chunk, shown.chunk, s.viewDistance) {
				visibleMobs = append(visibleMobs, shown.state)
				if viewer.canOpenCorpseLocked(shown.corpse) {
					visibleLootCorpses = append(visibleLootCorpses, shown.corpse.entityID)
				}
				if shown.resident != nil {
					// **The resident's description, on exactly the players' terms**: once
					// per entity per time it enters this viewer's cube, ahead of the
					// snapshot that first carries it, recorded only once the frame is in
					// the queue so a dropped one is retried next tick. It shares
					// `described` with the players rather than keeping a map of its own —
					// entity ids name one thing across every class, so one map is one
					// answer to "has this session been told about that id", and a second
					// would be a second answer to keep in step.
					if _, told := viewer.described[shown.resident.entityID]; told {
						viewer.described[shown.resident.entityID] = tick
					} else {
						if residentFaces == nil {
							residentFaces = make(map[uint64][]byte)
						}
						face, built := residentFaces[shown.resident.entityID]
						if !built {
							// No inventory to wait on and nothing that changes with a
							// tick, unlike a player's: a resident's name, role and face
							// are fixed the moment the seed placed them, so this encoding
							// can never fail to be available.
							face = shown.resident.appearanceFrame()
							residentFaces[shown.resident.entityID] = face
						}
						if viewer.deliver(face) {
							viewer.described[shown.resident.entityID] = tick
						}
					}
				}
			}
		}

		// What is no longer in view is forgotten, which is what keeps this map the size
		// of a view rather than of a session's whole history — and what makes an entity
		// that comes back described again. Deleting during a range is defined; the map is
		// this viewer's own and nothing else touches it.
		//
		// **After every pass that stamps the map, which is why it sits here rather than
		// beside the players it used to follow.** A resident stamped after this ran would
		// be swept on the next tick and described again on the one after, for as long as
		// it stayed in view — a permanent frame per resident per tick, which is precisely
		// what once-per-view bookkeeping exists to avoid.
		for id, at := range viewer.described {
			if at != tick {
				delete(viewer.described, id)
			}
		}

		// The same rule a fourth time. A structure anchored on a chunk this session
		// holds is one it can draw; one beyond that cube would be a shelter standing on
		// terrain the client has never been sent.
		visibleStructures = visibleStructures[:0]
		for i, held := range structures {
			if withinView(viewer.chunk, held.chunk, s.viewDistance) {
				visibleStructures = append(visibleStructures, standing[i])
			}
		}

		// The same complete visibility rule as drops: a projectile is streamed only
		// while it stands over terrain this viewer's chunk cube contains.
		visibleProjectiles = visibleProjectiles[:0]
		for i, shown := range projectiles {
			if withinView(viewer.chunk, chunkAt(shown.pos), s.viewDistance) {
				visibleProjectiles = append(visibleProjectiles, projectedProjectiles[i])
			}
		}

		partyLeader, partyMembers, partyRoster := s.partySnapshotLocked(viewer)

		// The wire field is a uint32 while ticks are counted in uint64. At 20 Hz the
		// truncation wraps after about seven years of uptime, and both sides compare
		// ticks with wrap-aware arithmetic, so a wrap is a discontinuity in a log line
		// rather than in the simulation.
		snapshot := protocol.EntitySnapshot{
			Tick:     uint32(tick),
			Entities: visible,
			Drops:    visibleDrops,
			// The newest snapshot is the complete set of what this session can see. A
			// mob or corpse that stops appearing has stopped existing for this viewer —
			// because it expired or moved out of the cube — and the client despawns it
			// rather than inferring which.
			Mobs: visibleMobs,
			// The same complete-existence-set rule. A structure that stops appearing has
			// stopped existing for this viewer — removed, collapsed, or simply out of the
			// cube — and the client despawns it rather than inferring which.
			Structures: visibleStructures,
			// The contract's one required field, and now the viewer's real health: a
			// snapshot is addressed to one session, so the vitals in it are that
			// player's and nobody else's. Superseded by the next tick's, which is why
			// health and the respawn countdown need no delivery guarantee of their own.
			Vitals: viewer.vitalsLocked(),
			// Who among those entities is down, the viewer included. The contract ties
			// this to the field above — the recipient's own id is here exactly when its
			// vitals say Dead — and both come from `p.alive()` and `p.lifeState`, which
			// are the same variable read twice.
			DeadPlayers:     visibleDead,
			BlockingPlayers: visibleBlocking,
			// The world's own time, the same for every recipient and the one field in
			// here that is not about an entity. Always less than DayLengthTicks, which
			// is what the welcome announced and what the client checks it against —
			// the invariant holds because advanceClockLocked above is the only thing
			// that writes it and RestoreClock refuses anything outside the day.
			TickOfDay: s.tickOfDay,
			// PartyMembers deliberately excludes this viewer and every offline roster entry.
			// PartyRoster carries the stable complete order; its first entry remains the
			// leader even when PartyLeaderEntityID is zero because that leader is offline.
			PartyLeaderEntityID:   partyLeader,
			PartyMembers:          partyMembers,
			PartyRoster:           partyRoster,
			AccessibleLootCorpses: visibleLootCorpses,
			Projectiles:           visibleProjectiles,
			// The sky over **this** recipient, sampled above at their own column, and
			// the third field here that is not about an entity. Unlike TickOfDay beside
			// it, it is not the same for every recipient: two players in different
			// climates are told different things in the same tick, which is the whole
			// point of sampling it per player.
			//
			// HasWeather is unconditionally true, and that is this issue's change to the
			// wire: the flag distinguishes "this server keeps no weather" from a present
			// WeatherKindUnknown, and this server now keeps weather for every player on
			// every tick. The value is never the Go zero — the loop above writes one for
			// every connected player before this loop runs — so there is no tick on
			// which a true flag could carry an Unknown.
			Weather:    viewer.weather,
			HasWeather: true,
		}
		if !viewer.deliverSnapshot(protocol.EncodeEntitySnapshot(snapshot), viewer.chunk.Column()) {
			// Debug, not warn: a full queue is a slow client rather than a broken
			// server, and one line per tick per slow client would bury whatever else
			// the log was needed for.
			s.log.Debug("snapshot dropped: the session's outbound queue is full",
				"entity_id", viewer.entityID, "tick", tick,
				"entities", len(visible), "drops", len(visibleDrops), "mobs", len(visibleMobs),
				"structures", len(visibleStructures), "projectiles", len(visibleProjectiles))
		}
	}

	return waterChanges
}

// sortedPlayersLocked is every connected player in identity order.
//
// Stable order for the reason the mobs, the drops and the structures have one: the
// order players are stepped in and the order they appear in a snapshot are both
// properties of the simulation rather than of a map's iteration. Map order would make
// the encoded bytes differ run to run for no reason, would leave any ordering assertion
// in a test passing by luck, and — since the spawn director draws from a generator once
// per player — would make where creatures appear depend on it too.
//
// The caller holds Sim.mu.
func (s *Sim) sortedPlayersLocked() []*Player {
	players := make([]*Player, 0, len(s.players))
	for _, p := range s.players {
		players = append(players, p)
	}
	slices.SortFunc(players, func(a, b *Player) int {
		return compareEntityIDs(a.entityID, b.entityID)
	})
	return players
}

// forgetDescribedLocked makes the next appearance pass describe entityID again to
// exactly the viewers currently caching it. Viewers outside the subject's view never
// held an entry and are left untouched; the once-per-tick faces cache keeps the retry
// to one encoding however many viewers need it.
//
// The caller holds Sim.mu.
func (s *Sim) forgetDescribedLocked(entityID uint64) {
	for _, viewer := range s.players {
		delete(viewer.described, entityID)
	}
}

// compareEntityIDs orders two identities.
//
// cmp.Compare rather than a subtraction: the difference of two uint64 identities does
// not fit in the int a comparator returns, and a wrapped difference sorts backwards.
// Same trap as compareCoords in session/streaming.go.
func compareEntityIDs(a, b uint64) int { return cmp.Compare(a, b) }

// step advances one player by one tick. Called with sim.mu held.
func (p *Player) step(dt float64, terrain Terrain) {
	// A corpse does not walk, jump or drift. dieLocked cleared the intent and both
	// velocities; this is what stops the integrator putting them back. Refusing the
	// input that would refill them is Submit's job — this is the half that holds even
	// for intent accepted on the tick before the death.
	if !p.alive() {
		return
	}

	// Intent persists until the client replaces it. PlayerInput describes the state of
	// the controls — "whether the jump control is held this tick" — not an event, so
	// one late frame must not stop a player mid-stride.
	//
	// It does not persist for ever, though. After idleLimit ticks of silence "still
	// held" stops being a fair reading, and a client that stopped sending would
	// otherwise walk to the horizon. The yaw is kept: a player who stops sending is
	// still facing where they were facing.
	if p.idleTicks >= p.sim.idleLimit {
		p.current.moveX, p.current.moveZ, p.current.jump = 0, 0, false
	}
	p.idleTicks++

	p.yaw = p.current.yaw

	// The movement basis. yaw 0 looks along -Z and +X is to its right; both sides
	// spell this out, because a mismatch sends players sideways and reads as a physics
	// bug rather than as a convention one. See client/src/player/constants.rs.
	sinYaw, cosYaw := math.Sincos(p.yaw)
	forward := [2]float64{-sinYaw, -cosYaw}
	right := [2]float64{cosYaw, -sinYaw}

	// Whether this body is in water, asked once and read by all three of the rules
	// below. A box query through the same Terrain seam the collision uses, so it sees
	// a player's own digging and an absent chunk answers "not water" — see
	// [Terrain.Fluid], where that direction is argued.
	inWater := overlapsFluid(terrain, playerBox(p.pos))

	// Which way the water is going, asked once and only in water: a unit horizontal
	// direction and a flag saying this voxel is a fall. **The server decides this**,
	// because a current that moves a body is a gameplay outcome like any other; a
	// client is free to mirror [FlowDirection] to animate the surface, and the drift
	// it renders is still the one this tick computed.
	//
	// One sample at the body's centre rather than a scan of the box. A box spanning a
	// bank and a channel would otherwise need a rule for combining two answers, and
	// the centre is the point every server-side rule that needs one already uses — see
	// EditReach, which measures reach from the same place.
	currentX, falling, currentZ := 0.0, 0.0, 0.0
	if inWater {
		cx, cy, cz := playerCentreVoxel(p.pos)
		currentX, falling, currentZ = FlowDirection(terrain, cx, cy, cz)
	}

	// Horizontal velocity is *set* from the intent on land, not accumulated into:
	// there is no momentum out of water, so releasing the controls stops the player on
	// the same tick and there is no acceleration curve to exploit.
	speed := WalkSpeed
	if p.hunger == 0 {
		speed *= StarvingSpeedScale
	}
	switch {
	case inWater:
		// A cap rather than a scale, so it composes with starvation the only way that
		// makes sense: a starving swimmer is a swimmer, not eight tenths of one.
		speed = min(speed, SwimSpeed)
	case snowBites(p.weather):
		// Deep snow is something to wade through, and a swimmer is not in it — which is
		// what the `switch` says that two `if`s would not. Read as a scale rather than a
		// cap, so it multiplies with starvation instead of replacing it: being starved in
		// a blizzard is worse than either, and SnowSpeedScale is where that is argued.
		speed *= SnowSpeedScale
	}
	targetX := (forward[0]*p.current.moveZ + right[0]*p.current.moveX) * speed
	targetZ := (forward[1]*p.current.moveZ + right[1]*p.current.moveX) * speed

	if inWater {
		// **The current is a target, not a force.** It is added to the swimmer's own
		// target and the velocity eases toward the sum, so what it can ever contribute
		// is CurrentSpeed and nothing accumulates across ticks — no accumulator is
		// stored on the Player, and there is nothing to leave behind when the swimmer
		// leaves the channel. Because CurrentSpeed is under SwimSpeed, full opposing
		// intent still settles upstream at the difference: a river is fought, not won
		// against instantly and not lost to.
		//
		// The ease is the same one the vertical has used since swimming arrived, and it
		// is what stops a channel from being a wall of velocity: entering, leaving and
		// turning inside a current all cost about a fifth of a second at
		// SwimAcceleration rather than a single tick's snap. Its price is that
		// horizontal water movement now has that much momentum, which is a change to
		// swimming in still water too — deliberately, because the alternative is a rule
		// that behaves differently depending on which voxel the body's centre happens
		// to be sampling.
		targetX += currentX * CurrentSpeed
		targetZ += currentZ * CurrentSpeed
		p.vel[0] = approach(p.vel[0], targetX, SwimAcceleration*dt)
		p.vel[2] = approach(p.vel[2], targetZ, SwimAcceleration*dt)
	} else {
		p.vel[0], p.vel[2] = targetX, targetZ
	}

	// Whether a jump *happens* is the server's decision, and ground contact is the
	// part of it a client cannot know. onGround is last tick's answer, which is the
	// only one that exists when the intent is applied.
	//
	// **In water the ground contact is not required**, and that is the whole of "hold
	// jump to swim up": the intent means "rise" there rather than "leap", so it is
	// answered every tick the body is in water instead of only on the tick it is
	// standing on something.
	switch {
	case p.current.jump && inWater:
		p.vel[1] = SwimRiseSpeed
	case p.current.jump && p.onGround:
		p.vel[1] = JumpImpulse
	}

	if inWater {
		// A fall pulls harder than still water does, and it pulls with a target rather
		// than with a force for the same reason the horizontal current does.
		//
		// **The jump intent wins outright.** Holding the rise leaves SwimSinkSpeed as
		// the target, so the SwimRiseSpeed the switch above just set survives its first
		// eased step and stays positive — a swimmer under a waterfall climbs out of it
		// by swimming, rather than being pinned there while the fall and the rise
		// average each other into nothing.
		sink := SwimSinkSpeed
		if falling < 0 && !p.current.jump {
			sink = WaterfallSinkSpeed
		}
		p.vel[1] = approach(p.vel[1], sink, SwimAcceleration*dt)
	} else {
		p.vel[1] = max(p.vel[1]-Gravity*dt, -TerminalFallSpeed)
	}

	delta := [3]float64{p.vel[0] * dt, p.vel[1] * dt, p.vel[2] * dt}
	pos, blocked := moveAndCollideWithStep(terrain, playerBody, p.pos, delta, playerStepHeight)
	p.pos = pos

	// The ground is "a downward move that was stopped". Deriving it from the collision
	// rather than probing for it means a player standing on a chunk that has not
	// arrived is also on the ground, which is what keeps them from accumulating fall
	// speed while they wait.
	p.onGround = blocked[1] && delta[1] <= 0

	// The impact, read here and nowhere else: this is the last moment the speed that
	// carried the player into the ground still exists. The loop below is what destroys
	// it, and a fall damage computed after it would always be zero.
	impact := 0.0
	if p.onGround && p.vel[1] < 0 {
		impact = -p.vel[1]
	}

	for axis := range 3 {
		if blocked[axis] {
			p.vel[axis] = 0
		}
	}

	// Only a landing on terrain the server actually holds may hurt. The collision reads
	// an absent chunk as solid — which is what stops a player falling out of a world
	// that is still loading — and that fiction must not also be a floor to break on.
	//
	// The damage is computed first and the terrain read second, deliberately. Every tick
	// a player stands still ends in a downward velocity the ground cancels, so this seam
	// is reached constantly; asking the chunk cache about the floor under every standing
	// player twenty times a second would be a real cost for an answer that only matters
	// on the rare tick an impact is hard enough to hurt.
	//
	// **Water resets the reference speed the impact is measured from, so no fall into
	// water hurts however far it fell.** It is read at the *landing* position rather
	// than reusing inWater from the top of this tick, because a fast fall can cross
	// the surface and reach the bed inside one step — and it is read last, beside the
	// residency check and for the same reason: a damaging impact is rare, and this
	// box scan is only paid on the ticks that have one.
	if damage := fallDamage(impact); damage > 0 &&
		!overlapsFluid(terrain, playerBox(p.pos)) &&
		landedOnResidentTerrain(terrain, p.pos) {
		p.damageLocked(damage)
	}
}

// approach moves current toward target by at most step, without overshooting.
//
// The whole of the swim integrator. A signed max/min pair rather than an
// exponential ease, because the result has to be the same on every build for the
// same reason the generator is integer-only — and because "eases toward a terminal
// velocity" is exactly what this says, with no time constant to tune.
func approach(current, target, step float64) float64 {
	if current > target {
		return max(current-step, target)
	}
	return min(current+step, target)
}

// acceptIntent turns accepted input into the intent the integrator reads.
//
// The speed clamp is on the *magnitude* of the movement vector, not on each axis
// separately, and that difference is the acceptance criterion. Clamping components to
// ±1 still admits (1, 1) — a vector of length √2, a diagonal 41% faster than any
// straight line, reachable by an honest client by accident and by a forged one on
// purpose. Scaling the vector to length 1 makes the fastest input a client can
// express exactly as fast as walking forwards, whatever numbers it puts in the frame.
//
// Computed in float64, and Hypot rather than a hand-rolled square root: a forged
// 1e38 squares to +Inf in float32, and the scale factor would then be zero — an
// overflow that rewards the forgery by freezing the player instead of clamping them.
func acceptIntent(in protocol.PlayerInput) intent {
	x, z := float64(in.MoveX), float64(in.MoveZ)
	if length := math.Hypot(x, z); length > 1 {
		x, z = x/length, z/length
	}

	return intent{
		moveX: x,
		moveZ: z,
		// Wrapped first so any finite value becomes an angle, then clamped so that angle
		// is a direction a body could look in. Clamping alone would leave a forged 100
		// radians pinned at straight up; wrapping alone would let π radians mean looking
		// backwards through the player's own feet.
		pitch: min(max(wrapAngle(float64(in.Pitch)), -math.Pi/2), math.Pi/2),
		// Wrapped, because the snapshot echoes it and the client interpolates it. A
		// client that counted turns without wrapping would be within the contract —
		// the field is radians and finite — and would still hand every other client a
		// number no renderer can lerp usefully.
		yaw:  wrapAngle(float64(in.Yaw)),
		jump: in.Jump,
	}
}

// wrapAngle brings radians into (-π, π].
func wrapAngle(radians float64) float64 {
	wrapped := math.Mod(radians+math.Pi, 2*math.Pi)
	if wrapped <= 0 {
		wrapped += 2 * math.Pi
	}
	return wrapped - math.Pi
}

// checkInputFinite refuses a non-finite component, naming the field.
//
// pitch is checked although movement ignores it. The contract says every float in a
// PlayerInput is finite; enforcing that only in the fields this issue happens to read
// would make the invariant "finite where convenient", and the field is carried for
// aiming, which is the next thing to read it.
func checkInputFinite(in protocol.PlayerInput) error {
	fields := [...]struct {
		name  string
		value float32
	}{
		{"move_x", in.MoveX},
		{"move_z", in.MoveZ},
		{"yaw", in.Yaw},
		{"pitch", in.Pitch},
	}

	for _, field := range fields {
		if err := requireFinite(field.name, field.value); err != nil {
			return err
		}
	}
	return nil
}

// requireFinite is the finiteness test, deliberately not a range clamp.
func requireFinite(name string, value float32) error {
	if f := float64(value); math.IsNaN(f) || math.IsInf(f, 0) {
		return fmt.Errorf("%s must be finite, got %v", name, value)
	}
	return nil
}

// newerTick reports whether a is later than b, tolerating the uint32 wrap.
//
// The subtraction is what makes it wrap-aware: reading it as a signed difference puts
// 0 immediately after 0xFFFFFFFF instead of four billion ticks before it. Without
// that, a session alive across the wrap would discard every input for ever.
func newerTick(a, b uint32) bool {
	return int32(a-b) > 0
}

// chunkAt is the chunk a simulated position falls in.
//
// Floors into integer block coordinates first and asks world.ChunkOf, rather than
// narrowing to the float32 world.ContainingChunk takes: the simulation's position is
// a float64 and rounding it on the way to a chunk lookup would put a player on the
// wrong side of a seam they are standing exactly on.
func chunkAt(pos [3]float64) world.Coord {
	return world.ChunkOf(
		int64(math.Floor(pos[0])),
		int64(math.Floor(pos[1])),
		int64(math.Floor(pos[2])),
	)
}

// withinView reports whether an entity in chunk `at` is inside the volume a session
// centred on `center` is streaming.
//
// A cube of (2r+1)³ chunks, exactly the shape View.visibleFrom uses, and not a
// sphere: a session's snapshots have to cover the terrain it actually holds, or an
// entity would stand on a chunk the client has and be invisible on it.
func withinView(center, at world.Coord, radius int32) bool {
	limit := int64(radius)
	return axisDistance(center.X, at.X) <= limit &&
		axisDistance(center.Y, at.Y) <= limit &&
		axisDistance(center.Z, at.Z) <= limit
}

// axisDistance is |a-b| in int64, because the difference of two int32 coordinates
// overflows an int32 — and an overflowed difference compares as *near*, which would
// make the far side of the world visible.
func axisDistance(a, b int32) int64 {
	d := int64(a) - int64(b)
	if d < 0 {
		return -d
	}
	return d
}

// toWire narrows a simulated vector to the float32 the contract carries.
//
// The simulation works in float64 and only narrows on the way out. The direction
// matters: a float32 stored and re-read would fold its rounding back into the
// collision arithmetic, where a hair of penetration is the difference between resting
// on a surface and being inside it.
func toWire(v [3]float64) [3]float32 {
	return [3]float32{float32(v[0]), float32(v[1]), float32(v[2])}
}

// chunkFeed publishes the chunk the simulation says a player is in. Newest value
// wins.
//
// The seam between the tick goroutine and the session's streaming goroutine, and it
// exists because Streamer.MoveTo calls Cache.Get, which generates a chunk on a miss.
// A tick that waits on terrain is a tick every connected player misses, so the tick
// publishes a coordinate and somebody else does the waiting.
//
// Newest-wins rather than a queue: a coordinate the player has already left is worth
// nothing, and a slow streamer must not be able to make the tick block or make a
// buffer grow.
type chunkFeed struct {
	mu     sync.Mutex
	latest world.Coord

	// wake is a doorbell, not a queue: it carries no value, so a ring already
	// waiting says exactly what a second one would.
	wake chan struct{}
}

func newChunkFeed() *chunkFeed {
	return &chunkFeed{wake: make(chan struct{}, 1)}
}

func (f *chunkFeed) publish(coord world.Coord) {
	f.mu.Lock()
	f.latest = coord
	f.mu.Unlock()

	f.ring()
}

// ring wakes the consumer without changing where the player is.
//
// The doorbell carries no value, so ringing it hands out `latest` — the newest chunk
// the tick has published, which is the centre the view should be diffed at whether or
// not the player has moved since. That is what lets somebody other than the tick ask
// for a diff: see Player.WakeStreaming.
//
// Non-blocking, which is what makes publish callable from the tick loop at all. A
// wake-up already pending is the same instruction, and the consumer reads the
// coordinate from `latest` rather than from the channel, so collapsing two rings into
// one loses nothing but a wasted diff.
func (f *chunkFeed) ring() {
	select {
	case f.wake <- struct{}{}:
	default:
	}
}

func (f *chunkFeed) next(ctx context.Context) (world.Coord, error) {
	// Checked before the select, because a select with both cases ready picks one at
	// random: with a coordinate pending and an already-cancelled context, this would
	// hand out work about half the time. Same trap as the chunk cache's semaphore, the
	// session's outbound queue and the clock's sleep.
	if err := ctx.Err(); err != nil {
		return world.Coord{}, err
	}

	select {
	case <-f.wake:
	case <-ctx.Done():
		return world.Coord{}, ctx.Err()
	}

	f.mu.Lock()
	defer f.mu.Unlock()
	return f.latest, nil
}
