package transport_test

import (
	"bytes"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"errors"
	"math/big"
	"net"
	"testing"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/transport"
)

// testCertificate is a throwaway self-signed pair.
//
// Generated here rather than imported from internal/certs, deliberately: this package
// imports nothing of ours and the test that proves the transport works must not be the
// thing that breaks that. What is under test is the framing over TLS, and any valid
// certificate exercises it.
func testCertificate(t *testing.T) tls.Certificate {
	t.Helper()

	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatalf("generating a key: %v", err)
	}
	template := x509.Certificate{
		SerialNumber:          big.NewInt(1),
		Subject:               pkix.Name{CommonName: "voxelheim-test"},
		NotBefore:             time.Now().Add(-time.Hour),
		NotAfter:              time.Now().Add(time.Hour),
		KeyUsage:              x509.KeyUsageDigitalSignature | x509.KeyUsageCertSign,
		BasicConstraintsValid: true,
		IsCA:                  true,
		IPAddresses:           []net.IP{net.IPv4(127, 0, 0, 1), net.IPv6loopback},
	}
	der, err := x509.CreateCertificate(rand.Reader, &template, &template, &key.PublicKey, key)
	if err != nil {
		t.Fatalf("signing: %v", err)
	}
	return tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key}
}

// dialled is a client connection, or the reason there is not one.
//
// A struct on a channel rather than a t.Fatal inside the dialling goroutine, and the
// distinction is not style: t.Fatal outside the test goroutine calls runtime.Goexit,
// which kills that goroutine *silently* and leaves the test blocked on a read that will
// never be answered. What looks like a hang is then a failure with its message thrown
// away — which is exactly what happened while this file was being written.
type dialled struct {
	conn *tls.Conn
	err  error
}

// dialTLS starts a client on the encrypted transport, trusting whatever it is handed.
//
// InsecureSkipVerify, and it is correct here for the same reason the real client uses a
// custom verifier: there is no CA and no hostname to check, so the default web-PKI
// verifier has nothing to work with. The *client's* pinning is a Rust concern and is
// tested there; what this file is about is whether frames survive the encryption.
//
// **Asynchronous, and it has to be.** tls.Dial completes the handshake before it
// returns, while the server side handshakes lazily inside its first read — which is the
// property TestAcceptDoesNotWaitForTheTLSHandshake exists to pin. Dialling inline would
// therefore deadlock against an Accept the test has not reached yet.
//
// # It takes a *testing.T so that the connection cannot be collected mid-test
//
// A net.Conn carries a runtime finalizer that closes its socket
// (net/fd_posix.go: `runtime.SetFinalizer(fd, (*netFD).Close)`), so a client the test
// stops referencing is a client the garbage collector hangs up — and the server sees a
// clean FIN, indistinguishable from a peer that left. A caller that drops the returned
// channel drops the only reference there is: the dialling goroutine sends into a
// buffered channel and returns, so nothing at all points at the connection from the
// moment the handshake completes. That is not hypothetical, it is legacy PR 176 — a read
// expecting its 50ms deadline got EOF after 4ms, on whichever runs a neighbouring test
// happened to allocate enough to trigger a collection, which is why the failure
// followed the *package* and never the test.
//
// The cleanup below is therefore two things at once, and the second is the load-bearing
// one: it closes the connection when the test ends, and until then it is what keeps the
// channel — and so the connection inside it — reachable. Exactly one of this cleanup and
// awaitDial's finds the connection, depending on whether the caller collected it.
func dialTLS(t *testing.T, addr string) <-chan dialled {
	t.Helper()

	out := make(chan dialled, 1)
	go func() {
		conn, err := tls.Dial("tcp", addr, &tls.Config{InsecureSkipVerify: true, MinVersion: tls.VersionTLS13})
		out <- dialled{conn: conn, err: err}
	}()
	t.Cleanup(func() {
		// Non-blocking: a dial still in flight at the end of the test is one nothing
		// can close yet, and waiting for it here would hang the test it is cleaning
		// up after.
		select {
		case result := <-out:
			if result.conn != nil {
				_ = result.conn.Close()
			}
		default:
		}
	})
	return out
}

// awaitDial is the connection dialTLS started. Test-goroutine only, by construction:
// it is the half of the pair that is allowed to call t.Fatal.
func awaitDial(t *testing.T, out <-chan dialled) *tls.Conn {
	t.Helper()

	select {
	case result := <-out:
		if result.err != nil {
			t.Fatalf("the TLS handshake failed: %v", result.err)
		}
		// Closes the connection at the end of the test, and holds the last reference to
		// it until then — see dialTLS on why the second half matters.
		t.Cleanup(func() { _ = result.conn.Close() })
		return result.conn
	case <-time.After(10 * time.Second):
		t.Fatal("the TLS handshake did not complete")
		return nil
	}
}

