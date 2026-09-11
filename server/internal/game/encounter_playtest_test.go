package game

import (
	"cmp"
	"fmt"
	"log/slog"
	"math"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// The combat measurement behind docs/reviews/dungeon-combat-1037.md.
//
// Parties of scripted players fight both bosses of the shipped first dungeon: its chamber,
// the production scheduler, collision, armour and damage, stepped by the instance manager.
// Each player perceives the fight only through the frames its own session is delivered —
// snapshots and encounter timelines, decoded from bytes — after a configurable delay and
// burst loss, and answers with ordinary movement intent and attack requests. Nothing a
// player does is read off the scheduler. The simulation is read afterwards, as the
// referee, to record what happened.
//
// A scripted player is a policy, not a person. The reader leaves every region it believes
// is announced by the straight walk that clears it soonest, and swings whenever it is in
// reach and not standing in one. The stander closes to the same gap and never moves again,
// which is the negative control: every hit it takes is a region it was shown and ignored.
// The delay stands for latency and reaction time together; a reader with no delay is a
// player with perfect reflexes, which is why the delays are swept.

const (
	playtestEnv    = "VOXELHEIM_PLAYTEST"
	playtestDirEnv = "VOXELHEIM_PLAYTEST_DIR"

	// playtestLimitSeconds ends a fight that has not finished. Far beyond the approved
	// design's longest intended fight, so reaching it is a finding and not a budget.
	playtestLimitSeconds = 900

	// playtestMargin is the room a reader keeps between its body and a believed boundary,
	// in blocks: a body stopped on a boundary is inside it after any non-tangential step.
	playtestMargin = 0.35

	// playtestStand is the body-to-body gap a player closes to before swinging, inside
	// SwordReach with room for the boss to shift by a tick of its own movement.
	playtestStand = SwordReach - 0.5

	// playtestRangedStand is the gap a ranged member holds: inside every distance band the
	// guardian's charge and leap and the king's spear are chosen in.
	playtestRangedStand = 6.0

	// playtestEscapeHorizon is the longest straight walk a reader will consider.
	playtestEscapeHorizon = 2 * time.Second
)

type playtestKit struct {
	name   string
	sword  ItemID
	armour []ItemID
}

// The entry equipment the registry verifies: the starter blade with nothing worn, the iron
// blade in leather, and the iron blade in the heaviest armour a character can craft before
// the dungeon. No shield is raised by either policy.
var (
	kitRusty   = playtestKit{name: "rusty-unarmoured", sword: ItemRustySword}
	kitLeather = playtestKit{name: "iron-leather", sword: ItemIronSword,
		armour: []ItemID{ItemLeatherCap, ItemLeatherJerkin, ItemLeatherLeggings}}
	kitIron = playtestKit{name: "iron-armoured", sword: ItemIronSword,
		armour: []ItemID{ItemIronHelm, ItemIronCuirass, ItemIronGreaves}}
)

type playtestPolicy uint8

const (
	policyReader playtestPolicy = iota
	policyStander

	// policyEvader reads and escapes exactly as the reader does and never swings, so a fight
	// held at one stage runs through that stage's whole repertoire instead of ending first.
	policyEvader
)

func (p playtestPolicy) String() string {
	switch p {
	case policyStander:
		return "stander"
	case policyEvader:
		return "evader"
	}
	return "reader"
}

// playtestNetwork is what happens to frames between the simulation and a player.
//
// Every frame is held for delayMillis. A burst of lossBurst ticks out of every lossEvery
// delivers nothing at all, snapshots and timelines alike: what a stalled link does to a
// stream that cannot reorder.
type playtestNetwork struct {
	delayMillis          int
	lossEvery, lossBurst uint64
}

func (n playtestNetwork) String() string {
	name := fmt.Sprintf("delay%dms", n.delayMillis)
	if n.lossEvery > 0 {
		name += fmt.Sprintf("-lose%dof%d", n.lossBurst, n.lossEvery)
	}
	return name
}

func (n playtestNetwork) lost(tick uint64) bool {
	return n.lossEvery > 0 && tick%n.lossEvery < n.lossBurst
}

type playtestConfig struct {
	boss    vnet.MobKind
	party   int
	kit     playtestKit
	policy  playtestPolicy
	network playtestNetwork

	// ranged is how many of the party hold playtestRangedStand instead of closing to melee,
	// which is what the moves chosen only at a distance — the charge, the leap, the spear —
	// need in order to be chosen at all. A ranged solo player holds it alone.
	ranged int

	// stage, when non-zero, pulls the boss by hand at the health that stage begins at, so
	// its repertoire is measured without the stages before it. seconds, when non-zero, ends
	// the fight after that long instead of at playtestLimitSeconds.
	stage   uint8
	seconds int

	// timed records the wall time of every simulation step, for the server cost harness.
	timed bool

	// levels is each member's level, in join order; a member past the end of the list is
	// level one. The boss scales to these at the pull (#1099); the kit never enters it.
	levels []uint16
}

// level is the level member i joins at.
func (c playtestConfig) level(i int) uint16 {
	if i < len(c.levels) {
		return c.levels[i]
	}
	return 1
}

// levelsLabel is every member's level joined by semicolons.
func (c playtestConfig) levelsLabel() string {
	labels := make([]string, c.party)
	for i := range labels {
		labels[i] = fmt.Sprint(c.level(i))
	}
	return strings.Join(labels, ";")
}

func (c playtestConfig) String() string {
	name := fmt.Sprintf("%s/party%d/%s/%s/%s", playtestBossName(c.boss), c.party, c.kit.name, c.policy, c.network)
	if c.ranged > 0 {
		name += fmt.Sprintf("/ranged%d", c.ranged)
	}
	if len(c.levels) > 0 {
		name += "/levels" + strings.ReplaceAll(c.levelsLabel(), ";", "-")
	}
	if c.stage > 0 {
		name += fmt.Sprintf("/stage%d-%ds", c.stage, c.seconds)
	}
	return name
}

func playtestBossName(kind vnet.MobKind) string {
	if kind == vnet.MobKindDraugrKing {
		return "king"
	}
	return "guardian"
}

// playtestMoveStats is everything one fight recorded about one move kind.
type playtestMoveStats struct {
	// moves counts committed instances; windows counts damaging windows (a channel has one
	// per pulse).
	moves, windows int

	// threatened counts (window, player) pairs where the player's body was inside the
	// region on the tick it was announced; escaped counts those the window never struck.
	threatened, escaped int

	hits, damage int

	// Every blow is checked against the announcement its own phase promises: the telegraph
	// for a release, or the last tick of the shown interval for a pulse. premature counts the
	// blows that landed sooner after the player perceived their region than that promise.
	// minWarning is the smallest margin's warning and required that same blow's promise, so
	// the pair printed together always describes one blow.
	premature            int
	minWarning, required int64

	// contactMax is the farthest a struck body's nearest damage sample lay from the boss's
	// centre, for moves whose region is anchored on the boss; negative when none were.
	contactMax float64

	// recoveries counts each recovery length, in ticks, the move actually paid.
	recoveries map[uint32]int

	// escapeTicks is the longest walk a reader needed to leave this move's region.
	escapeTicks uint64
}

// playtestResult is one fight.
type playtestResult struct {
	config        playtestConfig
	killed        bool
	killSeconds   float64
	stageSeconds  []float64
	deaths, wipes int
	hits          int
	damageTaken   int
	unperceived   int

	// unattributed counts player health losses the running move's own hit ledger does not
	// account for. The server records every target a release or pulse window strikes in that
	// window's ledger, so a loss with no new ledger entry is not that move's blow, and a
	// non-zero count means one this referee would otherwise have charged to the wrong move.
	unattributed int
	moves        map[vnet.EncounterMoveKind]*playtestMoveStats

	// bossDamage is the health the party took from the boss, by what the boss was doing.
	bossDamage map[string]int

	// stepNanos is the wall time of every simulation step, when the config asked for it.
	stepNanos []int64

	// scale is what the pull decided about the boss, read from the first engaged tick.
	scale bossScale
}

func (r *playtestResult) move(kind vnet.EncounterMoveKind) *playtestMoveStats {
	if r.moves[kind] == nil {
		r.moves[kind] = &playtestMoveStats{minWarning: math.MaxInt64, required: math.MaxInt64, contactMax: -1, recoveries: map[uint32]int{}}
	}
	return r.moves[kind]
}

type playtestFrame struct {
	arrival uint64
	bytes   []byte
}

type playtestBot struct {
	p          *Player
	inbox      []playtestFrame
	clientTick uint32

	bossID   uint64
	bossPos  [3]float64
	bossSeen bool

	// believed is every damaging region in the newest perceived timeline, and believedKinds
	// the move each came from.
	believed      []protocol.HazardVolume
	believedKinds []vnet.EncounterMoveKind

	// stand is the body-to-body gap this player closes to.
	stand float64

	// escaping is when this reader first found itself inside a believed region, and fleeing
	// the moves it was fleeing; zero while it stands clear.
	escaping uint64
	fleeing  map[vnet.EncounterMoveKind]bool

	// perceived is the tick each (instance, pulse) region was first perceived.
	perceived map[[2]uint64]uint64
}

type playtest struct {
	cfg     playtestConfig
	manager *InstanceManager
	s       *Sim
	bots    []*playtestBot
	tick    uint64
	delay   uint64

	// escapeTicks is the longest walk a reader needed, per move kind, from first finding
	// itself inside a believed region to standing clear of every one.
	escapeTicks map[vnet.EncounterMoveKind]uint64

	// beforeStep, when set, runs before every simulation step. A test seam for mutations
	// that prove the referee catches what it claims to.
	beforeStep func()

	// timed records the wall time of every simulation step in stepNanos.
	timed     bool
	stepNanos []int64
}

func newPlaytest(t *testing.T, cfg playtestConfig) *playtest {
	t.Helper()
	m, err := NewInstanceManager(DefaultTickRate, 3, 1, testEntityIDs(), slog.New(slog.DiscardHandler))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(m.Close)
	session, err := m.Reenter(InstanceRuin{}, instanceTestCharacter(1))
	if err != nil {
		t.Fatal(err)
	}
	for i := 2; i <= cfg.party; i++ {
		if _, err := m.Join(session.ID, instanceTestCharacter(uint64(i))); err != nil {
			t.Fatal(err)
		}
	}
	loadDungeon(t, session)
	pt := &playtest{cfg: cfg, manager: m, s: session.Sim, escapeTicks: map[vnet.EncounterMoveKind]uint64{}, timed: cfg.timed}
	if cfg.network.delayMillis > 0 {
		pt.delay = uint64(ticksFor(time.Duration(cfg.network.delayMillis)*time.Millisecond, DefaultTickRate))
	}
	s := pt.s

	if cfg.boss == vnet.MobKindDraugrKing {
		// The king cannot be fought until the guardian is dead, so its fight is measured on
		// its own after the guardian is removed by the ordinary kill path.
		s.mu.Lock()
		guardian := s.mobs[s.dungeon.guardianID]
		killed := guardian != nil && s.damageMobLocked(guardian, guardian.health)
		s.mu.Unlock()
		if !killed {
			t.Fatal("the guardian survived the setup kill")
		}
		pt.step()
	}

	s.mu.Lock()
	home := s.mobs[pt.bossIDLocked()].pos
	s.mu.Unlock()
	for i := range cfg.party {
		// Spread evenly around the boss, six blocks out on the arena floor.
		angle := math.Pi/2 + 2*math.Pi*float64(i)/float64(cfg.party)
		spawn := [3]float32{float32(home[0] + 6*math.Cos(angle)), float32(home[1]), float32(home[2] + 6*math.Sin(angle))}
		bot := &playtestBot{perceived: map[[2]uint64]uint64{}, fleeing: map[vnet.EncounterMoveKind]bool{}, stand: playtestStand}
		if i < cfg.ranged {
			bot.stand = playtestRangedStand
		}
		life := playtestLife(t, spawn, cfg.kit, cfg.level(i))
		character := instanceTestCharacter(uint64(i + 1))
		p, err := s.JoinCharacter(s.mintEntityID(), character.PlayerID, character.CharacterID, fmt.Sprintf("Reader%d", i+1),
			spawn, testAppearance(), &life, func(frame []byte) bool {
				if !cfg.network.lost(pt.tick) {
					bot.inbox = append(bot.inbox, playtestFrame{arrival: pt.tick, bytes: frame})
				}
				return true
			})
		if err != nil {
			t.Fatal(err)
		}
		bot.p = p
		pt.bots = append(pt.bots, bot)
	}

	if cfg.stage > 0 {
		s.mu.Lock()
		boss := s.mobs[pt.bossIDLocked()]
		s.startBossEncounterLocked(boss, pt.bots[0].p)
		if def := boss.species(); cfg.stage > 1 {
			boss.health = uint16(uint32(boss.maxHealth()) * uint32(def.phaseHealthPercents[cfg.stage-2]) / 100)
		}
		s.mu.Unlock()
	}
	return pt
}

func playtestLife(t *testing.T, spawn [3]float32, kit playtestKit, level uint16) Life {
	t.Helper()
	pieces := make([]testArmourPiece, 0, len(kit.armour))
	for _, item := range kit.armour {
		pieces = append(pieces, fullTestArmour(item))
	}
	life := lifeWearing(t, spawn, pieces...)
	life.Experience = experienceBefore(level)
	life.Health = maxHealthFor(level)
	sword := itemRegistry[kit.sword]
	life.Slots[0] = protocol.InventoryStack{ItemID: uint16(kit.sword), Count: 1,
		Durability: sword.maxDurability, MaxDurability: sword.maxDurability}
	return life
}

func (pt *playtest) bossIDLocked() uint64 {
	if pt.cfg.boss == vnet.MobKindDraugrKing {
		return pt.s.dungeon.kingID
	}
	return pt.s.dungeon.guardianID
}

func (pt *playtest) step() {
	pt.tick++
	if !pt.timed {
		pt.manager.Step()
		return
	}
	started := time.Now()
	pt.manager.Step()
	pt.stepNanos = append(pt.stepNanos, time.Since(started).Nanoseconds())
}

// playtestWindow is one damaging window: a move instance, and which pulse of it.
type playtestWindow struct {
	instance uint64
	pulse    uint8
}

// run plays the fight to the boss's death or the limit, refereeing every tick.
func (pt *playtest) run() playtestResult {
	s := pt.s
	result := playtestResult{config: pt.cfg, moves: map[vnet.EncounterMoveKind]*playtestMoveStats{}, bossDamage: map[string]int{}}

	var (
		current    playtestWindow
		inWindow   bool
		windowKind vnet.EncounterMoveKind
		threatened = map[*Player]bool{}
		struck     = map[*Player]bool{}
		recovery   uint64
		pullTick   uint64
		stage      uint8
		health     = map[*Player]uint16{}
		alive      = map[*Player]bool{}

		// attributed is every (instance, pulse, player) blow already charged to its window.
		attributed = map[[3]uint64]bool{}

		// held is the move running after the previous step, and heldBoss the boss performing
		// it. The step that deals a blow may also end the boss that dealt it: the last
		// player's death wipes the pull, and the reset replaces the boss inside that step.
		held     *runningMove
		heldBoss *mob
	)
	closeWindow := func() {
		if !inWindow {
			return
		}
		stats := result.move(windowKind)
		for p := range threatened {
			stats.threatened++
			if !struck[p] {
				stats.escaped++
			}
		}
		inWindow = false
		clear(threatened)
		clear(struck)
	}

	// settleLosses charges this tick's health losses. A blow is charged to a move only when a
	// window's own hit ledger gained this player: the server writes that entry in the call
	// that deals the damage, and a window strikes each player once. The window is the move
	// running now or the one that was running before this step, which is the only other
	// instance that can have acted in it. A loss neither ledger accounts for is not charged
	// to whichever move happens to be running. The caller holds Sim.mu.
	settleLosses := func(running *runningMove, boss *mob) {
		struckBy := func(p *Player) (*runningMove, *mob) {
			for _, candidate := range []struct {
				r *runningMove
				m *mob
			}{{running, boss}, {held, heldBoss}} {
				r := candidate.r
				if r == nil || (r.phase != vnet.MovePhaseRelease && r.phase != vnet.MovePhaseChannel) {
					continue
				}
				key := [3]uint64{r.instanceID, uint64(r.pulseIndex), p.entityID}
				if _, hit := r.hit[p.entityID]; !hit || attributed[key] {
					continue
				}
				attributed[key] = true
				return r, candidate.m
			}
			return nil, nil
		}
		for _, b := range pt.bots {
			p := b.p
			if alive[p] && p.health < health[p] {
				r, m := struckBy(p)
				if r == nil {
					result.unattributed++
					result.damageTaken += int(health[p] - p.health)
				} else {
					pt.recordHitLocked(&result, b, m, r, health[p]-p.health)
					if inWindow && current == (playtestWindow{r.instanceID, r.pulseIndex}) {
						struck[p] = true
					}
				}
			}
			if alive[p] && !p.alive() {
				result.deaths++
			}
			health[p], alive[p] = p.health, p.alive()
		}
	}

	s.mu.Lock()
	bossID := pt.bossIDLocked()
	bossHealth := s.mobs[bossID].health
	for _, b := range pt.bots {
		health[b.p], alive[b.p] = b.p.health, b.p.alive()
		b.bossID = bossID
	}
	s.mu.Unlock()

	seconds := playtestLimitSeconds
	if pt.cfg.seconds > 0 {
		seconds = pt.cfg.seconds
	}
	for range uint64(seconds) * uint64(DefaultTickRate) {
		for _, b := range pt.bots {
			pt.perceive(b)
			pt.act(b)
		}
		if pt.beforeStep != nil {
			pt.beforeStep()
		}
		pt.step()

		s.mu.Lock()
		if id := pt.bossIDLocked(); id != bossID {
			// Every participant was down, and the wipe put a fresh boss at its home. The blow
			// that caused it belongs to the boss that was just replaced.
			result.wipes++
			settleLosses(nil, nil)
			closeWindow()
			held, heldBoss = nil, nil
			bossID, stage = id, 0
			bossHealth = s.mobs[bossID].health
			for _, b := range pt.bots {
				b.bossID, b.believed, b.believedKinds, b.escaping = bossID, nil, nil, 0
				clear(b.fleeing)
				clear(b.perceived)
			}
			clear(attributed)
		}
		boss := s.mobs[bossID]
		if boss == nil {
			result.killed = true
			result.killSeconds = float64(pt.tick-pullTick) / float64(DefaultTickRate)
			result.bossDamage["killing blow"] += int(bossHealth)
			s.mu.Unlock()
			closeWindow()
			break
		}
		if boss.encounter != nil && pullTick == 0 {
			pullTick = pt.tick
			result.scale = boss.encounter.scale
		}
		if boss.health < bossHealth {
			result.bossDamage[playtestBossState(boss)] += int(bossHealth - boss.health)
		}
		bossHealth = boss.health

		var running *runningMove
		if boss.encounter != nil {
			running = boss.encounter.running
			if boss.encounter.phase != stage {
				if stage != 0 {
					result.stageSeconds = append(result.stageSeconds, float64(pt.tick-pullTick)/float64(DefaultTickRate))
				}
				stage = boss.encounter.phase
			}
		}

		if running != nil {
			window := playtestWindow{running.instanceID, running.pulseIndex}
			announcing := running.phase == vnet.MovePhaseTelegraph || running.phase == vnet.MovePhaseChannel
			if announcing && (!inWindow || window != current) {
				closeWindow()
				inWindow, current, windowKind = true, window, running.def.kind
				stats := result.move(windowKind)
				stats.windows++
				if running.phase == vnet.MovePhaseTelegraph {
					stats.moves++
				}
				for _, b := range pt.bots {
					if b.p.alive() && b.p.protectionTicks == 0 && anyHazardReaches(running.hazards, b.p.box()) {
						threatened[b.p] = true
					}
				}
			}
			if running.phase == vnet.MovePhaseRecovery && uint64(running.startedTick) == pt.tick && recovery != pt.tick {
				recovery = pt.tick
				result.move(running.def.kind).recoveries[running.phaseTicks]++
			}
		}

		settleLosses(running, boss)

		if inWindow && (running == nil || running.instanceID != current.instance || running.phase == vnet.MovePhaseRecovery) {
			closeWindow()
		}
		held, heldBoss = running, boss
		s.mu.Unlock()
	}
	for kind, ticks := range pt.escapeTicks {
		result.move(kind).escapeTicks = ticks
	}
	return result
}

// recordHitLocked records one blow against a player. The caller holds Sim.mu.
func (pt *playtest) recordHitLocked(result *playtestResult, b *playtestBot, boss *mob, running *runningMove, damage uint16) {
	result.hits++
	result.damageTaken += int(damage)
	stats := result.move(running.def.kind)
	stats.hits++
	stats.damage += int(damage)

	ticks := pt.s.encounterMoves[running.def.kind]
	required := int64(ticks.telegraph)
	if running.phase == vnet.MovePhaseChannel {
		// A pulse fires on the last tick of its own shown interval.
		required = int64(ticks.channelPulse) - 1
	}
	if seen, ok := b.perceived[[2]uint64{running.instanceID, uint64(running.pulseIndex)}]; ok {
		warning := int64(pt.tick) - int64(seen)
		if warning < required {
			stats.premature++
		}
		if stats.required == math.MaxInt64 || warning-required < stats.minWarning-stats.required {
			stats.minWarning, stats.required = warning, required
		}
	} else {
		// Struck before the region ever reached the player: sooner than any promise.
		result.unperceived++
		stats.premature++
	}
	if running.def.travel == travelNone && running.def.flightSpeed == 0 && running.def.hazard.pulse == pulseNone {
		best := math.Inf(1)
		for _, sample := range horizontalSamples(b.p.box()) {
			best = min(best, math.Hypot(sample[0]-boss.pos[0], sample[1]-boss.pos[2]))
		}
		stats.contactMax = max(stats.contactMax, best)
	}
}

// playtestBossState names what a boss was doing when it lost health.
func playtestBossState(m *mob) string {
	switch {
	case m.encounter == nil:
		return "before the pull"
	case m.encounter.staggerTicks > 0:
		return "interrupt opening"
	case m.encounter.running == nil:
		return "pursuit"
	}
	switch m.encounter.running.phase {
	case vnet.MovePhaseTelegraph:
		return "telegraph"
	case vnet.MovePhaseRecovery:
		return "recovery"
	}
	return "release or pulse"
}

// perceive releases this player's delayed frames and updates what it believes.
func (pt *playtest) perceive(b *playtestBot) {
	released := 0
	for _, frame := range b.inbox {
		if frame.arrival+pt.delay > pt.tick {
			break
		}
		released++
		env := vnet.GetRootAsEnvelope(frame.bytes, 0)
		var table flatbuffers.Table
		if !env.Payload(&table) {
			continue
		}
		switch env.PayloadType() {
		case vnet.PayloadEntitySnapshot:
			var snapshot vnet.EntitySnapshot
			snapshot.Init(table.Bytes, table.Pos)
			b.bossSeen = false
			var state vnet.MobState
			var pos vnet.Vec3
			for i := range snapshot.MobsLength() {
				if snapshot.Mobs(&state, i) && state.EntityId() == b.bossID {
					state.Pos(&pos)
					b.bossPos = [3]float64{float64(pos.X()), float64(pos.Y()), float64(pos.Z())}
					b.bossSeen = true
				}
			}
			if !b.bossSeen {
				b.believed, b.believedKinds = nil, nil
			}
		case vnet.PayloadEncounterTimeline:
			var timeline vnet.EncounterTimeline
			timeline.Init(table.Bytes, table.Pos)
			if timeline.BossEntityId() != b.bossID {
				continue
			}
			b.believed, b.believedKinds = b.believed[:0], b.believedKinds[:0]
			var move vnet.EncounterMove
			var hazard vnet.HazardVolume
			for i := range timeline.MovesLength() {
				if !timeline.Moves(&move, i) || move.Ended() != vnet.MoveEndUnknown || move.Phase() == vnet.MovePhaseRecovery {
					continue
				}
				for j := range move.HazardsLength() {
					if move.Hazards(&hazard, j) {
						b.believed = append(b.believed, playtestHazard(&hazard))
						b.believedKinds = append(b.believedKinds, move.Kind())
					}
				}
				window := [2]uint64{move.MoveInstanceId(), uint64(move.PulseIndex())}
				if _, ok := b.perceived[window]; !ok {
					b.perceived[window] = pt.tick
				}
			}
		}
	}
	b.inbox = b.inbox[released:]
}

func playtestHazard(h *vnet.HazardVolume) protocol.HazardVolume {
	var origin, direction vnet.Vec3
	h.Origin(&origin)
	h.Direction(&direction)
	return protocol.HazardVolume{
		Shape:       h.Shape(),
		Origin:      [3]float32{origin.X(), origin.Y(), origin.Z()},
		Direction:   [3]float32{direction.X(), direction.Y(), direction.Z()},
		Radius:      h.Radius(),
		Height:      h.Height(),
		InnerRadius: h.InnerRadius(),
		HalfAngle:   h.HalfAngle(),
		HalfWidth:   h.HalfWidth(),
	}
}

// act sends this player's intent for the coming tick, from what it believes.
func (pt *playtest) act(b *playtestBot) {
	s := pt.s
	s.mu.Lock()
	alive, pos := b.p.alive(), b.p.pos
	s.mu.Unlock()
	if !alive || !b.bossSeen {
		return
	}

	me := playerBox(pos)
	bossBox := mobRegistry[pt.cfg.boss].body.boxAt(b.bossPos)
	centre, target := boxCentre(me), boxCentre(bossBox)
	dx, dy, dz := target[0]-centre[0], target[1]-centre[1], target[2]-centre[2]
	yaw := wrapAngle(math.Atan2(-dx, -dz))
	gap := boxDistance(me, bossBox)

	var walk [2]float64
	touched := pt.cfg.policy != policyStander && playtestTouches(b.believed, me)
	if touched {
		if b.escaping == 0 {
			b.escaping = pt.tick
		}
		for i, region := range b.believed {
			if playtestTouches([]protocol.HazardVolume{region}, me) {
				b.fleeing[b.believedKinds[i]] = true
			}
		}
	} else if b.escaping != 0 {
		for kind := range b.fleeing {
			pt.escapeTicks[kind] = max(pt.escapeTicks[kind], pt.tick-b.escaping)
		}
		b.escaping = 0
		clear(b.fleeing)
	}
	switch {
	case touched:
		if angle, ok := pt.escapeBearing(pos, b.believed); ok {
			walk = [2]float64{math.Cos(angle), math.Sin(angle)}
		}
	case gap > b.stand, b.stand > playtestStand && gap < b.stand-1:
		// Close to this player's gap, or, holding a ranged gap, back away from a boss that
		// pursued inside it — never onto believed danger either way.
		length := math.Hypot(dx, dz)
		toward := [2]float64{dx / length, dz / length}
		if gap < b.stand {
			toward = [2]float64{-toward[0], -toward[1]}
		}
		ahead := pos
		ahead[0] += toward[0] * WalkSpeed * s.dt * 2
		ahead[2] += toward[1] * WalkSpeed * s.dt * 2
		if pt.cfg.policy == policyStander || !playtestTouches(b.believed, playerBox(ahead)) {
			walk = toward
		}
	}

	// Intent is relative to the facing: +X is the right of yaw 0, which looks along -Z.
	right := [2]float64{math.Cos(yaw), -math.Sin(yaw)}
	forward := [2]float64{-math.Sin(yaw), -math.Cos(yaw)}
	b.clientTick++
	_ = b.p.Submit(protocol.PlayerInput{
		ClientTick: b.clientTick,
		MoveX:      float32(walk[0]*right[0] + walk[1]*right[1]),
		MoveZ:      float32(walk[0]*forward[0] + walk[1]*forward[1]),
		Yaw:        float32(yaw),
		Pitch:      float32(math.Atan2(dy, math.Hypot(dx, dz))),
	})
	if gap <= SwordReach-0.05 && pt.cfg.policy != policyEvader {
		// Refused while the blade recovers, which is the attack cadence itself.
		_, _ = b.p.Attack(protocol.AttackRequest{Slot: 0, ClientTick: b.clientTick})
	}
}

// escapeBearing is the straight walk that leaves every believed region soonest, walked with
// the player's own collision and step-up so a wall or monolith ends a bearing.
func (pt *playtest) escapeBearing(start [3]float64, regions []protocol.HazardVolume) (float64, bool) {
	const bearings = 32
	s := pt.s
	s.mu.Lock()
	defer s.mu.Unlock()
	horizon := int(ticksFor(playtestEscapeHorizon, DefaultTickRate))
	best, bestTicks := 0.0, horizon+1
	for i := range bearings {
		angle := 2 * math.Pi * float64(i) / bearings
		delta := [3]float64{math.Cos(angle) * WalkSpeed * s.dt, 0, math.Sin(angle) * WalkSpeed * s.dt}
		pos := start
		for ticks := 1; ticks < bestTicks; ticks++ {
			next, blocked := moveAndCollideWithStep(s.terrain, playerBody, pos, delta, playerStepHeight)
			if blocked[0] || blocked[2] {
				break
			}
			pos = next
			if !playtestTouches(regions, playerBox(pos)) {
				best, bestTicks = angle, ticks
				break
			}
		}
	}
	return best, bestTicks <= horizon
}

// playtestTouches reports whether any believed region reaches a body or the margin around it.
//
// Sampled as a grid of points over the enlarged footprint rather than by handing the enlarged
// box to [hazardReaches]. That function samples a box's centre and corners, which is the
// server's own conservative contact rule, and it is not monotone in the box: enlarging a body
// standing at the start of a lane moves every corner behind the lane's origin, so a larger box
// can miss where the body inside it is struck. A reader that trusted it stopped short of a
// King's Sentence lane and was hit; the grid is what keeps the reader's "clear" a superset of
// the server's "not reached".
func playtestTouches(regions []protocol.HazardVolume, body box) bool {
	const samples = 7
	for i := range samples {
		for j := range samples {
			x := body.min[0] - playtestMargin + (body.max[0]-body.min[0]+2*playtestMargin)*float64(i)/(samples-1)
			z := body.min[2] - playtestMargin + (body.max[2]-body.min[2]+2*playtestMargin)*float64(j)/(samples-1)
			point := box{min: [3]float64{x, body.min[1], z}, max: [3]float64{x, body.max[1], z}}
			if anyHazardReaches(regions, point) {
				return true
			}
		}
	}
	return false
}

// TestFirstDungeonReadersEscapeEveryAnnouncedRegion is the always-run half of the
// measurement. A reader perceiving the fight through its own frames — at no delay, at 250 and
// 400 ms of delay, and through 200 ms stalls every second — kills both bosses solo in the
// starter kit and as a party of four in iron without a single hit: no schedule in either
// fight announced a region, a combination or a pulse sequence its walking target could not
// leave in time under those conditions. The review record names the conditions that fail.
func TestFirstDungeonReadersEscapeEveryAnnouncedRegion(t *testing.T) {
	for _, boss := range []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing} {
		for _, setup := range []struct {
			party int
			kit   playtestKit
		}{{1, kitRusty}, {4, kitIron}} {
			for _, network := range []playtestNetwork{{}, {delayMillis: 250}, {delayMillis: 400}, {lossEvery: 20, lossBurst: 4}} {
				cfg := playtestConfig{boss: boss, party: setup.party, kit: setup.kit, network: network}
				t.Run(cfg.String(), func(t *testing.T) {
					r := newPlaytest(t, cfg).run()
					if !r.killed || r.hits != 0 || r.unattributed != 0 || r.deaths != 0 || r.wipes != 0 {
						t.Fatalf("killed=%v hits=%d unattributed=%d deaths=%d wipes=%d; want a clean kill\n%s",
							r.killed, r.hits, r.unattributed, r.deaths, r.wipes, r)
					}
					for kind, stats := range r.moves {
						if stats.threatened != stats.escaped {
							t.Errorf("%s: escaped %d of %d threatened windows", kind, stats.escaped, stats.threatened)
						}
					}
				})
			}
		}
	}
}

