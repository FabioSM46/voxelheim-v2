package session_test

import (
	"bytes"
	"context"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Controls prove each sender acts; assertions require silence in both directions.
func TestLiveSessionsAreIsolatedAcrossWorldChanges(t *testing.T) {
	for _, pair := range []bool{true, false} {
		t.Run(map[bool]string{true: "two copies of one ruin", false: "instance and open world"}[pair], func(t *testing.T) {
			cfg := editConfig()
			cfg.Spawn = [3]float32{.5, 1, .5}
			group := game.NewWorldGroup()
			chunks, open, peers := editDeps(t, cfg, game.WithWorldGroup(group))
			manager, err := game.NewInstanceManager(cfg.TickRate, cfg.ViewDistance, 2, peers.NextID, discard(), game.WithWorldGroup(group))
			if err != nil {
				t.Fatal(err)
			}
			t.Cleanup(manager.Close)
			peers.NextID()
			peers.NextID() // reserve the two explicitly named session ids
			a, fa := admit(t, cfg, chunks, open, peers, 1)
			b, fb := admit(t, cfg, chunks, open, peers, 2)
			scopes := []*session.Registry{peers, peers}
			move := func(id uint64) {
				inst, e := manager.Create(game.InstanceRuin{})
				if e != nil {
					t.Fatal(e)
				}
				anchor, _ := world.InstanceAnchors(inst.Seed)
				ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
				defer cancel()
				if e = peers.ChangeWorld(ctx, id, session.WorldBinding{Chunks: inst.Chunks, Sim: inst.Sim, Context: inst.Context, Spawn: [3]float32{float32(anchor.X) + .5, float32(anchor.Y), float32(anchor.Z) + .5}}); e != nil {
					t.Fatal(e)
				}
				scopes[id-1] = peers.ForWorld(inst.Chunks)
			}
			move(1)
			if pair {
				move(2)
			}
			for range game.VoiceSetInterval {
				manager.Step()
			}
			open.Step(game.VoiceSetInterval)

			waitUntil(t, "both current-world snapshots", func() bool { return fa.snapshotCount() > 0 && fb.snapshotCount() > 0 })
			for i, f := range []*collector{fa, fb} {
				if _, seen := f.position(uint64(2 - i)); seen {
					t.Error("foreign position or health")
				}
			}
			coord := world.Coord{}
			waitUntil(t, "both worlds hold the same coordinate", func() bool { return fa.chunkCount(coord) > 1 && (!pair || fb.chunkCount(coord) > 1) })
			for i, scope := range scopes {
				update := protocol.EncodeBlockUpdate(protocol.BlockUpdate{Pos: [3]int32{int32(i), 1, 0}, BlockID: uint16(world.Stone)})
				if count := scope.BroadcastChunk(coord, update); count != 1 {
					t.Fatalf("world %d block update reached %d sessions", i, count)
				}
			}
			waitUntil(t, "both scoped block updates", func() bool { return len(fa.blockUpdates()) == 1 && len(fb.blockUpdates()) == 1 })
			for i, f := range []*collector{fa, fb} {
				if f.blockUpdates()[0].Pos[0] != int32(i) {
					t.Error("foreign block edit")
				}
			}
			for i, c := range []*fakeConn{a, b} {
				c.in <- protocol.EncodeVoiceFrame(protocol.VoiceFrame{Sequence: 1, Audience: vnet.VoiceAudienceEveryone, Opus: opusFixture})
				c.in <- protocol.EncodeChatRequest(protocol.ChatRequest{Text: []string{"first world", "second world"}[i]})
			}
			waitUntil(t, "both isolated senders' own chat", func() bool { return len(fa.chatMessages()) >= 1 && len(fb.chatMessages()) >= 1 })
			for i, f := range []*collector{fa, fb} {
				chats := f.chatMessages()
				if len(chats) != 1 || chats[0].SenderEntityID != uint64(i+1) {
					t.Errorf("foreign chat: %+v", chats)
				}
				if len(f.voicesHeard()) != 0 {
					t.Error("foreign voice")
				}
			}
		})
	}
}

// A writer already inside WriteFrame cannot be revoked. The transfer waits for
// it, discards queued old frames, writes Arrival, and only then enables workers.
func TestWorldChangeWaitsForBlockedOldWriterAndOrdersArrival(t *testing.T) {
	cfg := editConfig()
	group := game.NewWorldGroup()
	chunks, open, peers := editDeps(t, cfg, game.WithWorldGroup(group))
	manager, err := game.NewInstanceManager(cfg.TickRate, cfg.ViewDistance, 1, peers.NextID, discard(), game.WithWorldGroup(group))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(manager.Close)
	base := newFakeConn()
	conn := &bindingBarrierConn{fakeConn: base, entered: make(chan struct{}), release: make(chan struct{}), blockKind: vnet.PayloadChunkData}
	conn.block.Store(true)
	frames := collect(t, base)
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		done <- session.Serve(ctx, conn, cfg, noTimeouts(), chunks, open, peers, ephemeralIdentities(), 1, discard())
	}()
	t.Cleanup(func() {
		conn.unblock()
		cancel()
		_ = conn.Close()
		if err := <-done; err != nil {
			t.Error(err)
		}
	})
	base.in <- hello(1)
	createCharacter(base, "First")
	// Block while the old streamer is producing its initial view. Its bounded
	// queue fills behind this write, so cancellation must release its enqueue.

	select {
	case <-conn.entered:
	case <-time.After(5 * time.Second):
		t.Fatal("writer did not block")
	}
	_, _ = admit(t, cfg, chunks, open, peers, 2)
	open.Step(1)
	inst, err := manager.Create(game.InstanceRuin{})
	if err != nil {
		t.Fatal(err)
	}
	changed := make(chan error, 1)
	arrival := protocol.EncodeChatMessage(protocol.ChatMessage{Text: "authoritative arrival"})
	go func() {
		changed <- peers.ChangeWorld(ctx, 1, session.WorldBinding{Chunks: inst.Chunks, Sim: inst.Sim, Context: inst.Context, Spawn: [3]float32{.5, 1, .5}, Arrival: arrival})
	}()
	waitUntil(t, "transfer to reach its writer barrier", func() bool { return inst.Sim.Count() == 1 })
	select {
	case err := <-changed:
		t.Fatalf("transfer returned before old write finished: %v", err)
	default:
	}
	// Every queue can contain old work here. The barrier must discard it rather
	// than allowing it to arrive behind the transition frame.
	open.Step(2)
	conn.unblock()
	select {
	case err := <-changed:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("transfer stuck")
	}
	before := frames.snapshotCount()
	inst.Sim.Step(3)
	waitUntil(t, "destination snapshot", func() bool { return frames.snapshotCount() > before })
	conn.mu.Lock()
	captured := append([][]byte(nil), conn.frames...)
	conn.mu.Unlock()
	arrived := false
	sawSnapshot := false
	for _, frame := range captured {
		if bytes.Equal(frame, arrival) {
			arrived = true
			continue
		}
		if !arrived {
			continue
		}
		env := vnet.GetRootAsEnvelope(frame, 0)
		if env.PayloadType() == vnet.PayloadEntitySnapshot {
			table := payloadTable(t, env)
			var snapshot vnet.EntitySnapshot
			snapshot.Init(table.Bytes, table.Pos)
			if snapshot.EntitiesLength() != 1 {
				t.Fatal("old-world snapshot crossed arrival")
			}
			sawSnapshot = true
		}
	}
	if !arrived || !sawSnapshot {
		t.Fatal("arrival was not written before destination snapshot")
	}
}

type bindingBarrierConn struct {
	*fakeConn
	block     atomic.Bool
	blockKind vnet.Payload
	entered   chan struct{}
	release   chan struct{}
	once      sync.Once
	mu        sync.Mutex
	frames    [][]byte
}

func (c *bindingBarrierConn) unblock() { c.once.Do(func() { close(c.release) }) }
func (c *bindingBarrierConn) WriteFrame(frame []byte) error {
	if vnet.GetRootAsEnvelope(frame, 0).PayloadType() == c.blockKind && c.block.CompareAndSwap(true, false) {
		close(c.entered)
		<-c.release
	}
	err := c.fakeConn.WriteFrame(frame)
	if err == nil {
		c.mu.Lock()
		c.frames = append(c.frames, append([]byte(nil), frame...))
		c.mu.Unlock()
	}
	return err
}
