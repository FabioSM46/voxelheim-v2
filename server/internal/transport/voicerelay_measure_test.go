package transport_test

// The measurement behind docs/adr/0001-voice-transport.md.
//
// The ADR chooses to relay Opus frames over the TLS stream this package already
// carries, instead of standing up an SFU beside the server. That is a claim about
// timing — a voice frame is worthless late in a way a chunk payload is not — so it
// is measured here rather than argued: 20 ms frames at 50 per second through
// transport.ListenTLS, over loopback, with loss and delay applied by the harness
// before the write.
//
// # Why the harness simulates the network instead of the kernel doing it
//
// `tc netem` is the honest way to shape a link and it needs root, a real interface
// and a machine nobody else is using. None of those are available to a test that
// has to be re-runnable by whoever reads the ADR. Dropping and delaying frames on
// the *sending side* measures the same thing this decision turns on — what the
// jitter buffer downstream of #852 has to absorb — because a frame the harness
// never writes is indistinguishable, at the receiver, from one the network ate.
// What it deliberately does not model is reordering: TCP under TLS cannot deliver
// out of order, and that is a property of the transport being chosen, not a
// simplification of the harness.
//
// # Loss over a stream is two different measurements, and only one of them is easy
//
// A frame the harness never writes is a frame the receiver never sees — which is
// what loss does to a datagram transport, and it is the ladder the ADR was asked
// for. It is *not* what loss does to this one. TCP does not lose a segment, it
// retransmits it, and until the retransmission lands every byte queued behind it
// is held: the frames after the lost one are not late by a few hundred
// microseconds, they arrive in a burst one retransmission timeout later. Loopback
// cannot produce that on its own, because loopback never drops a packet.
//
// So the harness runs the ladder twice. `drop` writes nothing for a lost frame and
// answers "what does a jitter buffer have to cover". `retransmit` holds the lost
// frame for one RTO and lets the single writer deliver everything behind it in the
// burst that follows, and answers the question this decision actually turns on.
// Reporting only the first would have made a relay over TCP look free.
//
// # Why this is an env-gated skip rather than a build tag
//
// The issue offered either. A build tag keeps the file out of `go test ./...` by
// keeping it out of the *build*, which also keeps it out of `go vet`,
// `golangci-lint` and both cross-compilation gates — so the one file nobody
// compiles is the one that rots, and it rots silently until somebody needs the
// number again. Gating on an environment variable costs a skipped test per run and
// buys every gate reading this code on every pull request.

import (
	"encoding/binary"
	"fmt"
	"math/rand"
	"os"
	"sort"
	"testing"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/transport"
)

const (
	// voiceMeasureEnv is the switch. Unset, this file is a skip.
	voiceMeasureEnv = "VOICE_RELAY_MEASURE"

	// voiceMeasureFramesEnv overrides how many frames each condition sends, for
	// somebody re-running the measurement longer than the ADR did.
	voiceMeasureFramesEnv = "VOICE_RELAY_MEASURE_FRAMES"

	// voiceFrameInterval is one Opus frame at the codec's default framing. 50 of
	// them a second is what #852 will send while a key is held.
	voiceFrameInterval = 20 * time.Millisecond

	// voiceFramePayload is the size of one frame on the wire: an Opus packet at
	// roughly 24 kbit/s is about 60 bytes, and the relay of #850 will wrap it in a
	// FlatBuffers envelope with a speaker id and a sequence. 96 bytes is that,
	// rounded up. The number matters less than that it is small — this measurement
	// is about arrival timing, not bandwidth.
	voiceFramePayload = 96

	// voiceMeasureDelay is the one-way delay the harness adds before writing. A
	// constant delay adds no jitter of its own; what it buys is that every frame
	// spends time in the harness's own scheduling, which is where a Go program's
	// timing noise actually lives.
	voiceMeasureDelay = 40 * time.Millisecond

	// voiceMeasureFrames is 10 seconds of continuous speech per condition.
	voiceMeasureFrames = 500

	// voicePrimeByte is the one-byte frame that completes the TLS handshake before
	// the measurement starts. Its value is arbitrary; that it is checked on arrival
	// is not, because a priming frame silently mistaken for frame 0 would shift
	// every sequence number after it.
	voicePrimeByte = 0x7F

	// voiceMeasureSeed fixes which frames are lost, so two runs of the same
	// condition are comparable and the ADR's numbers can be reproduced rather than
	// merely re-measured.
	voiceMeasureSeed = 1

	// voiceRetransmitTimeout is how long a lost frame is held before the harness
	// writes it anyway, standing in for a TCP retransmission. 200 ms is Linux's
	// TCP_RTO_MIN, which is the floor a 40 ms path lands on: RFC 6298 computes
	// RTT + 4·RTTVAR and then clamps, and on a link this quiet the clamp always
	// wins. It is the optimistic end of the range — a second loss of the same
	// segment doubles it — and the ADR is set from the optimistic end deliberately,
	// because a decision that fails on the best case does not need the worst one.
	voiceRetransmitTimeout = 200 * time.Millisecond
)

