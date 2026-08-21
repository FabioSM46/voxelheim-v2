package transport

import (
	"crypto/tls"
	"fmt"
	"net"
)

// ListenTLS starts a TLS transport on addr, presenting cert.
//
// **The second implementation the package doc promised, and it changes nothing above
// this line.** Conn and Transport are what the rest of the server depends on; this
// adds a type and no caller learns a new word for it beyond which constructor main
// calls.
//
// The framing is not reimplemented — a *tls.Conn is a net.Conn, so it goes through
// [newFramedConn] exactly as an unencrypted socket does. That is the whole of the
// change: the same length-prefixed frames, over bytes nobody in the middle can read.
//
// # The handshake happens on the first read, not here
//
// tls.NewListener's Accept returns as soon as the TCP connection is up; the TLS
// handshake runs lazily inside the first Read or Write. That is the behaviour to
// want. Handshaking in Accept would put an unauthenticated peer's stalling in the
// accept loop, where one slow client delays every other connection; doing it on the
// first read puts it on the session goroutine, inside the handshake deadline the
// session already arms — so a peer that opens a socket and then says nothing is
// closed by the same timeout that already covered a peer who sent no ClientHello.
//
// # What this protects, and what it does not
//
// It protects the bytes in transit, which is what the identity token needs: a token
// on the wire is a credential anyone watching can replay. It says nothing about who
// the client is — there are no client certificates here, because the token *is* the
// client's identity — and nothing about who the server is beyond what the client
// pins for itself, because a self-signed certificate attests only that both ends of
// this connection are talking to the same key.
func ListenTLS(addr string, cert tls.Certificate) (Transport, error) {
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		return nil, fmt.Errorf("transport: listen on %q: %w", addr, err)
	}

	// MinVersion is stated rather than inherited. crypto/tls's default floor has
	// moved with releases and will again; a server whose floor changes under it on a
	// toolchain upgrade is a server whose security properties are decided by a
	// changelog nobody read. 1.3 is the floor because both ends are ours — there is
	// no browser and no legacy client to accommodate, so there is nothing to buy with
	// 1.2 and a downgrade path to lose.
	//
	// Nothing else is configured. Cipher suites, curves and the key exchange are
	// crypto/tls's to choose, and under 1.3 they are not negotiable anyway; a list
	// here would be a hand-rolled opinion that ages badly.
	return &tlsTransport{ln: tls.NewListener(ln, &tls.Config{
		Certificates: []tls.Certificate{cert},
		MinVersion:   tls.VersionTLS13,
	})}, nil
}

type tlsTransport struct {
	ln net.Listener
}

func (t *tlsTransport) Accept() (Conn, error) {
	c, err := t.ln.Accept()
	if err != nil {
		return nil, fmt.Errorf("transport: accept: %w", err)
	}
	return newFramedConn(c), nil
}

func (t *tlsTransport) Addr() string { return t.ln.Addr().String() }

func (t *tlsTransport) Close() error {
	if err := t.ln.Close(); err != nil {
		return fmt.Errorf("transport: close listener: %w", err)
	}
	return nil
}
