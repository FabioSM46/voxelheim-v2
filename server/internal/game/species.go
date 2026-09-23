package game

import (
	"slices"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// What each species *is*, and the only copy of it.
//
// mob.go is what a creature does; spawn.go is what puts one in the world and takes it
// away again. This file is the table both of them read, keyed by the same [vnet.MobKind]
// that crosses the wire.
//
// **It is the move itemRegistry made, for the same reason.** "How big is its body",
// "how far does it notice you" and "does it survive the sun" are now lookups keyed by a
// kind rather than comparisons against one, so the third species is a row here instead
// of an edit to combat, to the collision, to the snapshot and to the director. The
// concrete bug that shape avoids was already in the code: swingTargetLocked built
// draugrBody.boxAt(m.pos) for every mob it considered, which would have given a vargr
// the reach of a draugr the moment a second species existed.
//
// **Every species shares one state machine, and disposition is a column in this table.**
// Hostile rows choose a target and attack it; passive rows choose a threat and flee. The
// movement, collision, death and loot paths remain one of each rather than one copy per
// creature.

// mobDefinition is the server-only rule for one species.
//
// Nothing here is sent to a client. A snapshot carries the kind, the position, the
// health and the action; how fast the thing walks and how far it can reach are the
// server's answers, and a client that disagreed about either would gain nothing.
//
// **Every common field below is a real number, while attack fields are conditional.**
// A hostile row needs a complete attack; a passive row must set every attack field to
// zero because it never enters those states. Health, speed, awareness, body and loot are
// required for both. `passive` and `nocturnal` are positive boolean statements.
// TestEverySpeciesIsFullyDescribed is what holds that line rather than this paragraph.
//
// **loot is held to the same rule, which is why an empty table is not allowed either.**
// The user story this world is built on is that hunting has to be worth the durability it
// costs, so a species that leaves nothing is a decision somebody would have to argue for
// rather than a field they can forget. The sweep refuses the empty slice for exactly that
// reason: today the answer is "no such species", and the day there is one, this is where
// it gets written down.
type mobDefinition struct {
	// rank classifies the encounter contract for this species. Unknown is not a
	// usable default: every registry row must explicitly say normal or boss, so a
	// future boss cannot silently take the normal round-robin loot path.
	rank mobRank

	// maxHealth is what one arrives with, and the denominator of the health a snapshot
	// carries for it.
	maxHealth uint16

	// experience is the lifetime progress one kill is worth. It is required even for
	// passive prey: hunting a deer still spends time and durability, and a zero here
	// would make forgetting the reward indistinguishable from choosing none.
	experience uint16

	// speed is how fast it closes on a target, in blocks per second. Read against
	// [WalkSpeed], which is what decides whether running away is an answer.
	speed float64

	// aggroRange is how far off it notices a player, in blocks measured between bodies.
	//
	// **[MobSpawnRingInner] must stay above the widest of these**, or a creature arrives
	// already hunting — which reads as the server cheating rather than as the dark being
	// dangerous. TestTheSpawnGeometryHangsTogether asks the whole registry, not one row.
	aggroRange float64

	// passive selects the non-attacking branch of the shared state machine. Its zero is
	// deliberately hostile, so an old row does not silently stop fighting when this
	// column is added.
	passive bool

	// attackRange is how close it has to be to swing, in blocks between bodies. Under
	// [SwordReach], so a player who holds the edge of their own reach is not trading
	// blows evenly.
	attackRange float64

	// damage is what one landed blow costs a player, measured against the
	// [PlayerMaxHealth] level-one maximum.
	damage uint16

	// windup is the telegraph: the swing is committed and has not landed yet. It is what
	// makes an attack something a player can react to rather than something that simply
	// happens.
	//
	// recovery is how long after a swing before another may begin, and every attack pays
	// it whether or not it landed — which is what makes attack cadence the server's
	// answer instead of a consequence of the tick rate.
	//
	// Durations rather than tick counts, converted per server by [mobTimingsFor]: six
	// hundred milliseconds is six hundred milliseconds on a 5 Hz server and on a 60 Hz
	// one, or it is not a telegraph.
	windup   time.Duration
	recovery time.Duration

	// phaseHealthPercents is where a boss's repertoire changes, as remaining-health
	// percentages in descending order.
	//
	// **A boss row has at least one and every other row has none.** A stage is the
	// encounter contract this species fights under, and a creature with no stages is not
	// a creature whose stages were forgotten — TestEverySpeciesIsFullyDescribed holds
	// both halves of that. `encounterPhaseFor` counts how many of these the current
	// health has passed, so the wire's stage ordinal is one more than that count.
	//
	// Playtest values from the approved design, not final balance: the guardian tears
	// the last strap at just over half, and the king is a duellist, then a ritualist,
	// then a cracked-open thing that combines the two.
	phaseHealthPercents []uint8

	// body is the box this species occupies, and the only statement of it.
	//
	// **Read by the collision, by the swing that reaches it, by the separation the
	// director keeps between spawns and by the step it may hop.** A second box spelled
	// anywhere else is the bug this field exists to make impossible: it would agree with
	// this one for exactly as long as nobody rebalanced either.
	body body

	// nocturnal says the dark is the only thing that brings this species out.
	//
	// **A property of the creature, not of the spawn rule**, which is why the director
	// asks the registry which species may arrive rather than checking the clock and then
	// naming one. The same sentence has to be true from both ends: a nocturnal creature
	// arrives only at night and does not outlast the night either, and what survives the
	// dawn is one that is already hunting somebody — for exactly as long as that hunt
	// lasts. A species that is not nocturnal arrives at any hour and the dawn is nothing
	// to it.
	//
	// The false here is a real answer rather than an unset field; see the type's doc.
	nocturnal bool

	// loot is what a *kill* stores in its corpse, one line per item, rolled from the
	// simulation's own generator — see loot.go, which owns the roll and the spawn.
	//
	// **A field of the row rather than a table of its own**, for the reason every number
	// above is one: what a creature is worth killing for belongs beside what it costs to
	// kill, and a second map keyed by [vnet.MobKind] would be a second place a third
	// species has to be remembered in.
	//
	// **A kill, and nothing else.** A mob the director takes away — at dawn, or because
	// nobody has been near it for five seconds — leaves nothing, and the two removals in
	// spawn.go say so by not asking this field. Loot is the reward for the kill; a world
	// that paid it out for having existed would be a world where waiting is a strategy.
	loot []lootRoll

	// armour is the share of every player-authored blow this species' hide turns aside,
	// in percentage points of [ArmourScale] — the worn multiplier a player's armour
	// applies to a creature's blow, pointed the other way. Zero is the ordinary answer:
	// flesh, fur and dead skin take a blade at its full worth. See [mob.armoured], the
	// one place it is spent.
	armour uint16

	// swipe is a second, lighter attack this species alternates with the one above, or
	// the zero value for a species that has one attack. The main attack is always the
	// first one committed, so a creature with a heavy blow opens with it; after every
	// landed or whiffed swing the next is the other one. Alternation rather than a
	// choice by distance, because a telegraph a player can learn is a rhythm, and one
	// chosen from a range they cannot see is not.
	swipe mobAttack

	// emergeRange is how close a live player must come, body to body and measured from
	// where the creature would stand once risen, before a buried one breaks the surface.
	// Zero for a species that never lies buried; see dungeon_minor.go for the state.
	//
	// emergence is how long rising takes, spent as the recovery that precedes its first
	// attack: the wire has no emerging member and Recovery already says "cannot attack
	// until this expires", which is exactly what a creature still shaking off the sand is.
	emergeRange float64
	emergence   time.Duration

	// dungeonOnly says this species exists only where an instance places it. The open
	// world's director never offers one, whatever the hour — see [spawnableSpecies].
	// Like nocturnal, a property of the creature rather than of the spawn rule.
	dungeonOnly bool
}

// mobAttack is one attack's four numbers, the shape the main attack's fields have on the
// row itself. Used for a species' secondary attack; see mobDefinition.swipe.
type mobAttack struct {
	reach    float64
	damage   uint16
	windup   time.Duration
	recovery time.Duration
}

// minorLoot is the ordinary minor loot table: what every lesser creature of the descent
// leaves, one bone and nothing else. Shared by name so the minor rows cannot drift apart,
// and deliberately without silver — money in this world comes off the draugr, and
// TestSilverIsReservedCurrencyDroppedOnlyByDraugr holds that line for every row. A bone
// is the generic remains every other creature already yields, so the descent's minor
// kills feed the same economy the surface does rather than inventing a new item.
var minorLoot = []lootRoll{{item: ItemBone, min: 1, max: 1}}

type mobRank uint8

const (
	mobRankUnknown mobRank = iota
	mobRankNormal
	mobRankBoss
)

func (d mobDefinition) isBoss() bool { return d.rank == mobRankBoss }

func (r mobRank) valid() bool { return r == mobRankNormal || r == mobRankBoss }

// mobRegistry is every species this world can hold, and the only place their numbers
// live.
//
// Deliberately not sent to clients, exactly as itemRegistry is not: a client renders
// what a snapshot says and may have an opinion about how to draw it, but only this table
// decides how far a creature reaches or how much of it there is to hit.
var mobRegistry = map[vnet.MobKind]mobDefinition{
	// The draugr — the first thing in this world that was not scenery, and still the one
	// the numbers below are read against.
	//
	// Its 60 health is the scale the blades are balanced on: three rusty swings kill one
	// and two iron ones do (see itemRegistry). Its speed is deliberately under
	// [WalkSpeed], so a player who turns and runs can leave — close enough that walking
	// away is a decision rather than a formality. Its box is the player's dimensions,
	// because a draugr is a humanoid corpse, but stated here in full rather than written
	// as PlayerWidth and PlayerHeight: narrowing a corridor for players must not silently
	// narrow the thing that hunts them down it. Fifteen experience is the baseline reward
	// for that 60-health fight: meaningful progress, but deliberately short of a level
	// after three kills.
	vnet.MobKindDraugr: {
		rank:        mobRankNormal,
		maxHealth:   60,
		experience:  15,
		speed:       3.2,
		aggroRange:  16.0,
		attackRange: 2.0,
		damage:      10,
		windup:      600 * time.Millisecond,
		recovery:    900 * time.Millisecond,
		body:        body{width: 0.6, height: 1.8},
		nocturnal:   true,
		// One or two bones, which is the only variable count in the game and is variable
		// on purpose: a draugr is the creature a player kills most, and a fixed yield
		// would make a night's hunting arithmetic rather than a run of luck. Nothing
		// consumes bones yet — see ItemBone, where that is a decision rather than an
		// omission.
		// Two to six silver beside the bones, and the wider range is the point of it: a
		// draugr is the creature a player kills most, so it is the only creature whose
		// takings can be a *purse* rather than a fixed wage. Nothing sells anything yet —
		// see ItemSilver, where that is a decision rather than an omission — and the
		// vargr and the deer deliberately carry none, so money in this world comes off the
		// thing the night sends after you.
		loot: []lootRoll{
			{item: ItemBone, min: 1, max: 2},
			{silver: true, min: 2, max: 6},
		},
	},

	// The vargr — something faster than you.
	//
	// **Its speed is above [WalkSpeed] of 4.3, and that is the whole point of it: you do
	// not outrun a vargr, you turn and fight it.** Everything else about the row pays for
	// that one number. It has 35 health against the draugr's 60, so it dies in one iron
	// swing where a draugr takes two and in two rusty ones where a draugr takes three;
	// its blow costs 7 rather than 10; and its windup is 400 ms rather than 600, which is
	// a shorter telegraph and therefore the harder one to read. It notices you from 20
	// blocks rather than 16 — being seen is what starts the chase, and a creature you
	// cannot outrun that has to find you first is a different threat from one that
	// cannot be escaped once it exists.
	//
	// It is **not** nocturnal, which is the second half of the same design: the draugr
	// is what the night brings and the vargr is what the daylight does not save you from.
	//
	// Its body is wider and much shorter than a draugr's — a beast on four legs rather
	// than a corpse on two — and the width is what makes the box worth reading from this
	// table rather than assuming: at the same standing distance a vargr is inside a
	// sword's reach where a draugr is not, because a swing is measured body to body.
	// Twenty experience prices that speed above the draugr's fifteen even though the
	// vargr has less health: the fight is worth more because walking away is not an answer.
	vnet.MobKindVargr: {
		rank:        mobRankNormal,
		maxHealth:   35,
		experience:  20,
		speed:       5.4,
		aggroRange:  20.0,
		attackRange: 1.8,
		damage:      7,
		windup:      400 * time.Millisecond,
		recovery:    700 * time.Millisecond,
		body:        body{width: 0.9, height: 1.0},
		nocturnal:   false,
		// Exactly one pelt, and the fixed count is the balance rather than a
		// simplification: two pelts make one patch, so a vargr is half a repair and
		// killing two is a decision a player can make on purpose. A variable yield would
		// put that between them and the arithmetic.
		loot: []lootRoll{{item: ItemVargrPelt, min: 1, max: 1}},
	},

	// The deer is prey rather than an enemy. Its aggro range is awareness: inside it a
	// live player makes the deer flee, and the wider release radius in mob.go prevents a
	// body standing on the boundary from switching state every tick. Five experience is
	// the low end of the hunt: only 20 health, below both predators, but its speed still
	// makes bringing down food an active pursuit rather than free progress.
	vnet.MobKindDeer: {
		rank:       mobRankNormal,
		maxHealth:  20,
		experience: 5,
		speed:      4.0,
		aggroRange: 12.0,
		passive:    true,
		body:       body{width: 0.9, height: 1.4},
		nocturnal:  false,
		loot:       []lootRoll{{item: ItemRawMeat, min: 1, max: 2}},
	},

	// The Vargr that guarded the tomb — the first row in this game to carry
	// [mobRankBoss], and a species of its own rather than a bigger vargr. Everything
	// below is read against the draugr's 60 health and the two blades in itemRegistry,
	// which is what every other row here is priced against.
	//
	// **4,200 health is what each member of the party brings, not what the boss has.** The
	// pull multiplies it by the members inside the run, clamped to three to five, and never
	// by their levels or their equipment — see boss_scale.go, which owns the rule and argues
	// it. At 720 for any party the #1037 harness measured every fight between 3.9 and 32.5
	// seconds against the approved design's three to four minutes
	// (docs/reviews/dungeon-combat-1037.md); #1099 then calibrated 10,500 from the same
	// harness, when a swing cost nothing. **#1127's energy economy moved that number and
	// #1332 moves it back to the target**: a swing costs 25 of a reserve refilling 12.5 a
	// second, so a reader lands one iron blow every two seconds rather than every 0.6, and
	// 10,500 measured 524.6, 526.1 and 519.2 seconds for three, four and five iron readers —
	// 0.050 seconds per point of per-member health, kill time following health linearly.
	// 210 seconds, the middle of the target, asks for 4,200.
	// dungeon_route_estimate_test.go's energyReaderKills records the kills at this number.
	// The length still comes from the moves: no telegraph, recovery or opening was
	// shortened to reach it.
	//
	// **Its 4.0 speed is under [WalkSpeed] of 4.3, and that is the criterion rather than
	// a coincidence.** The field vargr is 5.4 — *above* a player, which is the whole
	// point of it: you do not outrun a vargr. A boss standing between a party and the
	// only door must not have that property, because a party that cannot retreat has no
	// decision to make. Three tenths of a block per second is a narrow margin on purpose:
	// leaving is possible, slow, and costs the ground you crossed.
	//
	// 22 damage against [PlayerMaxHealth]'s hundred is five blows on an unarmoured
	// level-one player, against the field vargr's seven and the draugr's ten. The 900 ms
	// windup is the near end of the approved design's 0.9–1.5 s band and more than twice
	// the field vargr's 400 ms: a boss blow is *readable*, and what makes it dangerous is
	// what it costs rather than how little warning it gives.
	//
	// Its body is the field vargr's proportions at a size no player mistakes from across
	// a room — 1.6 wide against 0.9, and 1.8 tall, a draugr's height on a beast's stance.
	// Its aggro range covers any arena it can be placed in, so entering the room is the
	// pull; it stays well under [MobSpawnRingInner] like every other row, which the
	// geometry sweep asks of the whole registry whether or not the director can place it.
	//
	// **120 experience is above the field vargr's 20, which the criterion requires, and
	// it is six times it rather than one more.** Eight draugr at 15 apiece is what one of
	// these is worth, because a party splits it and a boss is not a thing you grind.
	//
	// It is not nocturnal, and there is no other honest answer: a sealed chamber has no
	// sky, so the field that decides whether the dark brings a creature out has nothing
	// to say about one that is placed rather than spawned.
	vnet.MobKindVargrGuardian: {
		rank:        mobRankBoss,
		maxHealth:   4200,
		experience:  120,
		speed:       4.0,
		aggroRange:  24.0,
		attackRange: 2.2,
		damage:      22,
		windup:      900 * time.Millisecond,
		recovery:    1300 * time.Millisecond,
		// One change of stage, at the strap that finally tears.
		phaseHealthPercents: []uint8{55},
		body:                body{width: 1.6, height: 1.8},
		nocturnal:           false,
		// Three to five pelts and a pair of bones. The count is the reward: two pelts
		// make one leather patch, so a field vargr is half a repair and this is one and
		// a half to two and a half of them at once. No silver — money in this world
		// comes off the thing the night sends after you, which is the draugr, and
		// TestSilverIsReservedCurrencyDroppedOnlyByDraugr is where that is held.
		loot: []lootRoll{
			{item: ItemVargrPelt, min: 3, max: 5},
			{item: ItemBone, min: 2, max: 2},
		},
	},

	// The Draugr king at the far end — the second boss-rank row, and the harder half of
	// the same encounter. Read against the Vargr guardian above as well as against the
	// draugr, because the design places it second and nothing about it should read as a
	// repeat of the first fight.
	//
	// **6,600 health is what each member brings**, on the guardian's rule. #1099 set 16,383,
	// the largest per-member health whose four-member total fit the uint16 the wire carries
	// health in; #1332 changes both halves of that. A party of five is now counted, so the
	// bound is a fifth of the wire's ceiling, 13,107 — and under #1127's energy economy
	// 16,383 measured 816.7 and 814.7 seconds for three and four iron readers, over thirteen
	// minutes against the approved design's five to six. Kill time follows health at 0.050
	// seconds a point, as the guardian's does, so the middle of the target, 330 seconds,
	// asks for 6,600 — and five times 6,600 is 33,000, half the wire's ceiling, so the
	// target is reached at every party size for the first time.
	// TestBossHealthFitsTheWireAtEveryScale holds the bound.
	//
	// **Its 3.0 speed is the slowest hostile row in the game**, under the field draugr's
	// 3.2 and well under [WalkSpeed]. It is armoured and it carries a two-handed blade;
	// retreating across its hall is a real answer, and the price of taking it is that the
	// king is still standing when you come back. The criterion asks only for "below
	// WalkSpeed"; this is further below it than the guardian on purpose, because the two
	// bosses must not read as one creature at two sizes.
	//
	// 28 damage is four blows on an unarmoured level-one player and the heaviest in the
	// registry — a great funerary sword, swung by something that no longer tires. It pays
	// for that with the longest telegraph in the game at 1200 ms and an 1800 ms recovery,
	// both inside the approved design's bands (0.9–1.5 s signals, 1.2–2 s recoveries).
	// Its 2.4 reach is the longest here and still under [SwordReach]'s 2.5, so holding
	// the edge of your own reach still buys something — barely, which is what a
	// two-handed weapon should feel like.
	//
	// Its body is a draugr's 0.6 width taken to 1.0 by layered plate, at 2.8 tall: a
	// vertical silhouette against the guardian's low one, which is the readability the
	// design asks for and which the client's body row mirrors.
	//
	// 200 experience is above the guardian's 120 and ten times the field vargr's 20.
	// Not nocturnal, for the guardian's reason: a sealed hall has no sky.
	vnet.MobKindDraugrKing: {
		rank:        mobRankBoss,
		maxHealth:   6600,
		experience:  200,
		speed:       3.0,
		aggroRange:  24.0,
		attackRange: 2.4,
		damage:      28,
		windup:      1200 * time.Millisecond,
		recovery:    1800 * time.Millisecond,
		// Duel, then ritual, then the armour cracked open and both together.
		phaseHealthPercents: []uint8{70, 35},
		body:                body{width: 1.0, height: 2.8},
		nocturnal:           false,
		// The king's own iron sword, and the bones of what he was. One blade, fixed:
		// stackOf gives it full durability, so this is a whole second weapon rather than
		// a fraction of one, and a fixed count is what keeps a boss's reward a *thing*
		// the party divides rather than a number they roll. No silver, for the reason
		// the guardian carries none.
		loot: []lootRoll{
			{item: ItemIronSword, min: 1, max: 1},
			{item: ItemBone, min: 3, max: 5},
		},
	},

	// The cave spider — the descent's swarm, and the vargr's rule shrunk to fit a burrow.
	//
	// **Its 5.0 speed is above [WalkSpeed] of 4.3**, which is the criterion: nobody walks
	// away from a spider wave, you turn and cut. It stays under the vargr's 5.4, so the
	// surface's fastest hunter is still the fastest thing in the game. Everything else
	// pays for coming in numbers: 20 health dies to one rusty swing (25) or one iron one
	// (40), to two arrows (15) and to three orbs (8), so a wave is a count of blows
	// rather than a fight per spider.
	//
	// The bite is quick and light — 4 damage behind a 300 ms windup and a 600 ms
	// recovery. The shortest telegraph in the game, under the vargr's 400, and that is
	// what the cheap damage buys: one spider is a nuisance, five together are why the
	// cave is dangerous. 1.4 reach is a mouth, well under [SwordReach], so holding the
	// edge of your own reach keeps them off you one at a time.
	//
	// **Its body is 0.9 wide and 0.6 tall**, and both numbers are the cave's geometry:
	// under a block wide, so a spider fits the one-block burrows a wave pours out of, and
	// far under a player's height, so no ceiling a player can walk under stops one. The
	// client's body row mirrors these two numbers.
	//
	// 14 blocks of awareness — narrower than the draugr's 16, because the cave is dark
	// and close and a wave is triggered, not noticed. 6 experience is a deer's 5 and one
	// more for biting back: many kills, each worth little. Dungeon-only, and not
	// nocturnal: a cave has no sky.
	vnet.MobKindCaveSpider: {
		rank:        mobRankNormal,
		maxHealth:   20,
		experience:  6,
		speed:       5.0,
		aggroRange:  14.0,
		attackRange: 1.4,
		damage:      4,
		windup:      300 * time.Millisecond,
		recovery:    600 * time.Millisecond,
		body:        body{width: 0.9, height: 0.6},
		nocturnal:   false,
		loot:        minorLoot,
		dungeonOnly: true,
	},

	// The scorpion — the spider's opposite, and the sand hall's ambush.
	//
	// **Its 2.6 speed is the slowest in the game**, under the Draugr king's 3.0: a
	// scorpion is never chased away from, it is walked around. What it has instead is a
	// shell. **40 points of armour** leave sixty percent of every blow, the player's worn
	// multiplier pointed the other way: an iron swing lands for 24 rather than 40 and a
	// rusty one for 15 rather than 25, so its 72 health is three iron swings or five rusty
	// ones — one and two more than the same health would cost unarmoured. Arrows (9) and
	// orbs (4) are poor against it on purpose: the shell is a reason to close.
	//
	// **The sting is the heavy blow and it is readable**: 18 damage, the third heaviest
	// in the registry behind the two bosses, behind an 1100 ms windup that is the
	// longest of any normal species and inside the approved design's 0.9–1.5 s band for
	// a signal. 2.2 reach is the arched tail, still under [SwordReach]. A 1400 ms
	// recovery is the opening a player earns by reading it.
	//
	// **The pincer swipe is the other half of the rhythm**: 7 damage behind 450 ms at 1.5
	// reach, with a 700 ms recovery. Sting, swipe, sting — the heavy blow always opens,
	// so the first thing out of the sand is the one a player can see coming.
	//
	// **Its body is 1.3 wide and 0.6 tall** — low and wide, a vargr's width and a half
	// at a spider's height. The client's body row mirrors these two numbers.
	//
	// It lies buried until a live player comes within **5 blocks**, and rises over **800
	// ms** before it may strike — see dungeon_minor.go. 10 blocks of awareness once up:
	// wider than the ambush, so a scorpion that rose for one player keeps the fight
	// going with the party around them. 25 experience is above the vargr's 20: slower
	// than anything, but it takes longer to kill and hits harder than anything that is
	// not a boss.
	vnet.MobKindScorpion: {
		rank:        mobRankNormal,
		maxHealth:   72,
		experience:  25,
		speed:       2.6,
		aggroRange:  10.0,
		attackRange: 2.2,
		damage:      18,
		windup:      1100 * time.Millisecond,
		recovery:    1400 * time.Millisecond,
		body:        body{width: 1.3, height: 0.6},
		nocturnal:   false,
		loot:        minorLoot,
		armour:      40,
		swipe: mobAttack{
			reach:    1.5,
			damage:   7,
			windup:   450 * time.Millisecond,
			recovery: 700 * time.Millisecond,
		},
		emergeRange: 5.0,
		emergence:   800 * time.Millisecond,
		dungeonOnly: true,
	},
}

// mobByKind is one species' row, and whether the kind is one this server knows.
//
// The shape itemByID has, and it fails closed the same way: a kind nobody registered is
// not a creature with default numbers, it is a creature that cannot be made. See
// [Sim.spawnMobLocked], which is the one place that answer is acted on.
func mobByKind(kind vnet.MobKind) (mobDefinition, bool) {
	def, ok := mobRegistry[kind]
	return def, ok && def.rank.valid()
}

// species is this mob's registry row.
//
// Total rather than two-valued, and the spawn path is what makes it so: the only way a
// mob enters the world refuses a kind the registry does not hold, so every mob in
// Sim.mobs has a row waiting for it here.
//
// A lookup rather than a copy taken at creation, for the reason an itemDrop stores an
// ItemID rather than an itemDefinition: the table is the truth, and a copy is a second
// one that a balance pass would have to know to go and find.
func (m *mob) species() mobDefinition { return mobRegistry[m.kind] }

// spawnableSpecies is every kind that may arrive right now, in kind order.
//
// **This is the registry answering "who may spawn at this hour", and it is deliberately
// not the director asking "is it night" and then naming a draugr.** The clock question
// has one answer — [IsNight] — and the species question is a property of each row, so
// the two are composed here once instead of being spelled as a branch at the spawn site
// that a third species would have to be added to.
//
// **A boss-rank row is never offered, and this is the only place that has to say so.**
// spawn.go's four rules are about refilling the dark around a moving player: a ring
// around somebody who walked somewhere, a per-cube ceiling, a world ceiling, and a sweep
// that takes back what nobody has been near. Not one of them describes a fixed encounter
// standing in a sealed room, which is placed once when its session's world is made and
// is never respawned or reclaimed. The exclusion belongs *here* rather than in the
// director for the reason the nocturnal one does: the director does not know what a
// draugr is, and teaching it what an instance is would be the wrong shape twice over —
// it would make the open world aware of a room it can never reach.
//
// It is stated as `def.isBoss()` rather than as a list of kinds, so the next boss is a
// row in the table above and nothing else.
//
// Sorted, because the caller draws from the result with the simulation's own generator
// and map iteration order is deliberately random: an unsorted slice would make the same
// world place different creatures on different runs, which is exactly the property
// spawn_test.go pins.
func spawnableSpecies(night bool) []vnet.MobKind {
	kinds := make([]vnet.MobKind, 0, len(mobRegistry))
	for kind, def := range mobRegistry {
		if !def.rank.valid() {
			continue
		}
		if def.isBoss() {
			continue
		}
		// A dungeon-only row is the same sentence as a boss one, said of a lesser
		// creature: an instance places it, and the dark around a moving player never does.
		if def.dungeonOnly {
			continue
		}
		if def.nocturnal && !night {
			continue
		}
		kinds = append(kinds, kind)
	}
	slices.Sort(kinds)
	return kinds
}

// mobTicks is one species' two durations in the ticks Step counts.
type mobTicks struct {
	windup   uint32
	recovery uint32

	// The secondary attack's pair, zero for a species without one, and the rise from the
	// sand, zero for a species that never lies buried.
	swipeWindup   uint32
	swipeRecovery uint32
	emergence     uint32
}

// mobTimingsFor converts every registered species' telegraph and recovery at this
// server's tick rate.
//
// Converted once, at construction, beside every other duration [NewSim] turns into
// ticks — and per species rather than per simulation, because the two numbers stopped
// being the draugr's the moment a second row had different ones. [ticksFor] is what
// keeps a fast telegraph from rounding away to nothing at a coarse rate.
func mobTimingsFor(tickRate uint8) map[vnet.MobKind]mobTicks {
	timings := make(map[vnet.MobKind]mobTicks, len(mobRegistry))
	for kind, def := range mobRegistry {
		if def.passive {
			timings[kind] = mobTicks{}
			continue
		}
		t := mobTicks{
			windup:   ticksFor(def.windup, tickRate),
			recovery: ticksFor(def.recovery, tickRate),
		}
		if def.swipe != (mobAttack{}) {
			t.swipeWindup = ticksFor(def.swipe.windup, tickRate)
			t.swipeRecovery = ticksFor(def.swipe.recovery, tickRate)
		}
		if def.emergeRange > 0 {
			t.emergence = ticksFor(def.emergence, tickRate)
		}
		timings[kind] = t
	}
	return timings
}