// voiceMeasureLoss is the loss ladder the ADR reports, as fractions.
var voiceMeasureLoss = []float64{0, 0.02, 0.05}

// voiceLossModel is what the harness does with a frame the network took.
type voiceLossModel int

const (
	// voiceLossDrop never writes the frame. The datagram answer.
	voiceLossDrop voiceLossModel = iota

	// voiceLossRetransmit writes it one RTO late, with everything behind it. The
	// stream answer, and the one this transport would actually give.
	voiceLossRetransmit
)

func (m voiceLossModel) String() string {
	if m == voiceLossRetransmit {
		return "retransmit"
	}
	return "drop"
}

// voiceArrival is one frame that reached the client.
type voiceArrival struct {
	sequence uint32
	at       time.Time
}

// TestVoiceRelayJitterOverTLS measures what a voice relay over this transport
// costs in arrival timing. It is a measurement, not an assertion: it fails only
// when the harness itself breaks, and reports otherwise.
//
// Run it with:
//
//	cd server && VOICE_RELAY_MEASURE=1 go test ./internal/transport \
//	    -run TestVoiceRelayJitterOverTLS -v -timeout 5m
func TestVoiceRelayJitterOverTLS(t *testing.T) {
	if os.Getenv(voiceMeasureEnv) == "" {
		t.Skipf("measurement harness for docs/adr/0001-voice-transport.md; set %s=1 to run it", voiceMeasureEnv)
	}

	frames := voiceMeasureFrames
	if raw := os.Getenv(voiceMeasureFramesEnv); raw != "" {
		parsed, err := parsePositiveInt(raw)
		if err != nil {
			t.Fatalf("%s=%q: %v", voiceMeasureFramesEnv, raw, err)
		}
		frames = parsed
	}

	t.Logf("%d frames of %d bytes every %v, %v one-way delay, seed %d",
		frames, voiceFramePayload, voiceFrameInterval, voiceMeasureDelay, voiceMeasureSeed)

	for _, model := range []voiceLossModel{voiceLossDrop, voiceLossRetransmit} {
		for _, loss := range voiceMeasureLoss {
			name := fmt.Sprintf("%s/loss_%.0f%%", model, loss*100)
			t.Run(name, func(t *testing.T) {
				arrivals, sent := measureVoiceRelay(t, frames, loss, model)
				reportVoiceRelay(t, model, loss, frames, sent, arrivals)
			})
		}
	}
}

// measureVoiceRelay runs one condition and returns every arrival, with how many
// frames were actually written.
func measureVoiceRelay(t *testing.T, frames int, loss float64, model voiceLossModel) ([]voiceArrival, int) {
	t.Helper()

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

	// The handshake is completed before the clock starts, and it has to be done
	// this way round: tls.Dial finishes the client's half before it returns, while
	// the server handshakes lazily inside its first read or write. Dialling and then
	// waiting would deadlock, and letting the measurement's own first frame drive the
	// handshake would charge the handshake to that frame's arrival time — a hundred
	// milliseconds of key exchange reported as jitter, in the one row that is
	// supposed to be the quiet baseline.
	primed := make(chan error, 1)
	go func() { primed <- server.WriteFrame([]byte{voicePrimeByte}) }()

	client := awaitDial(t, pending)

	prime, err := transport.ReadFrame(client)
	if err != nil {
		t.Fatalf("priming the connection: %v", err)
	}
	if len(prime) != 1 || prime[0] != voicePrimeByte {
		t.Fatalf("the priming frame came back as %v", prime)
	}
	if err := <-primed; err != nil {
		t.Fatalf("priming the connection: %v", err)
	}

	// Which frames the network takes, decided up front from a fixed seed so that the
	// two models see exactly the same losses and their columns can be read against
	// each other. Under `drop` a lost frame is never written and its sequence number
	// simply never arrives; under `retransmit` every frame is written and the loss
	// shows up as when.
	lost := make([]bool, frames)
	sent := 0
	// math/rand and a fixed seed, deliberately: what is wanted here is a loss pattern
	// two runs agree on, which is the opposite of what a cryptographic source offers.
	rng := rand.New(rand.NewSource(voiceMeasureSeed))
	for i := range lost {
		if rng.Float64() < loss {
			lost[i] = true
			if model == voiceLossDrop {
				continue
			}
		}
		sent++
	}

	// One writer, and it must stay one: the Conn contract tolerates a single writer,
	// and two would interleave their bytes into frames the peer cannot decode. The
	// delay is therefore a sleep inside this goroutine against an absolute release
	// time, never a timer per frame.
	writeErr := make(chan error, 1)
	go func() {
		payload := make([]byte, voiceFramePayload)
		start := time.Now()
		for i := 0; i < frames; i++ {
			if lost[i] && model == voiceLossDrop {
				continue
			}
			binary.BigEndian.PutUint32(payload, uint32(i))
			releaseAt := start.Add(time.Duration(i)*voiceFrameInterval + voiceMeasureDelay)
			if lost[i] {
				releaseAt = releaseAt.Add(voiceRetransmitTimeout)
			}
			// A frame whose release time has already passed goes out immediately, and
			// that is the head-of-line burst rather than a rounding detail: the writer
			// held the stream for one RTO, so everything queued behind the retransmitted
			// frame leaves back to back the moment it lands.
			if wait := time.Until(releaseAt); wait > 0 {
				time.Sleep(wait)
			}
			if err := server.WriteFrame(payload); err != nil {
				writeErr <- fmt.Errorf("frame %d: %w", i, err)
				return
			}
		}
		writeErr <- nil
	}()

	// Generous enough that a slow machine reports a bad number rather than a failed
	// test — the point is to see the stall, not to hide it behind a timeout.
	budget := time.Duration(frames)*voiceFrameInterval + voiceMeasureDelay + voiceRetransmitTimeout + 30*time.Second
	if err := client.SetReadDeadline(time.Now().Add(budget)); err != nil {
		t.Fatalf("arming the read deadline: %v", err)
	}

	arrivals := make([]voiceArrival, 0, sent)
	for range sent {
		frame, rErr := transport.ReadFrame(client)
		if rErr != nil {
			t.Fatalf("after %d of %d frames: ReadFrame: %v", len(arrivals), sent, rErr)
		}
		at := time.Now()
		if len(frame) != voiceFramePayload {
			t.Fatalf("frame %d came back as %d bytes, want %d", len(arrivals), len(frame), voiceFramePayload)
		}
		arrivals = append(arrivals, voiceArrival{sequence: binary.BigEndian.Uint32(frame), at: at})
	}

	if err := <-writeErr; err != nil {
		t.Fatalf("the sender stopped: %v", err)
	}
	return arrivals, sent
}

