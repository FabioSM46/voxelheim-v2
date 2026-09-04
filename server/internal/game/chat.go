package game

import (
	"errors"
	"fmt"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// ErrChatTooFast is the one chat refusal a session answers on the wire. Invalid
// text and players that cannot act are deliberately silent. Commands spend this
// same bucket before they are parsed or dispatched.
var ErrChatTooFast = errors.New("chat was sent too fast")

// CommandSenderName is the reserved display name on private command answers.
//
// Reusing ChatMessage avoids a schema change. A character can share this name, an
// accepted ambiguity on a development server whose operator enabled cheats.
const CommandSenderName = "Server"

// ChatOutcome is the private state a command asks the session to send. Ordinary
// chat returns its zero value after broadcasting the accepted line.
type ChatOutcome struct {
	PrivateText string
	Inventory   *protocol.InventoryState
	command     string
	arguments   []string
}

// tokenBucket is one cadence allowance retained by Sim under the player's stable
// identity. It has no mutex of its own: every access happens under Sim.mu.
//
// **The burst and the refill are fields rather than constants**, because there are two
// of these and their numbers differ by two orders of magnitude: chat is five lines with
// one restored a second, voice is twenty frames with sixty restored a second. What must
// not differ is the arithmetic — the backwards-clock guard below is the part that is
// easy to get subtly wrong, and one copy of it is one thing to be right about.
type tokenBucket struct {
	tokens          float64
	burst           float64
	refillPerSecond float64
	last            time.Time
}

func newChatLimiter(now time.Time) *tokenBucket {
	return &tokenBucket{tokens: ChatBurst, burst: ChatBurst, refillPerSecond: ChatRefillPerSecond, last: now}
}

// allow refills from elapsed wall time and spends one token when possible.
func (l *tokenBucket) allow(now time.Time) bool {
	// A clock that stands still or moves backwards grants no credit and does not
	// move the refill origin backwards. SystemClock carries a monotonic reading;
	// the guard also makes deliberately frozen test clocks exact.
	if elapsed := now.Sub(l.last); elapsed > 0 {
		l.tokens = min(l.burst, l.tokens+elapsed.Seconds()*l.refillPerSecond)
		l.last = now
	}
	if l.tokens < 1 {
		return false
	}
	l.tokens--
	return true
}

// fullAt reports whether elapsed refill time has restored the whole burst. A
// fully restored limiter carries no information: deleting it is equivalent to
// retaining it and bounds the identity-keyed map to recent activity.
func (l *tokenBucket) fullAt(now time.Time) bool {
	if l.tokens >= l.burst {
		return true
	}
	elapsed := now.Sub(l.last)
	return elapsed > 0 && l.tokens+elapsed.Seconds()*l.refillPerSecond >= l.burst
}

func (s *Sim) pruneChatLimitersLocked(now time.Time) {
	for playerID, limiter := range s.chatLimiters {
		if limiter.fullAt(now) {
			delete(s.chatLimiters, playerID)
		}
	}
}

// acceptChat turns untrusted display text into one line the world chat may carry.
// Errors never quote the text or the control rune that caused them.
func acceptChat(text string) (string, error) {
	accepted := strings.TrimSpace(text)
	switch {
	case accepted == "":
		return "", errors.New("a chat line needs text")
	case len(accepted) > MaxChatBytes:
		return "", fmt.Errorf("%d bytes is longer than the %d a chat line may be", len(accepted), MaxChatBytes)
	case !utf8.ValidString(accepted):
		return "", errors.New("a chat line has to be text")
	}
	for _, r := range accepted {
		if unicode.IsControl(r) {
			return "", errors.New("a chat line may not contain control characters")
		}
	}
	return accepted, nil
}

// Chat accepts one development command or world-chat line. A raw leading slash is
// split before chat validation and cannot fall through to broadcast. Sim owns the
// limiter so reconnecting the stable player identity cannot reset it.
func (p *Player) Chat(text string) (ChatOutcome, error) {
	p.sim.mu.Lock()

	if strings.HasPrefix(text, "/") {
		allowed := p.spendChatTokenLocked()
		// Disabled commands still spend the shared bucket, but the startup gate is
		// the more specific refusal and always wins: every command on a server with
		// the feature off says why it is off.
		if !p.sim.devCommands {
			outcome := p.commandLocked(text)
			p.sim.mu.Unlock()
			return outcome, nil
		}
		if !allowed {
			p.sim.mu.Unlock()
			return ChatOutcome{}, ErrChatTooFast
		}
		outcome := p.commandLocked(text)
		p.sim.mu.Unlock()
		if outcome.command != "" {
			p.sim.log.Info("development command accepted",
				"entity_id", p.entityID,
				"command", outcome.command,
				"arguments", outcome.arguments)
		}
		return outcome, nil
	}
	if err := p.cannotActLocked(); err != nil {
		p.sim.mu.Unlock()
		return ChatOutcome{}, err
	}
	accepted, err := acceptChat(text)
	if err != nil {
		p.sim.mu.Unlock()
		return ChatOutcome{}, err
	}
	if !p.spendChatTokenLocked() {
		p.sim.mu.Unlock()
		return ChatOutcome{}, ErrChatTooFast
	}

	frame := protocol.EncodeChatMessage(protocol.ChatMessage{
		SenderEntityID: p.entityID,
		SenderName:     p.name,
		Text:           accepted,
	})
	p.sim.broadcastLocked(frame)
	p.sim.mu.Unlock()
	return ChatOutcome{}, nil
}

// spendChatTokenLocked is the single cadence gate for chat and commands.
func (p *Player) spendChatTokenLocked() bool {
	now := p.sim.chatNow()
	p.sim.pruneChatLimitersLocked(now)
	limiter := p.sim.chatLimiters[p.playerID]
	if limiter == nil {
		limiter = newChatLimiter(now)
		p.sim.chatLimiters[p.playerID] = limiter
	}
	return limiter.allow(now)
}

// Broadcast hands one non-superseded frame to every connected player in stable
// entity order. Delivery is nevertheless non-blocking: one stalled session must not
// hold Sim.mu and prevent the same line reaching every healthy session. A false
// delivery is therefore counted and logged rather than retried here.
func (s *Sim) Broadcast(frame []byte) (delivered, dropped int) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.broadcastLocked(frame)
}

// broadcastLocked is Broadcast for callers, such as Player.Chat, that already hold
// Sim.mu. Keeping one fan-out implementation avoids a re-entrant lock and preserves
// the public method's ordering and drop accounting.
func (s *Sim) broadcastLocked(frame []byte) (delivered, dropped int) {
	for _, recipient := range s.sortedPlayersLocked() {
		if recipient.deliver(frame) {
			delivered++
			continue
		}
		dropped++
		s.log.Debug("broadcast dropped: the session's outbound queue is full",
			"entity_id", recipient.entityID)
	}
	return delivered, dropped
}
