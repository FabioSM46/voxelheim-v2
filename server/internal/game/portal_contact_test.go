package game

import (
	"fmt"
	"log/slog"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// contactWorld is flat ground with its top at y=0 and one threshold standing on it. The
// terrain draws no arch — a body walks through where a jamb would be — so what these
// tests measure is the contact rule, not the collision that normally keeps a body out
// of the frame. Every threshold everyPortalSheet builds has its lowest veil course at
// y=1 when placed at origin height 0.
type contactWorld struct {
	sim   *Sim
	sheet portalSheet
	tick  uint64
	input uint32
}

func contactThresholds() map[string]world.PortalThreshold {
	thresholds := make(map[string]world.PortalThreshold)
	for variant := range uint8(world.RuinVariantCount) {
		for facing := world.Facing(0); facing < 4; facing++ {
			b := world.Building{Kind: world.BuildingRuin, Variant: variant, OriginX: -40, OriginY: 0, OriginZ: 23, Facing: facing}
			thresholds[fmt.Sprintf("ruin%d/facing%d", variant, facing)] = world.Ruin{Building: b}.Threshold()
		}
	}
	for seed := range int64(4) {
		thresholds[fmt.Sprintf("instance/seed%d", seed)] = world.InstanceExitThreshold(seed)
	}
	return thresholds
}

func newContactWorld(t *testing.T, threshold world.PortalThreshold, group *WorldGroup) *contactWorld {
	t.Helper()
	sim, err := NewSim(20, 1, 1, dropTerrain{groundTop: 0}, refusedEdits{}, testEntityIDs(), slog.New(slog.DiscardHandler), WithWorldGroup(group), WithPortals(threshold))
	if err != nil {
		t.Fatal(err)
	}
	w := &contactWorld{sim: sim, sheet: newPortalSheet(threshold)}
	if floor := w.sheet.at(0, 0, 0)[1]; floor != 1 {
		t.Fatalf("fixture threshold's lowest veil course is at y=%v, not on the ground", floor)
	}
	return w
}

func (w *contactWorld) join(t *testing.T, lateral, normal float64) *Player {
	t.Helper()
	pos := w.sheet.at(lateral, normal, 0)
	id := w.sim.mintEntityID()
	p, err := w.sim.JoinCharacter(id, testPlayerID(id), id, fmt.Sprintf("Walker%d", id), [3]float32{float32(pos[0]), float32(pos[1]), float32(pos[2])}, testAppearance(), nil, func([]byte) bool { return true })
	if err != nil {
		t.Fatal(err)
	}
	return p
}

// walk refreshes one intent every tick, as a client's input loop does, for ticks ticks:
// dir is +1 toward the side of the veil its plane's coordinate grows on, -1 away, and 0
// stands still. It returns every contact the tick handed over.
func (w *contactWorld) walk(t *testing.T, p *Player, dir float32, ticks int) []PortalContact {
	t.Helper()
	var got []PortalContact
	for range ticks {
		w.input++
		in := protocol.PlayerInput{ClientTick: w.input}
		// yaw 0 looks along -Z with +X to its right.
		if w.sheet.normal == 2 {
			in.MoveZ = -dir
		} else {
			in.MoveX = dir
		}
		if err := p.Submit(in); err != nil {
			t.Fatal(err)
		}
		w.tick++
		w.sim.Step(w.tick)
		select {
		case contact := <-p.PortalContacts():
			got = append(got, contact)
		default:
		}
	}
	return got
}

func (w *contactWorld) wantContacts(t *testing.T, what string, got []PortalContact, want int) {
	t.Helper()
	if len(got) != want {
		t.Fatalf("%s: %d crossing attempts, want %d", what, len(got), want)
	}
	for _, c := range got {
		heart := [3]int32{int32(w.sheet.heart[0]), int32(w.sheet.heart[1]), int32(w.sheet.heart[2])}
		if !c.In(w.sim) || c.Request() != (protocol.PortalRequest{HasArch: true, Arch: heart}) {
			t.Fatalf("%s: contact %+v does not name this world's arch %v", what, c, heart)
		}
	}
}

// At walking pace a tick is about a fifth of a block: 20 ticks carries a body from two
// blocks in front of the plane to two blocks behind it.
func TestWalkingIntoAVeilIsOneAttemptPerContact(t *testing.T) {
	for name, threshold := range contactThresholds() {
		t.Run(name, func(t *testing.T) {
			w := newContactWorld(t, threshold, NewWorldGroup())
			p := w.join(t, 0, -2)
			w.wantContacts(t, "standing in front", w.walk(t, p, 0, 5), 0)
			w.wantContacts(t, "walking straight through", w.walk(t, p, 1, 20), 1)
			w.wantContacts(t, "walking back through", w.walk(t, p, -1, 20), 1)

			// Into the veil and stopping there. The attempt is the entry; the minute spent
			// standing in it afterwards, refused or offered, asks for nothing more.
			entered := w.walk(t, p, 1, 9)
			w.wantContacts(t, "stopping inside", append(entered, w.walk(t, p, 0, 60)...), 1)
			w.sim.mu.Lock()
			inside := w.sheet.touches(p.box())
			w.sim.mu.Unlock()
			if !inside {
				t.Fatal("fixture did not stop the body inside the veil")
			}
			w.wantContacts(t, "leaving the way it came", w.walk(t, p, -1, 12), 0)
			w.wantContacts(t, "entering again", w.walk(t, p, 1, 12), 1)
		})
	}
}

func TestPassingBesideAnArchIsNotContact(t *testing.T) {
	for name, threshold := range contactThresholds() {
		t.Run(name, func(t *testing.T) {
			w := newContactWorld(t, threshold, NewWorldGroup())
			beside := w.join(t, 4.5, -2)
			w.wantContacts(t, "through the plane outside the jamb", w.walk(t, beside, 1, 20), 0)
			w.wantContacts(t, "and back", w.walk(t, beside, -1, 20), 0)
		})
	}
}

// Every way a server puts a body somewhere, rather than letting it walk there, is an
// arrival: joining at a stored position, a respawn or return that moves it between
// ticks, and a transfer into another world. None of them is an attempt, and each leaves
// the body having to walk out before the same veil can count.
func TestABodyPlacedInAVeilMustLeaveItBeforeItCounts(t *testing.T) {
	for name, threshold := range contactThresholds() {
		t.Run(name, func(t *testing.T) {
			group := NewWorldGroup()
			w := newContactWorld(t, threshold, group)

			joined := w.join(t, 0, 0)
			w.wantContacts(t, "joining inside", w.walk(t, joined, 0, 20), 0)
			w.wantContacts(t, "walking out of the arrival", w.walk(t, joined, -1, 15), 0)
			w.wantContacts(t, "walking back in", w.walk(t, joined, 1, 14), 1)

			placed := w.join(t, 0, -3)
			w.wantContacts(t, "standing clear", w.walk(t, placed, 0, 3), 0)
			w.sim.mu.Lock()
			placed.pos = w.sheet.at(0, 0, 0)
			w.sim.mu.Unlock()
			w.wantContacts(t, "relocated into the veil", w.walk(t, placed, 0, 20), 0)

			// A contact still waiting from one world does not follow the body into the next,
			// and the next world's veil it arrives in is an arrival too.
			other := newContactWorld(t, threshold, group)
			traveller := w.join(t, 0, -2)
			// Walked by hand rather than through walk, which would collect the contact.
			for range 9 {
				w.input++
				in := protocol.PlayerInput{ClientTick: w.input}
				if w.sheet.normal == 2 {
					in.MoveZ = -1
				} else {
					in.MoveX = 1
				}
				if err := traveller.Submit(in); err != nil {
					t.Fatal(err)
				}
				w.tick++
				w.sim.Step(w.tick)
			}
			if len(traveller.PortalContacts()) != 1 {
				t.Fatal("fixture: the walk produced no contact to be left waiting")
			}
			arrival := other.sheet.at(0, 0, 0)
			if err := w.sim.Transfer(traveller, other.sim, [3]float32{float32(arrival[0]), float32(arrival[1]), float32(arrival[2])}); err != nil {
				t.Fatal(err)
			}
			if len(traveller.PortalContacts()) != 0 {
				t.Fatal("a contact from the world left behind survived the transfer")
			}
			other.input = w.input
			other.wantContacts(t, "transferred into a veil", other.walk(t, traveller, 0, 20), 0)
			other.wantContacts(t, "leaving it", other.walk(t, traveller, -1, 15), 0)
			other.wantContacts(t, "walking in on purpose", other.walk(t, traveller, 1, 14), 1)
		})
	}
}

// A simulation told of no portals measures nothing, and one told of a portal does not
// measure a body nowhere near it.
func TestContactIsBoundedToTheWorldsOwnPortals(t *testing.T) {
	sim, err := NewSim(20, 1, 1, dropTerrain{groundTop: 0}, refusedEdits{}, testEntityIDs(), slog.New(slog.DiscardHandler))
	if err != nil {
		t.Fatal(err)
	}
	if len(sim.portalSheets) != 0 {
		t.Fatal("a simulation with no WithPortals has a threshold")
	}
	for _, threshold := range contactThresholds() {
		s := newPortalSheet(threshold)
		far := playerBox(s.at(portalContactReach+1, 0, 0))
		if s.near(far) {
			t.Fatal("a body beyond the contact reach is still measured")
		}
		if !s.near(playerBox(s.at(3, -2, 0))) {
			t.Fatal("a body beside the arch is not measured")
		}
	}
}

// An instance manager gives every copy its own exit and no other threshold, so one handed
// WithPortals would silently drop it. It refuses instead, and still accepts the options an
// instance does use.
func TestAnInstanceManagerRefusesPortalsItWouldDrop(t *testing.T) {
	log := slog.New(slog.DiscardHandler)
	threshold := world.InstanceExitThreshold(1)
	if _, err := NewInstanceManager(20, 1, 2, testEntityIDs(), log, WithWorldGroup(NewWorldGroup()), WithPortals(threshold)); err == nil {
		t.Fatal("an instance manager accepted a portal it would never place")
	}
	m, err := NewInstanceManager(20, 1, 2, testEntityIDs(), log, WithWorldGroup(NewWorldGroup()))
	if err != nil {
		t.Fatalf("an instance manager with no portals was refused: %v", err)
	}
	m.Close()
}
