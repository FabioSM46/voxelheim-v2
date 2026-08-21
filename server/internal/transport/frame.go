package transport

import (
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"syscall"
)

// MaxFrameSize is the largest payload one frame may carry.
//
// The limit exists so that it can be checked *before* allocating. The length
// prefix arrives from the network, so an unchecked prefix is an attacker-chosen
// allocation: four bytes asking for four gigabytes. 1 MiB leaves ample headroom
// for the largest message the contract can produce — an RLE-encoded 32³ chunk is
// a few kilobytes — while keeping a hostile prefix free to refuse.
const MaxFrameSize = 1 << 20

// frameHeaderSize is the width of the big-endian length prefix.
const frameHeaderSize = 4

var (
	// ErrFrameTooLarge means the frame's declared size exceeds MaxFrameSize.
	ErrFrameTooLarge = errors.New("transport: frame exceeds maximum size")

	// ErrEmptyFrame means the frame declared zero bytes of payload. No valid
	// Envelope is empty, so an empty frame is malformed rather than idle.
	ErrEmptyFrame = errors.New("transport: frame is empty")
)

// IsDisconnect reports whether err means "this connection ended" rather than
// "something went wrong".
//
// It exists so that callers can tell a routine disconnect from a fault without
// importing net and matching on transport-specific errors — the point of the Conn
// interface is that nothing above this package knows there is a socket involved.
// A half-read frame counts as a disconnect: a peer that vanishes mid-frame has
// disconnected rudely, not corrupted the protocol.
//
// So does an expired read deadline, and that is a decision rather than a
// convenience. Nothing but this process arms one, so its expiry is this side
// concluding the connection is over — the same outcome as a peer hanging up,
// reached by a different route. A caller that needs to tell the two apart asks
// IsTimeout first, which is what a session does in order to log *why* it ended;
// a caller that only needs "is this connection finished" gets the right answer
// without learning there is a clock involved.
func IsDisconnect(err error) bool {
	return errors.Is(err, io.EOF) ||
		errors.Is(err, io.ErrUnexpectedEOF) ||
		errors.Is(err, net.ErrClosed) ||
		errors.Is(err, syscall.ECONNRESET) ||
		errors.Is(err, syscall.EPIPE) ||
		IsTimeout(err)
}

// IsTimeout reports whether err means "a deadline we armed expired".
//
// Not a third question, but the narrower half of the first one: IsDisconnect
// answers "did this connection end", and this answers "did it end because nobody
// said anything". Only a caller that reports the reason needs it.
func IsTimeout(err error) bool {
	return errors.Is(err, os.ErrDeadlineExceeded)
}

// IsClosed reports whether err means "we closed this transport or connection".
//
// Deliberately narrower than IsDisconnect: that one answers "did the peer go
// away", this one answers "did we shut it down". An accept loop needs the second
// question, because a closed listener is the end of the loop while any other
// failure is something to retry.
func IsClosed(err error) bool {
	return errors.Is(err, net.ErrClosed)
}

// WriteFrame writes payload as one length-prefixed frame.
//
// Callers pass a buffered writer and flush it: the header and the payload are
// two Write calls, which unbuffered would be two packets carrying one frame.
func WriteFrame(w io.Writer, payload []byte) error {
	switch {
	case len(payload) == 0:
		return ErrEmptyFrame
	case len(payload) > MaxFrameSize:
		return fmt.Errorf("%w: %d > %d", ErrFrameTooLarge, len(payload), MaxFrameSize)
	}

	var header [frameHeaderSize]byte
	binary.BigEndian.PutUint32(header[:], uint32(len(payload)))
	if _, err := w.Write(header[:]); err != nil {
		return fmt.Errorf("transport: write frame header: %w", err)
	}
	if _, err := w.Write(payload); err != nil {
		return fmt.Errorf("transport: write frame payload: %w", err)
	}
	return nil
}

// ReadFrame reads one length-prefixed frame.
//
// The returned slice is freshly allocated, so a caller may hold on to it.
// Reusing a scratch buffer would save one allocation on a path that carries
// handshakes and 20 Hz input frames — tens of bytes each — and would buy that
// with a slice that silently changes contents under anyone who kept it. The
// traffic worth optimising travels the other way.
func ReadFrame(r io.Reader) ([]byte, error) {
	var header [frameHeaderSize]byte
	if _, err := io.ReadFull(r, header[:]); err != nil {
		return nil, fmt.Errorf("transport: read frame header: %w", err)
	}

	// Both checks precede the allocation below. That ordering is the security
	// property, not an optimisation.
	size := binary.BigEndian.Uint32(header[:])
	switch {
	case size == 0:
		return nil, ErrEmptyFrame
	case size > MaxFrameSize:
		return nil, fmt.Errorf("%w: %d > %d", ErrFrameTooLarge, size, MaxFrameSize)
	}

	payload := make([]byte, size)
	if _, err := io.ReadFull(r, payload); err != nil {
		return nil, fmt.Errorf("transport: read frame payload: %w", err)
	}
	return payload, nil
}
