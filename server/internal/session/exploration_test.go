package session_test

import (
	"context"
	"maps"
	"slices"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The hook, at the layer it lives at: a chunk recorded as delivered reports its column,
// and the whole vertical stack of one column reports the same one.
//
// **MarkLoaded and not MoveTo**, which is the distinction the streaming code already
// draws for a different reason and this reuses: a chunk that was scheduled and never
// arrived is terrain the client does not have, and calling it explored would draw a map
// of places the player never saw.
func TestMarkingChunksRevealsTheirColumns(t *testing.T) {
	t.Parallel()

	view := session.NewView(1)

	var revealed []world.Column
	view.RecordExploration(func(column world.Column) {
		revealed = append(revealed, column)
	})

	// Three chunks of one column and two of another. A column has no height, so five
	// chunks are two places.
	stack := []world.Coord{
		{X: 2, Y: 0, Z: 3}, {X: 2, Y: 1, Z: 3}, {X: 2, Y: -1, Z: 3},
		{X: -4, Y: 0, Z: 5}, {X: -4, Y: 7, Z: 5},
	}
	for _, coord := range stack {
		view.MarkLoaded(coord)
	}

	if len(revealed) != len(stack) {
		t.Fatalf("%d chunks reported %d columns; the hook must fire once per chunk", len(stack), len(revealed))
	}
	distinct := map[world.Column]struct{}{}
	for _, column := range revealed {
		distinct[column] = struct{}{}
	}
	want := map[world.Column]struct{}{{CX: 2, CZ: 3}: {}, {CX: -4, CZ: 5}: {}}
	if !maps.Equal(distinct, want) {
		t.Errorf("the columns reported are %v, want %v", slices.Collect(maps.Keys(distinct)), slices.Collect(maps.Keys(want)))
	}

	// Nothing to reveal to is the ordinary case for a view in a test, and it must not
	// be a nil dereference on the streaming path.
	view.RecordExploration(nil)
	view.MarkLoaded(world.Coord{X: 9})
}

// The issue end to end at this layer: a character joins, is streamed terrain, is told
// which columns that revealed, disconnects, and comes back to the same map.
//
// One simulation and one pair of stores across both sessions, which is what a reconnect
// actually is. The restart — a second process over the same directory — is
// cmd/voxelheimd's test.
func TestAMapSurvivesADisconnect(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	store, err := persist.OpenStore(dir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	ledgers, err := persist.OpenExplorationStore(dir)
	if err != nil {
		t.Fatalf("OpenExplorationStore: %v", err)
	}
	identities := identitiesExploring(store, ledgers)
	account := testAccount(31)

	cfg := serveConfig()
	cfg.Spawn = world.SpawnAt(cfg.WorldSeed)
	chunks := world.NewCache(cfg.WorldSeed, 4, 512)
	peers := session.NewRegistry()
	sim, err := game.NewSim(cfg.TickRate, cfg.ViewDistance, cfg.WorldSeed, game.NewCacheTerrain(chunks), chunks, peers.NextID, discard())
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	generateAround(t, chunks, cfg.Spawn, 2)

	// ---- the first life -------------------------------------------------------

	first := newFakeConn()
	firstDone := make(chan error, 1)
	go func() {
		firstDone <- session.Serve(context.Background(), first, cfg, noTimeouts(), chunks, sim, peers, identities, 1, discard())
	}()

	first.in <- protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", testTicket(account))
	chooseCharacter(t, first, "Eivor")
	if got := vnet.GetRootAsEnvelope(nextFrame(t, first), 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the first session got %s, want a welcome", got)
	}
	sink := collect(t, first)

	// The view volume is (2r+1)² columns once the whole cube has been streamed. Waiting
	// on the columns rather than on a chunk count is the honest wait: what this test is
	// about is columns, and a view that streamed fewer chunks than expected would fail
	// here rather than pass with a smaller map.
	side := 2*int(cfg.ViewDistance) + 1
	wantColumns := side * side
	waitUntil(t, "the whole view volume to be streamed", func() bool {
		return len(sink.exploredColumns()) >= wantColumns
	})

	// **The ledger a fresh character receives is exactly what streaming revealed.** No
	// page precedes the batches: a character who has never played has nothing to be sent
	// after the welcome, and an empty MapExplored is a message the contract forbids.
	streamed := sink.exploredColumns()
	if len(streamed) != wantColumns {
		t.Fatalf("streaming revealed %d columns, want the %d of one view volume", len(streamed), wantColumns)
	}
	for _, page := range sink.exploredPages() {
		if len(page) == 0 {
			t.Fatal("an empty MapExplored reached the wire, which the contract forbids")
		}
	}
	// And every column is one the client was actually sent terrain for.
	for column := range streamed {
		if !sink.holdsColumn(column) {
			t.Errorf("column %+v was reported as explored with no chunk delivered in it", column)
		}
	}

	if err := first.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	select {
	case err := <-firstDone:
		if err != nil {
			t.Fatalf("the first session returned %v", err)
		}
	case <-time.After(patience):
		t.Fatal("the first session did not return")
	}

	// What the teardown wrote, read back through the store rather than assumed.
	character := onlyCharacter(t, store, account)
	saved, found, err := ledgers.Load(character.ID)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("the session left no ledger behind")
	}
	if len(saved) != len(streamed) {
		t.Fatalf("the ledger holds %d columns, want the %d that were streamed", len(saved), len(streamed))
	}
	for _, column := range saved {
		if _, sent := streamed[column]; !sent {
			t.Errorf("the ledger holds %+v, which was never sent to the client", column)
		}
	}
	// Sorted, which is what makes an unchanged ledger the same bytes twice.
	if !slices.IsSortedFunc(saved, func(a, b world.Column) int {
		if a.CZ != b.CZ {
			return int(a.CZ - b.CZ)
		}
		return int(a.CX - b.CX)
	}) {
		t.Error("the stored ledger is not in the sorted order the format's caller promises")
	}

	// ---- the same map, one connection later -----------------------------------

	second := newFakeConn()
	secondDone := make(chan error, 1)
	go func() {
		secondDone <- session.Serve(context.Background(), second, cfg, noTimeouts(), chunks, sim, peers, identities, 2, discard())
	}()
	t.Cleanup(func() {
		_ = second.Close()
		<-secondDone
	})

	second.in <- protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", testTicket(account))
	chooseCharacter(t, second, "Eivor")
	if got := vnet.GetRootAsEnvelope(nextFrame(t, second), 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the second session got %s, want a welcome", got)
	}

	back := collect(t, second)
	waitUntil(t, "the recalled ledger", func() bool {
		return len(back.exploredPages()) > 0
	})

	// The **first** page after the welcome, taken rather than the union: what is being
	// pinned is that the whole stored ledger goes out before this session's streaming
	// has had a chance to add anything, so a client knows where it has been before it
	// sees a single new chunk.
	firstPage := back.exploredPages()[0]
	if len(firstPage) != len(saved) {
		t.Fatalf("the first page carries %d columns, want the whole stored ledger of %d", len(firstPage), len(saved))
	}
	if !slices.Equal(firstPage, saved) {
		t.Error("the first page is not the stored ledger, in the stored order")
	}
}

// holdsColumn reports whether this session was sent at least one chunk in the column.
func (c *collector) holdsColumn(column world.Column) bool {
	c.mu.Lock()
	defer c.mu.Unlock()

	for coord := range c.chunks {
		if coord.Column() == column {
			return true
		}
	}
	return false
}