// TestFirstDungeonEveryStageRepertoireIsEscapable holds each boss at each stage for a
// minute and a half against players who read and escape but never swing, so every move and
// combination a stage unlocks is announced many times — the rituals and the requiem
// included, which a party that is fighting kills the king before seeing. None lands.
func TestFirstDungeonEveryStageRepertoireIsEscapable(t *testing.T) {
	for _, boss := range []struct {
		kind   vnet.MobKind
		stages uint8
	}{{vnet.MobKindVargrGuardian, 2}, {vnet.MobKindDraugrKing, 3}} {
		for stage := uint8(1); stage <= boss.stages; stage++ {
			for _, setup := range []struct{ party, ranged int }{{1, 0}, {1, 1}, {4, 2}} {
				cfg := playtestConfig{boss: boss.kind, party: setup.party, ranged: setup.ranged, kit: kitRusty,
					policy: policyEvader, stage: stage, seconds: 90}
				t.Run(cfg.String(), func(t *testing.T) {
					r := newPlaytest(t, cfg).run()
					if r.hits != 0 || r.unattributed != 0 || r.deaths != 0 || r.wipes != 0 {
						t.Fatalf("hits=%d unattributed=%d deaths=%d wipes=%d; want every announced region left in time\n%s",
							r.hits, r.unattributed, r.deaths, r.wipes, r)
					}
					moves := 0
					for _, stats := range r.moves {
						moves += stats.moves
					}
					if moves == 0 {
						t.Fatalf("the boss announced nothing in %d seconds\n%s", cfg.seconds, r)
					}
				})
			}
		}
	}
}

