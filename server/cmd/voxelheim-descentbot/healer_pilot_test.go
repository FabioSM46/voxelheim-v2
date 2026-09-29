package main

import (
	"context"
	"fmt"
	"math"
	"net"
	"strings"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/transport"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func TestHealerFlagAndLastMemberAssignment(t *testing.T) {
	for _, args := range [][]string{{"-healers", "-1"}, {"-healers", "3"}, {"-party", "1", "-healers", "1"}, {"-healers", "1", "-hoard"}, {"-healers", "many"}} {
		if _, err := parseFlags("bot", append([]string{"-server", "fixture"}, args...)); err == nil {
			t.Fatalf("accepted %v", args)
		}
	}
	for count := 1; count <= len(memberNames); count++ {
		for healers := 0; healers < count; healers++ {
			o, err := parseFlags("bot", []string{"-server", "fixture", "-party", fmt.Sprint(count), "-healers", fmt.Sprint(healers)})
			if err != nil {
				t.Fatal(err)
			}
			for i := range count {
				want := i >= count-healers
				if healerMember(o, i) != want {
					t.Fatalf("party %d healers %d member %d", count, healers, i)
				}
			}
		}
	}
	o, err := parseFlags("bot", []string{"-server", "fixture"})
	if err != nil || o.healers != 0 {
		t.Fatalf("default %+v %v", o, err)
	}
	p := &pilot{c: &client{}}
	if p.mainHandItem() != game.ItemIronSword {
		t.Fatal("zero healer weapon changed")
	}
	p.c.healing = &healerState{}
	if p.mainHandItem() != game.ItemWoodenSceptre {
		t.Fatal("healer did not get sceptre")
	}
}

func sceptreInventory(durability uint16) protocol.InventoryState {
	state := protocol.InventoryState{Stacks: make([]protocol.InventoryStack, protocol.InventorySlots)}
	state.Stacks[mainHandSlot] = protocol.InventoryStack{ItemID: uint16(game.ItemWoodenSceptre), Count: 1, Durability: durability, MaxDurability: game.SceptreMaxDurability}
	return state
}

func healerPilotFixture() *pilot {
	stats := newRunStats(newTally())
	c := &client{entityID: 1, stats: stats, view: flatView(), alive: true, havePos: true, pos: [3]float64{2, 1, 3}, energy: 100,
		healing:   &healerState{members: []allyView{ally(2, 20, 10)}, seenAt: time.Now()},
		inventory: sceptreInventory(50), inventoryRevision: 1, chat: make(chan string, 4), mobs: map[uint64]mobView{
			9: {id: 9, kind: vnet.MobKindDraugr, pos: [3]float64{8, 1, 8}, health: 60, maxHealth: 100},
		}}
	return &pilot{c: c, stats: stats, rate: 100, say: func(string, ...any) {}, unreachable: map[uint64]bool{}}
}

func readBotMessage(t *testing.T, conn net.Conn) protocol.Message {
	t.Helper()
	frame, err := transport.ReadFrame(conn)
	if err != nil {
		t.Fatal(err)
	}
	msg, err := protocol.Decode(frame)
	if err != nil {
		t.Fatal(err)
	}
	return msg
}

func TestHealerFightLaunchesAtWoundedAllyThenFallsBackToPartyFocus(t *testing.T) {
	for _, healing := range []bool{true, false} {
		t.Run(fmt.Sprint(healing), func(t *testing.T) {
			p := healerPilotFixture()
			if !healing {
				p.c.healing.members[0].health = 100
			}
			local, remote := net.Pipe()
			defer func() { _ = local.Close(); _ = remote.Close() }()
			if err := remote.SetDeadline(time.Now().Add(2 * time.Second)); err != nil {
				t.Fatal(err)
			}
			p.c.conn = local
			ctx, cancel := context.WithTimeout(context.Background(), time.Second)
			defer cancel()
			done := make(chan error, 1)
			go func() { done <- p.fight(ctx, func(mobView) bool { return true }) }()
			aim := readBotMessage(t, remote)
			attack := readBotMessage(t, remote)
			if aim.PlayerInput == nil || attack.Attack == nil || attack.Attack.Slot != mainHandSlot || attack.Attack.ClientTick <= aim.PlayerInput.ClientTick {
				t.Fatal("aim did not precede main hand attack")
			}
			target := creatureOrbTarget(p.c.mobs[9])
			if healing {
				target, _ = woundedAlly(p.c.entityID, orbOrigin(p.c.pos), p.c.healing.members)
			}
			want, _ := p.healerFacing(p.c.self(), target, nil)
			if math.Abs(float64(aim.PlayerInput.Yaw)-want.yaw) > 1e-5 || math.Abs(float64(aim.PlayerInput.Pitch)-want.pitch) > 1e-5 {
				t.Fatal("orb aimed from the wrong height or at the wrong target")
			}
			cancel()
			<-done
			totals := p.c.healingTotals()
			if healing && (totals.allies != 1 || totals.creatures != 0) || !healing && (totals.allies != 0 || totals.creatures != 1) {
				t.Fatalf("counts %+v", totals)
			}
		})
	}
}