// A frame in is the same frame out. The encryption is beneath the framing and must be
// invisible to it — same bytes, same boundaries, whatever the TLS record layer did with
// them in between.
func TestTLSCarriesFramesUnchanged(t *testing.T) {
	t.Parallel()

	tr, err := transport.ListenTLS("127.0.0.1:0", testCertificate(t))
	if err != nil {
		t.Fatalf("ListenTLS: %v", err)
	}
	defer func() { _ = tr.Close() }()

	pending := dialTLS(t, tr.Addr())

	server, err := tr.Accept()
	if err != nil {
		t.Fatalf("Accept: %v", err)
	}
	defer func() { _ = server.Close() }()

	// Four payloads chosen for where they fall against a TLS record, which is at most
	// 16 KiB: a short one that fits many times over, one landing exactly on the boundary,
	// one crossing several, and the shortest a frame may be. The empty frame is not here
	// because the framing refuses it — see ErrEmptyFrame, which is a rule of this package
	// rather than of the encryption, and one this test found the hard way.
	payloads := [][]byte{
		[]byte("hello"),
		bytes.Repeat([]byte{0xCD}, 16<<10),
		bytes.Repeat([]byte{0xAB}, 64<<10),
		{0x7F},
	}

	// The server reads in a goroutine because the *client's* handshake completes only
	// when this side reads: the two are one interlocked pair, not a sequence.
	type readResult struct {
		payload []byte
		err     error
	}
	reads := make(chan readResult, len(payloads))
	go func() {
		for range payloads {
			payload, rErr := server.ReadFrame()
			reads <- readResult{payload: payload, err: rErr}
		}
	}()

	client := awaitDial(t, pending)
	for i, payload := range payloads {
		if wErr := transport.WriteFrame(client, payload); wErr != nil {
			t.Fatalf("frame %d: WriteFrame: %v", i, wErr)
		}
	}

	for i, want := range payloads {
		select {
		case got := <-reads:
			if got.err != nil {
				t.Fatalf("frame %d: ReadFrame: %v", i, got.err)
			}
			if !bytes.Equal(got.payload, want) {
				t.Errorf("frame %d came back as %d bytes, want %d", i, len(got.payload), len(want))
			}
		case <-time.After(10 * time.Second):
			t.Fatalf("frame %d never arrived", i)
		}
	}

	// And the other direction, because a Conn is bidirectional and the writer half has
	// its own buffering.
	if wErr := server.WriteFrame([]byte("welcome")); wErr != nil {
		t.Fatalf("WriteFrame: %v", wErr)
	}
	got, rErr := transport.ReadFrame(client)
	if rErr != nil {
		t.Fatalf("reading the reply: %v", rErr)
	}
	if string(got) != "welcome" {
		t.Errorf("the reply came back as %q", got)
	}
}

// idleReadDeadline is how long the silent read below is given. Long enough that a peer
// hanging up is unmistakable next to it — legacy PR 176's EOF landed 4ms into a 50ms window — and
// short enough to keep a parallel test cheap.
const idleReadDeadline = 200 * time.Millisecond

// **The deadline still fires through the TLS layer**, which is the one thing wrapping a
// connection could plausibly have broken. A TLS record boundary is not a frame boundary
// and the deadline is measured on the socket underneath, so this is pinned rather than
// assumed: without it, legacy PR 150's handshake and idle timeouts would silently stop bounding
// anything the moment -tls was passed, and a server would hold a silent connection open
// for ever while every log line looked healthy.
//
// # The handshake finishes before the deadline is armed
//
// What legacy PR 150's idle timeout bounds is the read that comes *after* a handshake, and that
// is what this measures. Arming the deadline first and letting the handshake complete
// underneath it measured the two together: a handshake slow enough to exhaust the window
// on a loaded machine also reports a timeout, so the assertion held either way and could
// not say which of them it had just watched. The write below drives the server's half of
// the handshake to completion — crypto/tls handshakes inside the first Read *or* Write —
// and awaitDial proves the client's, so the deadline that follows bounds nothing but
// silence.
func TestAReadDeadlineStillExpiresThroughTLS(t *testing.T) {
	t.Parallel()

	tr, err := transport.ListenTLS("127.0.0.1:0", testCertificate(t))
	if err != nil {
		t.Fatalf("ListenTLS: %v", err)
	}
	defer func() { _ = tr.Close() }()

	pending := dialTLS(t, tr.Addr())

	server, err := tr.Accept()
	if err != nil {
		t.Fatalf("Accept: %v", err)
	}
	defer func() { _ = server.Close() }()

	if wErr := server.WriteFrame([]byte("ready")); wErr != nil {
		t.Fatalf("WriteFrame: %v", wErr)
	}
	client := awaitDial(t, pending)
	// Drained so that both ends are genuinely quiet when the deadline is armed: a frame
	// left sitting in the client's receive buffer is silence on the wrong side of the
	// connection.
	if _, rErr := transport.ReadFrame(client); rErr != nil {
		t.Fatalf("reading the frame that completes the handshake: %v", rErr)
	}

	// Established, and now left silent — exactly as a connection that says nothing after
	// its handshake does.
	deadline := time.Now().Add(idleReadDeadline)
	if sErr := server.SetReadDeadline(deadline); sErr != nil {
		t.Fatalf("SetReadDeadline: %v", sErr)
	}

	_, rErr := server.ReadFrame()
	returned := time.Now()
	if rErr == nil {
		t.Fatal("a read on a silent connection returned without an error")
	}
	if !transport.IsTimeout(rErr) {
		t.Fatalf("ReadFrame = %v, want a timeout IsTimeout reports", rErr)
	}
	// The *when*, not only the *what*: a read that ends before its own deadline was ended
	// by something else, and the error it carries is then describing that something else.
	// Asserted with a few milliseconds of slack, because what this catches is a peer
	// closing the connection — orders of magnitude early — and not timer precision.
	if early := deadline.Sub(returned); early > 5*time.Millisecond {
		t.Errorf("the read ended %s before its deadline, so the deadline is not what ended it", early)
	}
	if late := returned.Sub(deadline); late > 5*time.Second {
		t.Errorf("the read ran %s past its deadline, so the deadline is not the socket's", late)
	}
}

