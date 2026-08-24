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

	// mobDeathTicks is MobDeathDuration in the ticks Step counts: how long a killed
	// creature's body lies in the world before it stops existing and its loot reaches the
	// ground. Derived per server for the reason deathTicks is — two and a half seconds of
	// dying has to be two and a half seconds at every rate.
	//
	// **It is neither of the two above.** The director's removals take away a creature
	// *instead of* killing it, which is what makes "a despawn leaves nothing" true; this
	// one counts a creature that has already been killed out of the world, and it is the
	// only countdown loot waits on.
	mobDeathTicks uint32

	// attackCooldown is SwordCooldown in the ticks Step counts, so a blade recovers in
	// six hundred milliseconds whatever rate the server is run at.
	attackCooldown uint32

	// dropLifetime is DropLifetime expressed in the ticks Step counts, derived from the
	// tick rate for the same reason the physics timestep is.
	dropLifetime int

	// hardness is handMiningTimes in the ticks Step counts, for the same reason again: the
	// table is written in seconds because a block should take the same time to break
	// whatever rate the server runs at. A block absent from it is not breakable.
	hardness map[world.Block]int

	log *slog.Logger

	mu      sync.Mutex
	players map[uint64]*Player

	// tickOfDay is where the world stands in its day, and it is always less than
	// DayLengthTicks. See clock.go for everything about it; the field is here because
	// this is the struct mu guards.
	tickOfDay uint32

	// drops is every item lying in the world. Keyed by identity like players, and for
	// the same reasons: a snapshot names entities by id, and a merge or a pickup has to
	// find one without scanning.
	drops map[uint64]*itemDrop

	// mobs is every live creature, keyed by identity like players and drops and for the
	// same reasons: a snapshot names entities by id, and a hit has to find one without
	// scanning.
	//
	// Nothing here is persisted, deliberately: a restart loses whatever was hunting,
	// because a mob is a moment in a simulation rather than a change to the world. The
	// director puts them back where the players actually are, which is a better answer
	// than a file could give.
	mobs map[uint64]*mob

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

	// structures is every placed tent and forge, keyed by identity for the reason the
	// three maps above are: a snapshot names them by id, and a removal has to find one
	// without scanning. Unlike the three, nothing in the tick advances them — a
	// structure has no state that changes with time — so they are read here and written
	// only by placement, removal, collapse and the restore at startup.
	structures map[uint64]*structure

	// structuresDirty says the camp has changed since it was last written down.
	//
	// The chunk cache's dirty flag, for a store that rewrites one file rather than many:
	// placement, removal and collapse set it, and the autosave loop and the shutdown
	// flush clear it through Sim.TakeDirtyStructures. A flag rather than a write on the
	// placing goroutine because nothing under this lock may touch a disk, and a flag
	// rather than an unconditional periodic save because a world nobody is building in
	// should cost no I/O at all.
	structuresDirty bool

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
// **worldSeed is here to seed the simulation's generators, not to generate anything.**
// The simulation still knows nothing about terrain: it does not call world.Generate, it
// cannot, and the seam it reads chunks through has no seed on it. What the number buys
// is that the spawn director's choices and a kill's yield are properties of the *world*
// rather than of the process — two runs of the same world, given the same ticks, place
// the same creatures in the same places and leave the same items on the same ground.
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
		dt:              1 / float64(tickRate),
		viewDistance:    int32(viewDistance),
		idleLimit:       idleLimitTicks(tickRate),
		terrain:         terrain,
		editor:          editor,
		mintEntityID:    mintEntityID,
		dropLifetime:    dropLifetimeTicks(tickRate),
		hardness:        handMiningTicksFor(tickRate),
		deathTicks:      deathDurationTicks(tickRate),
		protectionTicks: respawnProtectionTicks(tickRate),
		regenDelayTicks: ticksFor(HealthRegenDelay, tickRate),

		regenIntervalTicks: ticksFor(HealthRegenInterval, tickRate),
		hungerDrainTicks:   ticksFor(HungerDrainInterval, tickRate),
		mobTimings:         mobTimingsFor(tickRate),
		spawnEvery:         ticksFor(SpawnDirectorInterval, tickRate),
		mobDespawnTicks:    ticksFor(MobDespawnGrace, tickRate),
		mobDeathTicks:      ticksFor(MobDeathDuration, tickRate),
		spawns:             newSpawnRNG(worldSeed),
		loot:               newLootRNG(worldSeed),
		attackCooldown:     ticksFor(SwordCooldown, tickRate),
		log:                log,
		players:            make(map[uint64]*Player),
		drops:              make(map[uint64]*itemDrop),
		mobs:               make(map[uint64]*mob),
		structures:         make(map[uint64]*structure),
		byIdentity:         make(map[identity.PlayerID]*Player),
		minersByPos:        make(map[[3]int32]map[*Player]struct{}),
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

	// playerID is who this player *is*, across connections; entityID is what names
	// them in this one. Two identifiers rather than one because they answer different
	// questions and change at different rates: a snapshot addresses an entity, and
	// what a player carries back from a previous session is addressed by an identity.
	// Nothing about snapshots or indexes reads this — it is here so that the thing
	// being saved knows what it is being saved as.
	playerID identity.PlayerID

	deliver   func(frame []byte) bool
	chunks    *chunkFeed
	mineReady chan MiningCompletion
	inventory inventory

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
	pos      [3]float64
	vel      [3]float64
	yaw      float64
	onGround bool

	// chunk is the chunk pos falls in. Kept beside the position rather than derived
	// on demand because streaming is driven by the *change*, and comparing against
	// the value already published is how the tick notices one.
	chunk world.Coord

	current   intent
	idleTicks int
	haveTick  bool
	lastTick  uint32

	// leaving is the irrevocable server-owned linger state. The body remains a live
	// simulation entity — gravity, damage, snapshots and interaction all continue —
	// but no client intent may change it. It is guarded by sim.mu with the intents it
	// disables and is cleared only by removing this Player from the simulation.
	leaving bool

	// The life the server owns. See vitals.go, which is where every transition between
	// these values happens; nothing outside it writes them.
	//
	// spawn is the position Join was given, kept as the provisional respawn point. Kept
	// rather than recomputed because the world helper that produced it can generate
	// terrain, and the tick may not.
	health       uint16
	lifeState    vnet.LifeState
	respawnTicks uint32

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
	protectionTicks  uint32
	penaltyApplied   bool
	spawn            [3]float64

	// inventoryDirty records that a pickup changed the slots and the client has not
	// been told yet. Guarded by sim.mu rather than by the inventory's own lock: it is
	// the tick that sets it and the tick that clears it, and the inventory lock is only
	// ever taken here without waiting. See offerInventoryLocked.
	inventoryDirty bool

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
// position, facing where they faced, with the health, hunger and pack they had; a new
// one gets full health and hunger, [newStarterInventory] and spawn.
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
	pos, yaw, health, hunger, slots := joinSpawn, 0.0, uint16(PlayerMaxHealth), uint16(PlayerMaxHunger), starterSlots()
	if resume != nil {
		pos, yaw, health, hunger, slots = resume.Pos, resume.Yaw, resume.Health, resume.Hunger, restoredSlots(resume.Slots)
	}

	p := &Player{
		sim:        s,
		entityID:   entityID,
		playerID:   playerID,
		name:       name,
		appearance: appearance,
		// Empty, and that is the reconnect rule rather than an initialisation detail: a
		// new session has described nobody to this client, so everything it can see is
		// described again — including the players it was already looking at a moment ago.
		described: make(map[uint64]uint64),
		deliver:   deliver,
		chunks:    newChunkFeed(),
		mineReady: make(chan MiningCompletion, 1),

		// A composite literal for the reason newStarterInventory returns one: the struct
		// carries a mutex, which `go vet`'s copylocks check refuses to see assigned from
		// a variable.
		inventory: inventory{slots: slots},
		pos:       pos,
		spawn:     joinSpawn,
		yaw:       yaw,
		// The intent carries the yaw too, because step reads p.current.yaw and writes it
		// back over p.yaw on the first tick. Without this a restored player would snap to
		// facing north before their client's first input arrived.
		current:   intent{yaw: yaw},
		health:    health,
		hunger:    hunger,
		lifeState: vnet.LifeStateAlive,
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
	s.players[entityID] = p
	s.byIdentity[playerID] = p
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
		p.setMiningLocked(nil)
		p.mineCompleting = false
		p.mineReset = nil
		delete(s.players, p.entityID)
		// The same "only the player that was handed in" guard, applied to the second
		// index: a rejoin that already claimed this identity must keep it. The two maps
		// are written together here and in Join, which is what makes "an identity is in
		// one exactly while its player is in the other" a property rather than a hope.
		if current, indexed := s.byIdentity[p.playerID]; indexed && current == p {
			delete(s.byIdentity, p.playerID)
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

// Step advances the world by one tick, delivers what every session can see, and puts
// whatever died in it on the ground.
//
// **Two functions, and the split is the lock.** stepWorld below is the tick, and it holds
// Sim.mu for the whole of it; spawnLoot runs after that lock has been released, because
// Sim.spawnDrop takes it itself. It is exactly the pairing edit.go already has for a
// structure a break brought down — collapseStructuresAt collects under the lock,
// dropCollapsed spawns outside it — and it is why a kill on tick N is a drop on tick
// N+1. See loot.go, where the whole argument lives.
func (s *Sim) Step(tick uint64) {
	s.spawnLoot(s.stepWorld(tick))
}

// stepWorld advances every player, drop, creature and clock by one tick, delivers what
// every session can see, and returns the loot the kills in it left behind.
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
// already stale. Spawning a drop *would* block — on this very lock — which is why the
// loot leaves through the return value rather than through Sim.spawnDrop.
func (s *Sim) stepWorld(tick uint64) []lootDrop {
	s.mu.Lock()
	defer s.mu.Unlock()

	// The clock first, ahead of anything that could ever read it: time passes, and then
	// the world responds to the time it now is. Everything this tick produces — the
	// tick_of_day every snapshot below carries, and in time whatever asks IsNight —
	// therefore sees one value, and there is no ordering question about which half of a
	// tick ran before the day moved.
	s.advanceClockLocked()

	players := s.sortedPlayersLocked()

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
	}

	// The drops, in the order the world changed them: fall and age first, then combine
	// what ended up together, then hand over what somebody is standing on. Merging
	// before collecting is what makes a pile arrive as one insertion rather than as
	// several, and both run after the players have moved so a pickup uses the position
	// this tick produced rather than the last one's.
	drops := s.advanceDropsLocked(s.sortedDropsLocked())
	drops = s.mergeDropsLocked(drops)
	drops = s.collectDropsLocked(players, drops)

	// The swings, after every player has moved and before any mob acts. Both halves of
	// that are the guarantee: a swing is judged against the positions this tick
	// produced, so network scheduling cannot choose an in-between one to be judged at,
	// and a draugr killed here cannot land an attack later in the same tick.
	//
	// **A swing produces nothing to carry out of here any more.** A blow that kills starts
	// the creature dying; what it left behind reaches the ground MobDeathDuration later,
	// from the reap below, and travels out through the same return value it always did.
	for _, p := range players {
		p.resolveSwingLocked()
	}

	// The mobs, after the players have moved so a chase steers at the position this tick
	// produced rather than the last one's — and after the swings above. This is also where
	// a body whose time is up stops existing, which is why the loot comes from here: it is
	// gathered rather than spawned, and carried out of this function for Step to put on the
	// ground once the lock is gone. Nil for every tick nothing finished dying in, which is
	// almost all of them.
	mobs, loot := s.advanceMobsLocked(players)

	// And the director last, after the creatures it manages have been advanced: what it
	// spawns is judged against the positions this tick produced, and what it removes it
	// removes knowing the target each mob chose in it. A population that changed is
	// re-read rather than patched, so the snapshot below is the world as this tick left
	// it — a mob removed here is gone from it, and one created here is in it.
	if s.directMobsLocked(tick, players, mobs) {
		mobs = s.sortedMobsLocked()
	}

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
	prowling := mobStates(mobs)
	standing := s.structureStatesLocked(structures)

	// Ahead of the snapshots, deliberately. An inventory state is not superseded by the
	// next tick's and a snapshot is, so when a queue has room for one frame it should
	// be this one.
	for _, p := range players {
		p.offerInventoryLocked()
	}

	// One snapshot per session, carrying only the entities that session can see.
	// O(sessions × entities), knowingly: a spatial index is worth building when the
	// quadratic term matters and not one issue before.
	visible := make([]protocol.EntityState, 0, len(states))
	visibleDrops := make([]protocol.ItemDropState, 0, len(dropped))
	visibleMobs := make([]protocol.MobState, 0, len(prowling))
	visibleStructures := make([]protocol.StructureState, 0, len(standing))
	// **Filled from the same pass that fills `visible`, and that is what keeps the two
	// agreeing.** The contract says every id here names a player in the same snapshot's
	// entity vector, and a client refuses a frame where one does not; deriving it from a
	// second walk over `players` would be a second visibility decision to keep in step.
	// Nil until somebody in view is dead, so the ordinary tick allocates nothing and the
	// encoder writes no field at all.
	var visibleDead []uint64

	// At most one encoded appearance per player per tick, built the first time a viewer
	// turns out not to have been told about them and handed to every viewer after that.
	// It is Registry.BroadcastChunk's "one encode for every recipient", and it is what
	// makes asking this question inside the per-viewer loop cost nothing on the ticks
	// where nobody's view changed — which is almost all of them, because the answer is
	// only ever built when somebody has just come into somebody else's cube.
	faces := make([][]byte, len(players))

	for _, viewer := range players {
		visible = visible[:0]
		visibleDead = visibleDead[:0]
		for i, p := range players {
			if !withinView(viewer.chunk, p.chunk, s.viewDistance) {
				continue
			}
			visible = append(visible, states[i])

			// **The viewer's own death goes in this vector like everybody else's**, which
			// is the point of the field rather than an accident of the loop: a session is
			// inside its own view, so the body it is watching go down and the bodies
			// beside it are stated the same way and cannot drift apart. Its vitals still
			// carry the health and the countdown; those are per-recipient and this is not.
			if !p.alive() {
				visibleDead = append(visibleDead, p.entityID)
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
				faces[i] = protocol.EncodePlayerAppearance(protocol.PlayerAppearance{
					EntityID:      p.entityID,
					Appearance:    p.appearance,
					Name:          p.name,
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

		// What is no longer in view is forgotten, which is what keeps this map the size
		// of a view rather than of a session's whole history — and what makes an entity
		// that comes back described again. Deleting during a range is defined; the map is
		// this viewer's own and nothing else touches it.
		for id, at := range viewer.described {
			if at != tick {
				delete(viewer.described, id)
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
		for i, m := range mobs {
			if withinView(viewer.chunk, m.chunk, s.viewDistance) {
				visibleMobs = append(visibleMobs, prowling[i])
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

		// The wire field is a uint32 while ticks are counted in uint64. At 20 Hz the
		// truncation wraps after about seven years of uptime, and both sides compare
		// ticks with wrap-aware arithmetic, so a wrap is a discontinuity in a log line
		// rather than in the simulation.
		snapshot := protocol.EntitySnapshot{
			Tick:     uint32(tick),
			Entities: visible,
			Drops:    visibleDrops,
			// The newest snapshot is the complete set of what this session can see. A
			// mob that stops appearing has stopped existing for this viewer — because it
			// died, or because it walked out of the cube — and the client despawns it
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
			DeadPlayers: visibleDead,
			// The world's own time, the same for every recipient and the one field in
			// here that is not about an entity. Always less than DayLengthTicks, which
			// is what the welcome announced and what the client checks it against —
			// the invariant holds because advanceClockLocked above is the only thing
			// that writes it and RestoreClock refuses anything outside the day.
			TickOfDay: s.tickOfDay,
		}
		if !viewer.deliver(protocol.EncodeEntitySnapshot(snapshot)) {
			// Debug, not warn: a full queue is a slow client rather than a broken
			// server, and one line per tick per slow client would bury whatever else
			// the log was needed for.
			s.log.Debug("snapshot dropped: the session's outbound queue is full",
				"entity_id", viewer.entityID, "tick", tick,
				"entities", len(visible), "drops", len(visibleDrops), "mobs", len(visibleMobs),
				"structures", len(visibleStructures))
		}
	}

	return loot
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

	// Horizontal velocity is *set* from the intent, not accumulated into. There is no
	// momentum in this issue, so releasing the controls stops the player on the same
	// tick and there is no acceleration curve to exploit.
	speed := WalkSpeed
	if p.hunger == 0 {
		speed *= StarvingSpeedScale
	}
	p.vel[0] = (forward[0]*p.current.moveZ + right[0]*p.current.moveX) * speed
	p.vel[2] = (forward[1]*p.current.moveZ + right[1]*p.current.moveX) * speed

	// Whether a jump *happens* is the server's decision, and ground contact is the
	// part of it a client cannot know. onGround is last tick's answer, which is the
	// only one that exists when the intent is applied.
	if p.current.jump && p.onGround {
		p.vel[1] = JumpImpulse
	}
	p.vel[1] = max(p.vel[1]-Gravity*dt, -TerminalFallSpeed)

	delta := [3]float64{p.vel[0] * dt, p.vel[1] * dt, p.vel[2] * dt}
	pos, blocked := moveAndCollide(terrain, playerBody, p.pos, delta)
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
	if damage := fallDamage(impact); damage > 0 && landedOnResidentTerrain(terrain, p.pos) {
		p.damageLocked(damage)
	}
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