// TestFirstDungeonRitualPulsesSurviveLatency holds the king at his ritual stages against
// evaders perceiving the fight 400 ms late, and 250 ms late through 200 ms stalls every second.
// No burial, edict or requiem pulse lands. Before #1037 lengthened the shown intervals, a
// burial pulse struck 218 of 301 threatened windows at 400 ms.
func TestFirstDungeonRitualPulsesSurviveLatency(t *testing.T) {
	rituals := []vnet.EncounterMoveKind{vnet.EncounterMoveKindBurial, vnet.EncounterMoveKindEdictOfTheGraves, vnet.EncounterMoveKindRequiemOfTheBuried}
	for stage := uint8(2); stage <= 3; stage++ {
		for _, network := range []playtestNetwork{{delayMillis: 400}, {delayMillis: 250, lossEvery: 20, lossBurst: 4}} {
			for _, setup := range []struct{ party, ranged int }{{1, 0}, {1, 1}, {4, 2}} {
				cfg := playtestConfig{boss: vnet.MobKindDraugrKing, party: setup.party, ranged: setup.ranged, kit: kitRusty,
					policy: policyEvader, network: network, stage: stage, seconds: 120}
				t.Run(cfg.String(), func(t *testing.T) {
					r := newPlaytest(t, cfg).run()
					announced := 0
					for _, kind := range rituals {
						if stats := r.moves[kind]; stats != nil {
							announced += stats.windows
							if stats.hits != 0 {
								t.Errorf("%s: %d pulses landed on players who were escaping", kind, stats.hits)
							}
						}
					}
					if announced == 0 || r.unattributed != 0 {
						t.Fatalf("pulse windows=%d unattributed=%d; want rituals measured\n%s", announced, r.unattributed, r)
					}
				})
			}
		}
	}
}

