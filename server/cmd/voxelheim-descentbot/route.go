package main

import (
	"context"
	"errors"
	"fmt"
	"math"
	"sort"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The route, from the open world's portal to the return portal.
//
// The anchors come from world.InstanceDungeonAnchors for the seed the WorldChange named:
// the map a player learns by looking. Whether a door is open, a rune lit or a web still
// there, and where every creature stands, are read off the stream; the rune order is read
// off the delivered inscription and only then checked against world.InstanceRuneOrder.
//
// /additem gives the iron blade and rusty armour before the portal. /immortal is on for
// the boss fights, which the bot plays without evading, and for the rest of any phase that
// has cost maxDeaths deaths. /teleport places the bot beside the open world's portal
// before it walks in (a veil is never a path cell, so no walk reaches it from spawn), and
// is otherwise only [pilot.assist]; the report counts both. None of them opens a door,
// lights a rune, kills a creature or moves the bot past anything the server has not opened.

// maxDeaths is how many deaths one phase may cost before the bot finishes it under
// /immortal, so that a bot that cannot evade still reaches every later part of the route.
var maxDeaths = 3

type layout struct {
	seed                          int64
	arrival, exit                 world.PlacedAnchor
	guardian, king, gate          world.PlacedAnchor
	checkpoints                   [3]world.PlacedAnchor
	stones, runeDoor              []world.PlacedAnchor
	grilleLever                   world.PlacedAnchor
	twins, twinDoor, shortcut     []world.PlacedAnchor
	minors                        map[int][]world.PlacedAnchor
	caveLow, caveHigh, sandCentre cell
}

func layoutFor(seed int64) (layout, error) {
	l := layout{seed: seed, minors: map[int][]world.PlacedAnchor{}}
	l.arrival, l.exit = world.InstanceAnchors(seed)
	l.guardian, l.king, l.gate = world.InstanceEncounterAnchors(seed)
	triggers := map[int][]world.PlacedAnchor{}
	for _, a := range world.InstanceDungeonAnchors(seed) {
		switch a.Kind {
		case world.AnchorInstanceCheckpoint:
			if a.Index < 0 || a.Index >= len(l.checkpoints) {
				return l, fmt.Errorf("checkpoint anchor index %d outside the route's %d", a.Index, len(l.checkpoints))
			}
			l.checkpoints[a.Index] = a
		case world.AnchorInstanceMinorSpawn:
			l.minors[a.Index] = append(l.minors[a.Index], a)
		case world.AnchorInstanceTrigger:
			triggers[a.Index] = append(triggers[a.Index], a)
		case world.AnchorInstanceMechanism:
			switch a.Index {
			case world.RunePuzzle:
				l.stones = append(l.stones, a)
			case world.GrillePuzzle:
				l.grilleLever = a
			case world.TwinLeverPuzzle:
				l.twins = append(l.twins, a)
			}
		case world.AnchorInstanceDoor:
			switch a.Index {
			case world.RunePuzzle:
				l.runeDoor = append(l.runeDoor, a)
			case world.TwinLeverPuzzle:
				l.twinDoor = append(l.twinDoor, a)
			case world.ReturnShortcutDoor:
				l.shortcut = append(l.shortcut, a)
			}
		}
	}
	// A layout the route cannot be played over is a failed run with a report, not a panic.
	switch {
	case len(triggers[world.CaveTrigger]) != 2 || len(triggers[world.SandTrigger]) != 2:
		return l, fmt.Errorf("the cave and sand triggers have %d and %d corner anchors, want 2 each",
			len(triggers[world.CaveTrigger]), len(triggers[world.SandTrigger]))
	case len(l.stones) != 4 || len(l.runeDoor) == 0:
		return l, fmt.Errorf("%d rune stones and %d rune door cells, want 4 and at least one", len(l.stones), len(l.runeDoor))
	case len(l.twins) != 2 || len(l.twinDoor) == 0 || len(l.shortcut) == 0:
		return l, fmt.Errorf("%d twin levers, %d twin door cells and %d shortcut door cells", len(l.twins), len(l.twinDoor), len(l.shortcut))
	case len(l.minors[world.SandBuriedGroup]) == 0:
		return l, errors.New("the layout has no buried scorpion slots")
	}
	corners := func(t []world.PlacedAnchor) (cell, cell) {
		return cell{min(t[0].X, t[1].X), min(t[0].Y, t[1].Y), min(t[0].Z, t[1].Z)},
			cell{max(t[0].X, t[1].X), max(t[0].Y, t[1].Y), max(t[0].Z, t[1].Z)}
	}
	l.caveLow, l.caveHigh = corners(triggers[world.CaveTrigger])
	lo, hi := corners(triggers[world.SandTrigger])
	l.sandCentre = cell{(lo[0] + hi[0]) / 2, lo[1], (lo[2] + hi[2]) / 2}
	return l, nil
}

func at(a world.PlacedAnchor) cell { return cell{a.X, a.Y, a.Z} }

// near is a goal: any standing cell within radius blocks of c on the horizontal, within a
// few courses of it.
func near(c cell, radius float64) func(cell) bool {
	return func(x cell) bool {
		return math.Hypot(float64(x[0]-c[0]), float64(x[2]-c[2])) <= radius && abs(x[1]-c[1]) <= 3
	}
}

func exactly(c cell) func(cell) bool { return func(x cell) bool { return x == c } }

func abs(v int64) int64 {
	if v < 0 {
		return -v
	}
	return v
}

// runner is one bot playing the route.
type runner struct {
	*pilot
	opts   options
	lay    layout
	server *serverProcess
	timing map[string]time.Duration
	rune   runeReading
	rss    map[string]uint64
	// websCut is how many cobwebs of the curtain the bot cut.
	websCut int
}

// phase runs one part of the route, retrying it after each death: a respawn puts the bot
// back at the furthest checkpoint and the phase walks on from there.
func (r *runner) phase(ctx context.Context, name string, body func(context.Context) error) error {
	r.say("phase: %s", name)
	r.stats.begin(name)
	for {
		if r.stats.deathsIn(name) >= maxDeaths {
			if err := r.setImmortal(ctx, true); err != nil {
				return err
			}
		}
		err := body(ctx)
		if !errors.Is(err, errDied) {
			if immErr := r.setImmortal(ctx, false); err == nil {
				err = immErr
			}
			if err != nil {
				return fmt.Errorf("%s: %w", name, err)
			}
			return nil
		}
		r.say("died in %s (%d so far); waiting for the respawn", name, r.stats.deathsIn(name))
		if err := r.waitAlive(ctx); err != nil {
			return err
		}
	}
}

func (r *runner) sampleRSS(label string) {
	if rss, err := residentBytes(r.server.cmd.Process.Pid); err == nil {
		r.rss[label] = rss
	}
}

// play is the whole route.
func (r *runner) play(ctx context.Context) error {
	if err := r.equip(ctx); err != nil {
		return fmt.Errorf("equip: %w", err)
	}
	r.sampleRSS("open world, before the portal")
	start := time.Now()
	if err := r.enterPortal(ctx); err != nil {
		return fmt.Errorf("portal: %w", err)
	}
	r.sampleRSS("inside, on arrival")
	steps := []struct {
		name string
		body func(context.Context) error
	}{
		{"upper halls", r.upperHalls},
		{"rune hall", r.runeHall},
		{"guardian", r.guardianFight},
		{"chasm and pool", r.drop},
		{"cave waves", r.caveWaves},
		{"web curtain", r.webCurtain},
		{"timed grille", r.timedGrille},
		{"sand hall", r.sandHall},
		{"twin levers", r.twinLevers},
		{"king", r.kingFight},
		{"return shortcut", r.returnShortcut},
	}
	for _, s := range steps {
		began := time.Now()
		if err := r.phase(ctx, s.name, s.body); err != nil {
			return err
		}
		r.timing[s.name] = time.Since(began)
	}
	r.timing["total"] = time.Since(start)
	r.sampleRSS("back in the open world")
	r.stats.finish()
	return nil
}

// equip puts the #1099 reference's blade in the main hand and rusty armour on.
func (r *runner) equip(ctx context.Context) error {
	wear := []struct {
		item game.ItemID
		slot uint8
	}{
		{game.ItemIronSword, mainHandSlot},
		{game.ItemRustyHelm, uint8(protocol.InventorySlots - protocol.EquipmentSlots)},
		{game.ItemRustyCuirass, uint8(protocol.InventorySlots - protocol.EquipmentSlots + 1)},
		{game.ItemRustyGreaves, uint8(protocol.InventorySlots - protocol.EquipmentSlots + 2)},
	}
	for _, w := range wear {
		if _, err := r.c.command(ctx, fmt.Sprintf("/additem %d 1", w.item)); err != nil {
			return err
		}
		if err := sleep(ctx, 300*time.Millisecond); err != nil {
			return err
		}
		from := -1
		r.c.mu.Lock()
		for slot := 0; slot+1 < len(r.c.stacks) && slot/2 < int(protocol.InventorySlots-protocol.EquipmentSlots); slot += 2 {
			if game.ItemID(r.c.stacks[slot]) == w.item {
				from = slot / 2
				break
			}
		}
		r.c.mu.Unlock()
		if from < 0 {
			return fmt.Errorf("item %d never reached the pack", w.item)
		}
		if err := r.c.send(protocol.EncodeInventoryMoveRequest(protocol.InventoryMoveRequest{
			From: uint8(from), To: w.slot, Count: 1,
		})); err != nil {
			return err
		}
		if err := sleep(ctx, 300*time.Millisecond); err != nil {
			return err
		}
		r.c.mu.Lock()
		worn := int(w.slot)*2 < len(r.c.stacks) && game.ItemID(r.c.stacks[int(w.slot)*2]) == w.item
		r.c.mu.Unlock()
		if !worn {
			return fmt.Errorf("item %d is not worn in slot %d", w.item, w.slot)
		}
	}
	return nil
}

// enterPortal walks into the open world's portal and answers any offer it makes.
func (r *runner) enterPortal(ctx context.Context) error {
	ruin, ok := world.PortalRuin(r.opts.seed)
	if !ok {
		return errors.New("this world has no portal ruin")
	}
	threshold := ruin.Threshold()
	floor := threshold.Heart[1]
	for _, c := range threshold.Cells {
		floor = min(floor, c[1])
	}
	axis := threshold.Normal
	for attempt, side := range []int64{-3, 3, -4, 4, -2, 2} {
		spot := threshold.Heart
		spot[axis] += side
		spot[1] = floor
		// Centre the body on the heart's cell across the opening: /teleport takes a
		// corner, so the corner half a block into the heart column along the opening.
		across := 2 - axis
		spot[across]++
		r.stats.portalPlacement()
		if _, err := r.c.command(ctx, fmt.Sprintf("/teleport %d %d %d", spot[0], spot[1], spot[2])); err != nil {
			return err
		}
		if err := sleep(ctx, 3*time.Second); err != nil {
			return err
		}
		dir := [2]float64{}
		if axis == 0 {
			dir[0] = -float64(side)
		} else {
			dir[1] = -float64(side)
		}
		r.c.setIntent(intent{moveZ: 1, yaw: yawToward(dir[0], dir[1])})
		deadline := time.Now().Add(4 * time.Second)
		for time.Now().Before(deadline) {
			select {
			case offer := <-r.c.offers:
				r.say("entry offer %d; accepting", offer)
				if err := r.c.send(protocol.EncodeInstanceEntryAnswer(protocol.InstanceEntryAnswer{OfferID: offer, Accept: true})); err != nil {
					return err
				}
			case change := <-r.c.changes:
				r.c.stand()
				if change.id == 0 {
					continue
				}
				lay, err := layoutFor(change.seed)
				if err != nil {
					return fmt.Errorf("instance seed %d: %w", change.seed, err)
				}
				r.lay = lay
				r.say("entered instance %d, seed %d (attempt %d)", change.id, change.seed, attempt+1)
				return r.waitTerrain(ctx, at(r.lay.arrival))
			case <-ctx.Done():
				return ctx.Err()
			case <-time.After(50 * time.Millisecond):
			}
		}
		r.c.stand()
	}
	return errors.New("never crossed the portal")
}

// waitTerrain waits until the stream has delivered a standing cell.
func (r *runner) waitTerrain(ctx context.Context, c cell) error {
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		ready := false
		r.c.withView(func(v *blockView) { ready = v.standable(c) })
		if ready {
			return sleep(ctx, time.Second)
		}
		if err := sleep(ctx, 100*time.Millisecond); err != nil {
			return err
		}
	}
	return fmt.Errorf("the terrain at %v never arrived", c)
}