func TestHealerFocusAgreesWithBladesAndExcludesDeadCreatures(t *testing.T) {
	p := healerPilotFixture()
	mobs := []mobView{
		{id: 7, pos: [3]float64{4, 1, 3}, health: 80},
		{id: 8, pos: [3]float64{9, 1, 3}, health: 20},
		{id: 6, pos: [3]float64{8, 1, 3}, health: 20},
		{id: 5, pos: [3]float64{7, 1, 3}, health: 1, action: vnet.MobActionDying},
	}
	target, ok := p.orbFocus(p.c.self(), func(mobView) bool { return true }, mobs)
	if !ok || target.id != 6 {
		t.Fatalf("focus %+v", target)
	}
	if _, ok := p.orbFocus(p.c.self(), func(mobView) bool { return false }, mobs); ok {
		t.Fatal("ignored encounter filter")
	}
}

func TestHealerEscapesSameTimelineRegionsWhileFacingAlly(t *testing.T) {
	p := healerPilotFixture()
	danger := []region{{shape: vnet.HazardShapeDisc, origin: [3]float64{2, 1, 3}, radius: 1, height: 4}}
	p.c.believed, p.c.believedAt = danger, time.Now()
	target, _ := woundedAlly(1, orbOrigin(p.c.pos), p.c.healerMembers(time.Now()))
	face, escaping := p.healerFacing(p.c.self(), target, p.c.danger())
	if !escaping || math.Hypot(face.moveX, face.moveZ) < 0.9 {
		t.Fatal("healer stood inside announced region")
	}
	// Translate the chosen controls back into world movement and test its endpoint.
	forward := [2]float64{-math.Sin(face.yaw), -math.Cos(face.yaw)}
	right := [2]float64{math.Cos(face.yaw), -math.Sin(face.yaw)}
	end := p.c.pos
	end[0] += (face.moveX*right[0] + face.moveZ*forward[0]) * game.WalkSpeed * escapeHorizon.Seconds()
	end[2] += (face.moveX*right[1] + face.moveZ*forward[1]) * game.WalkSpeed * escapeHorizon.Seconds()
	if touches(danger, end) {
		t.Fatal("escape control led into hazard")
	}
}

func TestHealerFindsAFiringLineAroundAnInterceptingCreature(t *testing.T) {
	p := healerPilotFixture()
	target, _ := woundedAlly(1, orbOrigin(p.c.pos), p.c.healing.members)
	mobs := []mobView{{id: 3, pos: [3]float64{6, 1, 3}, health: 100, kind: vnet.MobKindDraugr}}
	route := healerRoute(p.c.view, feetCell(p.c.pos), target, p.c.healing.members, mobs, 1, nil)
	if len(route) == 0 {
		t.Fatal("no route around interceptor")
	}
	end := route[len(route)-1]
	pos := [3]float64{float64(end[0]) + .5, float64(end[1]), float64(end[2]) + .5}
	if !orbLineClear(p.c.view, orbOrigin(pos), target, p.c.healing.members, mobs, 1) {
		t.Fatal("route ended on blocked line")
	}
}

func TestHealerDoesNotSendStaleDeadSelfBrokenOrCooldownAttacks(t *testing.T) {
	for _, change := range []func(*pilot, *orbTarget){
		func(p *pilot, _ *orbTarget) { p.c.energy = 0 },
		func(p *pilot, _ *orbTarget) { p.c.alive = false },
		func(p *pilot, _ *orbTarget) { p.c.healing.members[0].alive = false },
		func(p *pilot, _ *orbTarget) { p.c.healing.members[0].health = 100 },
		func(p *pilot, t *orbTarget) { t.id = p.c.entityID },
		func(p *pilot, _ *orbTarget) { p.c.healing.seenAt = time.Now().Add(-time.Second) },
		func(p *pilot, _ *orbTarget) { p.c.inventory = sceptreInventory(0) },
		func(p *pilot, _ *orbTarget) { p.lastSwing = time.Now() },
	} {
		p := healerPilotFixture() // nil conn: any attempted write would panic
		target, _ := woundedAlly(1, orbOrigin(p.c.pos), p.c.healing.members)
		change(p, &target)
		if err := p.launchOrb(target, intent{}, time.Now()); err != nil {
			t.Fatal(err)
		}
		if p.c.healingTotals() != (healerTotals{}) {
			t.Fatal("non-launch counted")
		}
	}
}