// TestFirstDungeonDamageNeverPrecedesItsPerceivedAnnouncement is the negative control. A
// stander ignores what it is shown, so it is struck, and no blow lands sooner after the player
// perceived that region than the announcement its own phase promises: the telegraph for a
// release, the shown interval for a pulse. Every health loss is a new entry in the running
// window's hit ledger, so none is charged to the wrong move. The stage-two king adds burial
// and edict pulses, so one move kind's promise is never compared with another's.
func TestFirstDungeonDamageNeverPrecedesItsPerceivedAnnouncement(t *testing.T) {
	for _, cfg := range []playtestConfig{
		{boss: vnet.MobKindVargrGuardian, party: 1, kit: kitIron, policy: policyStander},
		{boss: vnet.MobKindVargrGuardian, party: 3, kit: kitRusty, policy: policyStander},
		{boss: vnet.MobKindDraugrKing, party: 1, kit: kitIron, policy: policyStander},
		{boss: vnet.MobKindDraugrKing, party: 3, kit: kitRusty, policy: policyStander},
		{boss: vnet.MobKindDraugrKing, party: 2, kit: kitIron, policy: policyStander, ranged: 1, stage: 2, seconds: 60},
	} {
		t.Run(cfg.String(), func(t *testing.T) {
			r := newPlaytest(t, cfg).run()
			assertPlaytestBlowsAnnounced(t, r)
		})
	}
}

