package session_test

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// marking is one admitted session with a map behind it, and the pieces a test needs to
// drive it: the connection to write requests to, the collector to read answers from, and
// the stores the teardown writes through.
type marking struct {
	conn   *fakeConn
	sink   *collector
	store  *persist.Store
	marks  *persist.MarkerStore
	done   chan error
	config session.Config

	// stopped says the teardown has already been waited for, so the cleanup below does
	// not wait for a second one that will never arrive.
	stopped bool
}

// startMarking admits one character over a real player store and a real marker store, and
// returns once the whole join has been sent — welcome, inventory, ledger and the first
// MarkerList.
//
// The whole join rather than the welcome alone, because every test here is about what
// happens *after* the list arrives, and a test that started measuring earlier would be
// counting the join's own list as one of its answers.
func startMarking(t *testing.T, dir string, account [16]byte, name string, entityID uint64) *marking {
	t.Helper()

	store, err := persist.OpenStore(dir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	marks, err := persist.OpenMarkerStore(dir)
	if err != nil {
		t.Fatalf("OpenMarkerStore: %v", err)
	}
	return resumeMarking(t, store, marks, account, name, entityID)
}

// resumeMarking is startMarking over stores that already exist, which is what a second
// connection for the same character is.
func resumeMarking(t *testing.T, store *persist.Store, marks *persist.MarkerStore, account [16]byte, name string, entityID uint64) *marking {
	t.Helper()

	identities := identitiesMapping(store, nil, marks)

	cfg := serveConfig()
	cfg.Spawn = world.SpawnAt(cfg.WorldSeed)
	chunks := world.NewCache(cfg.WorldSeed, 4, 512)
	peers := session.NewRegistry(session.DefaultConcurrentSessions)
	sim, err := game.NewSim(cfg.TickRate, cfg.ViewDistance, cfg.WorldSeed, game.NewCacheTerrain(chunks), chunks, peers.NextID, discard())
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}

	conn := newFakeConn()
	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, cfg, noTimeouts(), chunks, sim, peers, identities, entityID, discard())
	}()

	conn.in <- protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, name, testTicket(account))
	chooseCharacter(t, conn, name)
	if got := vnet.GetRootAsEnvelope(nextFrame(t, conn), 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the session got %s, want a welcome", got)
	}

	sink := collect(t, conn)
	m := &marking{conn: conn, sink: sink, store: store, marks: marks, done: done, config: cfg}

	// **Registered before the first assertion and after the collector's own cleanup**, so
	// it runs first and the session is demonstrably finished before t.TempDir starts
	// removing the directory it is still writing marker files into. A test that skipped
	// this failed in the cleanup rather than in the body, naming a directory that was not
	// empty and nothing about why.
	t.Cleanup(func() {
		if m.stopped {
			return
		}
		_ = conn.Close()
		select {
		case <-done:
		case <-time.After(patience):
			t.Error("the session did not return before the test ended")
		}
	})

	m.waitForLists(t, 1)
	return m
}

// waitForLists blocks until at least n MarkerLists have arrived.
func (m *marking) waitForLists(t *testing.T, n int) [][]protocol.Marker {
	t.Helper()

	waitUntil(t, "a MarkerList", func() bool {
		return len(m.sink.markerListsSeen()) >= n
	})
	return m.sink.markerListsSeen()
}

// place asks for one mark with a note and a kind that are never the point of the test.
func (m *marking) place(x, z int32, note string) {
	m.conn.in <- protocol.EncodeMarkerPlaceRequest(protocol.MarkerPlaceRequest{
		X: x, Z: z, Kind: vnet.MarkerKindResource, Note: note, ClientTick: 1,
	})
}

// stop closes the connection and waits for the session's teardown, which is where the
// marker file is written.
func (m *marking) stop(t *testing.T) {
	t.Helper()

	if err := m.conn.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	select {
	case err := <-m.done:
		m.stopped = true
		if err != nil {
			t.Fatalf("the session returned %v", err)
		}
	case <-time.After(patience):
		t.Fatal("the session did not return")
	}
}

