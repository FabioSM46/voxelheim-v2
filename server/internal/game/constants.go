package game

import "time"

// Movement constants — the authoritative numbers, and the only copy on this side.
//
// **They must stay in sync with `client/src/player/constants.rs`.** The client
// mirrors the two that describe the *body*, because it has to draw a capsule of the
// right size and put the camera at the right height inside it. It deliberately does
// **not** mirror the three that describe the *physics*: with no client-side
// prediction nothing there integrates, and a duplicated number with no reader is a
// synchronisation hazard that buys nothing. When prediction lands it will need
// them, and that is the issue that should copy them across — see the sync note in
// the client's file.
//
// Nothing here is on the wire. A client learns the tick rate from `ServerWelcome`
// and the results from `EntitySnapshot`; how fast a player walks is not something
// it is told, because it is not something it decides.
const (
	// WalkSpeed is the horizontal speed of a player at full intent, in blocks per
	// second. Roughly a brisk walk at one block to the metre.
	//
	// It is a *speed*, not an acceleration: horizontal velocity is set from the
	// intent each tick rather than accumulated, so there is no momentum to exploit
	// and stopping is immediate. Acceleration curves are a feel issue and belong
	// with the one that gives the client prediction.
	WalkSpeed = 4.3

	// Gravity is the downward acceleration, in blocks per second squared.
	//
	// Well above the real 9.81: a physically accurate fall over one block takes
	// nearly half a second, which reads as floating. This is the value the jump
	// height below is paired with.
	Gravity = 28.0

	// JumpImpulse is the upward velocity a jump starts with, in blocks per second.
	//
	// Chosen for what it clears rather than for itself: the peak has to be over one
	// block, so a player can step onto terrain, and under two, so a jump is not a
	// flight. See jumpApex in the tests, which asserts that relationship instead of
	// pinning the number the integrator happens to produce.
	JumpImpulse = 9.0

	// TerminalFallSpeed caps downward velocity, in blocks per second.
	//
	// Not for realism: it bounds how far a player moves in one tick, which is what
	// keeps the collision sub-steps below a block and therefore keeps a long fall
	// from passing through the ground. 60 blocks/s is 3 blocks per tick at 20 Hz.
	TerminalFallSpeed = 60.0

	// PlayerWidth is the edge of the player's square footprint, in blocks.
	//
	// Under one block on purpose: a body wider than the grid cannot fit through a
	// one-block gap, and a corridor a player cannot walk down is a level-design
	// problem created by a constant.
	PlayerWidth = 0.6

	// PlayerHeight is how tall the player's box is, in blocks.
	PlayerHeight = 1.8

	// PlayerMaxHealth is the health a player has, and the denominator of every health
	// display the client draws.
	//
	// **Nothing in this package can change a player's health yet**, and that is the
	// whole of what this constant means today. Protocol V3 makes per-recipient vitals a
	// required field of every snapshot, so the tick has to report *something*; what it
	// reports is the only true statement available, which is that every player is alive
	// and unharmed. Damage, death, the respawn countdown and invulnerability arrive
	// with the issue that owns them and read this same number — a second constant
	// appearing then would be two answers to "how much health is full".
	PlayerMaxHealth = 100

	// deathDurabilityKept over deathDurabilityScale is the approved death penalty on
	// equipment: a player who dies keeps four fifths of the remaining condition of every
	// durable item they had on them, losing the GDD's 20%. Which items those are is
	// carriedOnPerson's answer, not this constant's — this pair is only the fraction.
	//
	// A fraction rather than a float, so the arithmetic in wornByDeath is exact and its
	// boundaries are decidable — floor(1 * 4/5) is 0, and a blade can be worn out by
	// dying often enough. A sharpening stone is what brings one back, at
	// SharpeningStoneRestore a time — see `Player.Repair` — which is what makes death a
	// supply cost rather than an expiry date on the weapon.
	deathDurabilityKept  = 4
	deathDurabilityScale = 5

	// coarsestTickInterval is the longest a single tick may represent, in seconds. The
	// server accepts tick rates from 1 Hz upwards, so one tick can be a whole second.
	coarsestTickInterval = 1.0

	// SafeFallSpeed is the fastest landing that does no harm, in blocks per second.
	//
	// Derived from the coarsest tick rate rather than chosen for feel, because that is
	// what binds it: an ordinary jump and the two-block spawn settlement must both be
	// harmless at every rate an operator can set, and at 1 Hz a single tick applies a
	// whole second of gravity. The settlement is the worse of the two — it arrives at a
	// full second of gravity, where a jump arrives at that minus its impulse — so the
	// threshold is what the settlement lands at.
	//
	// **An integrator artefact is setting a gameplay number here, and that is worth
	// knowing rather than discovering.** At 20 Hz the settlement lands at 10.5 and a
	// jump at 7.8, so 12 would serve; 28 instead means damage does not begin until a
	// fall of about fourteen blocks. What would buy a lower threshold is narrowing the
	// accepted tick-rate range, which is a decision about operators rather than about
	// falling.
	SafeFallSpeed = Gravity * coarsestTickInterval

	// FallDamagePerSpeed is health lost per whole block-per-second of impact above
	// SafeFallSpeed.
	//
	// Four, so that a fall which reaches TerminalFallSpeed is fatal outright:
	// (60 - 28) * 4 is 128 against a hundred points of health, and the fall that first
	// kills is about fifty blocks.
	FallDamagePerSpeed = 4

	// DeathDuration is how long a dead player stays dead before the server respawns
	// them. Converted to ticks at the configured rate; the simulation's only clock is
	// Step.
	DeathDuration = 3 * time.Second

	// RespawnProtection is how long a respawned player cannot be damaged for. Counted
	// in ticks for the same reason.
	RespawnProtection = 2 * time.Second

	// HealthRegenDelay is how long after the last landed hit before health starts
	// coming back, and HealthRegenInterval is how long one point of it takes.
	//
	// **Tuned slow-and-soon, and that is the decision rather than the numbers.** A
	// wound is meant to outlive the encounter without ending the session: you walk
	// away hurt, you stay hurt for a while, and you get there. The opposite tuning —
	// fast and late — suits a game where fights are discrete episodes, and moving these
	// that way is a design change rather than a retune.
	//
	// Neither value is a taste. Both are ratios against numbers this package already
	// holds:
	//
	//   - Five seconds is three to four full swing cycles. A draugr's is windup 600ms
	//     plus recovery 900ms, a vargr's 1.1s, so regeneration never ticks inside a
	//     fight — and it clears RespawnProtection above, so the two never interact.
	//   - One point a second against PlayerMaxHealth's hundred is about a minute and a
	//     half from near death. Against a draugr's ten damage per 1.5s cycle — near
	//     seven a second — regeneration is slower by a factor of seven, which is the
	//     relationship that matters: it can recover you from a fight and can never
	//     carry you through one.
	//
	// They are a first guess from those ratios and not from play. Expect to revisit
	// them once somebody has actually been hurt.
	HealthRegenDelay    = 5 * time.Second
	HealthRegenInterval = 1 * time.Second

	// --- The creatures ----------------------------------------------------------
	//
	// **Their numbers are not here.** Health, speed, reach, damage, telegraph, body and
	// whether the dark is what brings one out are per-species rows in `mobRegistry` —
	// species.go — for the reason a blade's damage sits beside `itemRegistry` rather than
	// here: they describe one *creature*, and nothing about any of them generalises to the
	// next. What stays below is what the *director* believes, which is a rule about the
	// world rather than about anything living in it.

	// --- The spawn director -----------------------------------------------------
	//
	// What the dark puts near a player, how much of it, and how far away. Every number
	// here bounds something: the two caps bound the tick's cost and the snapshot's
	// size, the ring bounds where a creature may appear, and the two radii bound where
	// it may not. See spawn.go, which is the only file that reads any of them.

	// MobsPerPlayer is how many mobs may be alive inside one player's streamed cube.
	//
	// Six is a night that keeps arriving without being a siege: a draugr takes three
	// blows and swings back every 1.5 seconds, so six of them at once is already more
	// than one player can trade with — and the ring puts them 32 blocks out, so they
	// arrive as a handful rather than as a wall.
	//
	// Measured on the *streamed cube* rather than on a radius of its own, because that
	// is the volume a client is actually sent: a cap counted over anything else would
	// be a number about a set of creatures nobody can see.
	MobsPerPlayer = 6

	// MobsPerPlayerWorldwide is the multiplier on the world's ceiling: at most this
	// many mobs times the number of connected players exist at once.
	//
	// **It is a backstop, and it is deliberately above MobsPerPlayer so that it stays
	// one.** The per-player cap is what binds in the ordinary case, and it should be —
	// it is the number that describes what one player can be made to face. What it
	// cannot see is a mob that has left every streamed cube: those still exist for up to
	// MobDespawnGrace and count towards nothing, so a player walking hard across a world
	// at night spawns a fresh six into each new cube while the last six are still
	// expiring behind them. This is what bounds that tail. It also catches the moment
	// after a disconnect, when the world holds more creatures than the players who are
	// left justify: spawning stops until the population drains rather than the surplus
	// being killed off, because deleting a creature somebody is fighting is a worse
	// answer than not adding another one.
	//
	// Zero players is zero mobs, which is the same answer MobDespawnGrace reaches from
	// the other direction — and neither is load-bearing alone.
	MobsPerPlayerWorldwide = 8

	// MobSpawnRingInner and MobSpawnRingOuter bound how far from a player a creature
	// may appear, in blocks, measured between block columns.
	//
	// **The inner radius is chosen against the widest aggro range in mobRegistry and
	// must stay above every one of them.** Anything spawned inside its own aggro range
	// is hunting the player on the tick it arrives — a creature that materialises
	// already swinging, which reads as the server cheating rather than as the dark
	// being dangerous. The widest today is the vargr's twenty blocks, and thirty-two
	// keeps room over it: far enough that walking towards one is a decision, close
	// enough that the night finds you rather than the other way round.
	//
	// **The outer radius is chosen against the streamed cube and must stay inside it.**
	// At the default view distance of 3 the cube reaches at least 96 blocks from the
	// player on every axis, so 72 fits with room to spare even on the diagonal. It is
	// not *derived* from the view distance, because that is an operator's setting and
	// the distance at which the world feels populated is not: what the director does
	// instead is refuse a candidate outside the cube, so a server streaming less
	// terrain gets fewer spawns rather than creatures standing on ground its players
	// have never been sent.
	MobSpawnRingInner = 32
	MobSpawnRingOuter = 72

	// MobSpawnSeparation is how far a new creature must stand from every existing one,
	// in blocks between bodies.
	//
	// Six rather than "not overlapping", so that a night's spawning spreads into the
	// dark instead of stacking in whichever column happened to be legal first. It is
	// under the ring's width, so it constrains where a spawn lands without ever being
	// able to empty the ring.
	MobSpawnSeparation = 6.0

	// CampfireSafeRadius is how much ground a lit campfire keeps clear, in blocks.
	//
	// **Declared here, by the issue that first reads it.** The campfire structure, its
	// item and its recipe arrive after the director and consume this constant; they do
	// not declare it again. A constant defined in two places is a constant that will
	// eventually hold two values, and which of the two issues owns it was decided by
	// which of them needed it first.
	//
	// Sixteen is twice the ring's inner radius minus the draugr's aggro range, which is
	// the relationship that matters: a fire is a patch of ground worth sleeping on, and
	// one smaller than the range a creature notices you from would be a patch of ground
	// you can be reached across while standing in the middle of it. What actually keeps
	// anything from arriving at the fire's edge is MobSpawnRingInner, which is wider
	// than every aggro range in mobRegistry; this radius is the second line, and whether
	// it should widen to cover the vargr's twenty is a balance decision for whoever owns
	// the constant next.
	CampfireSafeRadius = 16.0

	// SpawnDirectorInterval is how often the director tries to place a creature.
	//
	// Converted to ticks at the configured rate, for the reason every other duration
	// here is: a night has to refill at the same speed on a 5 Hz server as on a 60 Hz
	// one. The two removals the director also performs are *not* on this cadence — see
	// spawn.go, which is where the split is argued.
	SpawnDirectorInterval = 1 * time.Second

	// MobDespawnGrace is how long a mob may stand outside every connected player's
	// streamed cube before it is removed.
	//
	// Long enough that a player stepping over a chunk boundary and back does not
	// despawn whatever is chasing them, short enough that walking away from a fight
	// really does end it. With nobody connected everything is outside every cube, so an
	// empty server is empty five seconds later.
	MobDespawnGrace = 5 * time.Second

	// --- The swing --------------------------------------------------------------
	//
	// What a swing is worth, and the only copy of it. The client sends the intent and
	// draws the answer; every number below is checked here and nowhere else, which is what
	// makes a forged client gain nothing by disagreeing with any of them.
	//
	// **What one blade is worth is deliberately not here.** Reach, arc and cadence
	// describe the *swing*, and every weapon swings the same way; damage and wear describe
	// the *blade*, and they live beside the registry that owns every other per-item stat —
	// `RustySwordDamage`, `IronSwordDamage` and their durabilities, in items.go. A second
	// weapon is a registry entry rather than an edit here, which is the whole reason the
	// split exists.

	// SwordReach is how far a swing carries, in blocks between bodies.
	//
	// Measured body to body like the draugr's own reach, and longer than it: a player
	// who holds the edge of their range is trading on purpose rather than by accident.
	SwordReach = 2.5

	// SwordConeDegrees is the total width of the arc a swing covers, centred on where
	// the player is looking. Ninety degrees is generous enough that aiming is a
	// direction rather than a pixel, and narrow enough that turning away misses.
	SwordConeDegrees = 90

	// SwordCooldown is how long after a swing before another may be accepted.
	//
	// It is paid on every valid swing, hit or miss. That is what stops a forged client
	// from probing every tick for a target it cannot see: asking costs the same as
	// connecting.
	SwordCooldown = 600 * time.Millisecond
)