func assertPlaytestBlowsAnnounced(t *testing.T, r playtestResult) {
	t.Helper()
	if r.hits == 0 || r.unattributed != 0 {
		t.Fatalf("hits=%d unattributed=%d; want struck players and every loss charged to its window\n%s", r.hits, r.unattributed, r)
	}
	for _, kind := range r.sortedKinds() {
		if stats := r.moves[kind]; stats.premature != 0 {
			t.Errorf("%s: %d of %d blows landed sooner after their region was perceived than their phase promises, "+
				"%d of them before it was perceived at all", kind, stats.premature, stats.hits, r.unperceived)
		}
	}
	if r.unperceived != 0 {
		t.Errorf("%d blows struck a region the player had not perceived\n%s", r.unperceived, r)
	}
}

// TestFirstDungeonPlaytestRefereeCatchesWhatItClaims mutates the running fight to prove the two
// checks above can fail. Cutting a telegraph or a pulse short after it was announced makes its
// blow land sooner than the frames promised; health lost that no window's hit ledger accounts
// for must not be charged to the move that happens to be running.
func TestFirstDungeonPlaytestRefereeCatchesWhatItClaims(t *testing.T) {
	// shorten cuts the announced phase short once, on the tick it begins, leaving the published
	// length untouched: exactly a server that damages before its own announcement.
	shorten := func(pt *playtest, phase vnet.MovePhase) func() {
		return func() {
			pt.s.mu.Lock()
			defer pt.s.mu.Unlock()
			boss := pt.s.mobs[pt.bossIDLocked()]
			if boss == nil || boss.encounter == nil || boss.encounter.running == nil {
				return
			}
			if r := boss.encounter.running; r.phase == phase && r.remaining+1 == r.phaseTicks {
				r.remaining = 2
			}
		}
	}
	for _, c := range []struct {
		name  string
		cfg   playtestConfig
		phase vnet.MovePhase
	}{
		{"telegraph", playtestConfig{boss: vnet.MobKindVargrGuardian, party: 1, kit: kitIron, policy: policyStander}, vnet.MovePhaseTelegraph},
		{"pulse", playtestConfig{boss: vnet.MobKindDraugrKing, party: 2, kit: kitIron, policy: policyStander, ranged: 1, stage: 2, seconds: 60}, vnet.MovePhaseChannel},
	} {
		t.Run(c.name, func(t *testing.T) {
			pt := newPlaytest(t, c.cfg)
			pt.beforeStep = shorten(pt, c.phase)
			r := pt.run()
			premature := 0
			for _, stats := range r.moves {
				premature += stats.premature
			}
			if premature == 0 {
				t.Fatalf("a %s cut short after its announcement was not reported as premature\n%s", c.name, r)
			}
		})
	}
	t.Run("unattributed", func(t *testing.T) {
		pt := newPlaytest(t, playtestConfig{boss: vnet.MobKindVargrGuardian, party: 1, kit: kitIron, policy: policyEvader})
		pt.beforeStep = func() {
			pt.s.mu.Lock()
			defer pt.s.mu.Unlock()
			boss := pt.s.mobs[pt.bossIDLocked()]
			if boss != nil && boss.encounter != nil && boss.encounter.running != nil &&
				boss.encounter.running.phase == vnet.MovePhaseTelegraph && boss.encounter.running.remaining > 3 {
				pt.bots[0].p.damageLocked(1)
			}
		}
		r := pt.run()
		if r.unattributed == 0 || r.hits != 0 {
			t.Fatalf("health lost during a telegraph: unattributed=%d hits=%d; want it kept out of every move\n%s", r.unattributed, r.hits, r)
		}
	})
}

