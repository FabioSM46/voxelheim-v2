package game

import (
	"bytes"
	"encoding/base64"
	"encoding/hex"
	"fmt"
	"log/slog"
	"strings"
	"testing"
	"time"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// testOpus is deliberately recognisable: nothing else could produce these bytes in a log
// line by accident, which is what TestVoiceLogsNeverCarryThePayload looks for.
var testOpus = []byte{0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04}

func voiceFrame(sequence uint32, audience vnet.VoiceAudience) protocol.VoiceFrame {
	return protocol.VoiceFrame{Sequence: sequence, Audience: audience, Opus: testOpus}
}

// voicesHeard is every VoiceHeard one sink received. The Opus is compared as bytes rather
// than described, which is the only thing a test of this feature may do with it.
func voicesHeard(t *testing.T, out *dropSink) []protocol.VoiceHeard {
	t.Helper()

	var heard []protocol.VoiceHeard
	for _, frame := range out.all() {
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		if envelope.PayloadType() != vnet.PayloadVoiceHeard {
			continue
		}
		var payload flatbuffers.Table
		if !envelope.Payload(&payload) {
			t.Fatal("VoiceHeard envelope has no payload")
		}
		var voice vnet.VoiceHeard
		voice.Init(payload.Bytes, payload.Pos)
		heard = append(heard, protocol.VoiceHeard{
			SpeakerEntityID: voice.SpeakerEntityId(),
			Sequence:        voice.Sequence(),
			Opus:            append([]byte(nil), voice.OpusBytes()...),
		})
	}
	return heard
}

// voiceHarness is two players in earshot with the audible sets already computed. The
// ticks are [VoiceSetInterval]: a set that has never been computed is empty, and a test
// that forgot them would be asserting about silence.
func voiceHarness(t *testing.T) (*vitalsHarness, *Player, *dropSink, *Player, *dropSink) {
	t.Helper()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	speaker, speakerOut := h.join(1, [3]float32{0.5, 64, 0.5})
	listener, listenerOut := h.join(2, [3]float32{4.5, 64, 0.5})
	h.advance(VoiceSetInterval)
	return h, speaker, speakerOut, listener, listenerOut
}

func (h *vitalsHarness) audibleTo(speaker *Player) []uint64 {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	ids := make([]uint64, 0, len(speaker.audible))
	for id := range speaker.audible {
		ids = append(ids, id)
	}
	return ids
}

func TestVoiceSetEntersAtTheRangeAndLeavesOnlyAtTheWiderOne(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	speaker, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	listener, _ := h.join(2, [3]float32{0.5, 64, 0.5})

	// The pass is driven directly rather than through Step: the boundary is what is
	// under test, and one tick of gravity would move it.
	recompute := func(separation float64) bool {
		h.sim.mu.Lock()
		defer h.sim.mu.Unlock()
		speaker.pos = [3]float64{0, 64, 0}
		listener.pos = [3]float64{separation, 64, 0}
		h.sim.advanceVoiceSetsLocked(VoiceSetInterval, h.sim.sortedPlayersLocked())
		_, heard := speaker.audible[listener.entityID]
		return heard
	}

	exit := VoiceRangeDefault * VoiceExitFactor
	for _, tc := range []struct {
		name       string
		separation float64
		want       bool
	}{
		{name: "exactly at the range enters", separation: VoiceRangeDefault, want: true},
		{name: "past the range but already in stays", separation: VoiceRangeDefault + 1, want: true},
		{name: "exactly at the widened range stays", separation: exit, want: true},
		{name: "past the widened range leaves", separation: exit + 0.01, want: false},
		{name: "outside the range does not re-enter", separation: VoiceRangeDefault + 0.01, want: false},
		{name: "back inside the range re-enters", separation: VoiceRangeDefault, want: true},
	} {
		if got := recompute(tc.separation); got != tc.want {
			t.Errorf("%s: audible at %.2f blocks = %t, want %t", tc.name, tc.separation, got, tc.want)
		}
	}
}

func TestVoiceSetIsRecomputedOnlyEveryFourthTick(t *testing.T) {
	t.Parallel()

	h, speaker, _, listener, _ := voiceHarness(t)
	if got := h.audibleTo(speaker); len(got) != 1 {
		t.Fatalf("audible set after the first pass = %v, want the listener alone", got)
	}

	// Well outside even the widened range, so the only thing that can keep the listener
	// in the set is the cadence.
	h.standAt(listener, [3]float64{500, 64, 0})
	for tick := 1; tick < VoiceSetInterval; tick++ {
		h.step()
		if got := h.audibleTo(speaker); len(got) != 1 {
			t.Fatalf("audible set %d tick(s) after the move = %v, want it unchanged", tick, got)
		}
		h.standAt(listener, [3]float64{500, 64, 0})
	}
	h.step()
	if got := h.audibleTo(speaker); len(got) != 0 {
		t.Errorf("audible set after the pass = %v, want empty", got)
	}
}

func TestVoiceReachesTheAudibleSetAndNobodyElse(t *testing.T) {
	t.Parallel()

	h, speaker, speakerOut, _, listenerOut := voiceHarness(t)
	distant, distantOut := h.join(3, [3]float32{0.5, 64, 0.5})
	h.standAt(distant, [3]float64{500, 64, 0})
	h.advance(VoiceSetInterval)

	delivered, dropped := speaker.Voice(voiceFrame(7, vnet.VoiceAudienceEveryone))
	if delivered != 1 || dropped != 0 {
		t.Fatalf("Voice = %d delivered, %d dropped; want 1, 0", delivered, dropped)
	}

	heard := voicesHeard(t, listenerOut)
	if len(heard) != 1 || heard[0].SpeakerEntityID != 1 || heard[0].Sequence != 7 ||
		!bytes.Equal(heard[0].Opus, testOpus) {
		t.Errorf("listener received %+v, want one frame from speaker 1, sequence 7", heard)
	}
	if got := len(voicesHeard(t, distantOut)); got != 0 {
		t.Errorf("a player 500 blocks away received %d frames", got)
	}
	// A speaker is not in their own audible set, so nothing is echoed back.
	if got := len(voicesHeard(t, speakerOut)); got != 0 {
		t.Errorf("the speaker was sent %d of their own frames", got)
	}
}

func TestVoicePartyReachesOnlyTheMembersInRange(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	speaker, _ := joinPartyPlayer(t, h, 1, "Eivor", [3]float32{0.5, 64, 0.5})
	member, memberOut := joinPartyPlayer(t, h, 2, "Liv", [3]float32{4.5, 64, 0.5})
	_, strangerOut := joinPartyPlayer(t, h, 3, "Kari", [3]float32{6.5, 64, 0.5})
	h.advance(VoiceSetInterval)

	// Everyone first: all three are audible to each other, so a Party frame that reached
	// the stranger would be a filter doing nothing rather than a set that was empty.
	if delivered, _ := speaker.Voice(voiceFrame(1, vnet.VoiceAudienceEveryone)); delivered != 2 {
		t.Fatalf("Everyone delivered %d, want 2", delivered)
	}
	// A speaker in no party asking for Party reaches nobody.
	if delivered, dropped := speaker.Voice(voiceFrame(2, vnet.VoiceAudienceParty)); delivered != 0 || dropped != 0 {
		t.Fatalf("Party from a soloist = %d delivered, %d dropped; want 0, 0", delivered, dropped)
	}

	inviteAndAccept(t, speaker, member, "Liv")
	if delivered, _ := speaker.Voice(voiceFrame(3, vnet.VoiceAudienceParty)); delivered != 1 {
		t.Fatalf("Party delivered %d, want 1", delivered)
	}
	if got := len(voicesHeard(t, memberOut)); got != 2 {
		t.Errorf("the party member received %d frames, want 2 (one Everyone, one Party)", got)
	}
	if got := len(voicesHeard(t, strangerOut)); got != 1 {
		t.Errorf("the stranger received %d frames, want 1 (the Everyone one only)", got)
	}
}

func TestVoiceDropsAFrameItCannotFilterOrCarry(t *testing.T) {
	t.Parallel()

	for name, frame := range map[string]protocol.VoiceFrame{
		"an audience this server cannot name": {
			Sequence: 1, Audience: vnet.VoiceAudience(9), Opus: testOpus,
		},
		"opus past the contract's bound": {
			Sequence: 2,
			Audience: vnet.VoiceAudienceEveryone,
			Opus:     bytes.Repeat([]byte{0x5A}, protocol.MaxVoiceOpusBytes+1),
		},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			_, speaker, _, _, listenerOut := voiceHarness(t)
			if delivered, dropped := speaker.Voice(frame); delivered != 0 || dropped != 0 {
				t.Fatalf("Voice = %d delivered, %d dropped; want 0, 0", delivered, dropped)
			}
			if got := len(voicesHeard(t, listenerOut)); got != 0 {
				t.Errorf("a refused frame reached %d listeners", got)
			}
		})
	}

	// The bound is a limit and not an error: exactly at it is an ordinary frame.
	_, speaker, _, _, listenerOut := voiceHarness(t)
	exact := protocol.VoiceFrame{
		Sequence: 3,
		Audience: vnet.VoiceAudienceEveryone,
		Opus:     bytes.Repeat([]byte{0x5A}, protocol.MaxVoiceOpusBytes),
	}
	if delivered, _ := speaker.Voice(exact); delivered != 1 {
		t.Fatalf("a frame exactly at the bound delivered %d, want 1", delivered)
	}
	if got := len(voicesHeard(t, listenerOut)); got != 1 {
		t.Errorf("listener received %d frames at the exact bound, want 1", got)
	}
}

