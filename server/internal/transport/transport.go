// Package transport carries length-prefixed frames between the server and its
// clients. It knows what a frame is and nothing about what a frame means:
// decoding bytes into messages belongs to package protocol, and deciding
// anything at all belongs to the game.
//
// Everything above this package depends on the two interfaces below rather than
// on TCP. That is the point of declaring them while there is exactly one
// implementation: replacing TCP with QUIC becomes a new type in this package and
// no change anywhere else.
package transport

import "time"

// Conn is one framed, bidirectional connection to a client.
//
// A Conn tolerates one reader and one writer running concurrently, which is how
// a session uses it. Two concurrent writers are a bug even where the
// implementation survives them: two payloads would interleave their bytes and
// the peer would decode neither.
type Conn interface {
	// ReadFrame returns the next frame's payload, blocking until one arrives.
	// The returned slice belongs to the caller.
	ReadFrame() ([]byte, error)

	// WriteFrame sends payload as a single frame.
	WriteFrame(payload []byte) error

	// RemoteAddr describes the peer for logging. It is never an identity: the
	// server assigns those.
	RemoteAddr() string

	// SetReadDeadline bounds how long the reads after it may block: a ReadFrame
	// still waiting when t passes fails with an error IsTimeout reports, and the
	// zero Time clears the deadline again.
	//
	// It belongs on this interface rather than on the TCP type because the question
	// it answers — how long may a client say nothing — is asked one layer up, by the
	// side that deliberately does not know there is a socket. Answering it there
	// would mean type-asserting for net.Conn, which is the assumption this interface
	// exists to remove.
	//
	// An expired deadline ends the connection. A Conn whose read has timed out is
	// not read again: a frame may be half-consumed, and there is no way to
	// resynchronise a stream whose framing is no longer trusted.
	SetReadDeadline(t time.Time) error

	// Close releases the connection. It is safe to call more than once, because
	// shutdown legitimately races a session that is closing itself.
	Close() error
}

// Transport accepts framed connections.
type Transport interface {
	// Accept blocks until a connection arrives, or returns an error once the
	// transport is closed — which is how a shutdown unblocks an accept loop.
	Accept() (Conn, error)

	// Addr is the address actually bound, which matters when the requested port
	// was 0.
	Addr() string

	Close() error
}
