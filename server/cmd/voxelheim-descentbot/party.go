package main

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// The party (#1333). The first dungeon is sized for three to five (#1332), so the run is
// played by that many bots, each its own session and character, formed into one party with
// the same PartyRequests the client sends and walked through the same portal, so the
// server puts them all in one instance.
//
// Every member plays the route. The party moves part by part: nobody starts a part until
// every member has finished the one before it, and a member waiting on the others still
// answers any creature that turns on it. The puzzles are the leader's alone — the rune
// stones, the web curtain and the twin levers — and the others wait for what the leader
// opens, as a party lets one hand work a mechanism. The timed grille is everybody's: each
// member has to be through it before it shuts, and any member finding the lever up pulls
// it again. Deaths are a member's own: it respawns where the server puts it — the furthest
// checkpoint the party has reached — and walks back into the part it died in.

// memberNames are the party's characters, the leader first.
var memberNames = [...]string{"Delver", "Hild", "Orm", "Sigrun", "Ulf"}

// party is the members and what they share: the gate between parts of the route and the
// boss fights' clocks, which start at the first member's pull and stop at the kill.
type party struct {
	members []*runner
	gate    *barrier

	mu     sync.Mutex
	fights map[string][2]time.Time
	// total is portal to portal: from the party setting off for the portal to the last
	// member back in the open world.
	total time.Duration
	// wipes counts the moments every member was dead at once, by the part of the route the
	// leader was in; barren is how many of the latest came with no kill since the one
	// before, and killsAtWipe the party's kills at the last.
	wipes       map[string]int
	barren      int
	killsAtWipe int
}

func newParty(members []*runner) *party {
	pt := &party{members: members, gate: newBarrier(len(members)), fights: map[string][2]time.Time{}, wipes: map[string]int{}}
	allies := map[uint64]bool{}
	for _, m := range members {
		allies[m.c.entityID] = true
	}
	for i, m := range members {
		m.party, m.leader, m.allies = pt, i == 0, allies
	}
	return pt
}

func (pt *party) leaderRunner() *runner { return pt.members[0] }

// fightBegan starts a boss fight's clock at the first member to pull, once.
func (pt *party) fightBegan(label string) {
	pt.mu.Lock()
	defer pt.mu.Unlock()
	if f := pt.fights[label]; f[0].IsZero() {
		pt.fights[label] = [2]time.Time{time.Now(), {}}
	}
}

// fightEnded stops a boss fight's clock at the kill, once.
func (pt *party) fightEnded(label string) {
	pt.mu.Lock()
	defer pt.mu.Unlock()
	if f := pt.fights[label]; !f[0].IsZero() && f[1].IsZero() {
		f[1] = time.Now()
		pt.fights[label] = f
	}
}

// fightTime is a finished boss fight's length.
func (pt *party) fightTime(label string) (time.Duration, bool) {
	pt.mu.Lock()
	defer pt.mu.Unlock()
	f := pt.fights[label]
	if f[0].IsZero() || f[1].IsZero() {
		return 0, false
	}
	return f[1].Sub(f[0]), true
}

// watchWipes counts every wipe — the moment the last living member falls — until ctx ends.
func (pt *party) watchWipes(ctx context.Context) {
	ticker := time.NewTicker(time.Second / tickRate)
	defer ticker.Stop()
	down := false
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
		everyone := true
		for _, m := range pt.members {
			if self := m.c.self(); self.alive || !self.have {
				everyone = false
			}
		}
		if everyone && !down {
			lead := pt.leaderRunner()
			phase, kills := lead.stats.currentPhase(), lead.stats.killTotal()
			pt.mu.Lock()
			pt.wipes[phase]++
			if kills == pt.killsAtWipe {
				pt.barren++
			} else {
				pt.barren = 0
			}
			pt.killsAtWipe = kills
			pt.mu.Unlock()
			pt.leaderRunner().say("the party wiped in %s", phase)
		}
		down = everyone
	}
}

func (pt *party) wipesIn(phase string) int {
	pt.mu.Lock()
	defer pt.mu.Unlock()
	return pt.wipes[phase]
}

