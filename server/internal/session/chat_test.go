package session_test

import (
	"fmt"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

func TestWorldChatReachesBothSessionsAndOnlyRateLimitAnswers(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	sender, senderFrames := admit(t, cfg, chunks, sim, peers, 1)
	_, receiverFrames := admit(t, cfg, chunks, sim, peers, 2)

	// A control rune is a game-level refusal, not a malformed frame. The five valid
	// lines after it prove both that the session stayed alive and that rejected text
	// did not spend a token.
	sender.in <- protocol.EncodeChatRequest(protocol.ChatRequest{Text: "split\nthe log"})
	for line := 1; line <= game.ChatBurst; line++ {
		sender.in <- protocol.EncodeChatRequest(protocol.ChatRequest{Text: fmt.Sprintf(" line %d ", line)})
	}
	waitUntil(t, "five accepted chat lines to reach both sessions", func() bool {
		return len(senderFrames.chatMessages()) == game.ChatBurst &&
			len(receiverFrames.chatMessages()) == game.ChatBurst
	})

	wantFirst := protocol.ChatMessage{SenderEntityID: 1, SenderName: "Eivor", Text: "line 1"}
	for name, frames := range map[string]*collector{"sender": senderFrames, "receiver": receiverFrames} {
		messages := frames.chatMessages()
		if messages[0] != wantFirst {
			t.Errorf("%s first line = %+v, want %+v", name, messages[0], wantFirst)
		}
		for _, message := range messages {
			if message.Text == "split\nthe log" {
				t.Errorf("%s received the refused control-character line", name)
			}
		}
	}

	sender.in <- protocol.EncodeChatRequest(protocol.ChatRequest{Text: "line 6"})
	waitUntil(t, "the sixth line's rate-limit refusal", func() bool {
		return len(senderFrames.actionRefusals()) == 1
	})
	refusal := senderFrames.actionRefusals()[0]
	if refusal.Action != vnet.RefusedActionChat || refusal.Reason != vnet.RefusalReasonTooFast || refusal.HasAnchor {
		t.Errorf("rate-limit refusal = %+v, want Chat/TooFast without an anchor", refusal)
	}
	if got := len(receiverFrames.actionRefusals()); got != 0 {
		t.Errorf("receiver got %d refusals for somebody else's rate limit", got)
	}
	if got := len(senderFrames.chatMessages()); got != game.ChatBurst {
		t.Errorf("sender received %d chat lines, want %d; the sixth landed", got, game.ChatBurst)
	}
	if got := len(receiverFrames.chatMessages()); got != game.ChatBurst {
		t.Errorf("receiver received %d chat lines, want %d; the sixth landed", got, game.ChatBurst)
	}
}
