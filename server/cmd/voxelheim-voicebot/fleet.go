package main

import (
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"encoding/hex"
	"errors"
	"fmt"
	"sync/atomic"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
)

// What every synthetic session shares, and the two edges that decide which frames a run is
// about.

// defaultTickRate is the rate the sessions heartbeat at and the rate a run asks the server
// for. It is game.DefaultTickRate's value, restated rather than imported because what this
// command needs is a default for a flag it passes to another process; a server started at
// another rate is still measured correctly.
const defaultTickRate = 20

// fleet is everything the bots share.
type fleet struct {
	opts         options
	pair         *ticket.Pair
	world        ticket.WorldID
	addr         string
	fingerprint  string
	template     []byte
	tickInterval time.Duration

	// ticket, when set, is presented by every session instead of one minted here. It is
	// how the probe joins a server whose signing key belongs to somebody else.
	ticket []byte

	// measuring gates every counter in the run. One atomic read per received frame, which
	// is cheaper than any of the alternatives and is what makes the window a window rather
	// than a subtraction of two snapshots taken at unknown moments on a thousand goroutines.
	measuring atomic.Bool

	// The window's own edges, in Unix nanoseconds, and the reason a frame's membership of
	// it is decided by the frame rather than by the clock at either end.
	//
	// **A relayed frame arrives after it was sent, so a window that opened and closed on
	// receipts would not hold the same frames as one that opened and closed on sends.** At
	// the leading edge the listener counts arrivals of frames the speaker did not count
	// sending; at the trailing edge the speaker counts frames whose arrivals fall outside.
	// Both are small and neither is zero: the first run of this command reported 100.134%
	// of the frames it owed delivered, which is not a number an ADR should carry.
	//
	// The send instant is already in the frame — it is what the latency is measured from —
	// so both ends decide membership from the same value and the two counts describe
	// exactly the same set of frames.
	windowStart atomic.Int64
	windowEnd   atomic.Int64
}

// inWindow says whether a frame sent at this instant belongs to the measured window.
func (f *fleet) inWindow(at time.Time) bool {
	nanos := at.UnixNano()
	return nanos >= f.windowStart.Load() && nanos < f.windowEnd.Load()
}

// tlsConfig pins the certificate the server logged rather than trusting a chain there is
// none of.
//
// The server mints its own certificate and announces its SHA-256 at startup; the real
// client is told the same value by the account service. InsecureSkipVerify turns off the
// name and chain checks that have nothing to verify here, and VerifyPeerCertificate is
// what replaces them — the pin is the whole of the check, not an addition to it.
func (f *fleet) tlsConfig() *tls.Config {
	return &tls.Config{
		InsecureSkipVerify: true, //nolint:gosec // the fingerprint below is the check.
		MinVersion:         tls.VersionTLS13,
		VerifyPeerCertificate: func(rawCerts [][]byte, _ [][]*x509.Certificate) error {
			if len(rawCerts) == 0 {
				return errors.New("the server presented no certificate")
			}
			sum := sha256.Sum256(rawCerts[0])
			if got := hex.EncodeToString(sum[:]); got != f.fingerprint {
				return fmt.Errorf("the server's certificate is %s, not the %s it announced", got, f.fingerprint)
			}
			return nil
		},
	}
}

// ticketFor is the credential one session presents.
//
// **Every session needs a distinct account**, because the server admits one live session
// per account and answers the second with ALREADY_CONNECTED. The account is derived from
// the session's name rather than counted alongside it, so the two cannot drift apart, and
// it is hashed rather than packed so an id is the full sixteen bytes whatever the name is.
//
// A fleet given a ticket already presents that one instead: the probe joins a server whose
// signing key this process does not have.
func (f *fleet) ticketFor(name string) ([]byte, error) {
	if f.ticket != nil {
		return f.ticket, nil
	}
	sum := sha256.Sum256([]byte(name))
	var account ticket.AccountID
	copy(account[:], sum[:])
	if account.IsZero() {
		return nil, fmt.Errorf("the account id derived from %q is zero", name)
	}
	minted, _, err := f.pair.Mint(account, f.world, time.Now())
	if err != nil {
		return nil, fmt.Errorf("mint a ticket: %w", err)
	}
	return minted[:], nil
}