// **A list follows the welcome even when there is nothing on it**, which is the one place
// this differs from the exploration ledger beside it. A MarkerList replaces the client's
// copy, so an empty one is a statement; an empty MapExplored is the absence of one, which
// is why the contract forbids it.
func TestAFreshCharacterIsSentAnEmptyMarkerList(t *testing.T) {
	t.Parallel()

	m := startMarking(t, t.TempDir(), testAccount(41), "Eivor", 1)
	lists := m.sink.markerListsSeen()
	if len(lists) != 1 {
		t.Fatalf("the join sent %d MarkerLists, want exactly one", len(lists))
	}
	if len(lists[0]) != 0 {
		t.Errorf("a character who has marked nothing was sent %d marks", len(lists[0]))
	}
}

// The ordinary path: a mark is placed, the whole list comes back, and the mark carries an
// id the server minted rather than anything the client chose.
func TestPlacingAMarkAnswersWithTheWholeList(t *testing.T) {
	t.Parallel()

	m := startMarking(t, t.TempDir(), testAccount(42), "Eivor", 1)
	m.place(120, -340, "iron under the hill")
	lists := m.waitForLists(t, 2)

	placed := lists[1]
	if len(placed) != 1 {
		t.Fatalf("the answer to one placement holds %d marks, want 1", len(placed))
	}
	if placed[0].MarkerID == 0 {
		t.Error("the mark came back with no id, which a MarkerList may not carry")
	}
	if placed[0].X != 120 || placed[0].Z != -340 {
		t.Errorf("the mark is at (%d, %d), want (120, -340)", placed[0].X, placed[0].Z)
	}
	if placed[0].Note != "iron under the hill" {
		t.Errorf("the note came back as %q, want the one that was typed", placed[0].Note)
	}
	if placed[0].Kind != vnet.MarkerKindResource {
		t.Errorf("the mark's kind is %s, want Resource", placed[0].Kind)
	}
}

// The sixty-fifth is refused, the sixty-fourth is not, and the refusal names the action
// and the reason the contract has for it.
func TestTheSixtyFifthMarkIsRefused(t *testing.T) {
	t.Parallel()

	m := startMarking(t, t.TempDir(), testAccount(43), "Eivor", 1)
	for i := range persist.MaxMarkers {
		m.place(int32(i), int32(i), "")
	}
	lists := m.waitForLists(t, persist.MaxMarkers+1)
	if got := len(lists[persist.MaxMarkers]); got != persist.MaxMarkers {
		t.Fatalf("after %d placements the list holds %d marks", persist.MaxMarkers, got)
	}

	m.place(999, 999, "one too many")
	waitUntil(t, "the refusal", func() bool {
		return len(m.sink.actionRefusals()) > 0
	})

	refusals := m.sink.actionRefusals()
	if len(refusals) != 1 {
		t.Fatalf("the sixty-fifth placement produced %d refusals, want 1", len(refusals))
	}
	if refusals[0].Action != vnet.RefusedActionPlaceMarker || refusals[0].Reason != vnet.RefusalReasonTooManyMarkers {
		t.Errorf("the refusal is %s/%s, want PlaceMarker/TooManyMarkers", refusals[0].Action, refusals[0].Reason)
	}
	// And it put nothing on the map: a refused placement answers with no list at all,
	// because the client's copy did not change.
	if got := len(m.sink.markerListsSeen()); got != persist.MaxMarkers+1 {
		t.Errorf("a refused placement sent a list: %d in total, want %d", got, persist.MaxMarkers+1)
	}
}

// A removal takes the mark off and answers with what is left; an id this character does
// not hold is refused with the one answer the contract gives for both "never existed" and
// "somebody else's".
func TestRemovingAMarkShrinksTheListAndAnUnknownIdIsRefused(t *testing.T) {
	t.Parallel()

	m := startMarking(t, t.TempDir(), testAccount(44), "Eivor", 1)
	m.place(10, 10, "first")
	m.place(20, 20, "second")
	lists := m.waitForLists(t, 3)

	held := lists[2]
	if len(held) != 2 {
		t.Fatalf("two placements produced a list of %d", len(held))
	}

	m.conn.in <- protocol.EncodeMarkerRemoveRequest(protocol.MarkerRemoveRequest{MarkerID: held[0].MarkerID})
	lists = m.waitForLists(t, 4)
	left := lists[3]
	if len(left) != 1 || left[0].MarkerID != held[1].MarkerID {
		t.Fatalf("after removing the first mark the list is %+v, want only the second", left)
	}

	// The same id again: it is gone, and gone is the same answer as never existed.
	m.conn.in <- protocol.EncodeMarkerRemoveRequest(protocol.MarkerRemoveRequest{MarkerID: held[0].MarkerID})
	waitUntil(t, "the refusal", func() bool {
		return len(m.sink.actionRefusals()) > 0
	})

	refusals := m.sink.actionRefusals()
	if refusals[0].Action != vnet.RefusedActionRemoveMarker || refusals[0].Reason != vnet.RefusalReasonMarkerUnknown {
		t.Errorf("the refusal is %s/%s, want RemoveMarker/MarkerUnknown", refusals[0].Action, refusals[0].Reason)
	}
	if got := len(m.sink.markerListsSeen()); got != 4 {
		t.Errorf("a refused removal sent a list: %d in total, want 4", got)
	}
}