// upperHalls walks the draugr and vargr halls to the rune hall, putting down every
// creature that turns on the bot, and then any hall creature still standing.
func (r *runner) upperHalls(ctx context.Context) error {
	for group := 0; group < 4; group++ {
		slots := r.lay.minors[group]
		if len(slots) == 0 {
			continue
		}
		mid := at(slots[len(slots)/2])
		if err := r.walkTo(ctx, fmt.Sprintf("minor group %d", group), near(mid, 2.5), true); err != nil {
			return err
		}
	}
	floor := r.lay.arrival.Y
	return r.fight(ctx, func(m mobView) bool {
		return (m.kind == vnet.MobKindDraugr || m.kind == vnet.MobKindVargr) && math.Abs(m.pos[1]-float64(floor)) < 4
	})
}

// runeReading is what the inscription said and whether the layout agrees.
type runeReading struct {
	read, want [4]int
	lit        [4]int
}

// runeHall reads the inscription off the delivered blocks and presses the stones in the
// order it gives.
func (r *runner) runeHall(ctx context.Context) error {
	stones := r.lay.stones
	if len(stones) != 4 || len(r.lay.runeDoor) == 0 {
		return fmt.Errorf("%d rune stones and %d door cells in the layout", len(stones), len(r.lay.runeDoor))
	}
	if err := r.walkTo(ctx, "the rune stones", near(at(stones[1]), 3), true); err != nil {
		return err
	}
	reading, err := r.readInscription()
	if err != nil {
		return err
	}
	r.rune = reading
	if reading.read != reading.want {
		return fmt.Errorf("the inscription reads %v and world.InstanceRuneOrder says %v", reading.read, reading.want)
	}
	for _, k := range reading.read {
		stone := at(stones[k])
		if err := r.walkTo(ctx, fmt.Sprintf("stone %d", k), r.besideGoal(stone, 0), true); err != nil {
			return err
		}
		if b, _ := r.c.blockAt(stone); b == world.RuneStoneLit {
			continue // already lit by an attempt before a death
		}
		if err := r.use(ctx, stone); err != nil {
			return err
		}
	}
	return r.waitOpen(ctx, r.lay.runeDoor, 5*time.Second)
}

