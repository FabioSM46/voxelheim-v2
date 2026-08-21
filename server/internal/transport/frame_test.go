package transport

import (
	"bytes"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"syscall"
	"testing"
	"testing/iotest"
)

func TestFrameRoundTrip(t *testing.T) {
	t.Parallel()

	for _, size := range []int{1, 7, 1024, MaxFrameSize} {
		payload := bytes.Repeat([]byte{0xAB}, size)

		var buf bytes.Buffer
		if err := WriteFrame(&buf, payload); err != nil {
			t.Fatalf("WriteFrame(%d bytes): %v", size, err)
		}
		if got, want := buf.Len(), size+frameHeaderSize; got != want {
			t.Errorf("framed length = %d, want %d", got, want)
		}

		got, err := ReadFrame(&buf)
		if err != nil {
			t.Fatalf("ReadFrame(%d bytes): %v", size, err)
		}
		if !bytes.Equal(got, payload) {
			t.Errorf("payload of %d bytes did not survive the round trip", size)
		}
		if buf.Len() != 0 {
			t.Errorf("%d bytes left unread after the frame", buf.Len())
		}
	}
}

func TestReadFrameSplitAcrossReads(t *testing.T) {
	t.Parallel()

	payload := []byte("a frame that arrives one byte at a time")
	var buf bytes.Buffer
	if err := WriteFrame(&buf, payload); err != nil {
		t.Fatalf("WriteFrame: %v", err)
	}

	// OneByteReader is the pathological case of a TCP stream delivering the
	// header and payload in fragments — the failure this codec exists to absorb.
	got, err := ReadFrame(iotest.OneByteReader(&buf))
	if err != nil {
		t.Fatalf("ReadFrame: %v", err)
	}
	if !bytes.Equal(got, payload) {
		t.Errorf("got %q, want %q", got, payload)
	}
}

func TestReadFrameRejectsOversizedPrefixBeforeAllocating(t *testing.T) {
	t.Parallel()

	// Header only: a declared size just past the limit, and then nothing. If the
	// size check ran after the read, this would fail with an unexpected EOF (or,
	// worse, allocate the declared size first). ErrFrameTooLarge is the proof
	// that the limit is enforced on the prefix alone.
	var header [frameHeaderSize]byte
	binary.BigEndian.PutUint32(header[:], MaxFrameSize+1)

	_, err := ReadFrame(bytes.NewReader(header[:]))
	if !errors.Is(err, ErrFrameTooLarge) {
		t.Fatalf("err = %v, want ErrFrameTooLarge", err)
	}
	if errors.Is(err, io.ErrUnexpectedEOF) {
		t.Error("the reader was consulted for the payload despite an over-limit prefix")
	}
}

func TestFrameEdgeCases(t *testing.T) {
	t.Parallel()

	t.Run("read rejects a zero-length frame", func(t *testing.T) {
		t.Parallel()
		var header [frameHeaderSize]byte // declared size 0
		if _, err := ReadFrame(bytes.NewReader(header[:])); !errors.Is(err, ErrEmptyFrame) {
			t.Fatalf("err = %v, want ErrEmptyFrame", err)
		}
	})

	t.Run("write rejects an empty payload", func(t *testing.T) {
		t.Parallel()
		if err := WriteFrame(io.Discard, nil); !errors.Is(err, ErrEmptyFrame) {
			t.Fatalf("err = %v, want ErrEmptyFrame", err)
		}
	})

	t.Run("write rejects an oversized payload", func(t *testing.T) {
		t.Parallel()
		err := WriteFrame(io.Discard, make([]byte, MaxFrameSize+1))
		if !errors.Is(err, ErrFrameTooLarge) {
			t.Fatalf("err = %v, want ErrFrameTooLarge", err)
		}
	})

	t.Run("read reports a clean disconnect as EOF", func(t *testing.T) {
		t.Parallel()
		// A peer that closes between frames is not an error condition; callers
		// distinguish it with errors.Is, so the wrapping must preserve it.
		if _, err := ReadFrame(bytes.NewReader(nil)); !errors.Is(err, io.EOF) {
			t.Fatalf("err = %v, want io.EOF", err)
		}
	})

	t.Run("read reports a truncated payload", func(t *testing.T) {
		t.Parallel()
		var header [frameHeaderSize]byte
		binary.BigEndian.PutUint32(header[:], 16)
		truncated := append(header[:], []byte("only 8..")...)
		if _, err := ReadFrame(bytes.NewReader(truncated)); !errors.Is(err, io.ErrUnexpectedEOF) {
			t.Fatalf("err = %v, want io.ErrUnexpectedEOF", err)
		}
	})
}

// An expired deadline is answered by both predicates, and the pair is the design:
// IsTimeout says why the connection ended, IsDisconnect says that it did. A caller
// that only needs the second answer must not have to learn there is a clock in order
// to end cleanly, and a caller that reports the reason must be able to tell the two
// apart.
func TestDeadlinePredicates(t *testing.T) {
	t.Parallel()

	// Wrapped the way ReadFrame wraps it, because a wrapped error is the only shape a
	// caller ever sees. A test against the bare sentinel would pass for a predicate
	// written with == , which is the bug errorlint is enabled for.
	expired := fmt.Errorf("transport: read frame header: %w", os.ErrDeadlineExceeded)

	if !IsTimeout(expired) {
		t.Errorf("IsTimeout(%v) = false, want true", expired)
	}
	if !IsDisconnect(expired) {
		t.Errorf("IsDisconnect(%v) = false, want true; an expired deadline ends the connection", expired)
	}

	// IsClosed stays where it was. It answers "did we shut this down", and a deadline
	// expiring is not a shutdown: an accept loop that treated it as one would stop
	// accepting because one client went quiet.
	if IsClosed(expired) {
		t.Errorf("IsClosed(%v) = true, want false", expired)
	}

	// And the narrow predicate stays narrow: every other way a connection ends is not
	// a timeout, or the reason a session logs would be wrong for all of them.
	for _, err := range []error{io.EOF, io.ErrUnexpectedEOF, net.ErrClosed, syscall.ECONNRESET, syscall.EPIPE} {
		if IsTimeout(err) {
			t.Errorf("IsTimeout(%v) = true, want false", err)
		}
	}
}