func TestVoiceLimiterSpendsABurstAndRefills(t *testing.T) {
	t.Parallel()

	h, speaker, _, _, listenerOut := voiceHarness(t)
	now := time.Unix(400, 0)
	h.sim.voiceNow = func() time.Time { return now }

	for frame := 1; frame <= VoiceBurst; frame++ {
		if delivered, _ := speaker.Voice(voiceFrame(uint32(frame), vnet.VoiceAudienceEveryone)); delivered != 1 {
			t.Fatalf("frame %d of the burst was refused", frame)
		}
	}
	if delivered, _ := speaker.Voice(voiceFrame(99, vnet.VoiceAudienceEveryone)); delivered != 0 {
		t.Fatal("a frame past the burst was relayed on a frozen clock")
	}
	// Half a frame interval is half a token, and half a token is not one.
	now = now.Add(time.Second / (VoiceRefillPerSecond * 2))
	if delivered, _ := speaker.Voice(voiceFrame(100, vnet.VoiceAudienceEveryone)); delivered != 0 {
		t.Fatal("half a token was spent as a whole one")
	}
	now = now.Add(time.Second / VoiceRefillPerSecond)
	if delivered, _ := speaker.Voice(voiceFrame(101, vnet.VoiceAudienceEveryone)); delivered != 1 {
		t.Fatal("an elapsed frame interval restored no credit")
	}
	if got := len(voicesHeard(t, listenerOut)); got != VoiceBurst+1 {
		t.Errorf("listener received %d frames, want %d", got, VoiceBurst+1)
	}

	// A completely refilled bucket carries no information, so it is pruned and remade.
	now = now.Add(time.Second)
	if delivered, _ := speaker.Voice(voiceFrame(102, vnet.VoiceAudienceEveryone)); delivered != 1 {
		t.Fatal("a fully refilled bucket refused a frame")
	}
	h.sim.mu.Lock()
	retained := len(h.sim.voiceLimiters)
	h.sim.mu.Unlock()
	if retained != 1 {
		t.Errorf("voice limiter count = %d, want the speaker's alone", retained)
	}
}