// readInscription counts the lit runes above each stone's place in the wall the door is
// in: the stone pressed k-th has k+1.
func (r *runner) readInscription() (runeReading, error) {
	reading := runeReading{want: world.InstanceRuneOrder(r.lay.seed)}
	door := r.lay.runeDoor[0]
	alongX := true // the door's wall is a plane of constant Z
	for _, d := range r.lay.runeDoor {
		if d.Z != door.Z {
			alongX = false
		}
	}
	seen := [4]bool{}
	for k, s := range r.lay.stones {
		column := cell{s.X, s.Y, door.Z}
		if !alongX {
			column = cell{door.X, s.Y, s.Z}
		}
		lit := 0
		for y := column[1] + 1; y <= column[1]+5; y++ {
			b, ok := r.c.blockAt(cell{column[0], y, column[2]})
			if !ok {
				return reading, fmt.Errorf("the inscription over stone %d has not been delivered", k)
			}
			if b == world.RuneStoneLit {
				lit++
			}
		}
		reading.lit[k] = lit
		if lit < 1 || lit > 4 || seen[lit-1] {
			return reading, fmt.Errorf("stone %d has %d lit runes over it; the inscription is not a permutation", k, lit)
		}
		seen[lit-1] = true
		reading.read[lit-1] = k
	}
	return reading, nil
}