// The handshake happens on the first read rather than in Accept, which is what keeps a
// stalling peer out of the accept loop and inside the session's own handshake deadline.
//
// Pinned because it is a property of tls.NewListener rather than of this package: a
// future rewrite that handshakes eagerly in Accept would move one client's stalling
// onto every other client's connection time, and nothing else would notice.
func TestAcceptDoesNotWaitForTheTLSHandshake(t *testing.T) {
	t.Parallel()

	tr, err := transport.ListenTLS("127.0.0.1:0", testCertificate(t))
	if err != nil {
		t.Fatalf("ListenTLS: %v", err)
	}
	defer func() { _ = tr.Close() }()

	// A bare TCP connection that never sends a TLS ClientHello: the worst peer the
	// accept loop can meet.
	silent, err := net.Dial("tcp", tr.Addr())
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer func() { _ = silent.Close() }()

	done := make(chan struct{})
	go func() {
		defer close(done)
		conn, aErr := tr.Accept()
		if aErr == nil {
			_ = conn.Close()
		}
	}()

	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("Accept blocked on a peer that never started a TLS handshake")
	}
}

// A transport that has been closed stops accepting, which is how shutdown unblocks the
// accept loop. The same contract ListenTCP keeps, restated for the second implementation
// because the accept loop only ever ends this way.
func TestClosingTheTLSTransportUnblocksAccept(t *testing.T) {
	t.Parallel()

	tr, err := transport.ListenTLS("127.0.0.1:0", testCertificate(t))
	if err != nil {
		t.Fatalf("ListenTLS: %v", err)
	}

	failed := make(chan error, 1)
	go func() {
		_, aErr := tr.Accept()
		failed <- aErr
	}()

	if cErr := tr.Close(); cErr != nil {
		t.Fatalf("Close: %v", cErr)
	}

	select {
	case aErr := <-failed:
		if aErr == nil {
			t.Fatal("Accept returned a connection after the transport was closed")
		}
		if !transport.IsClosed(aErr) {
			t.Errorf("Accept = %v, want an error IsClosed reports", aErr)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Accept did not return after the transport was closed")
	}
}

// A peer speaking plain TCP to an encrypted port is refused rather than admitted, which
// is the downgrade the client's own rule depends on being impossible.
func TestPlainTCPIsNotAdmittedOnTheEncryptedTransport(t *testing.T) {
	t.Parallel()

	tr, err := transport.ListenTLS("127.0.0.1:0", testCertificate(t))
	if err != nil {
		t.Fatalf("ListenTLS: %v", err)
	}
	defer func() { _ = tr.Close() }()

	plain, err := net.Dial("tcp", tr.Addr())
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer func() { _ = plain.Close() }()

	go func() {
		// A well-formed Voxelheim frame, and complete gibberish as far as a TLS record
		// header is concerned.
		_ = transport.WriteFrame(plain, []byte("hello"))
	}()

	server, err := tr.Accept()
	if err != nil {
		t.Fatalf("Accept: %v", err)
	}
	defer func() { _ = server.Close() }()

	if _, rErr := server.ReadFrame(); rErr == nil {
		t.Fatal("a plaintext frame was read off an encrypted transport")
	} else if errors.Is(rErr, nil) {
		t.Fatal("unreachable")
	}
}
