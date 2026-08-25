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
// text and players that cannot act are deliberately silent.
var ErrChatTooFast = errors.New("chat was sent too fast")

// chatLimiter is one token bucket retained by Sim under the player's stable
// identity. It has no mutex of its own: every access happens under Sim.mu.
type chatLimiter struct {
	tokens float64
	last   time.Time
}

func newChatLimiter(now time.Time) *chatLimiter {
	return &chatLimiter{tokens: ChatBurst, last: now}
}

// allow refills from elapsed wall time and spends one token when possible.
func (l *chatLimiter) allow(now time.Time) bool {
	// A clock that stands still or moves backwards grants no credit and does not
	// move the refill origin backwards. SystemClock carries a monotonic reading;
	// the guard also makes deliberately frozen test clocks exact.
	if elapsed := now.Sub(l.last); elapsed > 0 {
		l.tokens = min(float64(ChatBurst), l.tokens+elapsed.Seconds()*ChatRefillPerSecond)
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
// retaining it and bounds the identity-keyed map to recent chat activity.
func (l *chatLimiter) fullAt(now time.Time) bool {
	if l.tokens >= ChatBurst {
		return true
	}
	elapsed := now.Sub(l.last)
	return elapsed > 0 && l.tokens+elapsed.Seconds()*ChatRefillPerSecond >= ChatBurst
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

// Chat asks the authoritative simulation to accept and broadcast one world-chat
// line. The limiter belongs to Sim rather than this session-scoped Player: Join
// creates a new Player on reconnect, while the stable identity must keep the bucket.
func (p *Player) Chat(text string) error {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if err := p.cannotActLocked(); err != nil {
		return err
	}
	accepted, err := acceptChat(text)
	if err != nil {
		return err
	}

	now := p.sim.chatNow()
	p.sim.pruneChatLimitersLocked(now)
	limiter := p.sim.chatLimiters[p.playerID]
	if limiter == nil {
		limiter = newChatLimiter(now)
		p.sim.chatLimiters[p.playerID] = limiter
	}
	if !limiter.allow(now) {
		return ErrChatTooFast
	}

	frame := protocol.EncodeChatMessage(protocol.ChatMessage{
		SenderEntityID: p.entityID,
		SenderName:     p.name,
		Text:           accepted,
	})
	p.sim.broadcastLocked(frame)
	return nil
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