// besideGoal is a standing cell touching a mechanism's column, dy courses under it.
func (r *runner) besideGoal(m cell, dy int64) func(cell) bool {
	return func(c cell) bool {
		return c[1] == m[1]-dy && abs(c[0]-m[0])+abs(c[2]-m[2]) == 1
	}
}

// waitOpen waits for every cell of a door to be air in the stream.
func (r *runner) waitOpen(ctx context.Context, cells []world.PlacedAnchor, limit time.Duration) error {
	deadline := time.Now().Add(limit)
	for time.Now().Before(deadline) {
		open := true
		for _, a := range cells {
			if b, ok := r.c.blockAt(at(a)); !ok || world.Solid(b) {
				open = false
			}
		}
		if open {
			return nil
		}
		if err := r.tickWait(ctx); err != nil {
			return err
		}
	}
	return errors.New("the door did not open")
}

// bossFight fights one boss to its death under /immortal, timed.
func (r *runner) bossFight(ctx context.Context, kind vnet.MobKind, anchor world.PlacedAnchor, label string) error {
	if err := r.walkTo(ctx, label, near(at(anchor), 9), true); err != nil {
		return err
	}
	if err := r.setImmortal(ctx, true); err != nil {
		return err
	}
	began := time.Now()
	seen := false
	for !seen || r.stats.killedCount(kind) == 0 {
		if err := r.fight(ctx, func(m mobView) bool { return m.kind == kind }); err != nil {
			return err
		}
		for _, m := range r.c.mobList() {
			if m.kind == kind {
				seen = true
			}
		}
		if r.stats.killedCount(kind) == 0 {
			if err := r.pause(ctx, 200*time.Millisecond); err != nil {
				return err
			}
			if time.Since(began) > 20*time.Minute {
				return fmt.Errorf("the %s is still standing after %v", label, time.Since(began))
			}
		}
	}
	r.timing[label+" fight"] = time.Since(began)
	return r.setImmortal(ctx, false)
}

