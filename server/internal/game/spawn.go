package game

import (
	"math"
	"math/rand/v2"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The spawn director: what puts creatures where the players are, and what takes them
// away again.
//
// # What it replaced, and why none of that is left behind
//
// Until this file existed the world held exactly one draugr, placed at boot at a column
// derived from the seed and replaced at that same column ten seconds after it died. That
// model had a mob standing in an empty field for as long as the server ran and none at
// all anywhere a player actually went. The anchor, the boot-time placement and the
// respawn countdown are gone rather than disabled: left in place they are dead code
// somebody wires back up, and "the world has one draugr" would then be true again in a
// world that had stopped being built for it.
//
// # The four rules, and why they do not share a cadence
//
// Only the *spawn* is once a second. It is the expensive question — it reads a column of
// terrain through the seam collision uses — and it is the one whose rate is a gameplay
// decision, because it is how fast the dark refills around somebody who is clearing it.
//
// The three removals answer questions that change every tick and cost almost nothing, so
// they are asked every tick:
//
//   - **The ward.** Warded ground is somewhere nothing that hunts a player may be, and a
//     creature standing on it is taken out of the world rather than killed. Asked once a
//     second instead, a draugr would follow somebody nineteen ticks deep into the place
//     the barrier exists to keep it out of — which is the whole of what a player would
//     ever see of this rule.
//   - **Daylight.** A *nocturnal* creature with nobody to hunt does not survive the sun.
//     Asked once a second instead, one would stand in the daylight for up to a second
//     and the boundary would land on a different tick for every mob in the world.
//   - **Distance.** "Outside every streamed cube for more than five seconds" is a
//     counter, and a counter that is only advanced on one tick in twenty is not
//     measuring seconds.
//
// # Bounds before behaviour
//
// Two caps, and they are what keep the tick's cost and a snapshot's size flat as the
// population moves: at most [MobsPerPlayer] inside any one player's streamed cube, and
// at most [MobsPerPlayerWorldwide] × connected players in the world. The second is not
// implied by the first — cubes overlap, and two players standing together would
// otherwise be allowed twice the mobs one of them can see. With nobody connected the
// ceiling is zero, which is the same answer the removals arrive at from the other side.
//
// # Which species, and who decides
//
// The director does not know what a draugr is. It asks [spawnableSpecies] which rows of
// mobRegistry may arrive at this point in the day and draws one of them with the same
// generator it draws a position with — so "the draugr is what the night brings and the
// vargr is what the daylight does not save you from" is two booleans in a table rather
// than a branch here. A third species is a row; nothing in this file changes.
//
// # Determinism is a requirement here, not a preference
//
// Every random choice comes from [Sim.spawns], seeded from the world seed and advanced
// only inside the locked tick. No package-level rand, no reading of the wall clock: the
// same world and the same sequence of ticks produce the same creatures in the same
// places, which is what lets a test assert a spawn position exactly instead of
// statistically. The draw is integer arithmetic for the reason terrain generation is —
// a float expression is allowed to round differently on another architecture, and a
// position asserted exactly should not depend on which machine ran the test.

// mobSpawnStream is the second word of the director's PCG seed.
//
// PCG takes two, and only one of them is the world's. The constant is what makes this
// *the spawn stream* rather than whatever any other consumer of the same world seed
// would produce from it: a second generator seeded the same way would draw the same
// numbers, and two systems making the same "random" choice in step is a correlation
// nobody asked for and nobody would notice.
const mobSpawnStream = 0x766F78656C6D6F62 // "voxelmob"

// newSpawnRNG is the director's generator, from a world seed.
func newSpawnRNG(worldSeed int64) *rand.Rand {
	// The conversion is a reinterpretation rather than a range check: every int64 maps
	// to a distinct uint64, which is all a seed word has to be.
	return rand.New(rand.NewPCG(uint64(worldSeed), mobSpawnStream))
}

// directMobsLocked is one tick of the director, and reports whether the population
// changed.
//
// Runs after [Sim.advanceMobsLocked], which is what makes every position it reads —
// the players', the mobs', and the target each mob chose — this tick's rather than the
// last one's. The caller holds Sim.mu.
//
// The return value exists because the tick's mob list was taken before this ran: a
// caller that hears "yes" re-reads it, and one that hears "no" keeps the slice it has.
// The alternative is re-sorting every mob in the world twenty times a second to
// discover that nothing happened.
func (s *Sim) directMobsLocked(tick uint64, players []*Player, mobs []*mob) bool {
	changed := s.removeSpentMobsLocked(players, mobs)

	// The one part of this that is not every tick. See the file comment.
	if tick%uint64(s.spawnEvery) == 0 && s.spawnPassLocked(players) {
		changed = true
	}
	return changed
}

// removeSpentMobsLocked takes away every mob that has stopped being worth simulating,
// and reports whether it took any.
//
// Three rules and one loop, over the list the tick already sorted. Separate loops would
// mean a second sort, and — worse — a second loop reading a list the first had already
// deleted entries from, which is the kind of stale iteration that works until somebody
// makes one of the rules depend on the other. **A new rule joins the walk rather than
// adding a pass**, which is why the barrier below costs a map hit per mob per tick and
// not a second traversal of the population.
//
// # The ward
//
// **Warded ground is somewhere nothing that hunts a player may be**, and the barrier is
// the same predicate the director refuses a spawn spot with — see [Sim.wardBarsLocked],
// which is where the exemption and the ownership rule are both written down once.
//
// It is asked *first*, and that ordering is a statement rather than an optimisation: the
// other two rules ask whether this creature is still worth simulating, and this one asks
// whether it may be there at all. A draugr hunting somebody in plain sight survives the
// dawn and is inside every streamed cube, so both of the rules below would keep it — and
// following a player through the barrier is exactly the thing this exists to stop.
//
// **It is a removal and not a kill**, through the same [Sim.discardMobLocked] the other
// two use: no corpse, no loot, no experience, and every earned claim cleared on the way
// out. Nobody killed it, so nobody is owed anything for it — and a ward that paid out
// would be a loot farm a player could build, which is the opposite of a safe place.
//
// # Daylight
//
// **Nocturnal is a property of the creature, not of the spawn rule** — literally, now:
// it is a field of the registry row, read here and in [spawnableSpecies], and the two
// together are what make the same sentence true from both ends. A nocturnal species
// arrives in the dark and does not outlast it; spawning it only at night without this
// half would leave the last of them wandering through the whole day. A species that is
// not nocturnal is untouched by the dawn, because the dawn was never what put it there.
//
// What survives the dawn is one that is already hunting somebody, and it survives
// exactly as long as that hunt does — which is what makes running from a draugr into
// the sunrise something a player can do, and being followed into it the price of having
// been seen.
//
// The hunt is read through [huntable] rather than from the target field, because the
// field can name a player who has died, respawned or disconnected since it was written.
// A stale id is not a hunt, and reading one as though it were would leave a draugr
// standing in the daylight for ever.
//
// # Distance
//
// **The same cube the snapshot uses**, asked of every player rather than of the one the
// mob was spawned for: a creature is worth simulating while anybody can see it, and
// "the player who caused it left" is not a reason to delete something another player is
// standing next to.
//
// A grace period rather than an immediate removal, because the cube's edge is a place a
// player walks back and forth across. Five seconds is long enough that stepping over a
// chunk boundary and back does not despawn what is chasing you, and short enough that
// walking away from a fight really does end it. With nobody connected every mob is
// unwatched, so an empty server empties itself — which is the answer the spawn ceiling
// reaches from the other side, arrived at independently so that neither is load-bearing
// alone.
//
// The caller holds Sim.mu.
func (s *Sim) removeSpentMobsLocked(players []*Player, mobs []*mob) bool {
	daylight := !IsNight(s.tickOfDay)

	var removed bool
	// **Nothing here can reach a creature that has been killed, and that is now
	// structural.** Both rules below remove a mob *instead of* killing it, which is exactly
	// what makes "a despawn leaves nothing" true — so either of them reaching a body a
	// player had already earned would delete that loot. This loop used to guard against it
	// explicitly, because a killed creature stayed in Sim.mobs for MobDeathDuration and,
	// being nocturnal with its target cleared at the blow, matched the daylight rule on
	// every tick of its own death. A killing blow now takes the creature out of Sim.mobs
	// before the caller of this ever assembles the slice, so there is no window left to
	// guard.
	for _, m := range mobs {
		if s.wardBarsLocked(m.kind, m.pos) {
			s.discardMobLocked(m)
			s.log.Debug("mob crossed into a ward", "entity_id", m.entityID, "kind", m.kind,
				"column", chunkAt(m.pos).Column())
			removed = true
			continue
		}

		if daylight && m.species().nocturnal && huntable(players, m.target) == nil {
			s.discardMobLocked(m)
			s.log.Debug("mob left with the night", "entity_id", m.entityID, "kind", m.kind,
				"tick_of_day", s.tickOfDay)
			removed = true
			continue
		}

		if watchedBy(players, m, s.viewDistance) {
			m.unseenTicks = 0
			continue
		}
		m.unseenTicks++
		// Strictly past the grace, so a mob is given the whole of it rather than the
		// whole of it minus one tick — the off-by-one the death countdown avoids the
		// same way.
		if m.unseenTicks <= s.mobDespawnTicks {
			continue
		}
		s.discardMobLocked(m)
		s.log.Debug("mob left every streamed cube", "entity_id", m.entityID, "kind", m.kind,
			"unseen_ticks", m.unseenTicks)
		removed = true
	}
	return removed
}

// discardMobLocked is the non-kill exit. It consumes neither loot RNG nor the corpse
// transition, and clears every earned/combat claim before the entity leaves the world.
// A future boss reset can use this same boundary instead of growing a second reward path.
func (s *Sim) discardMobLocked(m *mob) {
	if m == nil {
		return
	}
	delete(s.mobs, m.entityID)
	m.firstHit = nil
	m.encounter = nil
}

// watchedBy reports whether any connected player is streaming the chunk this mob stands
// in. A free function for the reason [huntable] is one: it decides nothing about the
// simulation, it answers a question about a list somebody else has already taken.
//
// The caller holds Sim.mu, which is what guards both chunks it reads.
func watchedBy(players []*Player, m *mob, viewDistance int32) bool {
	for _, p := range players {
		if withinView(p.chunk, m.chunk, viewDistance) {
			return true
		}
	}
	return false
}

// spawnPassLocked is the once-a-second half: one species and one candidate per player,
// and at most one creature.
//
// **Which species may arrive is the registry's answer, not a branch here.** The clock is
// asked once, [spawnableSpecies] turns that into the rows whose nocturnal field allows
// it, and the draw picks one. An hour at which nothing may arrive is an empty slice and
// an immediate no — which is what daylight is for a world whose only species is
// nocturnal, and what it stopped being when the vargr was registered.
//
// **One attempt per player per pass, and a refused attempt is not retried inside it.**
// That is what makes a pass a constant amount of work whatever the terrain looks like:
// a player standing in the middle of a lake would otherwise cost an unbounded search
// for somewhere legal, on the tick goroutine, under the lock every other player's tick
// is waiting on. What a refusal costs instead is a second, in a night that is six
// minutes long.
//
// The caller holds Sim.mu.
func (s *Sim) spawnPassLocked(players []*Player) bool {
	species := spawnableSpecies(IsNight(s.tickOfDay))
	if len(species) == 0 {
		return false
	}

	// The world ceiling, read once: it moves only when somebody joins or leaves, and a
	// pass that is already over it has nothing to ask any player about.
	ceiling := MobsPerPlayerWorldwide * len(players)

	var spawned bool
	for _, p := range players {
		if len(s.mobs) >= ceiling {
			break
		}
		if s.mobsInViewLocked(p) >= MobsPerPlayer {
			continue
		}
		// The species before the spot, because the spot depends on it: the separation
		// the candidate has to keep is measured from *this* body, and a wider creature
		// needs a wider gap than a narrower one standing in the same column.
		//
		// One draw per attempt whether one species is eligible or five, so a pass stays
		// the constant amount of work the paragraph above claims it is.
		kind := species[s.spawns.IntN(len(species))]

		pos, ok := s.candidateSpotLocked(p, kind)
		if !ok {
			continue
		}
		id, made := s.spawnMobLocked(kind, pos)
		if !made {
			// Unreachable: kind came out of the registry a moment ago. Refused rather
			// than asserted, because a director that logged a spawn it had not made
			// would be the harder of the two bugs to see.
			continue
		}
		s.log.Debug("mob spawned", "entity_id", id, "kind", kind, "pos", pos, "near", p.entityID)
		spawned = true
	}
	return spawned
}

// mobsInViewLocked is how many mobs stand inside one player's streamed cube.
//
// **The per-player cap is read through the same rule the snapshot's visibility filter
// uses**, deliberately: the cap is a statement about what one player can be made to
// face, and a cap measured on a volume different from the one they are sent would be a
// number about nothing.
//
// O(mobs) per player per pass, which is the trade the drops, the swings and the
// snapshot fan-out all already record: a spatial index is worth building when the
// quadratic term matters and not one issue before. This is the cheapest of them — once
// a second rather than once a tick.
//
// The caller holds Sim.mu.
func (s *Sim) mobsInViewLocked(p *Player) int {
	var count int
	for _, m := range s.mobs {
		if withinView(p.chunk, m.chunk, s.viewDistance) {
			count++
		}
	}
	return count
}

// candidateSpotLocked draws one spot near a player and answers whether a creature of
// this species may stand there.
//
// The kind is a parameter because the last two checks are not about the ground alone:
// what a ward bars is the species that hunts, and how much room a spot needs is the
// arriving body's. Everything above them is a question about the *ground*, which is the
// same question whatever is going to stand on it.
//
// The caller holds Sim.mu.
func (s *Sim) candidateSpotLocked(p *Player, kind vnet.MobKind) ([3]float64, bool) {
	dx, dz, inRing := s.ringOffsetLocked()
	if !inRing {
		return [3]float64{}, false
	}

	x := int64(math.Floor(p.pos[0])) + dx
	z := int64(math.Floor(p.pos[2])) + dz

	// The vertical span of this player's streamed cube, which is as far as the column
	// may be read: a spot outside it stands on terrain the client has never been sent,
	// and a scan that ran past it would be asking the cache about chunks nobody has
	// asked it to compose.
	top := (int64(p.chunk.Y)+int64(s.viewDistance)+1)*world.ChunkSize - 1
	bottom := (int64(p.chunk.Y) - int64(s.viewDistance)) * world.ChunkSize

	ground, found := surfaceUnderSky(s.terrain, x, z, top, bottom)
	if !found {
		return [3]float64{}, false
	}
	// Two cells of headroom for the tallest body in the registry, and both of them
	// inside the cube: a surface at the very top of it has a clear sky by accident
	// rather than in fact, because the scan started below where the sky would be.
	//
	// The same two cells for every species, deliberately. A vargr is a block tall and
	// would fit under one, but a shorter creature asking for less room is a legality
	// rule that differs by species — which would let the dark put something in a gap a
	// player cannot follow it into. Two is what the ground has to offer; what stands on
	// it is not the ground's business.
	if ground+2 > top {
		return [3]float64{}, false
	}
	// The surface has to be terrain the server actually holds. Solid answers true for a
	// chunk that has not been composed — that is what keeps a player from falling out
	// of a world that is merely still loading — so without this a column of chunks
	// nobody has generated reads as perfectly good ground under a perfectly clear sky.
	//
	// **And it has to be ground rather than merely not-air.** The scan stops at the
	// first [Terrain.Solid] voxel, and ice is solid — so the lid on a frozen lake is
	// a perfectly good surface under a perfectly clear sky, and nothing but this
	// names it. Water cannot reach here (the scan walks straight through it), but it
	// is refused by name beside the ice, because the two are one rule: a creature is
	// not stood on water and not stood on the thing floating on top of it.
	if block, resident := s.terrain.Block(x, ground, z); !resident || !standableFloor(block) {
		return [3]float64{}, false
	}
	// And the two cells the body stands in have to be air, asked of the blocks rather
	// than inferred from the scan that found the surface.
	//
	// **This check was written before there was anything for it to catch, and water
	// is what it was written for.** `surfaceUnderSky` stops at the first *solid*
	// voxel, and until worldgen 5 "not solid" was exactly "resident air", so the loop
	// read as a check that could not fail. It can now: the scan walks straight down
	// through a lake and hands back the bed with the whole lake still on top of it,
	// and this is the only thing between that and a draugr standing on the bottom.
	for _, y := range [2]int64{ground + 1, ground + 2} {
		if block, resident := s.terrain.Block(x, y, z); !resident || block != world.Air {
			return [3]float64{}, false
		}
	}

	// Centred in the column and standing on the surface, which is the convention every
	// entity in this simulation shares: the position is the bottom of the body's box.
	pos := [3]float64{float64(x) + 0.5, float64(ground + 1), float64(z) + 0.5}

	if !withinView(p.chunk, chunkAt(pos), s.viewDistance) {
		// Reachable at a view distance smaller than the ring — `-view-distance 1` is a
		// cube 32 blocks deep and the ring starts at 32 — and refusing is the right
		// answer rather than a reason to shrink the ring: an operator who streams less
		// terrain gets fewer spawns, not spawns on ground their players cannot see.
		return [3]float64{}, false
	}
	if s.nearACampfireLocked(pos) {
		return [3]float64{}, false
	}
	if s.wardBarsLocked(kind, pos) {
		return [3]float64{}, false
	}
	if !s.spotIsClearLocked(pos, mobRegistry[kind].body) {
		return [3]float64{}, false
	}
	return pos, true
}

// ringOffsetLocked draws one column offset near a player, and says whether it landed in
// the ring.
//
// **The ring is [MobSpawnRingInner]..[MobSpawnRingOuter] blocks out, and the draw is a
// square that contains it.** About three draws in eight land in the corners and those
// passes spawn nothing — which is a second of waiting rather than a defect, and it is
// what keeps a pass to exactly one draw. Looping until a point lands inside would make
// the cost of a pass a random variable for no gain a player could ever perceive.
//
// Integer arithmetic throughout, for the reason [Sim.spawns] exists at all: a position
// a test asserts exactly must not depend on how a compiler decided to round a product,
// and the annulus test is one multiplication per axis.
//
// The caller holds Sim.mu, which is the only place the generator is advanced.
func (s *Sim) ringOffsetLocked() (dx, dz int64, inRing bool) {
	const span = 2*MobSpawnRingOuter + 1

	dx = s.spawns.Int64N(span) - MobSpawnRingOuter
	dz = s.spawns.Int64N(span) - MobSpawnRingOuter

	distance := dx*dx + dz*dz
	if distance < MobSpawnRingInner*MobSpawnRingInner || distance > MobSpawnRingOuter*MobSpawnRingOuter {
		return 0, 0, false
	}
	return dx, dz, true
}

// standableFloor reports whether a surface voxel is ground a creature may be put on.
//
// **A whitelist would be wrong here and a blacklist is right**, which is the
// opposite of the rule the item registry follows, because the two questions differ:
// the registry decides what a *player* may do with a named thing, and this decides
// what the *world* is. A block nobody has classified is ordinary ground, and
// refusing to spawn on it would silently empty a region every time the palette grew.
// The exceptions are the water family, which is not a floor, and the ice on top of
// it, which is a lid over water and not a place to stand.
func standableFloor(block world.Block) bool {
	return !world.IsWater(block) && block != world.Ice && world.Solid(block)
}

// surfaceUnderSky is the highest solid voxel in a column with nothing above it, or
// false for a column that is air all the way down.
//
// **The scan runs downwards, and that is what makes this a surface rule rather than a
// ground rule.** The first solid voxel found from the top of the cube has only air
// above it by construction, so "there is sky over this spot" and "this is the ground"
// are one answer to one question. Scanning up from the player instead would find the
// floor of the cave they are standing in, and this game has no light propagation — dark
// underground is a question nothing here can answer, so night plus an open sky is the
// rule the server can actually check (see the GDD's darkness rules, which are their own
// issue).
//
// A non-generating read, on the tick, by contract. An absent chunk answers solid, so a
// column of terrain that has not arrived stops the scan at the top of the cube — and
// the caller's residency check is what turns that into a refusal instead of a spawn.
func surfaceUnderSky(t Terrain, x, z, top, bottom int64) (int64, bool) {
	for y := top; y >= bottom; y-- {
		if t.Solid(x, y, z) {
			return y, true
		}
	}
	return 0, false
}

// nearACampfireLocked reports whether a spot lies on the ground a *burning* campfire
// keeps.
//
// **A fire the rain has put out keeps nothing**, and the skip is [Sim.stationWithinLocked]'s
// rather than this function's for the reason the scan itself is: one question, one
// implementation. A downpour therefore hands the dark back the ring it was holding, and
// hands it back again when the rain eases, with nothing here needing to know that weather
// exists.
//
// **This issue declares [CampfireSafeRadius] and this predicate; the campfire itself
// arrives after it.** The two halves are deliberately split that way round, because a
// constant defined in two places is a constant that will eventually hold two values.
// What is here is a function over the placed structures of that kind, and it is correct
// when there are none — which is why nothing about the director waits on the fire being
// buildable.
//
// The scan is [Sim.stationWithinLocked]'s, unchanged: "does a structure of this kind
// stand within r of here" is one question, and a second implementation of it is a
// second answer that can disagree. The forge asks it to find somewhere to work and the
// director asks it to find somewhere to keep away from; the arithmetic is the same
// either way, measured from the centre of a body standing at pos to the centre of the
// anchor voxel.
//
// The caller holds Sim.mu.
func (s *Sim) nearACampfireLocked(pos [3]float64) bool {
	return s.stationWithinLocked(vnet.StructureKindCampfire, pos, CampfireSafeRadius)
}

// wardBarsLocked reports whether a creature of this species may not be on the ground at
// pos, because somebody's ward claims the column it stands in.
//
// **The one question both halves of the barrier ask.** The director asks it of a spot it
// is about to put a creature on, and [Sim.removeSpentMobsLocked] asks it of a creature
// already standing somewhere; a suppression rule and a removal rule written separately
// could disagree about where the boundary is, and the seam between them would be a strip
// of ground where creatures spawn and are deleted on the same tick for ever.
//
// **Passive is the exemption, and it is a registry column rather than a list of kinds**
// (see [mobDefinition]). A deer may walk into a village and live: the barrier is about
// what hunts the player, not about what happens to be a creature. A passive species added
// later is exempt for free, and a predator added later is barred for free — neither
// touches this function.
//
// **The boolean, never the owner.** [Sim.wardOf] answers with the zero identity for a
// settlement, because a settlement is owned by nobody — so a caller that compared owners
// would let every hostile creature stand in exactly the place this rule most exists to
// keep clear. There is no exemption to want in any case: a runestone bars what hunts its
// own owner as readily as what hunts anybody else, which is the point of raising one.
//
// **The column is asked the way every other question about where a mob *is* asks it** —
// [chunkAt] of the standing position, which is what [mob.chunk] itself is set from at the
// end of the physics step. The body box decides distances in this simulation, not which
// column something occupies: a ward is a claim over columns, and a body that overhangs a
// boundary is standing on one side of it.
//
// **The position is this tick's, not the last one's**, because the director runs after
// [Sim.advanceMobsLocked]: a creature that crosses the boundary during a tick is removed
// on the tick it crossed, so it never reaches a snapshot standing inside the ward. The
// narrow converse is deliberate rather than missed — one that spends a whole tick walking
// back out is judged where the tick left it, which is outside, and a creature leaving is
// not the thing a barrier exists to stop.
//
// The lookup is [Sim.wardOf]'s cache — a bounded lattice query the first time a column is
// asked about and a map hit every time after — which is what lets this ride the removal
// walk instead of costing a pass of its own. **It does feed that cache**, and that is
// worth saying rather than leaving to be found: an entry now appears for every column a
// creature stands in and not only for every column somebody edits. The set is bounded by
// the ground connected players have streamed, since a mob is removed five seconds after
// it leaves every cube, and an entry is a boolean and an identity. How far [Sim.wards]
// may grow is a question about the whole cache rather than about this caller.
//
// The caller holds Sim.mu.
func (s *Sim) wardBarsLocked(kind vnet.MobKind, pos [3]float64) bool {
	if mobRegistry[kind].passive {
		return false
	}
	_, warded := s.wardOf(chunkAt(pos).Column())
	return warded
}

// spotIsClearLocked reports whether a body of this shape may appear at pos without
// arriving inside somebody.
//
// Two different rules, because they are two different problems. A player is a body a
// creature must not materialise *inside* — a spawn that overlapped one would put two
// solids in the same place and leave the collision to sort it out. Another mob is
// something to keep a distance from: [MobSpawnSeparation] blocks, so a night's worth of
// spawning spreads out into the dark instead of piling into whichever column happened
// to be legal first.
//
// **Both boxes are the registry's**, the arriving creature's and each standing one's.
// Measuring either with a single hardcoded shape would make the separation a different
// distance depending on which species happened to be involved, while reading as one
// number.
//
// Distances are measured between bodies, like everything else in this simulation that
// asks how far apart two things are.
//
// The caller holds Sim.mu.
func (s *Sim) spotIsClearLocked(pos [3]float64, arriving body) bool {
	spot := arriving.boxAt(pos)

	for _, p := range s.players {
		if boxDistance(spot, playerBox(p.pos)) == 0 {
			return false
		}
	}
	for _, m := range s.mobs {
		if boxDistance(spot, m.species().body.boxAt(m.pos)) < MobSpawnSeparation {
			return false
		}
	}
	return true
}