func (pt *party) barrenWipes() int {
	pt.mu.Lock()
	defer pt.mu.Unlock()
	return pt.barren
}

func (pt *party) wipeCount() int {
	pt.mu.Lock()
	defer pt.mu.Unlock()
	total := 0
	for _, n := range pt.wipes {
		total += n
	}
	return total
}

// barrier lets the members through together: each arrival is counted, and the channel an
// arrival is handed closes once every member has arrived.
type barrier struct {
	mu      sync.Mutex
	n       int
	waiting int
	release chan struct{}
}

func newBarrier(n int) *barrier { return &barrier{n: n, release: make(chan struct{})} }

func (b *barrier) arrive() <-chan struct{} {
	b.mu.Lock()
	defer b.mu.Unlock()
	released := b.release
	b.waiting++
	if b.waiting == b.n {
		b.waiting = 0
		b.release = make(chan struct{})
		close(released)
	}
	return released
}

// regroup waits for every member to reach the same point of the route, fighting whatever
// turns on this one while it waits, and coming back from a death if one finds it here.
func (r *runner) regroup(ctx context.Context) error {
	released := r.party.gate.arrive()
	for {
		select {
		case <-released:
			return nil
		case <-ctx.Done():
			return ctx.Err()
		default:
		}
		err := r.tickWait(ctx)
		if err == nil {
			if _, ok := r.threat(); ok {
				err = r.fight(ctx, r.hostile)
			}
		}
		if errors.Is(err, errDied) {
			if err := r.waitAlive(ctx); err != nil {
				return err
			}
			continue
		}
		if err != nil {
			return err
		}
	}
}

// form makes the party: the leader invites each other member by name and each accepts
// the invitation the server delivers, and then every member's roster must name them all.
func (pt *party) form(ctx context.Context) error {
	lead := pt.leaderRunner()
	for _, m := range pt.members[1:] {
		drain(m.c.invites)
		if err := lead.c.send(protocol.EncodePartyRequest(protocol.PartyRequest{
			Action: vnet.PartyActionInvite, TargetName: m.c.name,
		})); err != nil {
			return err
		}
		select {
		case <-m.c.invites:
		case <-time.After(5 * time.Second):
			return fmt.Errorf("%s never received the invitation", m.c.name)
		case <-ctx.Done():
			return ctx.Err()
		}
		if err := m.c.send(protocol.EncodePartyRequest(protocol.PartyRequest{Action: vnet.PartyActionAccept})); err != nil {
			return err
		}
	}
	deadline := time.Now().Add(5 * time.Second)
	for {
		formed := true
		for _, m := range pt.members {
			if m.c.rosterSize() != len(pt.members) {
				formed = false
			}
		}
		if formed {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("the leader's roster names %d of %d members", lead.c.rosterSize(), len(pt.members))
		}
		if err := sleep(ctx, 100*time.Millisecond); err != nil {
			return err
		}
	}
}

// play is the whole run: every member equipped, the party formed, and then every member
// playing the route at once. The first member to fail ends the run for all of them.
func (pt *party) play(ctx context.Context) error {
	for _, m := range pt.members {
		if err := m.equip(ctx); err != nil {
			return fmt.Errorf("equip %s: %w", m.c.name, err)
		}
	}
	if len(pt.members) > 1 {
		if err := pt.form(ctx); err != nil {
			return fmt.Errorf("form the party: %w", err)
		}
	}
	pt.leaderRunner().sampleRSS("open world, before the portal")
	run, stop := context.WithCancel(ctx)
	defer stop()
	go pt.watchWipes(run)
	start := time.Now()
	errs := make(chan error, len(pt.members))
	for _, m := range pt.members {
		go func() {
			err := m.play(run)
			if err != nil {
				stop()
			}
			errs <- err
		}()
	}
	var first error
	for range pt.members {
		// A member stopped by another's failure reports the cancellation; the failure that
		// caused it is the one worth reporting.
		if err := <-errs; err != nil && (first == nil || errors.Is(first, context.Canceled)) {
			first = err
		}
	}
	if first != nil {
		return first
	}
	pt.total = time.Since(start)
	return nil
}
