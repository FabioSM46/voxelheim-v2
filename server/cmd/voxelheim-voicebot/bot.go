package main

import (
	"bufio"
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"net"
	"sync/atomic"
	"time"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/transport"
)

// One synthetic session: the client half of a handshake, a heartbeat, and — for the share
// of them that speak — a frame every twenty milliseconds.
//
// **There is no client library in this module to reuse.** Every handshake driver in the
// server tree is a `_test.go` helper in an external test package, so a command cannot
// import one; what it can import is `internal/protocol`'s encoders,
// `internal/transport`'s framing and `internal/ticket`'s minting, which between them are
// the whole of a client. That is deliberate rather than a gap: the wire is the contract,
// and a bot built out of the same three packages the real server admits is a bot that
// proves the contract rather than a mock of it.
//
// **The connection keeps the one-reader-one-writer promise** `transport.Conn` makes. The
// handshake writes before either goroutine starts; after it, [bot.listen] owns the read
// side and [bot.speak] owns the write side, and nothing else touches the socket.

// connBufferSize matches internal/transport's own framed connection, so a bot's socket
// behaves like a session's on the other end of it.
const connBufferSize = 64 << 10

// joinDeadline bounds the handshake. It is generous against the server's five-second
// handshake window on purpose: a thousand sessions arriving at once queue behind each
// other's TLS handshakes, and a bot that gave up early would be reported as a refusal.
const joinDeadline = 60 * time.Second

type bot struct {
	fleet *fleet
	place placement
	name  string

	conn   *tls.Conn
	reader *bufio.Reader

	// entityID is this session's own id, learned from ServerWelcome. A speaker's frames
	// come back to listeners carrying it, which is how a listener knows who it heard —
	// though nothing here needs to: the send instant rides in the frame.
	entityID uint64

	// The three counters the report is built from. Atomic because the write goroutine
	// increments `sent` while the collector reads it at the window boundaries, and the
	// read goroutine increments the other two.
	sent      atomic.Uint64
	heard     atomic.Uint64
	unstamped atomic.Uint64

	// latency is touched only by this bot's read goroutine and merged once, after the run.
	latency histogram

	// limiterRefused is how many of this speaker's own frames the server's allowance would
	// have dropped, predicted by [speakerBucket] from the instants it wrote them at.
	limiterRefused atomic.Uint64

	// The tick observation. EntitySnapshot.server_tick is the simulation's own counter, so
	// the rate it advances at over wall-clock time is the rate Sim.Step actually achieved
	// — see soakReport.tickRate for what that can and cannot say about the tick budget.
	firstTick, lastTick     uint32
	firstTickAt, lastTickAt time.Time
	haveTick                bool

	joined chan struct{}

	// firstVoice, when it is not nil, is handed the first frame relayed to this session.
	//
	// **It is the probe's whole answer and the soak never asks for one**, which is why it
	// is a nil channel there: the soak counts millions of receipts and a non-blocking send
	// on each of them would be work done for nobody. A buffer of one and a send that gives
	// up rather than blocks, because what the probe wants is that a frame arrived, not
	// which of them was first past a scheduler.
	firstVoice chan relayed
}