// TestFirstDungeonPlaytest is the full measurement matrix: every party size from one to
// four, every entry kit, both policies, then a delay and loss sweep. It is a measurement,
// not a gate — it fails only when the harness breaks — and it reports otherwise.
//
//	cd server && VOXELHEIM_PLAYTEST=1 VOXELHEIM_PLAYTEST_DIR=<review-output> \
//	    go test ./internal/game -run TestFirstDungeonPlaytest -v
func TestFirstDungeonPlaytest(t *testing.T) {
	if os.Getenv(playtestEnv) == "" {
		t.Skipf("measurement harness for docs/reviews/dungeon-combat-1037.md; set %s=1 to run it", playtestEnv)
	}
	var results []playtestResult
	for _, boss := range []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing} {
		for _, kit := range []playtestKit{kitRusty, kitLeather, kitIron} {
			for party := 1; party <= 4; party++ {
				for _, policy := range []playtestPolicy{policyReader, policyStander} {
					results = append(results, runPlaytest(t, playtestConfig{boss: boss, party: party, kit: kit, policy: policy}))
				}
			}
		}
		// Representative levels (#1099): solo at ten and thirty, a mixed party and a full
		// party at the level cap. Health reads only the member count, so a reader's kill time
		// should not move; the stander is where the level-scaled blows show.
		for _, levels := range [][]uint16{{10}, {30}, {1, 10, 20, 30}, {30, 30, 30, 30}} {
			for _, policy := range []playtestPolicy{policyReader, policyStander} {
				results = append(results, runPlaytest(t, playtestConfig{boss: boss, party: len(levels), kit: kitIron, policy: policy, levels: levels}))
			}
		}
		for _, network := range []playtestNetwork{
			{delayMillis: 100}, {delayMillis: 250}, {delayMillis: 400}, {delayMillis: 500}, {delayMillis: 600}, {delayMillis: 800},
			{lossEvery: 2, lossBurst: 1}, {lossEvery: 20, lossBurst: 4}, {delayMillis: 250, lossEvery: 20, lossBurst: 4},
		} {
			for _, setup := range []struct {
				party int
				kit   playtestKit
			}{{1, kitRusty}, {1, kitIron}, {4, kitIron}} {
				results = append(results, runPlaytest(t, playtestConfig{boss: boss, party: setup.party, kit: setup.kit, network: network}))
			}
		}
	}
	for _, boss := range []struct {
		kind   vnet.MobKind
		stages uint8
	}{{vnet.MobKindVargrGuardian, 2}, {vnet.MobKindDraugrKing, 3}} {
		for stage := uint8(1); stage <= boss.stages; stage++ {
			for _, network := range []playtestNetwork{{}, {delayMillis: 250}, {delayMillis: 400}, {lossEvery: 20, lossBurst: 4}} {
				for _, setup := range []struct{ party, ranged int }{{1, 0}, {1, 1}, {4, 0}, {4, 2}} {
					results = append(results, runPlaytest(t, playtestConfig{boss: boss.kind, party: setup.party, ranged: setup.ranged,
						kit: kitIron, policy: policyEvader, network: network, stage: stage, seconds: 300}))
				}
			}
		}
	}
	if dir := os.Getenv(playtestDirEnv); dir != "" {
		writePlaytestCSV(t, filepath.Join(dir, "dungeon-combat-1037-fights.csv"), results)
		writePlaytestMoveCSV(t, filepath.Join(dir, "dungeon-combat-1037-moves.csv"), results)
	}
}

