package game

import (
	"errors"
	"slices"
	"strings"
	"testing"
	"time"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

func TestAcceptChat(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name string
		text string
		want string
		ok   bool
	}{
		{name: "trimmed", text: "  hold the gate  ", want: "hold the gate", ok: true},
		{name: "command is ordinary text", text: "/dance", want: "/dance", ok: true},
		{name: "empty", text: " \t ", ok: false},
		{name: "exact byte limit", text: strings.Repeat("a", MaxChatBytes), want: strings.Repeat("a", MaxChatBytes), ok: true},
		{name: "over byte limit", text: strings.Repeat("a", MaxChatBytes+1), ok: false},
		{name: "multibyte exact limit", text: strings.Repeat("ø", MaxChatBytes/2), want: strings.Repeat("ø", MaxChatBytes/2), ok: true},
		{name: "multibyte over limit", text: strings.Repeat("ø", MaxChatBytes/2) + "a", ok: false},
		{name: "invalid utf8", text: string([]byte{0xff}), ok: false},
		{name: "nul control", text: "left\x00right", ok: false},
		{name: "line control", text: "left\nright", ok: false},
		{name: "delete control", text: "left\x7fright", ok: false},
		{name: "unicode control", text: "left\u0085right", ok: false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, err := acceptChat(tc.text)
			if (err == nil) != tc.ok {
				t.Fatalf("acceptChat error = %v, want accepted %t", err, tc.ok)
			}
			if got != tc.want {
				t.Errorf("accepted text has %d bytes, want %d", len(got), len(tc.want))
			}
			if err != nil && strings.Contains(err.Error(), tc.text) && tc.text != "" {
				t.Error("refusal quoted the untrusted line")
			}
		})
	}
}

func TestChatLimiterRefillsAndGuardsItsClock(t *testing.T) {
	t.Parallel()

	start := time.Unix(100, 0)
	limiter := newChatLimiter(start)
	for line := 1; line <= ChatBurst; line++ {
		if !limiter.allow(start) {
			t.Fatalf("line %d was refused from a full bucket", line)
		}
	}
	if limiter.allow(start) {
		t.Fatal("a sixth line passed a frozen clock")
	}
	if limiter.allow(start.Add(500 * time.Millisecond)) {
		t.Fatal("half a token was spent as a whole one")
	}
	if limiter.allow(start.Add(-time.Second)) {
		t.Fatal("a backwards clock refilled the bucket")
	}
	if !limiter.allow(start.Add(time.Second)) {
		t.Fatal("one elapsed second did not restore one line")
	}
	if limiter.allow(start.Add(time.Second)) {
		t.Fatal("the restored token was spent twice")
	}
}

func TestBroadcastUsesStableOrderAndCountsDrops(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	var order []uint64
	for _, entityID := range []uint64{3, 1, 2} {
		id := entityID
		if _, err := h.sim.Join(id, testPlayerID(id), testCharacterName, [3]float32{0.5, 64, 0.5}, testAppearance(), nil,
			func([]byte) bool {
				order = append(order, id)
				return id != 2
			}); err != nil {
			t.Fatalf("Join entity %d: %v", id, err)
		}
	}

	delivered, dropped := h.sim.Broadcast([]byte("one encoded frame"))
	if delivered != 2 || dropped != 1 {
		t.Errorf("Broadcast counted %d delivered and %d dropped, want 2 and 1", delivered, dropped)
	}
	if !slices.Equal(order, []uint64{1, 2, 3}) {
		t.Errorf("delivery order = %v, want [1 2 3]", order)
	}
}

func TestChatFansOneAcceptedLineBackToTheSenderAndTheWorld(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	senderOut, receiverOut := &dropSink{}, &dropSink{}
	sender, err := h.sim.Join(7, testPlayerID(7), "Astrid", [3]float32{0.5, 64, 0.5}, testAppearance(), nil, senderOut.deliver)
	if err != nil {
		t.Fatalf("Join sender: %v", err)
	}
	if _, err := h.sim.Join(9, testPlayerID(9), "Bjorn", [3]float32{0.5, 64, 0.5}, testAppearance(), nil, receiverOut.deliver); err != nil {
		t.Fatalf("Join receiver: %v", err)
	}

	if _, err := sender.Chat("  shields up  "); err != nil {
		t.Fatalf("Chat: %v", err)
	}
	want := protocol.ChatMessage{SenderEntityID: 7, SenderName: "Astrid", Text: "shields up"}
	for name, out := range map[string]*dropSink{"sender": senderOut, "receiver": receiverOut} {
		messages := chatMessages(t, out)
		if len(messages) != 1 || messages[0] != want {
			t.Errorf("%s received %+v, want [%+v]", name, messages, want)
		}
	}
}