func TestHealerReportShowsInferenceAssistanceAndNoBladeTimeEstimate(t *testing.T) {
	for _, count := range []int{0, 1} {
		var members []*runner
		for i := range 3 {
			p := healerPilotFixture()
			p.c.name = memberNames[i]
			if i < 3-count {
				p.c.healing = nil
			} else {
				p.c.healing.totals = healerTotals{allies: 7, creatures: 3, restored: 20, replacements: 2}
			}
			members = append(members, &runner{pilot: p, opts: options{level: 21, members: 3, healers: count}, server: &serverProcess{}, timing: map[string]time.Duration{}})
		}
		var report strings.Builder
		writeReport(&report, newParty(members), nil)
		text := report.String()
		if count == 0 {
			if strings.Contains(text, "healers:") || strings.Contains(text, "inferred") || !strings.Contains(text, "estimated human clear for a party of 3:") {
				t.Fatal("zero-healer report changed")
			}
		} else {
			for _, want := range []string{"Orm: orb launch requests sent to allies 7, creatures 3", "restored near own ally requests 20 (inferred)", "2 worn sceptres replaced", "regeneration and other healers can overlap", "unavailable for healer parties"} {
				if !strings.Contains(text, want) {
					t.Fatalf("missing %q", want)
				}
			}
			if strings.Contains(text, "estimated human clear for a party of 3:") {
				t.Fatal("blade baseline applied to healer run")
			}
		}
	}
}

func TestHealerRangeLeavesMarginBeforeServerOrbExpiry(t *testing.T) {
	if healerRange >= game.OrbSpeed*game.OrbLifetime.Seconds() {
		t.Fatal("bot range reaches the expiry boundary")
	}
}

// signalWriteConn announces each actual socket Write before blocking on the
// underlying pipe. transport.WriteFrame writes a length and then a payload.
type signalWriteConn struct {
	net.Conn
	writes chan struct{}
}

func (c *signalWriteConn) Write(data []byte) (int, error) {
	c.writes <- struct{}{}
	return c.Conn.Write(data)
}

func TestLaunchWriteFailureDoesNotCommitRequestsOrEarlyHealing(t *testing.T) {
	for _, failAttack := range []bool{false, true} {
		t.Run(fmt.Sprint(failAttack), func(t *testing.T) {
			p := healerPilotFixture()
			local, remote := net.Pipe()
			defer func() { _ = local.Close(); _ = remote.Close() }()
			if err := remote.SetDeadline(time.Now().Add(2 * time.Second)); err != nil {
				t.Fatal(err)
			}
			writes := make(chan struct{}, 8)
			p.c.conn = &signalWriteConn{Conn: local, writes: writes}
			target, _ := woundedAlly(1, orbOrigin(p.c.pos), p.c.healing.members)
			done := make(chan error, 1)
			go func() { done <- p.launchOrb(target, intent{}, time.Now()) }()
			<-writes // first frame's prefix is now blocked on the pipe
			readDone := make(chan struct{})
			go func() { p.c.inventoryAnswer(); close(readDone) }()
			select {
			case <-readDone:
			case <-time.After(time.Second):
				t.Fatal("reader mutex held over aim write")
			}
			if failAttack {
				if readBotMessage(t, remote).PlayerInput == nil {
					t.Fatal("first frame is not aim")
				}
				<-writes // first frame payload
				<-writes // attack prefix: provisional observation exists, write is blocked
				p.c.mu.Lock()
				if len(p.c.healing.pending) != 1 {
					t.Fatal("attack has no provisional observation")
				}
				shot := p.c.healing.pending[0]
				shot.earliest = time.Now().Add(-time.Millisecond)
				p.c.healing.observe([]allyView{ally(2, 30, 10)}, time.Now())
				if p.c.healing.totals != (healerTotals{}) || shot.restored != 10 {
					t.Fatal("unwritten attack committed evidence")
				}
				p.c.mu.Unlock()
			}
			_ = remote.Close()
			if err := <-done; err == nil {
				t.Fatal("failed write returned success")
			}
			if p.c.healingTotals() != (healerTotals{}) {
				t.Fatal("failed write counted request or healing")
			}
			p.c.mu.Lock()
			pending := len(p.c.healing.pending)
			p.c.mu.Unlock()
			if pending != 0 || !p.lastSwing.IsZero() {
				t.Fatal("failed launch retained attempt state")
			}
		})
	}
}