// join dials, handshakes and returns once the server has welcomed this session.
//
// Hello and the character creation are written back to back before anything is read, which
// is what the session's own TCP test does and what the protocol allows: a client that
// already knows which character it wants need not wait to be shown the list.
func (b *bot) join(ctx context.Context) error {
	dialer := &net.Dialer{Timeout: joinDeadline}
	conn, err := tls.DialWithDialer(dialer, "tcp", b.fleet.addr, b.fleet.tlsConfig())
	if err != nil {
		return fmt.Errorf("dial: %w", err)
	}
	b.conn = conn
	b.reader = bufio.NewReaderSize(conn, connBufferSize)

	credential, err := b.fleet.ticketFor(b.name)
	if err != nil {
		return err
	}

	if err := conn.SetDeadline(time.Now().Add(joinDeadline)); err != nil {
		return fmt.Errorf("arm the join deadline: %w", err)
	}
	hello := protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, b.name, credential)
	creation := protocol.EncodeCreateCharacterRequest(protocol.CreateCharacterRequest{
		Name:          b.name,
		Appearance:    botAppearance(),
		HasAppearance: true,
	})
	for _, frame := range [][]byte{hello, creation} {
		if err := transport.WriteFrame(conn, frame); err != nil {
			return fmt.Errorf("write the handshake: %w", err)
		}
	}

	// Read until the welcome. Everything before it — the character list — is answered
	// already, and everything after it is the world arriving, which the read loop handles.
	for {
		frame, err := transport.ReadFrame(b.reader)
		if err != nil {
			return fmt.Errorf("read the handshake: %w", err)
		}
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		switch envelope.PayloadType() {
		case vnet.PayloadServerWelcome:
			var table flatbuffers.Table
			if !envelope.Payload(&table) {
				return errors.New("the welcome carried no payload")
			}
			var welcome vnet.ServerWelcome
			welcome.Init(table.Bytes, table.Pos)
			b.entityID = welcome.EntityId()
			// The deadline is cleared here rather than left armed: from now on the session
			// is long-lived and the run's context is what ends it.
			if err := conn.SetDeadline(time.Time{}); err != nil {
				return fmt.Errorf("clear the join deadline: %w", err)
			}
			close(b.joined)
			return nil
		case vnet.PayloadServerReject:
			return fmt.Errorf("refused: %s", rejectDetail(envelope))
		default:
			if ctx.Err() != nil {
				return ctx.Err()
			}
		}
	}
}

// listen is the read half, and it is the only goroutine that touches this bot's histogram.
//
// Everything that is not a snapshot or a relayed voice frame is read and dropped on the
// floor: a soak test is not a client, and the chunks, inventories and marker lists a
// session is handed exist here only as the bytes a real listener would have to receive
// while the relay is trying to reach it.
func (b *bot) listen(ctx context.Context) error {
	for {
		frame, err := transport.ReadFrame(b.reader)
		if err != nil {
			if ctx.Err() != nil || transport.IsDisconnect(err) {
				return nil
			}
			return err
		}
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		switch envelope.PayloadType() {
		case vnet.PayloadVoiceHeard:
			b.absorbVoice(envelope)
		case vnet.PayloadEntitySnapshot:
			b.absorbSnapshot(envelope)
		}
	}
}

// absorbVoice counts one receipt and measures how long it took to arrive.
//
// **The Opus is read for its last eight bytes and for nothing else**, and those eight bytes
// are the padding this command wrote — see opus.go. Nothing about a frame is logged, kept
// or written down: the repository's rule that a voice payload never reaches a diagnostic
// holds for a load generator exactly as it holds for the relay.
func (b *bot) absorbVoice(envelope *vnet.Envelope) {
	if !b.fleet.measuring.Load() {
		return
	}
	var table flatbuffers.Table
	if !envelope.Payload(&table) {
		return
	}
	var heard vnet.VoiceHeard
	heard.Init(table.Bytes, table.Pos)

	if b.firstVoice != nil {
		select {
		case b.firstVoice <- relayed{
			speaker:   heard.SpeakerEntityId(),
			sequence:  heard.Sequence(),
			opusBytes: heard.OpusLength(),
		}:
		default:
		}
	}

	sentAt, ok := readSilenceStamp(heard.OpusBytes())
	if !ok {
		b.unstamped.Add(1)
		return
	}
	if !b.fleet.inWindow(sentAt) {
		return
	}
	b.heard.Add(1)
	b.latency.add(time.Since(sentAt))
}

// absorbSnapshot records the simulation's own tick counter against the wall clock.
func (b *bot) absorbSnapshot(envelope *vnet.Envelope) {
	var table flatbuffers.Table
	if !envelope.Payload(&table) {
		return
	}
	var snapshot vnet.EntitySnapshot
	snapshot.Init(table.Bytes, table.Pos)

	if !b.fleet.measuring.Load() {
		return
	}
	now := time.Now()
	if !b.haveTick {
		b.firstTick, b.firstTickAt, b.haveTick = snapshot.ServerTick(), now, true
	}
	b.lastTick, b.lastTickAt = snapshot.ServerTick(), now
}