func TestReconnectKeepsTheIdentityChatBucket(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	now := time.Unix(200, 0)
	h.sim.chatNow = func() time.Time { return now }
	playerID := testPlayerID(41)
	firstOut := &dropSink{}
	first, err := h.sim.Join(41, playerID, "Eira", [3]float32{0.5, 64, 0.5}, testAppearance(), nil, firstOut.deliver)
	if err != nil {
		t.Fatalf("first Join: %v", err)
	}
	for line := 1; line <= ChatBurst; line++ {
		if _, err := first.Chat("line"); err != nil {
			t.Fatalf("first session line %d: %v", line, err)
		}
	}
	h.sim.Leave(first)

	secondOut := &dropSink{}
	second, err := h.sim.Join(42, playerID, "Eira", [3]float32{0.5, 64, 0.5}, testAppearance(), nil, secondOut.deliver)
	if err != nil {
		t.Fatalf("reconnect Join: %v", err)
	}
	if _, err := second.Chat("fresh session"); !errors.Is(err, ErrChatTooFast) {
		t.Fatalf("reconnect line error = %v, want ErrChatTooFast", err)
	}
	if got := len(chatMessages(t, secondOut)); got != 0 {
		t.Fatalf("reconnect delivered %d lines from an empty bucket", got)
	}

	now = now.Add(time.Second)
	if _, err := second.Chat("after waiting"); err != nil {
		t.Fatalf("line after refill: %v", err)
	}
	if got := len(chatMessages(t, secondOut)); got != 1 {
		t.Errorf("reconnected session received %d lines after refill, want 1", got)
	}
}

func TestAFullChatLimiterIsPrunedWhenTheNextLineArrives(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	now := time.Unix(300, 0)
	h.sim.chatNow = func() time.Time { return now }
	first, err := h.sim.Join(51, testPlayerID(51), "Eira", [3]float32{0.5, 64, 0.5}, testAppearance(), nil, (&dropSink{}).deliver)
	if err != nil {
		t.Fatalf("first Join: %v", err)
	}
	second, err := h.sim.Join(52, testPlayerID(52), "Liv", [3]float32{0.5, 64, 0.5}, testAppearance(), nil, (&dropSink{}).deliver)
	if err != nil {
		t.Fatalf("second Join: %v", err)
	}

	if _, err := first.Chat("one line"); err != nil {
		t.Fatalf("first Chat: %v", err)
	}
	if got := len(h.sim.chatLimiters); got != 1 {
		t.Fatalf("chat limiter count = %d after first line, want 1", got)
	}

	now = now.Add(time.Duration(ChatBurst) * time.Second)
	if _, err := second.Chat("later line"); err != nil {
		t.Fatalf("second Chat: %v", err)
	}
	if got := len(h.sim.chatLimiters); got != 1 {
		t.Errorf("chat limiter count = %d after stale entry was fully refilled, want 1", got)
	}
	if _, retained := h.sim.chatLimiters[first.playerID]; retained {
		t.Error("fully refilled limiter was retained")
	}
}

func TestDeadAndLeavingPlayersSayNothing(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	dead, deadOut := h.join(1, [3]float32{0.5, 64, 0.5})
	h.hurt(dead, PlayerMaxHealth)
	if _, err := dead.Chat("from the grave"); err == nil || errors.Is(err, ErrChatTooFast) {
		t.Fatalf("dead player's Chat error = %v", err)
	}
	if got := len(chatMessages(t, deadOut)); got != 0 {
		t.Errorf("dead player delivered %d lines", got)
	}

	leaving, leavingOut := h.join(2, [3]float32{0.5, 64, 0.5})
	leaving.BeginLeaving()
	if _, err := leaving.Chat("on the way out"); err == nil || errors.Is(err, ErrChatTooFast) {
		t.Fatalf("leaving player's Chat error = %v", err)
	}
	if got := len(chatMessages(t, leavingOut)); got != 0 {
		t.Errorf("leaving player delivered %d lines", got)
	}
}

func chatMessages(t *testing.T, out *dropSink) []protocol.ChatMessage {
	t.Helper()

	var messages []protocol.ChatMessage
	for _, frame := range out.all() {
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		if envelope.PayloadType() != vnet.PayloadChatMessage {
			continue
		}
		var payload flatbuffers.Table
		if !envelope.Payload(&payload) {
			t.Fatal("ChatMessage envelope has no payload")
		}
		var chat vnet.ChatMessage
		chat.Init(payload.Bytes, payload.Pos)
		messages = append(messages, protocol.ChatMessage{
			SenderEntityID: chat.SenderEntityId(),
			SenderName:     string(chat.SenderName()),
			Text:           string(chat.Text()),
		})
	}
	return messages
}
