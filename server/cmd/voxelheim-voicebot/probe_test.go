package main

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
)

func TestTheProbeRefusesWhatItCannotJoinWith(t *testing.T) {
	t.Parallel()

	good := []string{
		"-probe", "-addr", "127.0.0.1:7777",
		"-fingerprint", strings.Repeat("ab", 32),
		"-ticket-file", "/dev/null",
	}
	if _, err := parseFlags("test", good); err != nil {
		t.Fatalf("a complete probe invocation was refused: %v", err)
	}

	cases := []struct {
		name string
		args []string
		says string
	}{
		{"no address", []string{"-probe", "-ticket-file", "/dev/null", "-fingerprint", strings.Repeat("ab", 32)}, "needs -addr"},
		{"no ticket", []string{"-probe", "-addr", "127.0.0.1:1", "-fingerprint", strings.Repeat("ab", 32)}, "needs -ticket-file"},
		{"a short fingerprint", []string{"-probe", "-addr", "127.0.0.1:1", "-ticket-file", "/dev/null", "-fingerprint", "abcd"}, "64 hex characters"},
		{"a fingerprint that is not hex", []string{"-probe", "-addr", "127.0.0.1:1", "-ticket-file", "/dev/null", "-fingerprint", strings.Repeat("zz", 32)}, "64 hex characters"},
		{"no wait at all", append(append([]string{}, good...), "-probe-wait", "0s"), "-probe-wait must be positive"},
	}
	for _, c := range cases {
		if _, err := parseFlags("test", c.args); err == nil {
			t.Errorf("%s was accepted", c.name)
		} else if !strings.Contains(err.Error(), c.says) {
			t.Errorf("%s was refused with %q, which does not say %q", c.name, err, c.says)
		}
	}
}

func TestTheProbeReadsATicketOutOfTheClientsCacheRecord(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	record := filepath.Join(dir, "world-ticket")
	// ticket.Size bytes of ticket and eight of the expiry the client keeps for itself.
	raw := make([]byte, ticket.Size+8)
	for i := range raw {
		raw[i] = byte(i)
	}
	if err := os.WriteFile(record, raw, 0o600); err != nil {
		t.Fatalf("write the record: %v", err)
	}
	got, err := readTicketFile(record)
	if err != nil {
		t.Fatalf("readTicketFile: %v", err)
	}
	if len(got) != ticket.Size {
		t.Errorf("the probe read %d bytes, want a ticket's %d", len(got), ticket.Size)
	}
	if !bytes.Equal(got, raw[:ticket.Size]) {
		t.Error("the probe read something other than the front of the record")
	}

	short := filepath.Join(dir, "short")
	if err := os.WriteFile(short, raw[:ticket.Size-1], 0o600); err != nil {
		t.Fatalf("write the short record: %v", err)
	}
	if _, err := readTicketFile(short); err == nil {
		t.Error("a record too short to hold a ticket was accepted")
	}
}

// **The probe's whole answer is this hand-off**, and the read loop it comes out of is
// exercised by every run this command makes — so what is left to pin is the one thing only
// the probe uses: that a relayed frame reaches the channel, that it carries the frame's
// length rather than its bytes, and that a soak, whose channel is nil, pays nothing for it.
func TestARelayedFrameReachesTheProbeAndCarriesNoPayload(t *testing.T) {
	t.Parallel()

	const opusBytes = 96
	opus := make([]byte, opusBytes)
	for i := range opus {
		opus[i] = byte(i)
	}
	frame := protocol.EncodeVoiceHeard(protocol.VoiceHeard{
		SpeakerEntityID: 7,
		Sequence:        11,
		Opus:            opus,
	})

	f := &fleet{}
	f.measuring.Store(true)
	b := &bot{fleet: f, firstVoice: make(chan relayed, 1)}
	b.absorbVoice(vnet.GetRootAsEnvelope(frame, 0))

	select {
	case heard := <-b.firstVoice:
		if heard.speaker != 7 || heard.sequence != 11 || heard.opusBytes != opusBytes {
			t.Errorf("the probe was handed %+v, want speaker 7, sequence 11, %d bytes", heard, opusBytes)
		}
	default:
		t.Fatal("no frame reached the probe's channel")
	}

	// A soak sets no channel, and absorbing a frame must not be a nil-channel send.
	quiet := &bot{fleet: f}
	quiet.absorbVoice(vnet.GetRootAsEnvelope(frame, 0))
}