func (r *runner) guardianFight(ctx context.Context) error {
	return r.bossFight(ctx, vnet.MobKindVargrGuardian, r.lay.guardian, "guardian")
}

// drop walks off the edge of the opened trapdoor, falls the chasm into the pool and swims
// to the shore.
func (r *runner) drop(ctx context.Context) error {
	gate := at(r.lay.gate)
	shore := at(r.lay.checkpoints[0])
	if r.c.self().pos[1] > float64(shore[1])+2 {
		if err := r.waitOpen(ctx, []world.PlacedAnchor{r.lay.gate}, 10*time.Second); err != nil {
			return fmt.Errorf("the trapdoor: %w", err)
		}
		edge := func(c cell) bool {
			d := math.Hypot(float64(c[0]-gate[0]), float64(c[2]-gate[2]))
			return c[1] == gate[1]+1 && d >= 2.5 && d <= 4
		}
		if err := r.walkTo(ctx, "the trapdoor's edge", edge, false); err != nil {
			return err
		}
		fallFrom := r.c.self().pos[1]
		for r.c.self().pos[1] > fallFrom-3 {
			self := r.c.self()
			r.c.setIntent(intent{moveZ: 1, yaw: yawToward(float64(gate[0])+.5-self.pos[0], float64(gate[2])+.5-self.pos[2])})
			if err := r.tickWait(ctx); err != nil {
				return err
			}
		}
		r.c.stand()
		// The fall: wait for the water to take the body.
		if err := r.pause(ctx, 3*time.Second); err != nil {
			return err
		}
	}
	// The swim: head for the shore holding jump, which is "rise" in water.
	deadline := time.Now().Add(60 * time.Second)
	for time.Now().Before(deadline) {
		self := r.c.self()
		here := feetCell(self.pos)
		standing := false
		r.c.withView(func(v *blockView) { standing = v.standable(here) && abs(here[1]-shore[1]) <= 1 })
		if standing {
			break
		}
		r.c.setIntent(intent{moveZ: 1, jump: true, yaw: yawToward(float64(shore[0])+.5-self.pos[0], float64(shore[2])+.5-self.pos[2])})
		if err := r.tickWait(ctx); err != nil {
			return err
		}
	}
	return r.walkTo(ctx, "the shore checkpoint", exactly(shore), false)
}