// reportVoiceRelay turns arrivals into the four numbers the ADR quotes.
//
// Jitter here is arrival jitter against the *schedule*, not against the previous
// frame: for two consecutively received frames the gap that should separate them
// is the number of frame slots between their sequence numbers, so a 60 ms gap
// after two lost frames is on time and contributes nothing. That distinction is
// the whole reason the harness numbers the frames — measured as a raw
// inter-arrival deviation, every loss would be reported as jitter and the 5%
// column would say nothing about the transport at all.
//
// The stall is the opposite measurement and is reported raw: the longest a
// receiver went with nothing to play, lost frames included, because that is what
// a jitter buffer has to cover and what a listener hears.
func reportVoiceRelay(t *testing.T, model voiceLossModel, loss float64, frames, sent int, arrivals []voiceArrival) {
	t.Helper()

	if len(arrivals) < 2 {
		t.Fatalf("nothing to measure: %d arrivals", len(arrivals))
	}

	jitter := make([]time.Duration, 0, len(arrivals)-1)
	var longestStall time.Duration
	for i := 1; i < len(arrivals); i++ {
		previous, current := arrivals[i-1], arrivals[i]
		if current.sequence <= previous.sequence {
			t.Fatalf("frames arrived out of order: %d after %d", current.sequence, previous.sequence)
		}
		gap := current.at.Sub(previous.at)
		if gap > longestStall {
			longestStall = gap
		}
		expected := time.Duration(current.sequence-previous.sequence) * voiceFrameInterval
		deviation := gap - expected
		if deviation < 0 {
			deviation = -deviation
		}
		jitter = append(jitter, deviation)
	}

	sort.Slice(jitter, func(i, j int) bool { return jitter[i] < jitter[j] })

	t.Logf("%-10s loss %4.1f%%  sent %d/%d  received %d  p50 %v  p99 %v  longest stall %v",
		model, loss*100, sent, frames, len(arrivals),
		percentile(jitter, 0.50).Round(time.Microsecond),
		percentile(jitter, 0.99).Round(time.Microsecond),
		longestStall.Round(time.Microsecond))
}

// percentile reads a sorted slice at the nearest rank.
func percentile(sorted []time.Duration, p float64) time.Duration {
	if len(sorted) == 0 {
		return 0
	}
	index := int(p * float64(len(sorted)-1))
	return sorted[index]
}

// voiceMeasureFramesMax bounds the override. It is not a resource limit — a run
// this long would take days — but the guarantee that a frame index fits the uint32
// the payload carries it in, so that the conversion in the writer cannot silently
// wrap a sequence number and report the result as reordering.
const voiceMeasureFramesMax = 1 << 24

// parsePositiveInt reads the frame-count override and refuses everything the
// measurement cannot represent, in one place with one message.
func parsePositiveInt(raw string) (int, error) {
	var value int
	if _, err := fmt.Sscanf(raw, "%d", &value); err != nil {
		return 0, fmt.Errorf("not a number: %w", err)
	}
	if value <= 0 {
		return 0, fmt.Errorf("must be positive, got %d", value)
	}
	if value > voiceMeasureFramesMax {
		return 0, fmt.Errorf("must be at most %d, got %d", voiceMeasureFramesMax, value)
	}
	return value, nil
}