func TestVoiceFromALeavingSpeakerIsDroppedAndFromADeadOneDelivered(t *testing.T) {
	t.Parallel()

	h, speaker, _, _, listenerOut := voiceHarness(t)

	h.sim.mu.Lock()
	speaker.damageLocked(PlayerMaxHealth)
	h.sim.mu.Unlock()
	if delivered, _ := speaker.Voice(voiceFrame(1, vnet.VoiceAudienceEveryone)); delivered != 1 {
		t.Fatal("a dead speaker was silenced; death is a three-second wait, not a gag")
	}

	speaker.BeginLeaving()
	if delivered, dropped := speaker.Voice(voiceFrame(2, vnet.VoiceAudienceEveryone)); delivered != 0 || dropped != 0 {
		t.Fatalf("a leaving speaker relayed %d and dropped %d; want 0, 0", delivered, dropped)
	}
	if got := len(voicesHeard(t, listenerOut)); got != 1 {
		t.Errorf("listener received %d frames, want the dead speaker's one", got)
	}
}

func TestVoiceCountsWhatARefusingSessionDropped(t *testing.T) {
	t.Parallel()

	_, speaker, _, _, listenerOut := voiceHarness(t)
	listenerOut.setFull(true)

	delivered, dropped := speaker.Voice(voiceFrame(1, vnet.VoiceAudienceEveryone))
	if delivered != 0 || dropped != 1 {
		t.Fatalf("Voice against a full lane = %d delivered, %d dropped; want 0, 1", delivered, dropped)
	}
}

func TestVoiceSetForgetsAListenerWhoLeft(t *testing.T) {
	t.Parallel()

	h, speaker, _, listener, _ := voiceHarness(t)
	h.sim.Leave(listener)

	if got := h.audibleTo(speaker); len(got) != 0 {
		t.Errorf("audible set after the listener left = %v, want empty", got)
	}
	if delivered, dropped := speaker.Voice(voiceFrame(1, vnet.VoiceAudienceEveryone)); delivered != 0 || dropped != 0 {
		t.Errorf("Voice after the only listener left = %d, %d; want 0, 0", delivered, dropped)
	}
}