func TestUnreachableHealerCreatureIsRecordedAndDroppedButNotABoss(t *testing.T) {
	p := healerPilotFixture()
	now := time.Now()
	m := p.c.mobs[9]
	engaged := map[uint64]time.Time{}
	if p.healerUnreachable(m.id, false, []mobView{m}, engaged, now) {
		t.Fatal("gave up immediately")
	}
	if !p.healerUnreachable(m.id, false, []mobView{m}, engaged, now.Add(time.Minute+time.Nanosecond)) {
		t.Fatal("unreachable creature retained forever")
	}
	if _, ok := p.orbFocus(p.c.self(), func(mobView) bool { return true }, []mobView{m}); ok {
		t.Fatal("unreachable creature reselected")
	}
	p.healerUnreachable(m.id, false, []mobView{m}, engaged, now.Add(2*time.Minute))
	if len(p.stats.unreachedMobs) != 1 {
		t.Fatal("unreachable report duplicated")
	}
	for _, kind := range []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing} {
		m.kind = kind
		if p.healerUnreachable(m.id, false, []mobView{m}, engaged, now.Add(2*time.Minute)) {
			t.Fatal("mandatory boss abandoned")
		}
	}
	p = healerPilotFixture()
	m = p.c.mobs[9]
	engaged = map[uint64]time.Time{m.id: now.Add(-2 * time.Minute)}
	if p.healerUnreachable(m.id, true, []mobView{m}, engaged, now) || engaged[m.id] != now {
		t.Fatal("firing line did not reset timeout")
	}
	p.stats.hitAt[m.id] = now
	engaged[m.id] = now.Add(-2 * time.Minute)
	if p.healerUnreachable(m.id, false, []mobView{m}, engaged, now) {
		t.Fatal("recent party hit ignored")
	}
}

func TestHealerRecoveryWindowIsBoundedAndNewCombatResetsIt(t *testing.T) {
	now := time.Unix(100, 0)
	var recovery healerRecovery
	if recovery.finished(false, now) {
		t.Fatal("recovery ended before any healing opportunity")
	}
	if recovery.finished(false, now.Add(healerAfterFightLimit-time.Nanosecond)) {
		t.Fatal("recovery grace cut short")
	}
	if !recovery.finished(false, now.Add(healerAfterFightLimit)) {
		t.Fatal("cleared encounter never ended")
	}
	if recovery.finished(true, now.Add(healerAfterFightLimit+time.Second)) || !recovery.quietSince.IsZero() {
		t.Fatal("new combat did not reset recovery")
	}
	if recovery.finished(false, now.Add(2*healerAfterFightLimit)) {
		t.Fatal("old quiet period ended new combat prematurely")
	}
}

func TestClearedRoomReturnsWithFreshWoundedAllyBehindWall(t *testing.T) {
	p := healerPilotFixture()
	p.c.mobs = nil
	// The only delivered chunk is cut in two by a solid wall; the ally is in
	// range on the far side, but neither an orb nor a walk can reach that side.
	for z := int64(0); z < world.ChunkSize; z++ {
		for y := int64(1); y < world.ChunkSize; y++ {
			p.c.view.set(6, y, z, world.Stone)
		}
	}
	target, _ := woundedAlly(1, orbOrigin(p.c.pos), p.c.healing.members)
	if orbLineClear(p.c.view, orbOrigin(p.c.pos), target, p.c.healing.members, nil, 1) {
		t.Fatal("fixture has a firing line")
	}
	ctx, cancel := context.WithTimeout(context.Background(), healerAfterFightLimit+2*time.Second)
	defer cancel()
	refreshed := make(chan struct{})
	go func() {
		defer close(refreshed)
		ticker := time.NewTicker(time.Second / tickRate)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				p.c.mu.Lock()
				p.c.healing.observe([]allyView{ally(2, 20, 10)}, time.Now())
				p.c.mu.Unlock()
			}
		}
	}()
	started := time.Now()
	err := p.fight(ctx, func(mobView) bool { return true })
	cancel()
	<-refreshed
	if err != nil {
		t.Fatalf("cleared encounter waited for global timeout: %v", err)
	}
	if time.Since(started) < healerAfterFightLimit {
		t.Fatal("test exited via stale snapshot instead of bounded recovery")
	}
	if p.c.healingTotals() != (healerTotals{}) {
		t.Fatal("unreachable ally received a request")
	}
	p.c.mu.Lock()
	controls := p.c.control
	p.c.mu.Unlock()
	if controls.moveX != 0 || controls.moveZ != 0 {
		t.Fatal("healer kept moving after completing the fight")
	}
}
