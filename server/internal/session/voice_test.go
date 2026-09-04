package session_test

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/hex"
	"fmt"
	"log/slog"
	"slices"
	"strings"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
)

// opusFixture is four bytes that are not Opus and do not have to be: nothing on this
// path parses them. They are distinctive so a test can say the payload arrived whole.
var opusFixture = []byte{0xF8, 0x1A, 0x2B, 0x3C}

func TestAVoiceFrameReachesTheOtherSessionAndNotItsSpeaker(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	speaker, speakerFrames := admit(t, cfg, chunks, sim, peers, 1)
	_, listenerFrames := admit(t, cfg, chunks, sim, peers, 2)

	// Audibility is the tick's answer, not the frame's. Both characters stand on the
	// same spawn, and one recompute is what puts each in the other's audible set; without
	// it a frame sent now would correctly reach nobody.
	sim.Step(game.VoiceSetInterval)

	speaker.in <- protocol.EncodeVoiceFrame(protocol.VoiceFrame{
		Sequence: 7,
		Audience: vnet.VoiceAudienceEveryone,
		Opus:     opusFixture,
	})

	waitUntil(t, "the relayed voice frame to reach the second session", func() bool {
		return len(listenerFrames.voicesHeard()) == 1
	})
	heard := listenerFrames.voicesHeard()[0]
	if heard.SpeakerEntityID != 1 || heard.Sequence != 7 || !slices.Equal(heard.Opus, opusFixture) {
		t.Errorf("VoiceHeard = %+v, want speaker 1, sequence 7 and the payload unchanged", heard)
	}
	// The speaker is not in its own audible set, so it is not a recipient. A client that
	// heard itself back would be one deciding what to play from a frame it sent.
	if got := len(speakerFrames.voicesHeard()); got != 0 {
		t.Errorf("the speaker was relayed %d of its own frames, want 0", got)
	}
}

// The latency lane, and the whole of what a second lane buys a voice frame: twenty
// milliseconds of speech is worthless once the next frame has been spoken, so it must
// never wait behind a queue of bulk frames the way a snapshot used to (#668). Audible as
// a gap in a sentence rather than visible as a stutter, and the same defect.
//
// The proof is an overtaking: the bulk lane is loaded first, the voice is relayed second,
// and the writer is held until both are queued. What makes the hold exact is the chat line
// sent behind the voice on the *speaker's* connection — one read goroutine handles both in
// order, so the speaker hearing its own line back is the fact that the voice frame has
// already been handed to the listener's lane.
func TestARelayedVoiceFrameOvertakesTheBulkLane(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	speaker, speakerFrames := admit(t, cfg, chunks, sim, peers, 1)
	listener, listenerFrames := admit(t, cfg, chunks, sim, peers, 2)
	sim.Step(game.VoiceSetInterval)

	held := listenerFrames.chunkCoords()
	if len(held) == 0 {
		t.Fatal("the listener holds no chunk to broadcast into")
	}

	listener.holdWrites()
	before := len(listenerFrames.kindsReceived())

	const backlog = 8
	for i := range backlog {
		update := protocol.EncodeBlockUpdate(protocol.BlockUpdate{Pos: [3]int32{int32(i), 70, 0}, BlockID: 1})
		if reached := peers.BroadcastChunk(held[0], update); reached == 0 {
			t.Fatalf("bulk frame %d reached no session, so there is no backlog to overtake", i)
		}
	}

	speaker.in <- protocol.EncodeVoiceFrame(protocol.VoiceFrame{
		Sequence: 1,
		Audience: vnet.VoiceAudienceEveryone,
		Opus:     opusFixture,
	})
	speaker.in <- protocol.EncodeChatRequest(protocol.ChatRequest{Text: "behind the voice"})
	waitUntil(t, "the speaker's own chat line, which is when its voice frame is already queued", func() bool {
		return len(speakerFrames.chatMessages()) == 1
	})

	listener.releaseWrites()
	waitUntil(t, "the relayed voice frame to reach the second session", func() bool {
		return len(listenerFrames.voicesHeard()) == 1
	})

	arrived := listenerFrames.kindsReceived()[before:]
	spoken := slices.Index(arrived, vnet.PayloadVoiceHeard)
	if spoken < 0 {
		t.Fatalf("the voice frame is not among the %d frames that arrived after the hold", len(arrived))
	}
	overtaken := 0
	for _, kind := range arrived[spoken:] {
		if kind == vnet.PayloadBlockUpdate {
			overtaken++
		}
	}
	// One fewer than the backlog: the writer may already have been inside WriteFrame with
	// a bulk frame when the gate closed, and that one is past overtaking.
	if want := backlog - 1; overtaken < want {
		t.Errorf("the voice frame overtook %d bulk frames, want at least %d; arrival order was %v",
			overtaken, want, arrived)
	}
}

// The other half of the promise the relay makes, asked one layer up from
// game.TestVoiceLogsNeverCarryThePayload: **no log line anywhere carries the bytes of a
// voice frame**, and a session is where those bytes are decoded, dispatched and refused.
//
// The frame is oversized so the refusal is certain and the payload is present while it is
// being written about — which is the only state in which this could go wrong. What the
// logger must carry is a length and an identity; what it must never carry is the audio.
func TestASessionsVoiceDiagnosticsNeverCarryThePayload(t *testing.T) {
	t.Parallel()

	cfg := serveConfig()
	logged := &syncWriter{}
	log := slog.New(slog.NewTextHandler(logged, &slog.HandlerOptions{Level: slog.LevelDebug}))

	// One logger for both halves rather than serveDeps' discarded one: the refusal is
	// written by the simulation and the dispatch around it by the session, and what this
	// test is about is every line either of them produces about the same frame.
	chunks, peers := testChunks(), session.NewRegistry(session.DefaultConcurrentSessions)
	sim, err := game.NewSim(cfg.TickRate, cfg.ViewDistance, cfg.WorldSeed,
		game.NewCacheTerrain(chunks), chunks, peers.NextID, log)
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}

	conn := newFakeConn()
	frames := collect(t, conn)
	ctx, cancel := context.WithCancel(context.Background())
	served := make(chan error, 1)
	go func() {
		served <- session.Serve(ctx, conn, cfg, noTimeouts(), chunks, sim, peers, ephemeralIdentities(), 9, log)
	}()
	t.Cleanup(func() {
		cancel()
		_ = conn.Close()
		if err := <-served; err != nil {
			t.Errorf("the session ended with %v", err)
		}
	})

	conn.in <- hello(9)
	createCharacter(conn, "Eivor")
	waitUntil(t, "the session to reach the world", func() bool { return len(frames.chunkCoords()) > 0 })

	conn.in <- protocol.EncodeVoiceFrame(protocol.VoiceFrame{
		Sequence: 1,
		Audience: vnet.VoiceAudienceEveryone,
		Opus:     append(bytes.Repeat([]byte{0x5A}, protocol.MaxVoiceOpusBytes), opusFixture...),
	})
	waitUntil(t, "the refusal to be written down", func() bool {
		return strings.Contains(logged.String(), "voice frame dropped")
	})

	captured := logged.String()
	for name, rendering := range map[string]string{
		"raw bytes":        string(opusFixture),
		"the %v rendering": fmt.Sprintf("%v", opusFixture),
		"hexadecimal":      hex.EncodeToString(opusFixture),
		"standard base64":  base64.StdEncoding.EncodeToString(opusFixture),
		"raw base64":       base64.RawStdEncoding.EncodeToString(opusFixture),
	} {
		if strings.Contains(captured, rendering) {
			t.Errorf("a log line carried the voice payload as %s", name)
		}
	}
}
