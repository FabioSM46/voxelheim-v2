package main

import (
	"context"
	"encoding/hex"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
)

// The probe: one session that joins a server somebody else started, listens, and says
// whether a voice frame reached it.
//
// **It exists for scripts/interop-check.sh and it is the one thing here that does not start
// its own server.** That check asks a different question — whether a frame a *Rust* client
// encoded arrives at a Go session as a VoiceHeard — and its server is the script's, with a
// ticket key the script generated with openssl. So the probe takes an address, the
// fingerprint the server announced, and a ticket already minted for it.
//
// It says nothing about the frame beyond its length: `internal/game/voice.go` holds the
// line that a voice payload never reaches a diagnostic, and a probe whose job is to prove
// one arrived is exactly where somebody would be tempted to print it.

// probeOptions are the flags the probe needs and the soak does not.
type probeOptions struct {
	addr        string
	fingerprint string
	ticketFile  string
	name        string
	wait        time.Duration
}

func registerProbeFlags(flags *flag.FlagSet, probe *probeOptions) {
	flags.StringVar(&probe.addr, "addr", "",
		"the host:port of a server somebody else started. Required with -probe, and the only "+
			"mode in which this command does not start the server it talks to")
	flags.StringVar(&probe.fingerprint, "fingerprint", "",
		"the SHA-256 the server announced in its `listening with an encrypted session` line, in "+
			"lowercase hex. It is the whole of the certificate check: there is no chain to verify")
	flags.StringVar(&probe.ticketFile, "ticket-file", "",
		"a file whose first bytes are a session ticket minted for this world. The client's own "+
			"cache record is one, and scripts/interop-check.sh writes those already")
	flags.StringVar(&probe.name, "probe-name", "Probe",
		"the character name the probe creates. It has to be one no other session on the world holds")
	flags.DurationVar(&probe.wait, "probe-wait", 30*time.Second,
		"how long to wait for a relayed frame before reporting that none arrived")
}

// validate checks the probe's flags, and does it before a socket is opened so a mistyped
// fingerprint is a sentence rather than a TLS handshake failure.
func (p probeOptions) validate() error {
	switch {
	case p.addr == "":
		return errors.New("-probe needs -addr: it joins a server it did not start")
	case p.ticketFile == "":
		return errors.New("-probe needs -ticket-file: the server it joins was given somebody else's ticket key")
	case p.name == "":
		return errors.New("-probe-name must not be empty; the probe creates a character with it")
	case p.wait <= 0:
		return fmt.Errorf("-probe-wait must be positive, got %v", p.wait)
	}
	return checkFingerprint(p.fingerprint)
}

// runProbe joins once and waits for a relayed frame.
func runProbe(ctx context.Context, probe probeOptions, out io.Writer) error {
	raw, err := readTicketFile(probe.ticketFile)
	if err != nil {
		return err
	}

	f := &fleet{
		addr:         probe.addr,
		fingerprint:  probe.fingerprint,
		tickInterval: time.Second / defaultTickRate,
		ticket:       raw,
	}
	f.measuring.Store(true)

	b := &bot{fleet: f, name: probe.name, joined: make(chan struct{}), firstVoice: make(chan relayed, 1)}
	defer b.close()

	joining, cancelJoin := context.WithTimeout(ctx, probe.wait)
	defer cancelJoin()
	if err := b.join(joining); err != nil {
		return fmt.Errorf("the probe could not join: %w", err)
	}
	l := &lines{to: out}
	l.printf("probe joined as entity %d\n", b.entityID)

	// The probe listens for as long as it is told to, and the server closes a welcomed
	// session that has said nothing for its idle window. speak sends no voice — the probe
	// is a listener and this placement has no speaker flag — so what it contributes is the
	// PlayerInput heartbeat a real client sends every tick.
	listening, stopListening := context.WithTimeout(ctx, probe.wait)
	defer stopListening()
	go b.speak(listening)

	go func() {
		// The read loop's own errors are not the probe's answer: a session that ends is a
		// session that heard nothing, which the select below says in a sentence.
		_ = b.listen(listening)
	}()

	select {
	case heard := <-b.firstVoice:
		// Length, speaker and sequence. Never the bytes: see this file's header.
		l.printf("probe heard a voice frame: speaker entity %d, sequence %d, %d opus bytes\n",
			heard.speaker, heard.sequence, heard.opusBytes)
		return l.err
	case <-listening.Done():
		return errors.New("no voice frame reached the probe before it gave up")
	}
}

// relayed is what the probe reports about the one frame it was waiting for.
type relayed struct {
	speaker   uint64
	sequence  uint32
	opusBytes int
}

// readTicketFile reads a minted ticket out of the record a client caches it in.
//
// The interop check writes the client's own cache record: the ticket's [ticket.Size] bytes
// followed by an expiry the *client* keeps and the server never sees. The probe reads the
// front of it, so the script mints one ticket per session with the code it already has
// rather than growing a second format.
func readTicketFile(path string) ([]byte, error) {
	raw, err := os.ReadFile(path) //nolint:gosec // the path is the operator's own flag.
	if err != nil {
		return nil, fmt.Errorf("read the probe's ticket: %w", err)
	}
	if len(raw) < ticket.Size {
		return nil, fmt.Errorf("the probe's ticket file is %d bytes, and a ticket is %d", len(raw), ticket.Size)
	}
	return raw[:ticket.Size], nil
}

// checkFingerprint refuses a pin that is not the shape the server announces, before a
// connection is opened rather than inside a TLS callback where the error is harder to read.
func checkFingerprint(fingerprint string) error {
	if len(fingerprint) != 2*32 {
		return fmt.Errorf("a certificate fingerprint is 64 hex characters, got %d", len(fingerprint))
	}
	if _, err := hex.DecodeString(fingerprint); err != nil {
		return fmt.Errorf("a certificate fingerprint is 64 hex characters: %w", err)
	}
	return nil
}