func runPlaytest(t *testing.T, cfg playtestConfig) playtestResult {
	t.Helper()
	var result playtestResult
	t.Run(cfg.String(), func(t *testing.T) {
		pt := newPlaytest(t, cfg)
		result = pt.run()
		result.stepNanos = pt.stepNanos
		t.Log(result)
	})
	return result
}

func (r playtestResult) sortedKinds() []vnet.EncounterMoveKind {
	kinds := make([]vnet.EncounterMoveKind, 0, len(r.moves))
	for kind := range r.moves {
		kinds = append(kinds, kind)
	}
	slices.SortFunc(kinds, func(a, b vnet.EncounterMoveKind) int { return cmp.Compare(a, b) })
	return kinds
}

func (r playtestResult) String() string {
	var b strings.Builder
	fmt.Fprintf(&b, "killed=%v after %.2fs stages=%v deaths=%d wipes=%d hits=%d damage-taken=%d unperceived=%d unattributed=%d boss-damage=%v scale=%d/%d/%d%%",
		r.killed, r.killSeconds, r.stageSeconds, r.deaths, r.wipes, r.hits, r.damageTaken, r.unperceived, r.unattributed, r.bossDamage,
		r.scale.members, r.scale.maxHealth, r.scale.damagePercent)
	for _, kind := range r.sortedKinds() {
		s := r.moves[kind]
		fmt.Fprintf(&b, "\n  %-20s moves=%d windows=%d threatened=%d escaped=%d hits=%d damage=%d",
			kind, s.moves, s.windows, s.threatened, s.escaped, s.hits, s.damage)
		if s.hits > 0 {
			fmt.Fprintf(&b, " warning>=%d/%d premature=%d", s.minWarning, s.required, s.premature)
		}
		if s.contactMax >= 0 {
			fmt.Fprintf(&b, " contact<=%.3f", s.contactMax)
		}
		fmt.Fprintf(&b, " escape<=%d recoveries=%v", s.escapeTicks, s.recoveries)
	}
	return b.String()
}