// speak is the write half: the heartbeat every session owes, and the frames the speakers
// among them send.
//
// **A silent session still sends PlayerInput**, because the server closes a welcomed
// session that has said nothing for its idle window and because that is what a real client
// does every tick. The cost of the heartbeat is part of what the relay is competing with,
// so removing it would measure a server nobody runs.
func (b *bot) speak(ctx context.Context) {
	heartbeat := time.NewTicker(b.fleet.tickInterval)
	defer heartbeat.Stop()

	var frames <-chan time.Time
	if b.place.speaker {
		voice := time.NewTicker(time.Second / time.Duration(b.fleet.opts.frameRate))
		defer voice.Stop()
		frames = voice.C
	}

	packet := make([]byte, len(b.fleet.template))
	copy(packet, b.fleet.template)

	var clientTick uint32
	var sequence uint32
	var bucket speakerBucket
	for {
		select {
		case <-ctx.Done():
			return
		case <-heartbeat.C:
			clientTick++
			if err := transport.WriteFrame(b.conn, protocol.EncodePlayerInput(protocol.PlayerInput{
				ClientTick: clientTick,
			})); err != nil {
				return
			}
		case now := <-frames:
			stampSilenceFrame(packet, now)
			if err := transport.WriteFrame(b.conn, protocol.EncodeVoiceFrame(protocol.VoiceFrame{
				Sequence: sequence,
				Audience: vnet.VoiceAudienceEveryone,
				Opus:     packet,
			})); err != nil {
				return
			}
			sequence++
			refused := bucket.refused(now)
			if b.fleet.inWindow(now) {
				b.sent.Add(1)
				if refused {
					b.limiterRefused.Add(1)
				}
			}
		}
	}
}

func (b *bot) close() {
	if b.conn != nil {
		_ = b.conn.Close()
	}
}

// botAppearance is one valid appearance, reused by every session.
//
// The values are the ones internal/session's own tests use. Appearance.Validate refuses a
// colour with a non-zero top byte and an unknown hair model, so this is a fixture rather
// than a choice.
func botAppearance() protocol.Appearance {
	return protocol.Appearance{
		SkinColor:     0x00E3C4A0,
		ShirtColor:    0x004A5D3B,
		TrousersColor: 0x002B2118,
		ShoesColor:    0x00553311,
		HairModel:     vnet.HairModelBraided,
		HairColor:     0x00B07A32,
	}
}

// rejectDetail is the sentence a ServerReject carries, for the one place this command
// reports a refusal.
func rejectDetail(envelope *vnet.Envelope) string {
	var table flatbuffers.Table
	if !envelope.Payload(&table) {
		return "no detail"
	}
	var reject vnet.ServerReject
	reject.Init(table.Bytes, table.Pos)
	return fmt.Sprintf("%s (%s)", reject.Detail(), vnet.EnumNamesRejectReason[reject.Reason()])
}

// speakerBucket is this command's own copy of the allowance the server charges a speaker,
// run over the instants at which this session actually wrote a frame.
//
// **It exists because the server answers nothing and logs a refusal at Debug.** At a
// hundred speakers relaying to nine hundred and ninety-nine listeners, turning Debug on to
// attribute drops writes one line per dropped delivery — a hundred and forty-five million
// of them in a thirty-second run — which is a load of its own and not a measurement. So the
// one class of drop that depends on *this command's* behaviour rather than the server's is
// predicted here, from the two constants game exports for the purpose, and confirmed
// against the server's own Debug lines on the runs small enough to log.
//
// It is a prediction and the report says so. What makes it a useful one is that it is
// falsifiable in exactly the way an assertion would not be: a run at -server-log-level
// debug prints both numbers, and they either agree or this is wrong.
type speakerBucket struct {
	tokens float64
	last   time.Time
	primed bool
}

// refused advances the bucket to this instant and says whether the server would have spent
// a token on the frame or dropped it.
//
// The rule is game.Player.spendVoiceTokenLocked's: refill at game.VoiceRefillPerSecond,
// clamp at game.VoiceBurst, and a frame costs one token.
func (s *speakerBucket) refused(at time.Time) bool {
	if !s.primed {
		s.tokens, s.last, s.primed = game.VoiceBurst, at, true
	} else {
		s.tokens += at.Sub(s.last).Seconds() * game.VoiceRefillPerSecond
		if s.tokens > game.VoiceBurst {
			s.tokens = game.VoiceBurst
		}
		s.last = at
	}
	if s.tokens < 1 {
		return true
	}
	s.tokens--
	return false
}