// **An id a removal freed is never minted again**, which is what the counter in the
// file's header exists for. Derived from the marks it holds it would fall back the moment
// the newest one went, and the next placement would carry an id the client already knows.
func TestARemovedIdIsNeverMintedAgain(t *testing.T) {
	t.Parallel()

	m := startMarking(t, t.TempDir(), testAccount(45), "Eivor", 1)
	m.place(1, 1, "first")
	lists := m.waitForLists(t, 2)
	first := lists[1][0].MarkerID

	m.conn.in <- protocol.EncodeMarkerRemoveRequest(protocol.MarkerRemoveRequest{MarkerID: first})
	m.waitForLists(t, 3)

	m.place(2, 2, "second")
	lists = m.waitForLists(t, 4)
	if got := lists[3][0].MarkerID; got == first {
		t.Fatalf("the mark placed after a removal took the freed id %d", got)
	}
}

// A mark outside the world is refused, and refused in silence: the contract has no member
// for it, so there is no true sentence to send, and the answer is the one every other
// admission with nothing to say gives — no list, no refusal, and the client's copy
// unchanged.
//
// The bound is world.BlockLimit, which is where a float32 stops addressing individual
// blocks and where internal/game already ends the world.
func TestAMarkOutsideTheWorldIsRefusedInSilence(t *testing.T) {
	t.Parallel()

	m := startMarking(t, t.TempDir(), testAccount(46), "Eivor", 1)

	// On the edge is inside it: a mark exactly on the limit is a place a body may stand.
	m.place(world.BlockLimit, -world.BlockLimit, "the edge of the world")
	lists := m.waitForLists(t, 2)
	if len(lists[1]) != 1 {
		t.Fatalf("a mark on the world's edge was not accepted: the list holds %d", len(lists[1]))
	}

	// One block past it is not.
	for _, beyond := range []protocol.MarkerPlaceRequest{
		{X: world.BlockLimit + 1, Z: 0},
		{X: 0, Z: -(world.BlockLimit + 1)},
	} {
		beyond.Kind = vnet.MarkerKindCave
		beyond.ClientTick = 2
		m.conn.in <- protocol.EncodeMarkerPlaceRequest(beyond)
	}

	// A round trip through the same connection, so the two refusals above have certainly
	// been handled by the time it is answered. Without it this would be asserting on an
	// absence that has not had time to happen.
	m.place(5, 5, "a place in the world")
	lists = m.waitForLists(t, 3)

	if len(lists[2]) != 2 {
		t.Errorf("the list holds %d marks, want the two that named places inside the world", len(lists[2]))
	}
	if got := m.sink.actionRefusals(); len(got) != 0 {
		t.Errorf("a mark outside the world was answered with %+v, want silence", got)
	}
}