func writePlaytestCSV(t *testing.T, path string, results []playtestResult) {
	t.Helper()
	var b strings.Builder
	b.WriteString("boss,party,ranged,kit,policy,network,stage,killed,kill_seconds,stage_seconds,deaths,wipes,hits,damage_taken,unperceived_hits,unattributed_losses,boss_damage_recovery,boss_damage_telegraph,boss_damage_release,boss_damage_pursuit,boss_damage_other,levels,boss_max_health,boss_damage_percent\n")
	for _, r := range results {
		c := r.config
		stages := make([]string, len(r.stageSeconds))
		for i, v := range r.stageSeconds {
			stages[i] = fmt.Sprintf("%.2f", v)
		}
		other := r.bossDamage["killing blow"] + r.bossDamage["interrupt opening"] + r.bossDamage["before the pull"]
		fmt.Fprintf(&b, "%s,%d,%d,%s,%s,%s,%d,%v,%.2f,%s,%d,%d,%d,%d,%d,%d,%d,%d,%d,%d,%d,%s,%d,%d\n",
			playtestBossName(c.boss), c.party, c.ranged, c.kit.name, c.policy, c.network, c.stage, r.killed, r.killSeconds,
			strings.Join(stages, ";"), r.deaths, r.wipes, r.hits, r.damageTaken, r.unperceived, r.unattributed, r.bossDamage["recovery"], r.bossDamage["telegraph"],
			r.bossDamage["release or pulse"], r.bossDamage["pursuit"], other, c.levelsLabel(), r.scale.maxHealth, r.scale.damagePercent)
	}
	if err := os.WriteFile(path, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
}

func writePlaytestMoveCSV(t *testing.T, path string, results []playtestResult) {
	t.Helper()
	var b strings.Builder
	b.WriteString("boss,party,ranged,kit,policy,network,stage,move,moves,windows,threatened,escaped,hits,damage,premature,min_warning_ticks,required_ticks,max_contact,max_escape_ticks,recoveries\n")
	for _, r := range results {
		c := r.config
		for _, kind := range r.sortedKinds() {
			s := r.moves[kind]
			warning, required := "", ""
			if s.required != math.MaxInt64 {
				warning, required = fmt.Sprint(s.minWarning), fmt.Sprint(s.required)
			}
			contact := ""
			if s.contactMax >= 0 {
				contact = fmt.Sprintf("%.3f", s.contactMax)
			}
			lengths := slices.Sorted(func(yield func(uint32) bool) {
				for length := range s.recoveries {
					if !yield(length) {
						return
					}
				}
			})
			recoveries := make([]string, len(lengths))
			for i, length := range lengths {
				recoveries[i] = fmt.Sprintf("%dx%d", length, s.recoveries[length])
			}
			fmt.Fprintf(&b, "%s,%d,%d,%s,%s,%s,%d,%s,%d,%d,%d,%d,%d,%d,%d,%s,%s,%s,%d,%s\n",
				playtestBossName(c.boss), c.party, c.ranged, c.kit.name, c.policy, c.network, c.stage, kind, s.moves, s.windows,
				s.threatened, s.escaped, s.hits, s.damage, s.premature, warning, required, contact, s.escapeTicks, strings.Join(recoveries, ";"))
		}
	}
	if err := os.WriteFile(path, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
}