func TestVoiceRangeZeroRelaysNothingAtAll(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.sim.mu.Lock()
	h.sim.voiceRange = 0
	h.sim.mu.Unlock()

	speaker, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	_, listenerOut := h.join(2, [3]float32{0.5, 64, 0.5})
	h.advance(VoiceSetInterval)

	if got := h.audibleTo(speaker); len(got) != 0 {
		t.Errorf("a server with no voice computed an audible set of %v", got)
	}
	if delivered, dropped := speaker.Voice(voiceFrame(1, vnet.VoiceAudienceEveryone)); delivered != 0 || dropped != 0 {
		t.Errorf("Voice on a silent server = %d, %d; want 0, 0", delivered, dropped)
	}
	if got := len(voicesHeard(t, listenerOut)); got != 0 {
		t.Errorf("a silent server relayed %d frames", got)
	}
}

func TestVoiceRangeMustBeAFiniteDistance(t *testing.T) {
	t.Parallel()

	var zero float64
	for name, blocks := range map[string]float64{"negative": -1, "not a number": zero / zero} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			if _, err := NewSim(DefaultTickRate, 8, testWorldSeed, dropTerrain{groundTop: 63}, refusedEdits{},
				testEntityIDs(), slog.New(slog.NewTextHandler(&bytes.Buffer{}, nil)), WithVoiceRange(blocks)); err == nil {
				t.Errorf("NewSim accepted a voice range of %v", blocks)
			}
		})
	}
}

// TestVoiceLogsNeverCarryThePayload is the acceptance criterion the rest of this file
// supports: **no log line anywhere carries the bytes of a voice frame.**
//
// The logger is captured at Debug — the level every voice diagnostic is written at, and
// therefore the only level at which the claim can fail — across an exchange that walks
// every refusal the relay has, plus a delivery that succeeds.
//
// It looks for the payload in the shapes a Go logger could produce: the raw bytes, slog's
// %v rendering of a []byte, hex, and base64. None is hypothetical — the first two are
// what a text handler prints if the value is ever passed, hex is what somebody debugging
// a codec reaches for, and base64 is what a JSON handler produces unasked.
func TestVoiceLogsNeverCarryThePayload(t *testing.T) {
	t.Parallel()

	var captured bytes.Buffer
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.sim.log = slog.New(slog.NewTextHandler(&captured, &slog.HandlerOptions{Level: slog.LevelDebug}))

	speaker, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	_, listenerOut := h.join(2, [3]float32{0.5, 64, 0.5})
	h.advance(VoiceSetInterval)

	speaker.Voice(voiceFrame(1, vnet.VoiceAudienceEveryone))
	speaker.Voice(protocol.VoiceFrame{Sequence: 2, Audience: vnet.VoiceAudience(9), Opus: testOpus})
	speaker.Voice(protocol.VoiceFrame{
		Sequence: 3,
		Audience: vnet.VoiceAudienceEveryone,
		Opus:     append(bytes.Repeat([]byte{0x5A}, protocol.MaxVoiceOpusBytes), testOpus...),
	})
	listenerOut.setFull(true)
	speaker.Voice(voiceFrame(4, vnet.VoiceAudienceEveryone))
	listenerOut.setFull(false)
	for frame := range VoiceBurst + 2 {
		speaker.Voice(voiceFrame(uint32(100+frame), vnet.VoiceAudienceEveryone))
	}
	h.sim.mu.Lock()
	h.sim.voiceRange = 0
	h.sim.mu.Unlock()
	speaker.Voice(voiceFrame(200, vnet.VoiceAudienceEveryone))

	captures := captured.String()
	if !strings.Contains(captures, "voice frame dropped") {
		t.Fatal("no voice diagnostic was captured; the test would pass for the wrong reason")
	}
	for name, rendering := range map[string]string{
		"raw bytes":        string(testOpus),
		"the %v rendering": fmt.Sprintf("%v", testOpus),
		"hexadecimal":      hex.EncodeToString(testOpus),
		"standard base64":  base64.StdEncoding.EncodeToString(testOpus),
		"URL-safe base64":  base64.URLEncoding.EncodeToString(testOpus),
		"raw base64":       base64.RawStdEncoding.EncodeToString(testOpus),
	} {
		if strings.Contains(captures, rendering) {
			t.Errorf("a log line carried the voice payload as %s", name)
		}
	}
}