// The note the contract will not carry is refused **at the decode boundary**, by closing
// the session, which schemas/player.fbs names as the stricter of the two answers it
// allows. So there is no ActionRefused to look for and the session ends — which is the
// behaviour to pin, because it is the reason `NoteTooLong` is declared and never sent.
func TestANoteTheContractWillNotCarryEndsTheSession(t *testing.T) {
	t.Parallel()

	for name, note := range map[string]string{
		"one byte too long":    strings.Repeat("a", protocol.MarkerNoteMaxBytes+1),
		"not valid UTF-8":      "\xc3\x28",
		"valid UTF-8 too wide": strings.Repeat("é", protocol.MarkerNoteMaxBytes),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			m := startMarking(t, t.TempDir(), testAccount(47), "Eivor", 1)
			m.conn.in <- protocol.EncodeMarkerPlaceRequest(protocol.MarkerPlaceRequest{
				X: 1, Z: 1, Kind: vnet.MarkerKindNote, Note: note, ClientTick: 1,
			})

			select {
			case err := <-m.done:
				m.stopped = true
				if !errors.Is(err, protocol.ErrMalformed) {
					t.Fatalf("Serve returned %v, want an error wrapping protocol.ErrMalformed", err)
				}
			case <-time.After(patience):
				t.Fatal("the session survived a note the contract will not carry")
			}
			if got := m.sink.actionRefusals(); len(got) != 0 {
				t.Errorf("a refusal was sent for a frame that closed the session: %+v", got)
			}
		})
	}
}

// The issue end to end: a character marks the map, disconnects, and comes back to the
// same marks — the ones the *file* holds, read through the store rather than assumed.
func TestAMapOfMarksSurvivesADisconnect(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	account := testAccount(48)

	first := startMarking(t, dir, account, "Eivor", 1)
	first.place(-90, 12, "cave by the river")
	first.place(400, -400, "")
	lists := first.waitForLists(t, 3)
	placed := lists[2]
	if len(placed) != 2 {
		t.Fatalf("two placements produced a list of %d", len(placed))
	}
	first.stop(t)

	// What the teardown wrote, read back through the store rather than assumed.
	character := onlyCharacter(t, first.store, account)
	saved, found, err := first.marks.Load(character.ID)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("the session left no marker file behind")
	}
	if len(saved.Markers) != len(placed) {
		t.Fatalf("the file holds %d marks, want the %d that were placed", len(saved.Markers), len(placed))
	}
	for i := range saved.Markers {
		if saved.Markers[i] != placed[i] {
			t.Errorf("the file holds %+v at %d, want %+v", saved.Markers[i], i, placed[i])
		}
	}

	// ---- the same map, one connection later -----------------------------------

	second := resumeMarking(t, first.store, first.marks, account, "Eivor", 2)

	back := second.sink.markerListsSeen()
	if len(back) != 1 {
		t.Fatalf("the second join sent %d MarkerLists, want exactly one", len(back))
	}
	if len(back[0]) != len(placed) {
		t.Fatalf("the recalled list holds %d marks, want %d", len(back[0]), len(placed))
	}
	for i := range back[0] {
		if back[0][i] != placed[i] {
			t.Errorf("the recalled list holds %+v at %d, want %+v", back[0][i], i, placed[i])
		}
	}

	// And the counter came back with it: the next mark is not one of the two already
	// drawn, whatever the client does with the list.
	second.place(1, 1, "a third")
	after := second.waitForLists(t, 2)
	minted := after[1][len(after[1])-1].MarkerID
	for _, earlier := range placed {
		if minted == earlier.MarkerID {
			t.Fatalf("the mark placed after a reconnect took id %d, which is already drawn", minted)
		}
	}
}

// The ephemeral world, driven end to end through a real session: marks are placed and
// answered, and nothing is written down.
//
// **The store is nil here, and that is a shape main really produces** — openMarkers
// answers nil for an empty -world-dir, and NewIdentities keeps it as given. So every
// method Markers reaches for is called on a nil *persist.MarkerStore: Load through
// recallMarkers on the way in, Save through the teardown on the way out. Each is
// nil-receiver-safe by construction, which TestANilMarkerStoreKeepsNothing pins one layer
// down; this test is the other end of that contract, because a nil check at the call site
// is the alternative that package deliberately refused and the one that is forgotten
// panics.
//
// The player store is real, because a character has to be selected before there is a
// session to mark from — the nil under test is the marker store and nothing else.
func TestAnEphemeralWorldMarksAndRemembersNothing(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	store, err := persist.OpenStore(dir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}

	m := resumeMarking(t, store, nil, testAccount(49), "Eivor", 1)

	m.place(120, -340, "iron under the hill")
	lists := m.waitForLists(t, 2)
	if placed := lists[1]; len(placed) != 1 {
		t.Fatalf("the answer to one placement holds %d marks, want 1", len(placed))
	}

	// The teardown's save runs here, on the nil store, and returns rather than panics.
	m.stop(t)
}