// expectedSpiders is how many spiders the twelve waves bring for a party of members: each
// wave is a pack of one spider per member the dungeon is sized for, three to five
// (internal/game's spiderWaveCount and dungeonPackSize).
func expectedSpiders(members int) int {
	return 12 * min(max(members, 3), 5)
}

// caveWaves walks into the cavern — which starts the waves — and holds it until every
// wave has come out and died.
func (r *runner) caveWaves(ctx context.Context) error {
	centre := cell{(r.lay.caveLow[0] + r.lay.caveHigh[0]) / 2, r.lay.caveLow[1], (r.lay.caveLow[2] + r.lay.caveHigh[2]) / 2}
	base := r.stats.killedCount(vnet.MobKindCaveSpider)
	if err := r.walkTo(ctx, "the cavern", near(centre, 3), true); err != nil {
		return err
	}
	want := expectedSpiders(1)
	quietSince := time.Now()
	sighted := map[uint64]bool{}
	for {
		for _, m := range r.c.mobList() {
			if m.kind == vnet.MobKindCaveSpider && !sighted[m.id] {
				sighted[m.id] = true
				r.say("spider %d out at %.1f, %.1f, %.1f", len(sighted), m.pos[0], m.pos[1], m.pos[2])
			}
		}
		before := r.stats.killedCount(vnet.MobKindCaveSpider)
		if err := r.fight(ctx, func(m mobView) bool { return m.kind == vnet.MobKindCaveSpider }); err != nil {
			return err
		}
		killed := r.stats.killedCount(vnet.MobKindCaveSpider) - base
		if killed > before-base {
			quietSince = time.Now()
		}
		alive := false
		for _, m := range r.c.mobList() {
			if m.kind == vnet.MobKindCaveSpider && !m.dying() && !r.unreachable[m.id] {
				alive = true
			}
		}
		if alive {
			quietSince = time.Now()
			continue
		}
		if killed >= want {
			r.say("the waves are over: %d spiders killed", killed)
			return nil
		}
		// The interval is the longest a wave waits; nothing for longer than that and a
		// margin means the schedule has nothing left, whatever the count says.
		if killed > 0 && time.Since(quietSince) > 2*time.Minute {
			r.say("no spider for two minutes after %d of %d; treating the waves as over", killed, want)
			return nil
		}
		// Hold the middle of the cavern between waves.
		if err := r.walkTo(ctx, "the cavern", near(centre, 3), true); err != nil {
			return err
		}
		if err := r.pause(ctx, 250*time.Millisecond); err != nil {
			return err
		}
	}
}

