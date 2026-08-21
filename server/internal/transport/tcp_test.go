package transport

import (
	"bufio"
	"bytes"
	"net"
	"testing"
	"time"
)

// TestTCPRoundTrip exercises the real listener on a loopback port: the framing
// codec is unit-tested above, so what this covers is the wiring — buffering,
// flushing, and that a frame written by one side arrives whole at the other.
func TestTCPRoundTrip(t *testing.T) {
	t.Parallel()

	tr, err := ListenTCP("127.0.0.1:0")
	if err != nil {
		t.Fatalf("ListenTCP: %v", err)
	}
	t.Cleanup(func() {
		if err := tr.Close(); err != nil {
			t.Errorf("close transport: %v", err)
		}
	})

	accepted := make(chan Conn, 1)
	go func() {
		c, err := tr.Accept()
		if err != nil {
			close(accepted)
			return
		}
		accepted <- c
	}()

	client, err := net.Dial("tcp", tr.Addr())
	if err != nil {
		t.Fatalf("dial %s: %v", tr.Addr(), err)
	}
	defer client.Close() //nolint:errcheck // test teardown

	server, ok := <-accepted
	if !ok {
		t.Fatal("accept failed")
	}
	defer server.Close() //nolint:errcheck // test teardown

	// Client to server.
	up := []byte("hello from the client")
	cw := bufio.NewWriter(client)
	if err := WriteFrame(cw, up); err != nil {
		t.Fatalf("client WriteFrame: %v", err)
	}
	if err := cw.Flush(); err != nil {
		t.Fatalf("client flush: %v", err)
	}
	got, err := server.ReadFrame()
	if err != nil {
		t.Fatalf("server ReadFrame: %v", err)
	}
	if !bytes.Equal(got, up) {
		t.Errorf("server read %q, want %q", got, up)
	}

	// Server to client, through the Conn's own buffered writer and flush.
	down := bytes.Repeat([]byte{0x5A}, 40_000) // larger than one MTU, smaller than the buffer
	if err := server.WriteFrame(down); err != nil {
		t.Fatalf("server WriteFrame: %v", err)
	}
	got, err = ReadFrame(bufio.NewReader(client))
	if err != nil {
		t.Fatalf("client ReadFrame: %v", err)
	}
	if !bytes.Equal(got, down) {
		t.Errorf("client read %d bytes, want %d identical bytes", len(got), len(down))
	}

	if server.RemoteAddr() == "" {
		t.Error("RemoteAddr is empty")
	}
}

func TestTCPConnCloseIsIdempotent(t *testing.T) {
	t.Parallel()

	tr, err := ListenTCP("127.0.0.1:0")
	if err != nil {
		t.Fatalf("ListenTCP: %v", err)
	}
	defer tr.Close() //nolint:errcheck // test teardown

	accepted := make(chan Conn, 1)
	go func() {
		c, err := tr.Accept()
		if err != nil {
			close(accepted)
			return
		}
		accepted <- c
	}()

	client, err := net.Dial("tcp", tr.Addr())
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer client.Close() //nolint:errcheck // test teardown

	server, ok := <-accepted
	if !ok {
		t.Fatal("accept failed")
	}

	if err := server.Close(); err != nil {
		t.Fatalf("first Close: %v", err)
	}
	// Shutdown closes every registered connection while sessions close their own,
	// so the second call is a race the server relies on being harmless.
	if err := server.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}
}

// The deadline on a real socket rather than on a fake one. Forwarding a method is
// only worth anything if what it forwards to behaves the way the interface promises,
// and both halves of that promise are here: an armed deadline ends a read that is
// waiting, and clearing it hands the connection back intact.
func TestTCPConnReadDeadline(t *testing.T) {
	t.Parallel()

	tr, err := ListenTCP("127.0.0.1:0")
	if err != nil {
		t.Fatalf("ListenTCP: %v", err)
	}
	defer tr.Close() //nolint:errcheck // test teardown

	accepted := make(chan Conn, 1)
	go func() {
		c, aErr := tr.Accept()
		if aErr != nil {
			close(accepted)
			return
		}
		accepted <- c
	}()

	client, err := net.Dial("tcp", tr.Addr())
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer client.Close() //nolint:errcheck // test teardown

	server, ok := <-accepted
	if !ok {
		t.Fatal("accept failed")
	}
	defer server.Close() //nolint:errcheck // test teardown

	// A client that connects and says nothing, which is the whole point of the flag.
	const window = 50 * time.Millisecond
	if err := server.SetReadDeadline(time.Now().Add(window)); err != nil {
		t.Fatalf("SetReadDeadline: %v", err)
	}

	start := time.Now()
	if _, err := server.ReadFrame(); !IsTimeout(err) {
		t.Fatalf("ReadFrame err = %v, want an expired deadline", err)
	}
	// Only the lower bound is asserted. A loaded machine may notice late and that is
	// not a defect; returning *early* would mean the deadline was not what ended the
	// read, and the test would be passing on the wrong error.
	if waited := time.Since(start); waited < window {
		t.Errorf("ReadFrame returned after %s, before the %s deadline", waited, window)
	}

	// Cleared, the connection is a connection again: the deadline bounds a read, it
	// does not break the socket. The session never needs this — an expired deadline
	// ends it — but a Conn that could not be handed back would be a trap for the next
	// caller who arms one.
	if err := server.SetReadDeadline(time.Time{}); err != nil {
		t.Fatalf("clearing the deadline: %v", err)
	}

	payload := []byte("late, but not absent")
	w := bufio.NewWriter(client)
	if err := WriteFrame(w, payload); err != nil {
		t.Fatalf("client WriteFrame: %v", err)
	}
	if err := w.Flush(); err != nil {
		t.Fatalf("client flush: %v", err)
	}

	got, err := server.ReadFrame()
	if err != nil {
		t.Fatalf("ReadFrame after clearing the deadline: %v", err)
	}
	if !bytes.Equal(got, payload) {
		t.Errorf("read %q, want %q", got, payload)
	}
}
