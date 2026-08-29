package game

import "time"

const (
	// WaterTickDelay is the flow settling delay.
	WaterTickDelay uint64 = 5

	// WaterChangesPerTick is 32 decodes/frame * 60 fps / 20 ticks/s.
	WaterChangesPerTick = 32 * 60 / 20
)

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
	// MaxChatBytes bounds one accepted world-chat line after surrounding whitespace
	// is trimmed. Four times the character-name allowance makes room for a line, not
	// a paragraph, and bytes are what the wire and its buffers pay for.
	MaxChatBytes = 256

	// ChatBurst is how many accepted lines one identity may send immediately. The
	// bucket starts full; ChatRefillPerSecond restores one line of credit each second.
	ChatBurst           = 5
	ChatRefillPerSecond = 1.0

	// MaxPartySize includes the leader. Five keeps a cooperative group large enough
	// for every current role without making out-of-view state an unbounded fan-out.
	MaxPartySize = 5

	// PartyInviteTTL is how long an unanswered invitation remains actionable. NewSim
	// converts it once to authoritative ticks; a client only presents the remaining
	// duration and never decides expiry.
	PartyInviteTTL = 60 * time.Second

	// PartyOfflineGrace is how long a disconnected character keeps its ordered
	// roster slot. NewSim turns it into ticks so expiry is authoritative,
	// deterministic and independent of scheduler timing.
	PartyOfflineGrace = 10 * time.Minute

	// PartyShareRadius is how near an online party member must stand to share a kill.
	// Thirty-two blocks is twice the draugr's sixteen-block aggro range, so everybody
	// spread across one fight counts, and well inside the default streamed view.
	PartyShareRadius = 32.0

	// CorpseLifetime is how long an owned normal-mob container remains lootable.
	// NewSim converts it to authoritative ticks; opening a corpse never moves the
	// deadline and no wall clock participates in expiry.
	CorpseLifetime = 10 * time.Minute

	// WalkSpeed is the horizontal speed of a player at full intent, in blocks per
	// second. Roughly a brisk walk at one block to the metre.
	//
	// It is a *speed*, not an acceleration: horizontal velocity is set from the
	// intent each tick rather than accumulated, so there is no momentum to exploit
	// and stopping is immediate. Acceleration curves are a feel issue and belong
	// with the one that gives the client prediction.
	WalkSpeed = 4.3

	// StarvingSpeedScale is the fraction of WalkSpeed available at zero hunger.
	// WalkSpeed's 4.3 multiplied by 0.8 is 3.44, which still — barely — outruns
	// the draugr's 3.2: starvation costs mobility without handing the slowest
	// predator a guaranteed kill.
	StarvingSpeedScale = 0.8

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

	// The swim rules: what a body does while its box overlaps water. Four numbers,
	// all of them relationships against the three above rather than tastes of their
	// own.
	//
	// **Water replaces gravity; it does not fight it.** A drag term that opposed
	// Gravity would need a mass and a coefficient to be anything but a second guess,
	// and the result would still be a terminal velocity — so this integrates straight
	// to the terminal velocity and skips the invention. What a player feels is the
	// same either way: you stop falling and start sinking.
	//
	// SwimSinkSpeed is one block a second, which is slow enough to read as floating
	// and fast enough that a player who does nothing reaches the bottom of a
	// ten-block basin in ten seconds. SwimRiseSpeed is three, a third of
	// JumpImpulse: rising out of water is deliberate and unhurried where a jump is a
	// shove. SwimAcceleration is how fast the vertical speed eases toward whichever
	// of those applies — twelve blocks per second squared is well under Gravity's
	// twenty-eight, so entering water decelerates a fall over about a second rather
	// than stopping it dead, and leaving the surface on a rise costs no jerk either.
	//
	// SwimSpeed is horizontal, and it is a fraction of WalkSpeed rather than a number
	// because what is being said is "slower than walking". Six tenths is 2.58 blocks
	// a second: still faster than nothing, and slower than the draugr on the bank.
	SwimSinkSpeed    = -1.0
	SwimRiseSpeed    = 3.0
	SwimAcceleration = 12.0
	SwimSpeed        = WalkSpeed * 0.6

	// PlayerWidth is the edge of the player's square footprint, in blocks.
	//
	// Under one block on purpose: a body wider than the grid cannot fit through a
	// one-block gap, and a corridor a player cannot walk down is a level-design
	// problem created by a constant.
	PlayerWidth = 0.6

	// PlayerHeight is how tall the player's box is, in blocks.
	PlayerHeight = 1.8

	// PlayerMaxHealth is the level-one health maximum. Higher levels add
	// HealthPerLevel through maxHealthFor, and the resulting per-player value is the
	// denominator of every health display the client draws.
	PlayerMaxHealth = 100

	// ArmourScale is the denominator for the percentage points every wearable row
	// records in itemRegistry. A value of 30 therefore leaves seventy percent of a
	// mob's blow. TestEveryWearableCombinationFitsTheArmourScale sweeps the strongest
	// piece for each body slot and keeps every possible worn sum below this number, so
	// the subtraction at the strike cannot underflow.
	ArmourScale uint16 = 100

	// ThreatScale is the denominator for the tenths of hostile attention a worn
	// item records. Full iron contributes fifteen, so its weight is 2.5: ten damage
	// generates twenty-five threat. The distance / weight comparison uses the same
	// number only when no player in range has positive ledger threat, preserving how
	// an untouched creature acquires its first target.
	ThreatScale = 10

	// ThreatSwitchRatio is the lead another player must exceed before a hostile
	// creature abandons a valid current target. The comparison is strict: against
	// forty threat, forty-four is not enough and anything greater is.
	ThreatSwitchRatio = 1.1

	// ThreatDecayPerSecond is what every remembered player loses after a full second
	// in which the creature is outside Chase, Windup and Recovery.
	ThreatDecayPerSecond = 1.0

	// ThreatForgetSeconds is how long a hostile creature may have no target before
	// forgetting the ledger whole. It is converted to ticks once by NewSim.
	ThreatForgetSeconds = 10

	// ShieldTauntThreat is the attention earned when a raised shield absorbs a landed
	// mob blow. The shield issue owns deciding that a block landed; this package owns
	// what that authoritative outcome means to the attacker.
	ShieldTauntThreat = 10.0

	// HealthPerLevel is how much maximum health each level after the first adds.
	//
	// Five makes a level-30 player reach 245 health, 2.45 times the level-one base.
	// Ten would reach 390 and make today's non-scaling mobs irrelevant long before the
	// cap; five makes progression tangible while their existing damage remains useful.
	HealthPerLevel uint16 = 5

	// PlayerMaxHunger is a full food reserve and the denominator of every hunger
	// display. Zero remains a living state: it stops regeneration and does nothing
	// else by itself.
	PlayerMaxHunger = 100

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

	// HungerDrainInterval is how much connected, living play costs one point of
	// hunger. At PlayerMaxHunger, 432 seconds per point is twelve hours from full to
	// empty and six hours from half to empty. It is converted to ticks once by NewSim;
	// wall time never advances it.
	HungerDrainInterval = 432 * time.Second

	// HealthRegenPointsPerHunger is how many health points one point of food pays
	// for. Two makes a recovery from near death cost roughly half a full reserve,
	// while hunger zero remains a hard stop rather than a debt.
	HealthRegenPointsPerHunger uint16 = 2

	// RespawnHungerFloor is the minimum reserve a new life receives. A death never
	// lowers a better-fed player, and it never leaves an empty one unable to recover.
	RespawnHungerFloor uint16 = 50

	// --- The creatures ----------------------------------------------------------
	//
	// **Their numbers are not here.** Health, speed, reach, damage, telegraph, body and
	// whether the dark is what brings one out are per-species rows in `mobRegistry` —
	// species.go — for the reason a blade's damage sits beside `itemRegistry` rather than
	// here: they describe one *creature*, and nothing about any of them generalises to the
	// next.
	//
	// **There is no death duration here any more, and its absence is the point.**
	// MobDeathDuration was two and a half seconds a killed creature spent in
	// [vnet.MobActionDying] before its corpse existed, and it was defended as a statement
	// about when an item *exists*: a client with the animation turned off had to wait
	// exactly as long as one watching it, because the wait was not the animation's.
	//
	// That argument was sound and the premise under it was wrong. The wait was never
	// deciding *whether* the drop was earned — the killing blow decides that — only when
	// the player was allowed to reach for it, and two and a half seconds of not being
	// allowed to press F is a pause in a fight rather than a rule about it. A body still
	// goes down; the fall is drawn on the client, out of the same snapshot that already
	// says Corpse, and the client is still told nothing about how long it lasts. What was
	// deleted is the *server* waiting for an animation it does not play.
	//
	// So there is no number to convert to ticks and no state between the blow and the
	// corpse: [Sim.damageMobLocked] rolls the container on the tick the health runs out.
	// [vnet.MobActionDying] stays in the contract — a wire enumeration is not narrowed
	// because one server stopped sending one of its values — and no snapshot carries it.

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

	// --- The residents -----------------------------------------------------------
	//
	// The two numbers a village's people have. Neither is a fight and neither is on the
	// wire: a resident's yaw is computed here and echoed in the snapshot like every other
	// entity's, so a client that disagreed about either would gain nothing.

	// ResidentNoticeRadius is how close somebody has to be for a resident to look at
	// them, in blocks between bodies.
	//
	// Six, which is deliberately far short of the draugr's sixteen: this is the distance
	// at which a person walking past is *company*, not the distance at which something
	// has seen you. Inside a hut it is the whole room; outside it is about a doorway and
	// the path in front of it, so walking down a village street turns each of them in
	// turn rather than all of them at once.
	ResidentNoticeRadius = 6.0

	// ResidentTurnRate is how fast a resident turns, in radians per second.
	//
	// Three, so half a turn takes about a second: fast enough to read as somebody
	// noticing you and slow enough that a player circling one does not make it spin.
	// A rate rather than a snap is also what makes the return to rest cost nothing extra
	// — the same arithmetic turns a resident back once you have gone.
	ResidentTurnRate = 3.0

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

	// BowCooldownTicks is derived from this duration per simulation. One second is
	// deliberately its own cadence rather than an alias of the blade's recovery.
	BowCooldown     = 1 * time.Second
	SceptreCooldown = 750 * time.Millisecond

	// --- Projectiles ---------------------------------------------------------
	//
	// These are authoritative simulation numbers. The ranged-item registry only
	// decides which kind to spawn; flight, collision and effects are all resolved here.
	ProjectileMaxStep   = 0.5
	ProjectileBodySize  = 0.1
	ProjectileEyeHeight = PlayerHeight * 0.9
	ArrowDamage         = 15
	OrbDamage           = 8
	OrbHeal             = 10
	ArrowSpeed          = 30.0
	OrbSpeed            = 16.0
	// ProjectileMaxLaunchSpeed bounds accepted spawn inputs independently of the
	// caller. Gravity may make an arrow faster later; that acceleration remains
	// bounded by TerminalFallSpeed and the finite flight lifetime.
	ProjectileMaxLaunchSpeed = ArrowSpeed
	ArrowLifetime            = 5 * time.Second
	OrbLifetime              = 1500 * time.Millisecond
	// ArrowStuckTicks is converted to the configured server's ticks by NewSim.
	// The name records what the projectile stores; the constant remains a duration
	// so three seconds means the same thing at every tick rate.
	ArrowStuckTicks = 3 * time.Second

	// --- The weather that bites --------------------------------------------------

	// WeatherHeavy is where weather stops being scenery, on the 0..255 intensity the
	// wire carries.
	//
	// **One threshold for all three effects, and that is a decision rather than an
	// economy.** Three numbers would be three balance dials nobody could hold in their
	// head at once, and worse, they would make "it is heavy out" mean a different thing
	// depending on what was falling — a player who has learnt that a sandstorm shortens
	// their arm would have no way to read whether this snow is deep enough to slow them.
	// One number means the sky says one thing, and the kind decides only *which* rule it
	// says it about.
	//
	// 160 of 255 is the top 37% of the scale, and it is read against what the field
	// actually produces rather than against the range: world.WeatherAt ramps 1..255
	// between its clear threshold and the p99.9 of its measured distribution, so an
	// intensity is already a percentile of the weather that happens rather than of the
	// weather that could. Below it nothing at all applies — there is no ramp, no partial
	// scale and no interpolation, because a reach that shrank continuously would be a
	// reach a player could never learn the edge of, and every refusal here is silent.
	WeatherHeavy = 160

	// SandstormReachScale is the fraction of EditReach a player keeps in a heavy
	// sandstorm.
	//
	// Half, which puts the reach at 2.25 blocks. That still clears the block under the
	// feet (1.4) and the block over the head (1.6), so a player caught out in one can
	// still dig down and roof over — being unable to shelter would make the storm a
	// death sentence rather than a cost. What it takes away is the shaft dug from its
	// edge (3.5) and the block placed at arm's length: building in a sandstorm becomes
	// something you do standing on top of the work.
	SandstormReachScale = 0.5

	// SnowSpeedScale is the fraction of walking speed left in heavy snow.
	//
	// WalkSpeed's 4.3 multiplied by 0.7 is 3.01, which is *below* the draugr's 3.2 —
	// deliberately, and it is the one place a weather effect is allowed to be worse than
	// starvation's. StarvingSpeedScale is 0.8 precisely so that a starving player still
	// outruns the slowest predator; deep snow is a condition you can see coming and walk
	// out of, so it is allowed to be the thing that makes you turn and fight. The two
	// compose — 2.41 at zero hunger in deep snow — because being starved in a blizzard
	// is worse than either, and nothing here needs a special case to say so.
	SnowSpeedScale = 0.7
)