// webCurtain cuts every web of the curtain across the neck, one hit each.
func (r *runner) webCurtain(ctx context.Context) error {
	floor := r.lay.checkpoints[0].Y
	curtain := r.curtainCells(floor)
	if len(curtain) == 0 {
		return nil // cut by an earlier attempt
	}
	var mid [3]float64
	for _, c := range curtain {
		for i := range 3 {
			mid[i] += float64(c[i]) / float64(len(curtain))
		}
	}
	centre := cell{int64(math.Floor(mid[0])), floor, int64(math.Floor(mid[2]))}
	cavern := cell{(r.lay.caveLow[0] + r.lay.caveHigh[0]) / 2, floor, (r.lay.caveLow[2] + r.lay.caveHigh[2]) / 2}
	webs := map[cell]bool{}
	for _, c := range curtain {
		webs[c] = true
	}
	// Stand on the cavern's side of the curtain, a step off it. The goal runs under the
	// view's lock, so it reads the curtain it was given rather than the view.
	stand := func(c cell) bool {
		if c[1] != floor || webs[c] {
			return false
		}
		d := math.Hypot(float64(c[0]-centre[0]), float64(c[2]-centre[2]))
		toCavern := math.Hypot(float64(c[0]-cavern[0]), float64(c[2]-cavern[2])) <
			math.Hypot(float64(centre[0]-cavern[0]), float64(centre[2]-cavern[2]))
		return d >= 1 && d <= 1.5 && toCavern
	}
	if err := r.walkTo(ctx, "the web curtain", stand, true); err != nil {
		return err
	}
	for _, web := range curtain {
		if err := r.cut(ctx, web); err != nil {
			return err
		}
	}
	r.websCut += len(curtain)
	return nil
}

// curtainCells is the curtain: the connected webs that reach down to the cave's floor
// level. Every other web in the cave hangs under a ceiling.
func (r *runner) curtainCells(floor int64) []cell {
	var seeds []cell
	r.c.withView(func(v *blockView) {
		for coord, blocks := range v.chunks {
			ox, oy, oz := coord.Origin()
			if floor < oy || floor >= oy+world.ChunkSize {
				continue
			}
			for z := range int64(world.ChunkSize) {
				for x := range int64(world.ChunkSize) {
					if blocks[world.Index(int(x), int(floor-oy), int(z))] == world.Cobweb {
						seeds = append(seeds, cell{ox + x, floor, oz + z})
					}
				}
			}
		}
	})
	seen := map[cell]bool{}
	var out []cell
	for len(seeds) > 0 {
		c := seeds[0]
		seeds = seeds[1:]
		if seen[c] {
			continue
		}
		seen[c] = true
		if b, _ := r.c.blockAt(c); b != world.Cobweb {
			continue
		}
		out = append(out, c)
		for _, d := range [...]cell{{1, 0, 0}, {-1, 0, 0}, {0, 1, 0}, {0, -1, 0}, {0, 0, 1}, {0, 0, -1}} {
			seeds = append(seeds, cell{c[0] + d[0], c[1] + d[1], c[2] + d[2]})
		}
	}
	// Bottom row first, nearest the middle first: the order a hand clears a doorway in.
	sort.Slice(out, func(i, j int) bool {
		if out[i][1] != out[j][1] {
			return out[i][1] < out[j][1]
		}
		return out[i][0]+out[i][2] < out[j][0]+out[j][2]
	})
	return out
}

// timedGrille pulls the cavern's lever and makes it through the grille before it shuts.
func (r *runner) timedGrille(ctx context.Context) error {
	lever := at(r.lay.grilleLever)
	past := at(r.lay.checkpoints[1])
	for attempt := 1; attempt <= 3; attempt++ {
		if err := r.walkTo(ctx, "the grille lever", r.besideGoal(lever, 1), true); err != nil {
			return err
		}
		if b, _ := r.c.blockAt(lever); b != world.LeverOn {
			if err := r.use(ctx, lever); err != nil {
				return err
			}
		}
		pulled := time.Now()
		err := r.walkTo(ctx, "past the grille", exactly(past), false)
		if err == nil {
			r.timing["grille lever to checkpoint"] = time.Since(pulled)
			return nil
		}
		if errors.Is(err, errDied) {
			return err
		}
		r.say("attempt %d at the grille failed: %v", attempt, err)
	}
	return errors.New("never made it through the grille")
}

