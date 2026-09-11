package game

import (
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Crossing a portal begins with the body, not with a key.
//
// **The tick notices; the session crosses.** A body that walks into a veil is observed
// here, under Sim.mu, at the positions the tick has just produced — but admission takes
// the instance manager's lock, which is ordered *before* this one, and a world change
// stops and restarts the session's own workers. Neither may happen on the tick
// goroutine. So a contact is a value handed across the same kind of seam mining uses: a
// one-slot, non-blocking handoff the session's owner loop selects on, which then asks
// [InstanceManager.EnterPortal] or [Player.AtPortal] exactly as a request once did. Every
// eligibility, routing, binding and consent rule is therefore the one that was already
// there; contact only decides *when* it is asked.
//
// **One continuous contact is one attempt.** A body inside a veil after its attempt —
// refused, offered and declined, or simply still standing there — produces nothing more
// until it has stopped touching that veil and touched it again. Nothing counts ticks or
// time: the episode is the contact itself, so a player who wants another try walks out
// and back in, which is also what they would do in front of a closed door.
//
// **Arriving inside a veil is not entering it.** A body the server *placed* in a
// threshold — joining at a stored position, respawning, returning from a dungeon to the
// spot it crossed at, or transferred into a world — starts its episode already spent.
// That is what makes a return trip unable to bounce straight back through the arch it
// came out of, and it is decided here rather than by every caller that moves a body:
// anything that relocates a player between ticks leaves a position the last tick did
// not produce, and that difference is the whole test.

// PortalContact is one contact episode's crossing attempt, produced by the tick.
//
// It names the world it was observed in, because it is delivered asynchronously: a
// session that has changed worlds since must be able to tell that the veil this
// contact touched is not in front of it any more.
type PortalContact struct {
	sim  *Sim
	arch [3]int32
}

// In reports whether this contact was observed in s.
func (c PortalContact) In(s *Sim) bool { return c.sim == s }

// Request is the crossing this contact asks for, in the vocabulary admission already
// decides: the touched arch's heart, which is the portal's anchor.
func (c PortalContact) Request() protocol.PortalRequest {
	return protocol.PortalRequest{HasArch: true, Arch: c.arch}
}

// PortalContacts is the channel the tick offers contacts on. It never changes for the
// life of the player, so a session may read it once.
func (p *Player) PortalContacts() <-chan PortalContact { return p.portalContacts }

// WithPortals names the thresholds a simulation's bodies can walk into.
//
// **Bounded by construction rather than by a search.** A simulation is told which
// portals stand in it — the open world has one ruin, an instance has one exit — and the
// tick measures each body against that list, skipping any sheet further away than a body
// could reach in one tick. Nothing here asks the world generator a question.
func WithPortals(thresholds ...world.PortalThreshold) SimOption {
	return func(options *simOptions) { options.portals = append(options.portals, thresholds...) }
}

// portalContactReach is how far a body's centre may be from a heart before any sheet
// arithmetic is done. The widest opening reaches three blocks from its heart and a body
// travels at most a few blocks in a tick, so this is a cheap refusal and never a miss.
const portalContactReach = 8

func (s portalSheet) near(b box) bool {
	var sum float64
	for axis := range 3 {
		d := (b.min[axis]+b.max[axis])/2 - (float64(s.heart[axis]) + .5)
		sum += d * d
	}
	return sum <= portalContactReach*portalContactReach
}

// portalTouchedLocked is the heart of the sheet b is standing in, if any.
func (s *Sim) portalTouchedLocked(b box) ([3]int64, bool) {
	for _, sheet := range s.portalSheets {
		if sheet.near(b) && sheet.touches(b) {
			return sheet.heart, true
		}
	}
	return [3]int64{}, false
}

// advancePortalContactLocked settles one tick's contact for a body that moved from `from`
// (standing at fromPos) to where it stands now.
//
// The caller holds Sim.mu and captured from after every relocation this tick could make
// and before the body's own movement, so the segment between the two is walking and
// nothing else.
func (p *Player) advancePortalContactLocked(from box, fromPos [3]float64) {
	s := p.sim
	if len(s.portalSheets) == 0 {
		return
	}
	if !p.portalObserved || fromPos != p.portalLastPos {
		// Placed rather than walked: whatever veil it stands in is an arrival.
		p.portalInside, p.portalInsideAny = s.portalTouchedLocked(from)
	}
	to := p.box()
	for _, sheet := range s.portalSheets {
		if p.portalInsideAny && p.portalInside == sheet.heart {
			continue
		}
		if !sheet.near(from) && !sheet.near(to) {
			continue
		}
		if !sheet.crossedBy(from, to) || !p.alive() || p.leaving {
			continue
		}
		contact := PortalContact{sim: s, arch: [3]int32{int32(sheet.heart[0]), int32(sheet.heart[1]), int32(sheet.heart[2])}}
		// One slot, never blocking the tick. A full slot is a crossing the session has
		// not handled yet, and a session has at most one crossing in flight.
		select {
		case p.portalContacts <- contact:
		default:
		}
	}
	p.portalInside, p.portalInsideAny = s.portalTouchedLocked(to)
	p.portalLastPos = p.pos
	p.portalObserved = true
}

// forgetPortalContactLocked restarts contact observation for a body moved into another
// world: its next tick is an arrival, and a contact still waiting from the world it left
// is discarded. The caller holds Sim.mu.
func (p *Player) forgetPortalContactLocked() {
	p.portalObserved = false
	p.portalInsideAny = false
	select {
	case <-p.portalContacts:
	default:
	}
}
