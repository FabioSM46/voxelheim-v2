package transport

import (
	"bufio"
	"fmt"
	"net"
	"sync"
	"time"
)

// connBufferSize sizes both per-connection buffers. It is comfortably larger
// than an encoded chunk, so a chunk frame leaves in one flush.
const connBufferSize = 64 << 10

type tcpTransport struct {
	ln net.Listener
}

// ListenTCP starts a TCP transport on addr. An addr ending in ":0" binds a free
// port, which Addr then reports.
func ListenTCP(addr string) (Transport, error) {
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		return nil, fmt.Errorf("transport: listen on %q: %w", addr, err)
	}
	return &tcpTransport{ln: ln}, nil
}

func (t *tcpTransport) Accept() (Conn, error) {
	c, err := t.ln.Accept()
	if err != nil {
		return nil, fmt.Errorf("transport: accept: %w", err)
	}
	return newFramedConn(c), nil
}

func (t *tcpTransport) Addr() string { return t.ln.Addr().String() }

func (t *tcpTransport) Close() error {
	if err := t.ln.Close(); err != nil {
		return fmt.Errorf("transport: close listener: %w", err)
	}
	return nil
}

// framedConn is the length-prefixed framing over any net.Conn.
//
// Named for what it does rather than for the socket underneath, because it now has
// two callers: [ListenTCP] hands it a bare socket and [ListenTLS] hands it a
// *tls.Conn. Everything below is written against net.Conn and needs no part in
// telling them apart — which is the reason encrypting the wire cost this file a
// rename and nothing else.
type framedConn struct {
	c net.Conn
	r *bufio.Reader

	// mu guards w. A session owns a single writer goroutine, so this is not a
	// licence for concurrent writers: it is there so that a future caller who
	// ignores that rule gets serialised frames rather than two payloads
	// interleaved on the wire.
	mu sync.Mutex
	w  *bufio.Writer

	closeOnce sync.Once
	closeErr  error
}

func newFramedConn(c net.Conn) *framedConn {
	return &framedConn{
		c: c,
		r: bufio.NewReaderSize(c, connBufferSize),
		w: bufio.NewWriterSize(c, connBufferSize),
	}
}

func (t *framedConn) ReadFrame() ([]byte, error) { return ReadFrame(t.r) }

func (t *framedConn) WriteFrame(payload []byte) error {
	t.mu.Lock()
	defer t.mu.Unlock()

	if err := WriteFrame(t.w, payload); err != nil {
		return err
	}
	if err := t.w.Flush(); err != nil {
		return fmt.Errorf("transport: flush frame: %w", err)
	}
	return nil
}

// SetReadDeadline forwards to the socket. The buffered reader in front of it needs
// no part in this: a deadline that expires mid-frame may leave bytes in that buffer,
// and nothing reads them, because an expired deadline ends the connection.
//
// **Through TLS it is still the socket's deadline, and that is the right one.** A
// TLS record boundary is not a frame boundary, so an expiry can land with a record
// half-read — and crypto/tls answers exactly as this interface already requires: the
// connection is finished, and no later read on it can be trusted. The rule above
// ("a Conn whose read has timed out is not read again") was written for the buffered
// reader and covers the TLS layer unchanged.
func (t *framedConn) SetReadDeadline(at time.Time) error {
	if err := t.c.SetReadDeadline(at); err != nil {
		return fmt.Errorf("transport: set read deadline: %w", err)
	}
	return nil
}

func (t *framedConn) RemoteAddr() string { return t.c.RemoteAddr().String() }

// Close is idempotent. Shutdown closes every registered connection while each
// session is closing its own, so a second Close is an expected race rather than
// an error worth reporting twice.
func (t *framedConn) Close() error {
	t.closeOnce.Do(func() {
		if err := t.c.Close(); err != nil {
			t.closeErr = fmt.Errorf("transport: close conn: %w", err)
		}
	})
	return t.closeErr
}
