package session_test

import (
	"bufio"
	"context"
	"net"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/transport"
)

// TestServeOverRealTCP runs the handshake through the real listener rather than a
// fake conn. The unit tests above cover the admission rules; what this covers is
// everything between them and a socket — framing, buffering, flushing, and the
// fact that a reply actually leaves the process.
func TestServeOverRealTCP(t *testing.T) {
	t.Parallel()

	tr, err := transport.ListenTCP("127.0.0.1:0")
	if err != nil {
		t.Fatalf("ListenTCP: %v", err)
	}
	t.Cleanup(func() {
		if err := tr.Close(); err != nil {
			t.Errorf("close transport: %v", err)
		}
	})

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	chunks, sim, peers := serveDeps(t)
	served := make(chan error, 1)
	go func() {
		conn, aErr := tr.Accept()
		if aErr != nil {
			served <- aErr
			return
		}
		defer conn.Close() //nolint:errcheck // test teardown
		served <- session.Serve(ctx, conn, serveConfig(), noTimeouts(), chunks, sim, peers, ephemeralIdentities(), 11, discard())
	}()

	client, err := net.Dial("tcp", tr.Addr())
	if err != nil {
		t.Fatalf("dial %s: %v", tr.Addr(), err)
	}
	defer client.Close() //nolint:errcheck // test teardown

	w := bufio.NewWriter(client)
	if err := transport.WriteFrame(w, hello(1)); err != nil {
		t.Fatalf("WriteFrame: %v", err)
	}
	if err := w.Flush(); err != nil {
		t.Fatalf("flush: %v", err)
	}

	if err := client.SetReadDeadline(time.Now().Add(5 * time.Second)); err != nil {
		t.Fatalf("SetReadDeadline: %v", err)
	}
	frame, err := transport.ReadFrame(bufio.NewReader(client))
	if err != nil {
		t.Fatalf("ReadFrame: %v", err)
	}

	env := vnet.GetRootAsEnvelope(frame, 0)
	if env.PayloadType() != vnet.PayloadServerWelcome {
		t.Fatalf("reply is %s, want %s", env.PayloadType(), vnet.PayloadServerWelcome)
	}
	welcome := welcomeFrom(t, env)
	if got := welcome.EntityId(); got != 11 {
		t.Errorf("EntityId = %d, want 11", got)
	}
	if got := welcome.ChunkSize(); got != serveConfig().ChunkSize {
		t.Errorf("ChunkSize = %d, want %d", got, testConfig().ChunkSize)
	}

	if err := client.Close(); err != nil {
		t.Fatalf("client Close: %v", err)
	}
	select {
	case err := <-served:
		if err != nil {
			t.Fatalf("Serve returned %v, want nil after the client hung up", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Serve did not return after the client hung up")
	}
}

// The idle deadline over a real socket, which is the one path the tests above compose
// but never walk end to end: the fake conn proves what Serve does when a read reports
// an expired deadline, and the transport test proves a socket reports one. This is
// both at once, through the listener, with the session ending the way it ends for a
// player who quits.
func TestServeClosesAnIdleSessionOverRealTCP(t *testing.T) {
	t.Parallel()

	// Equal windows: the handshake is written immediately after the dial, so the only
	// one the test actually waits out is the idle window.
	const window = 300 * time.Millisecond

	tr, err := transport.ListenTCP("127.0.0.1:0")
	if err != nil {
		t.Fatalf("ListenTCP: %v", err)
	}
	t.Cleanup(func() {
		if err := tr.Close(); err != nil {
			t.Errorf("close transport: %v", err)
		}
	})

	chunks, sim, peers := serveDeps(t)
	served := make(chan error, 1)
	go func() {
		conn, aErr := tr.Accept()
		if aErr != nil {
			served <- aErr
			return
		}
		defer conn.Close() //nolint:errcheck // test teardown
		served <- session.Serve(context.Background(), conn, serveConfig(),
			session.Timeouts{Handshake: window, Idle: window}, chunks, sim, peers, ephemeralIdentities(), 12, discard())
	}()

	client, err := net.Dial("tcp", tr.Addr())
	if err != nil {
		t.Fatalf("dial %s: %v", tr.Addr(), err)
	}
	defer client.Close() //nolint:errcheck // test teardown

	w := bufio.NewWriter(client)
	if err := transport.WriteFrame(w, hello(1)); err != nil {
		t.Fatalf("WriteFrame: %v", err)
	}
	if err := w.Flush(); err != nil {
		t.Fatalf("flush: %v", err)
	}

	// The welcome is what proves the handshake window was met, so anything that ends
	// the session after this point ended it for being idle and not for being late.
	if err := client.SetReadDeadline(time.Now().Add(5 * time.Second)); err != nil {
		t.Fatalf("SetReadDeadline: %v", err)
	}
	frame, err := transport.ReadFrame(bufio.NewReader(client))
	if err != nil {
		t.Fatalf("ReadFrame: %v", err)
	}
	if got := vnet.GetRootAsEnvelope(frame, 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("reply is %s, want %s", got, vnet.PayloadServerWelcome)
	}

	// And now the client says nothing at all, which is what a dead connection looks
	// like from here.
	select {
	case err := <-served:
		if err != nil {
			t.Fatalf("Serve returned %v, want nil for an idle session", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Serve did not return; the idle deadline never closed the session")
	}

	if got := sim.Count(); got != 0 {
		t.Errorf("simulation holds %d players after the idle session ended, want 0", got)
	}
}