// sandHall walks down into the sand hall and past every buried slot, so each scorpion
// rises, and puts each one down.
func (r *runner) sandHall(ctx context.Context) error {
	if err := r.walkTo(ctx, "the sand hall", near(r.lay.sandCentre, 4), true); err != nil {
		return err
	}
	for i, slot := range r.lay.minors[world.SandBuriedGroup] {
		if err := r.walkTo(ctx, fmt.Sprintf("buried slot %d", i), near(at(slot), 3), true); err != nil {
			return err
		}
		if err := r.pause(ctx, 1200*time.Millisecond); err != nil {
			return err
		}
		if err := r.fight(ctx, func(m mobView) bool { return m.kind == vnet.MobKindScorpion }); err != nil {
			return err
		}
	}
	return nil
}

// twinLevers pulls one lever and runs to the other inside its ten seconds.
func (r *runner) twinLevers(ctx context.Context) error {
	if len(r.lay.twins) != 2 {
		return fmt.Errorf("%d twin levers in the layout", len(r.lay.twins))
	}
	a, b := at(r.lay.twins[0]), at(r.lay.twins[1])
	for attempt := 1; attempt <= 3; attempt++ {
		if err := r.walkTo(ctx, "the first twin lever", r.besideGoal(a, 1), true); err != nil {
			return err
		}
		if block, _ := r.c.blockAt(a); block != world.LeverOn {
			if err := r.use(ctx, a); err != nil {
				return err
			}
		}
		pulled := time.Now()
		if err := r.walkTo(ctx, "the second twin lever", r.besideGoal(b, 1), false); err != nil {
			return err
		}
		if err := r.use(ctx, b); err != nil {
			r.say("attempt %d at the twin levers: %v", attempt, err)
			continue
		}
		r.timing["twin lever run"] = time.Since(pulled)
		if err := r.waitOpen(ctx, r.lay.twinDoor, 3*time.Second); err == nil {
			return nil
		}
	}
	return errors.New("the twin levers never opened the door")
}

func (r *runner) kingFight(ctx context.Context) error {
	if err := r.walkTo(ctx, "past the sand hall's door", exactly(at(r.lay.checkpoints[2])), true); err != nil {
		return err
	}
	return r.bossFight(ctx, vnet.MobKindDraugrKing, r.lay.king, "king")
}

// returnShortcut climbs the passage the king's death opened back to the arrival court and
// walks out through the return portal.
func (r *runner) returnShortcut(ctx context.Context) error {
	if err := r.waitOpen(ctx, r.lay.shortcut, 10*time.Second); err != nil {
		return fmt.Errorf("the return shortcut: %w", err)
	}
	arrival := at(r.lay.arrival)
	if err := r.walkTo(ctx, "the arrival court", exactly(arrival), true); err != nil {
		return err
	}
	exit := world.InstanceExitThreshold(r.lay.seed)
	axis := exit.Normal
	side := float64(arrival[axis] - exit.Heart[axis])
	drain(r.c.changes)
	deadline := time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) {
		self := r.c.self()
		target := [3]float64{float64(exit.Heart[0]) + .5, 0, float64(exit.Heart[2]) + .5}
		target[axis] -= math.Copysign(3, side)
		r.c.setIntent(intent{moveZ: 1, yaw: yawToward(target[0]-self.pos[0], target[2]-self.pos[2])})
		select {
		case change := <-r.c.changes:
			r.c.stand()
			if change.id == 0 {
				r.say("back in the open world")
				return nil
			}
		case <-time.After(50 * time.Millisecond):
		case <-ctx.Done():
			return ctx.Err()
		}
	}
	r.c.stand()
	return errors.New("the return portal never took the bot")
}
