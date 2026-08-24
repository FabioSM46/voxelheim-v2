// Package session owns one client connection's lifetime: the handshake that
// admits it, the goroutines that read and write it, and the identity the server
// assigns it.
//
// It decides nothing about gameplay. A session moves messages between a
// transport.Conn and the simulation; what a message *means* for the world is the
// game's business, which is why nothing here reaches into world state.
package session

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"math"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/transport"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// outboundQueue is how many frames may wait for the writer goroutine before a
// producer blocks. Deep enough that a burst of chunk frames does not stall the
// tick, shallow enough that a client which has stopped reading is noticed.
const outboundQueue = 32

// Config carries the authoritative session parameters announced in
// ServerWelcome. Every field is the server's decision.
type Config struct {
	WorldSeed    int64
	TickRate     uint8
	ChunkSize    uint16
	ViewDistance uint8
	Spawn        [3]float32
}

// Validate enforces, on the producing side, the decoder invariants that
// schemas/handshake.fbs documents for ServerWelcome.
//
// The client is required to reject a malformed welcome; that is no reason for the
// server to be capable of sending one. Checking here means a bad flag value fails
// at startup, where an operator sees it, instead of at the client's decoder.
func (c Config) Validate() error {
	switch {
	case c.TickRate < 1:
		return fmt.Errorf("tick rate must be at least 1, got %d", c.TickRate)
	case c.ChunkSize < 1 || c.ChunkSize > protocol.MaxChunkSize:
		return fmt.Errorf("chunk size must be in 1..%d, got %d", protocol.MaxChunkSize, c.ChunkSize)
	case c.ViewDistance > protocol.MaxViewDistance:
		return fmt.Errorf("view distance must be at most %d, got %d", protocol.MaxViewDistance, c.ViewDistance)
	}

	for axis, value := range c.Spawn {
		v := float64(value)
		if math.IsNaN(v) || math.IsInf(v, 0) {
			return fmt.Errorf("spawn axis %d must be finite, got %v", axis, value)
		}
	}
	return nil
}

// The default read deadlines, and why the idle one is measured in seconds rather
// than minutes.
//
// PlayerInput is the heartbeat. The client sends one every tick — standing still
// and dead included, because the server decides what "no input" means and must be
// told it — so a healthy client is never silent for longer than one tick interval.
// Twenty seconds is therefore hundreds of missed frames rather than a client
// thinking, and the handshake window is shorter still: a connection that has said
// nothing has not even claimed to be a client.
//
// The character window is the odd one out and is measured in minutes, because it is the
// one phase of a connection paced by a person rather than by a machine. Between the
// character list and the selection there is somebody reading names, picking colours and
// typing — and neither of the other two numbers describes that. Five seconds is what a
// connection that has said nothing gets; twenty seconds is what a client that sends a
// heartbeat every tick gets; a character screen sends nothing at all and is not idle.
//
// Two minutes rather than "as long as they like": the claim on that account is already
// held, so a connection parked here is one the same person cannot reconnect past from
// another machine. It is a judgement rather than a measurement, and it is the operator's
// to change — see -character-timeout.
const (
	DefaultHandshakeTimeout = 5 * time.Second
	DefaultCharacterTimeout = 2 * time.Minute
	DefaultIdleTimeout      = 20 * time.Second
	// DefaultLeaveLinger is how long an admitted character remains after every kind
	// of connection end. It is not a flag: ten seconds is a gameplay rule, not an
	// operator tuning knob. Tests may provide a shorter Timeouts.Leave directly.
	DefaultLeaveLinger = 10 * time.Second
)

// Timeouts bounds how long a connection may say nothing.
//
// Deliberately not part of Config: every field of that struct is announced to the
// client in ServerWelcome, and these are the server's business alone. The
// client is never told how long its silence buys it, because a client that needs
// to know is one that is planning to be silent for exactly that long.
//
// A zero duration disables that deadline, which is what net.Conn's zero Time
// means and what the session tests want — they are about admission, not about the
// clock. Validate refuses it, so a *server* never runs without one.
type Timeouts struct {
	// Handshake bounds the first read: the time between a connection arriving and
	// its ClientHello.
	Handshake time.Duration

	// Character bounds the read between the character list and the choice that answers
	// it — a SelectCharacterRequest or a CreateCharacterRequest.
	//
	// **Its own number rather than either of the other two**, because the phase it
	// bounds is the only one a person is inside. Held to the handshake window a player
	// would be disconnected for reading their own character list; held to the idle
	// window they would be disconnected for choosing carefully. What bounds it at all is
	// that the account's single live session is already claimed by the time this phase
	// starts.
	Character time.Duration

	// Idle bounds every read after the welcome, re-armed on each frame.
	Idle time.Duration

	// Leave is the server-owned linger after an in-world session ends. It begins only
	// after the idle deadline (or another disconnect) has ended the connection, so the
	// two timers are sequential and an idle timeout can never reap the body early.
	// Zero disables the wait in tests; production always uses DefaultLeaveLinger.
	Leave time.Duration
}

// DefaultTimeouts is the policy the flags default to.
func DefaultTimeouts() Timeouts {
	return Timeouts{
		Handshake: DefaultHandshakeTimeout,
		Character: DefaultCharacterTimeout,
		Idle:      DefaultIdleTimeout,
		Leave:     DefaultLeaveLinger,
	}
}

// Validate enforces the rules a deadline policy has to obey.
//
// It lives here rather than in the flag parser so that the server runs under the
// same rule the operator was checked against, instead of a copy of it that can
// drift. A handshake window longer than the idle window is refused because it
// cannot mean anything: the first read would outlive the budget every later read
// is held to, so the stricter number would apply only to clients that had already
// proved they were talking.
//
// **The character window is held to the same floor and to no ceiling**, and the absence
// of one is deliberate. It must be at least the handshake window by the argument above —
// a peer that has presented a ticket this server accepted must not be held to a stricter
// number than one that has presented nothing — and it is *expected* to exceed the idle
// window, because a character screen is not an idle session. A rule tying the two
// together would be a rule about two different things.
func (t Timeouts) Validate() error {
	switch {
	case t.Handshake <= 0:
		return fmt.Errorf("handshake timeout must be greater than zero, got %s", t.Handshake)
	case t.Character <= 0:
		return fmt.Errorf("character timeout must be greater than zero, got %s", t.Character)
	case t.Idle <= 0:
		return fmt.Errorf("idle timeout must be greater than zero, got %s", t.Idle)
	case t.Leave <= 0:
		return fmt.Errorf("leave linger must be greater than zero, got %s", t.Leave)
	case t.Handshake > t.Idle:
		return fmt.Errorf("handshake timeout %s must not exceed the idle timeout %s", t.Handshake, t.Idle)
	case t.Handshake > t.Character:
		return fmt.Errorf("handshake timeout %s must not exceed the character timeout %s", t.Handshake, t.Character)
	}
	return nil
}

// phase is how far one connection has got through the handshake, and it is what decides
// which messages are legal and which deadline the next read is armed with.
//
// **Three phases where V6 had two**, and the middle one is what this contract added:
// ClientHello is answered with ServerCharacterList, and only a selection or a creation
// earns a ServerWelcome. schemas/handshake.fbs holds the reason — the welcome's spawn
// belongs to a character, so it cannot be sent before there is one.
type phase uint8

const (
	// phaseHello is a connection that has said nothing yet. It is bounded by the
	// shortest window and is answered by a character list or by a refusal.
	phaseHello phase = iota

	// phaseCharacter is an account admitted and choosing. Its ticket has been verified
	// and its single live session claimed; what it owes the server is one message.
	phaseCharacter

	// phaseInWorld is a welcomed session: it has a body in the simulation, and every
	// gameplay message becomes legal at exactly this moment and not before.
	phaseInWorld
)

// String names the phase in a log line. Without it slog would print the number, and the
// number is the one thing about a phase nobody can read.
func (p phase) String() string {
	switch p {
	case phaseHello:
		return "hello"
	case phaseCharacter:
		return "character"
	case phaseInWorld:
		return "in-world"
	default:
		return "phase(" + strconv.Itoa(int(p)) + ")"
	}
}

// Welcome builds the last message of a handshake: the one that says a character is in
// the world.
//
// A pure function of the server's configuration and the character that was chosen: no
// I/O, no state, no clock. The connection lifecycle around it is hard to test
// exhaustively; what a welcome announces is the part that must never drift, so it lives
// where a test can read every field of it.
//
// **self is the character this session plays, already settled.** Settling one verifies a
// ticket, reads the player store, claims exclusivity and answers a choice the player
// made — state, I/O and several decisions that can be refused, none of which may happen
// in here without making this function untestable in exactly the way it exists to avoid.
// So Serve settles it first and hands the answer in; [Identities.Admit],
// [Identities.Select] and [Identities.Create] are where those rules live and where they
// are tested.
//
// **This is why it takes no message.** Through V6 a welcome answered the hello, so the
// hello was an argument and this function refused one it could not read. It answers a
// selection or a creation now, and what a *hello* is refused for is asked one phase
// earlier — see [unspeakable], which Serve calls before a ticket is verified.
//
// **The welcome's spawn is where the player will actually stand**, which for a returning
// character is the position their record holds and not the world spawn.
// schemas/handshake.fbs says spawn is the position the player begins at, and a client
// that placed itself at the world spawn and was then corrected by the first snapshot
// would show every reconnect as a teleport. That is the whole reason the record is
// loaded while the character is being settled: this function is pure, so the answer has
// to arrive with it.
func Welcome(cfg Config, entityID uint64, self Resolved) []byte {
	return protocol.EncodeServerWelcome(protocol.Welcome{
		EntityID:       entityID,
		Spawn:          placementSpawn(cfg, self),
		WorldSeed:      cfg.WorldSeed,
		TickRate:       cfg.TickRate,
		ChunkSize:      cfg.ChunkSize,
		ViewDistance:   cfg.ViewDistance,
		InventorySlots: protocol.InventorySlots,
		HotbarSlots:    protocol.HotbarSlots,
		// **The retired field, filled rather than dropped.** V7 settles identity from
		// `session_ticket`, so this server mints no tokens and has nothing of its own to
		// say here — but schemas/handshake.fbs still requires the field to be present and
		// exactly protocol.PlayerTokenLen bytes on every accepted handshake, and a
		// decoder is required to treat any other length as a protocol error. Zeroes are
		// therefore the honest value: the right shape, and not a credential.
		//
		// Nothing can be resumed with them. A V7 server reads past `player_token` on the
		// way in, so whatever a client stores from here names nobody and admits nobody —
		// and no V6 client is on the far end of this frame to store it, because the
		// version check two blocks up refused them before this was built.
		PlayerToken: make([]byte, protocol.PlayerTokenLen),
		// The world's clock, read from the constants that own it exactly as the two slot
		// counts above are read from protocol's. Deliberately **not** fields of [Config]:
		// everything in that struct is a decision an operator makes, and this is one the
		// design makes — putting it there would invent a knob and then have to validate
		// it. There is one copy of these three numbers on this side, in internal/game,
		// and this is where they leave it.
		DayLengthTicks:  game.DayLengthTicks,
		NightStartTicks: game.NightStartTicks,
		NightEndTicks:   game.NightEndTicks,
	})
}

// unspeakable is the half of a handshake decided from the message alone: is this a
// hello at all, and does it speak this protocol.
//
// **Asked before a ticket is verified**, and the order is what forced it out of the
// welcome. Admission happens between the decode and the answer, so with the version
// check living where the welcome was built a client speaking an older protocol — which
// presents no ticket, because a ticket is what V7 added — was refused for the ticket and
// never told about the version. That is the one refusal it could have acted on, replaced
// by one it cannot: "sign in again" to a client that would then present a ticket its own
// protocol has no field for.
//
// It has one caller now. [Welcome] no longer takes a message at all, because the message
// it answers is the character choice rather than the hello — so the rule is asked once,
// where the hello is, instead of twice.
//
// It is also the cheaper question. Everything here is a comparison; everything after it
// is an Ed25519 verification on bytes chosen by a connection nobody has authenticated.
func unspeakable(msg protocol.Message) (refusal []byte, refused bool) {
	if msg.Kind != vnet.PayloadClientHello || msg.ClientHello == nil {
		return protocol.EncodeServerReject(
			vnet.RejectReasonBAD_REQUEST,
			fmt.Sprintf("expected %s as the first message, got %s", vnet.PayloadClientHello, msg.Kind),
		), true
	}

	if got := msg.ClientHello.ProtocolVersion; got != vnet.ProtocolVersionCurrent {
		// Covers the absent-field case too: a hello with no version decodes as
		// ProtocolVersion.Unknown, which is not Current, so it lands here.
		return protocol.EncodeServerReject(
			vnet.RejectReasonPROTOCOL_MISMATCH,
			fmt.Sprintf("server speaks protocol %d, client speaks %d", vnet.ProtocolVersionCurrent, got),
		), true
	}
	return nil, false
}

// placementSpawn is the position the welcome announces: the stored one for a returning
// player, the world spawn for everyone else.
//
// Narrowed to float32 here and nowhere else. The simulation keeps the float64 it was
// restored with, so the rounding lives in the frame rather than folding back into the
// server's own arithmetic — the same direction game.toWire narrows in, for the same
// reason. cfg.Spawn has already been checked finite by Config.Validate, and a stored
// position bounded to the world's edge by game.Life.Validate — the stronger of the two
// claims, and the one this narrowing actually needs: a merely finite float64 is not
// enough, because 1e300 is finite and float32(1e300) is +Inf. So neither source can put
// a non-finite spawn in a welcome.
func placementSpawn(cfg Config, self Resolved) [3]float32 {
	if self.Life == nil {
		return cfg.Spawn
	}
	return [3]float32{
		float32(self.Life.Pos[0]),
		float32(self.Life.Pos[1]),
		float32(self.Life.Pos[2]),
	}
}

// Serve runs one connection until it ends.
//
// The shape is one reader (this goroutine) and one writer (spawned below), which
// is the only arrangement transport.Conn promises to survive. Serve does not
// close conn: the caller owns it, because shutdown has to be able to close it
// from outside in order to unblock this read.
//
// A clean disconnect, a refused handshake and an expired read deadline all return
// nil — they are how sessions normally end. An error means the peer broke the
// protocol or the write side failed.
//
// The deadline is the third of those and the newest, so it is worth saying why it
// is not an error: a session that goes quiet has ended, and the server is the one
// that decided when. Returning an error would have the caller log
// "session ended with an error" for the most ordinary way a dead connection is
// noticed, which is how a warning stops being read.
func Serve(ctx context.Context, conn transport.Conn, cfg Config, timeouts Timeouts, chunks *world.Cache, sim *game.Sim, peers *Registry, identities *Identities, entityID uint64, log *slog.Logger) (err error) {
	out := make(chan []byte, outboundQueue)

	// A session-scoped context, so teardown can stop the streamer without waiting
	// for the server to shut down.
	sctx, stopStreaming := context.WithCancel(ctx)

	var (
		wg           sync.WaitGroup
		streaming    sync.WaitGroup
		writeFailure error
		player       *game.Player
		streamer     *Streamer
		leavingAt    time.Time

		// Declared up here rather than beside the read loop because the deferred
		// teardown below reads them, and a closure can only see what already exists.
		//
		// account is the account this connection was admitted as and claimed says the
		// claim on it is this session's to release — both settled at the hello, before a
		// character exists. self is the character it went on to play, and it is filled in
		// one phase later. current is what separates a session that joined from one that
		// was refused on the way: only a welcomed session leaves a record behind, because
		// a refused client never entered the world and a file for it would be a life
		// nobody ever lived.
		current     phase
		claimed     bool
		account     Admitted
		self        Resolved
		displayName string
	)
	wg.Add(1)
	go func() {
		defer wg.Done()
		for frame := range out {
			if writeFailure != nil {
				// Keep draining: the reader must never block on a dead writer.
				continue
			}
			if wErr := conn.WriteFrame(frame); wErr != nil {
				writeFailure = wErr
				// Closing here is what unblocks this session's reader, which then
				// closes out and lets this goroutine finish.
				_ = conn.Close()
			}
		}
	}()

	defer func() {
		// Every in-world ending converges on the same authoritative state: a polite
		// request, EOF, an idle deadline, a dead writer and a process-level socket close
		// all leave the character present but inert. A polite request starts the clock
		// before its acknowledgement; every other path starts it here.
		if current == phaseInWorld && player != nil {
			if leavingAt.IsZero() {
				player.BeginLeaving()
				leavingAt = time.Now()
			}
			if remaining := time.Until(leavingAt.Add(timeouts.Leave)); remaining > 0 {
				timer := time.NewTimer(remaining)
				select {
				case <-timer.C:
				case <-ctx.Done():
					// The world itself is stopping, so there is no simulation for a body
					// to remain visible in. Persistence still runs below before release.
					if !timer.Stop() {
						select {
						case <-timer.C:
						default:
						}
					}
				}
			}
		}

		// Order matters, and there are now four producers to stop before the channel
		// they produce into can be closed. Closing it first would make a send on a
		// closed channel — a panic, in a goroutine, taking the process with it.
		//
		// The two that send from *another* goroutine go first, because they are the ones
		// this function does not wait on. Each stops through the same shape of guarantee:
		// Sim.Leave takes the lock Sim.Step holds for a whole tick, and
		// Registry.Unsubscribe takes the lock BroadcastChunk holds while it sends, so once
		// both have returned nothing outside this function can still reach the queue.
		//
		// Then stop the session-scoped workers (streaming and mining), wait for both to
		// stop producing, and only then close the channel.
		sim.Leave(player)
		peers.Unsubscribe(entityID)
		stopStreaming()
		streaming.Wait()
		close(out)
		wg.Wait() // also publishes writeFailure to this goroutine

		// The read loop's question, asked on the other side of the session. A peer that
		// goes away mid-write is the same event as one that goes away mid-read; all that
		// differs is which goroutine noticed first. So it ends the session the way the read
		// path ends it, with nil. Promoted unconditionally, it meant every ordinary
		// disconnect that landed while a chunk was still in flight reached the caller as
		// "session ended with an error" and was logged at WARN — a warning that fires for
		// the most routine thing a player does, and one that therefore stops being read.
		//
		// This narrows what counts as a failure; it does not stop reporting failures. An
		// error IsDisconnect does not recognise is still returned, because then the write
		// really did fail and nothing else here is going to say so.
		if err == nil && writeFailure != nil {
			if transport.IsDisconnect(writeFailure) {
				log.Debug("write failed on a connection that had already gone", "error", writeFailure)
			} else {
				err = fmt.Errorf("session: write: %w", writeFailure)
			}
		}

		// The identity goes last, and everything above is why it can.
		//
		// sim.Leave has returned, so no tick still holds this player; the record is
		// written before the claim is released, so a client reconnecting the instant
		// this function returns is neither refused for a session that has gone nor
		// served a record that is still being written. That order is settled.
		//
		// **The life is captured here rather than earlier, and that is the point.**
		// Player.Record reads the player's own fields, which sim.Leave does not take
		// away — so what is written is the last thing the simulation decided about this
		// player, with no tick able to be part-way through changing it. A player who was
		// dead when they left is captured as their respawn would have left them, penalty
		// and all: quitting mid-death neither escapes it nor pays for it twice.
		//
		// This runs on every path out of Serve, an expired read deadline included —
		// which is what makes an idle session save its life and give its identity back
		// rather than hold both until the process restarts.
		if claimed {
			if current == phaseInWorld && player != nil {
				if rErr := identities.Remember(self, player.Record()); rErr != nil {
					// Logged rather than returned: the session is over and the connection
					// was fine, so failing it would report the wrong thing. Loud, because
					// this is the line that says a player's record did not survive.
					log.Error("the player's record was not saved",
						"player_id", self.ID.Short(), "error", rErr)
				}
			}
			// The account's, not the character's: the claim was taken when the ticket
			// verified, which is one phase before there was a character to name it by.
			identities.Release(account.ID)
		}
	}()

	// enqueue hands a frame to the writer goroutine. It blocks while the queue is
	// full, and gives up when the session ends, so a streamer can never outlive the
	// connection it is streaming to.
	enqueue := func(frame []byte) error {
		// Cancellation is checked before the select, not only inside it: a select with
		// both cases ready picks at random, so a finished session with room in its
		// queue would keep accepting frames roughly half the time — which delays
		// teardown for as long as the streamer keeps finding space.
		if err := sctx.Err(); err != nil {
			return err
		}

		select {
		case out <- frame:
			return nil
		case <-sctx.Done():
			return sctx.Err()
		}
	}

	// trySend is enqueue's non-blocking counterpart for simulation feedback. A
	// snapshot or positive mining-progress fraction describes one tick and is
	// worthless by the time a full queue drains, so a tick must never wait for room:
	// waiting would stall every other player's simulation in order to deliver
	// something already stale. A mining reset is different but keeps this seam:
	// game.Player retains its zero and retries it before later progress until trySend
	// accepts it. A chunk is not replaced by a later one, so streaming keeps the
	// blocking path.
	//
	// What keeps this safe against `close(out)` is the teardown order above, and only
	// that: Sim.Leave returns once no tick can be inside this function and no later
	// tick will enter it. There is deliberately **no** "is this session ending?" check
	// here, unlike in enqueue. It would earn nothing — one last snapshot queued for a
	// session nobody will read is discarded by the writer a moment later — and it would
	// mask the exact reordering that TestSnapshotsStopBeforeTheOutboundQueueIsClosed
	// exists to catch, turning a panic into a test that passes.
	trySend := func(frame []byte) bool {
		select {
		case out <- frame:
			return true
		default:
			return false
		}
	}

	// refuse answers an admission error and says how the session ends.
	//
	// **Two kinds of failure, and only one of them is the client's.** A [Refused] is a
	// refusal the contract has a code for — a ticket this server will not admit, an
	// account already playing, a name somebody else has, a character this account may
	// not play — and it is answered with a ServerReject and a clean close, so the
	// client learns why. Anything else is *this server* failing: it could not read its
	// own player store, or it cannot verify a ticket at all, and RejectReason has no
	// member that says so. Sending one that says something else would be worse than
	// saying nothing, so the error is returned and the connection ends unanswered.
	//
	// One closure rather than the same fifteen lines in both phases, because what must
	// not drift between them is which failures get a reply.
	refuse := func(cause error) error {
		var refused *Refused
		if !errors.As(cause, &refused) {
			return cause
		}

		out <- protocol.EncodeServerReject(refused.Reason, refused.Detail)
		// **The cause is logged and never sent**, and that asymmetry is the point: five
		// different ticket refusals leave this server as one identical frame, and so do
		// the two ways a character can be one this account may not play — so a client
		// learns nothing it could ask this server about somebody else's credential or
		// somebody else's character, while an operator reading a log can tell them
		// apart. Nothing in a cause quotes a ticket's bytes: internal/ticket's refusals
		// name lengths, world ids and expiry times, all of which are safe to write down.
		attrs := []any{"reason", refused.Reason.String(), "detail", refused.Detail}
		if refused.Cause != nil {
			attrs = append(attrs, "cause", refused.Cause.Error())
		}
		log.Info("handshake refused", attrs...)
		return nil
	}

	// armRead bounds the read that follows it. The zero Time clears the deadline,
	// which is what a zero duration asks for.
	armRead := func(window time.Duration) error {
		var at time.Time
		if window > 0 {
			at = time.Now().Add(window)
		}
		if sErr := conn.SetReadDeadline(at); sErr != nil {
			return fmt.Errorf("session: arm the read deadline: %w", sErr)
		}
		return nil
	}

	lastFrame := time.Now()
	for {
		// Armed before every read, which is the same thing as re-armed after every
		// frame and is one call site instead of two. Each phase gets its own window, and
		// each is measured from *after* the previous phase's own work rather than from
		// the frame that started it.
		window := timeouts.Handshake
		switch current {
		case phaseCharacter:
			window = timeouts.Character
		case phaseInWorld:
			window = timeouts.Idle
		}
		if aErr := armRead(window); aErr != nil {
			// Setting a deadline fails on a connection that has already been closed,
			// which is a disconnect noticed one call early rather than a fault.
			if transport.IsDisconnect(aErr) {
				log.Info("client disconnected", "phase", current.String())
				return nil
			}
			return aErr
		}

		frame, rErr := conn.ReadFrame()
		if rErr != nil {
			// Asked before IsDisconnect, which also answers for a deadline: both end the
			// session the same way, and only this branch knows which sentence to log.
			if transport.IsTimeout(rErr) {
				switch current {
				case phaseInWorld:
					log.Info("session idle",
						"silent_for", time.Since(lastFrame).Round(time.Millisecond).String(),
						"idle_timeout", timeouts.Idle.String())
				case phaseCharacter:
					// Nothing is written back here either, and for the same reason: the
					// client was answered with a character list and then said nothing, so
					// there is no message for a ServerReject to be the answer to.
					log.Info("no character was chosen; closing without a reply",
						"player_id", account.ID.Short(),
						"character_timeout", timeouts.Character.String())
				default:
					// Nothing is written back. There is no reply to a client that has not
					// spoken, and ServerReject answers a message rather than a silence.
					log.Info("handshake timed out; closing without a reply",
						"handshake_timeout", timeouts.Handshake.String())
				}
				return nil
			}
			if transport.IsDisconnect(rErr) {
				log.Info("client disconnected", "phase", current.String())
				return nil
			}
			return fmt.Errorf("session: read: %w", rErr)
		}
		lastFrame = time.Now()

		msg, dErr := protocol.Decode(frame)
		if dErr != nil {
			// Undecodable input is not recoverable: there is no way to resynchronise
			// a stream whose framing we can no longer trust.
			log.Warn("closing connection on undecodable frame", "error", dErr, "bytes", len(frame))
			return fmt.Errorf("session: decode: %w", dErr)
		}

		if current == phaseHello {
			// **Is this a hello, and does it speak this protocol** — asked before a
			// ticket is verified, because a client speaking an older protocol has no
			// ticket to present and would otherwise be told about the one thing it
			// cannot fix instead of the one thing it can. See unspeakable.
			if refusal, refused := unspeakable(msg); refused {
				out <- refusal
				log.Info("handshake refused", "kind", msg.Kind.String())
				// The deferred close drains the refusal before the caller closes the
				// connection, so the client learns why.
				return nil
			}

			// The account is admitted between deciding the hello is legible and
			// answering it, which is the one place it can: the answer is this account's
			// character list, and only the store knows what is in it. The check above
			// guarantees the hello is here.
			admittedAccount, aErr := identities.Admit(msg.ClientHello)
			if aErr != nil {
				return refuse(aErr)
			}
			account, claimed = admittedAccount, true

			// **The list, not a welcome.** There is no body yet and no spawn to announce,
			// because which character is playing has not been said — schemas/handshake.fbs
			// puts the choice here for exactly that reason. An empty list is a legal
			// answer and not a refusal: it says the only way forward is a creation.
			out <- protocol.EncodeServerCharacterList(account.list())
			current = phaseCharacter

			// The first line that says who is on the far end, and on a connection that
			// never chooses it is the only one. The player id is the first 8 hex
			// characters of a digest; the account it hashes is never printed at any
			// level, on any path, nor is the ticket that named it.
			log.Info("account admitted; waiting for a character",
				"player_id", account.ID.Short(),
				"characters", len(account.Characters))
			continue
		}

		if current == phaseCharacter {
			resolved, cErr := chooseCharacter(msg, account, identities)
			if cErr != nil {
				return refuse(cErr)
			}
			self = resolved

			// The welcome answers the choice rather than the hello, and it is the first
			// moment every field in it is true: the spawn is this character's, because
			// there is finally a character to have one.
			out <- Welcome(cfg, entityID, self)
			current = phaseInWorld
			// **The character's name, not the hello's.** The hello carries a display name
			// and this server no longer reads it: what a player is called here is the name
			// their character was created under, which is the one that is unique on this
			// world and the one every other session sees.
			displayName = self.Name
			// entity_id is already on the logger the caller passed in; repeating it
			// here would print it twice. The character id is a number this server minted
			// and names nobody on its own.
			log.Info("session admitted",
				"player_name", displayName,
				"player_id", self.ID.Short(),
				"character", self.Character.String(),
				"returning", self.Returning)

			// The player exists in the world from here on. Joining before streaming
			// starts is what gives the streamer its first coordinate, and it is also
			// what makes "input before the simulation admitted the player" unreachable
			// rather than merely unlikely.
			// cfg.Spawn and not the welcome's spawn: what Join is given is the *join*
			// spawn, which is also where a player with no tent comes back to when they
			// die. self.Life is what moves them somewhere else, and restoring a position
			// is not the same thing as moving somebody's respawn point to wherever they
			// happened to log out.
			//
			// The appearance goes in beside the life and comes from the same place: the
			// stored character. Nothing the client said at any point in this handshake
			// reaches it, and a creation's appearance reaches it only by having been
			// written down first.
			admitted, jErr := sim.Join(entityID, self.ID, cfg.Spawn, self.Appearance, self.Life, trySend)
			if jErr != nil {
				return fmt.Errorf("session: join the simulation: %w", jErr)
			}
			player = admitted

			// Welcome first, then the whole inventory before streaming can fill the queue
			// with chunks — the starter pack, or the restored one, through this one path
			// either way. Authoritative state is sent once on every join, so a reconnect
			// never keeps a previous session's local mirror.
			if iErr := enqueue(protocol.EncodeInventoryState(admitted.InventoryState())); iErr != nil {
				return fmt.Errorf("session: send the inventory on join: %w", iErr)
			}

			// Built here rather than beside enqueue because it needs the player: a repair
			// has to be able to ask for a view diff, and the only thing that can ask is
			// the doorbell the tick loop rings — game.Player.WakeStreaming. Assigned to
			// the outer variable, once, before the goroutine below starts reading it and
			// before any post-handshake frame can reach the handler that uses it.
			streamer = NewStreamer(chunks, cfg.ViewDistance, enqueue, admitted.WakeStreaming, time.Now, log)

			// Follow the player from its own goroutine. Two reasons, and the second is
			// structural: the initial view is hundreds of frames, and producing them
			// from the read loop would leave the session unable to notice a disconnect
			// until the last chunk had been written — the client gone and the server
			// still busy talking to it. And Streamer.MoveTo calls Cache.Get, which
			// generates on a miss, so it can never run on the tick goroutine.
			streaming.Add(1)
			go func() {
				defer streaming.Done()
				followPlayer(sctx, admitted, streamer, log)
			}()

			// Registered as a broadcast target only now, and with the streamer's own view:
			// what this session holds is exactly what the streamer has managed to send, so
			// there is one copy of that set and no second one to fall out of step. Before
			// this point the session holds nothing, so there is nothing to send it.
			peers.Subscribe(entityID, streamer.View(), trySend)

			// Mining completion may wait on Editor and therefore cannot run on Step. One
			// session-scoped worker consumes the tick's bounded handoff, applies the shared
			// break path, and delivers the resulting world/inventory state. It starts only
			// after subscription so the mining session receives its own BlockUpdate by the
			// same broadcast rule as every observer.
			streaming.Add(1)
			go func() {
				defer streaming.Done()
				followMining(sctx, admitted, peers, enqueue, log)
			}()
			continue
		}

		if hErr := handlePostHandshake(ctx, msg, player, streamer, peers, enqueue, log); hErr != nil {
			if errors.Is(hErr, errLeaveRequested) {
				// Inert before the acknowledgement is queued: once the server accepts the
				// request, no input already behind it can become one last action.
				player.BeginLeaving()
				leavingAt = time.Now()
				if sErr := enqueue(protocol.EncodeLeaveStarted(timeouts.Leave)); sErr != nil {
					return fmt.Errorf("session: announce the leave: %w", sErr)
				}
				log.Info("character is leaving", "linger", timeouts.Leave.String())
				return nil
			}
			return hErr
		}

		if ctx.Err() != nil {
			log.Info("session stopping: server is shutting down")
			return nil
		}
	}
}

// chooseCharacter answers the one message the character phase is waiting for.
//
// **Two payloads are legal here and nothing else is.** A selection names a character the
// account already owns; a creation makes one and plays it, because creation and
// selection are one step in this contract. Everything else — a hello arriving twice, a
// PlayerInput from a client that has decided it is already in the world, a payload only
// a server sends — is answered with BAD_REQUEST and a closed connection, which is what
// [unspeakable] does one phase earlier and for the same reason: the handshake is the one
// place a refusal has a reply payload to say so in, and a client told nothing cannot
// tell a refusal from a lost packet.
//
// It decides nothing itself. Whether an id names a character this account may play, and
// whether a name and a face may be written down, are [Identities]' answers against a
// store this function cannot see.
func chooseCharacter(msg protocol.Message, account Admitted, identities *Identities) (Resolved, error) {
	switch msg.Kind {
	case vnet.PayloadSelectCharacterRequest:
		if msg.SelectCharacter == nil {
			// Unreachable: Decode sets the payload for this kind or fails the frame.
			// Stated rather than dereferenced, because the alternative to an answer here
			// is a nil-pointer panic in the goroutine holding a socket.
			return Resolved{}, malformedChoice(msg.Kind)
		}
		return identities.Select(account, persist.CharacterID(msg.SelectCharacter.CharacterID))

	case vnet.PayloadCreateCharacterRequest:
		if msg.CreateCharacter == nil {
			// Unreachable for the same reason, and doubly so: Decode refuses a creation
			// carrying no appearance outright, so a frame that reaches here has one.
			return Resolved{}, malformedChoice(msg.Kind)
		}
		return identities.Create(account, msg.CreateCharacter.Name, msg.CreateCharacter.Appearance)

	default:
		return Resolved{}, malformedChoice(msg.Kind)
	}
}

// malformedChoice is the answer to a message that is not one of the two the character
// phase accepts. It names what arrived, because a client whose build is sending the
// wrong thing is the only party that can fix it.
func malformedChoice(kind vnet.Payload) *Refused {
	return &Refused{
		Reason: vnet.RejectReasonBAD_REQUEST,
		Detail: fmt.Sprintf("expected %s or %s while a character is being chosen, got %s",
			vnet.PayloadSelectCharacterRequest, vnet.PayloadCreateCharacterRequest, kind),
	}
}

// followPlayer streams the chunks around wherever the simulation says the player is,
// for as long as the session lasts.
//
// The consumer half of the seam game.chunkFeed describes: the tick loop publishes the
// authoritative chunk coordinate and this goroutine does the waiting, because
// generating terrain on the tick goroutine would cost every connected player a tick.
// The position it follows is the server's own answer — there is no message in which
// the client says where it is.
func followPlayer(ctx context.Context, player *game.Player, streamer *Streamer, log *slog.Logger) {
	for {
		center, err := player.NextChunk(ctx)
		if err != nil {
			// The only error is the session ending, which is not worth a line.
			return
		}

		if sErr := streamer.MoveTo(ctx, center); sErr != nil {
			if ctx.Err() == nil {
				log.Warn("streaming the view failed", "center", center, "error", sErr)
			}
			return
		}
	}
}

// followMining moves completed hardness work off the tick. NextMining is a bounded,
// non-blocking handoff from Step; CompleteMining may generate/compose a chunk and is
// therefore allowed to wait here on the session's own worker.
func followMining(ctx context.Context, player *game.Player, peers *Registry, send func([]byte) error, log *slog.Logger) {
	for {
		completion, err := player.NextMining(ctx)
		if err != nil {
			return
		}

		result, err := player.CompleteMining(ctx, completion)
		if err != nil {
			if ctx.Err() == nil {
				log.Debug("refusing completed mining write", "pos", completion.Pos(), "reason", err.Error())
			}
			continue
		}

		frame := protocol.EncodeBlockUpdate(protocol.BlockUpdate{Pos: result.Pos, BlockID: uint16(result.Block)})
		notified := peers.BroadcastChunk(result.Chunk, frame)
		if result.Inventory != nil {
			if err := send(protocol.EncodeInventoryState(*result.Inventory)); err != nil {
				return
			}
		}
		log.Debug("mining completed",
			"pos", result.Pos,
			"chunk", result.Chunk,
			"sessions_notified", notified,
		)
	}
}

// handlePostHandshake routes a message from an admitted client.
//
// Direction is a protocol rule rather than a type rule — both sides share one
// union — so a client sending a server-only payload is a protocol violation and
// the connection ends.
func handlePostHandshake(ctx context.Context, msg protocol.Message, player *game.Player, streamer *Streamer, peers *Registry, send func([]byte) error, log *slog.Logger) error {
	switch msg.Kind {
	case vnet.PayloadPlayerInput:
		if player == nil || msg.PlayerInput == nil {
			// Unreachable: this function is only called on an admitted session, which
			// has a player, and Decode sets PlayerInput for this kind or fails. Stated
			// rather than dereferenced, because the alternative to a log line here is a
			// nil-pointer panic in the one goroutine that is holding a socket.
			log.Debug("player input arrived with no player to apply it to; discarding")
			return nil
		}

		// Refused input is dropped and logged, never applied, and never fatal. The
		// frame was well formed — the stream is still trustworthy — and only a value is
		// wrong: a non-finite axis, or a client tick that is not newer than the last
		// one accepted. Ending the connection over that would disconnect a merely buggy
		// client mid-game for something with no consequence.
		if sErr := player.Submit(*msg.PlayerInput); sErr != nil {
			log.Debug("discarding player input", "reason", sErr.Error(), "client_tick", msg.PlayerInput.ClientTick)
		}
		return nil

	case vnet.PayloadBlockEditRequest:
		if player == nil || msg.BlockEditRequest == nil {
			// Unreachable for the same reason as the PlayerInput case above, and stated for
			// the same reason: the alternative to a log line is a nil dereference in the
			// goroutine holding a socket.
			log.Debug("block edit arrived with no player to attribute it to; discarding")
			return nil
		}

		// Resolved on this goroutine, which means the read loop pauses for as long as it
		// takes. That is acceptable where the initial view's hundreds of chunks were not:
		// this waits on at most *one* chunk, and almost always on none, because the target
		// is within EditReach of a player whose own chunk was streamed long ago.
		result, eErr := player.Edit(ctx, *msg.BlockEditRequest)
		if eErr != nil {
			if errors.Is(eErr, game.ErrBreakActionWithdrawn) {
				return fmt.Errorf("session: %w: %w", protocol.ErrMalformed, eErr)
			}
			// Refusal is silence plus a debug line, exactly as a refused PlayerInput is.
			// There is no rejection message in the contract and no acknowledgement of any
			// kind: the client learns its edit did not apply by not seeing it apply.
			log.Debug("refusing block edit",
				"reason", eErr.Error(),
				"pos", msg.BlockEditRequest.Pos,
				"action", msg.BlockEditRequest.Action.String(),
				"client_tick", msg.BlockEditRequest.ClientTick,
			)
			return nil
		}

		// One encode for every recipient: the frame is immutable and the writer goroutines
		// only read it.
		frame := protocol.EncodeBlockUpdate(protocol.BlockUpdate{Pos: result.Pos, BlockID: uint16(result.Block)})
		// On its own line rather than inside the log call below. Arguments to slog are
		// evaluated whatever the level, so nesting it would work — and would read as though
		// the world stopped being broadcast when somebody turned debug logging off.
		notified := peers.BroadcastChunk(result.Chunk, frame)

		if result.Inventory != nil {
			// Unlike a snapshot, an inventory state is not superseded on the next tick.
			// Use the blocking session send so a full queue cannot leave the client with a
			// permanently stale count.
			if iErr := send(protocol.EncodeInventoryState(*result.Inventory)); iErr != nil {
				return fmt.Errorf("session: send changed inventory: %w", iErr)
			}
		}

		log.Debug("block edit applied",
			"pos", result.Pos,
			"block", uint16(result.Block),
			"chunk", result.Chunk,
			"sessions_notified", notified,
		)
		return nil

	case vnet.PayloadChunkResendRequest:
		if streamer == nil || msg.ChunkResendRequest == nil {
			// Unreachable for the same reason as the two cases above, and stated for the
			// same reason: this function only runs on an admitted session, which has a
			// streamer, and Decode sets the payload for this kind or fails.
			log.Debug("chunk resend request arrived with nothing to repair; discarding")
			return nil
		}

		request := *msg.ChunkResendRequest
		if !request.HasCoord {
			// An absent struct field decodes as null, and chunk (0, 0, 0) is a real chunk:
			// reading a missing coordinate as the origin would resend terrain nobody named.
			// schemas/world.fbs states it as a decoder invariant.
			log.Debug("refusing chunk resend", "reason", "the request named no coordinate")
			return nil
		}

		// Refusal is silence plus a debug line, exactly as a refused edit is. There is no
		// rejection message in the contract and no acknowledgement of any kind: an
		// honoured request is answered by the ChunkData that follows it, and a refused one
		// by the absence of one. Never fatal — a request for a chunk this session may not
		// have is a wrong value in a well-formed frame, and the stream is still
		// trustworthy.
		coord := fromProtocolCoord(request.Coord)
		if rErr := streamer.Resend(coord); rErr != nil {
			log.Debug("refusing chunk resend", "reason", rErr.Error(), "coord", coord)
			return nil
		}

		log.Debug("chunk resend accepted", "coord", coord)
		return nil

	case vnet.PayloadMineRequest:
		if player == nil || streamer == nil || msg.MineRequest == nil {
			// Decode either supplies the value or fails the frame. Keep the guard so a
			// malformed Message constructed inside the process cannot panic this loop.
			log.Debug("mine request arrived with no player, view or intent; discarding")
			return nil
		}

		request := *msg.MineRequest
		target := world.ChunkOf(int64(request.Pos[0]), int64(request.Pos[1]), int64(request.Pos[2]))
		if mErr := player.Mine(request, streamer.View().Holds(target)); mErr != nil {
			// Wrong values in a well-formed mining request are silent refusals. In
			// particular there is no response that would let a forged client probe
			// unloaded chunks or Air beyond its view.
			log.Debug("refusing mine request",
				"reason", mErr.Error(),
				"pos", request.Pos,
				"active", request.Active,
				"client_tick", request.ClientTick,
			)
		}
		return nil

	case vnet.PayloadInventoryMoveRequest:
		if player == nil || msg.InventoryMove == nil {
			log.Debug("inventory move arrived with no player or intent; discarding")
			return nil
		}

		state, mErr := player.MoveInventory(*msg.InventoryMove)
		if mErr != nil {
			log.Debug("refusing inventory move",
				"reason", mErr.Error(),
				"from", msg.InventoryMove.From,
				"to", msg.InventoryMove.To,
				"count", msg.InventoryMove.Count,
			)
			return nil
		}
		if sErr := send(protocol.EncodeInventoryState(state)); sErr != nil {
			return fmt.Errorf("session: send moved inventory: %w", sErr)
		}
		log.Debug("inventory move applied",
			"from", msg.InventoryMove.From,
			"to", msg.InventoryMove.To,
			"count", msg.InventoryMove.Count,
		)
		return nil

	case vnet.PayloadAttackRequest:
		if player == nil || msg.Attack == nil {
			// Unreachable for the reason the cases above are, and stated for the same
			// one: the alternative to a log line is a nil dereference in the goroutine
			// holding a socket.
			log.Debug("attack arrived with no player to attribute it to; discarding")
			return nil
		}

		// Admission only. The swing is judged on the tick, against the positions that
		// tick produces — resolving it here would let network scheduling pick which
		// moment the world is measured at.
		//
		// Refusal is silence plus a debug line, exactly as a refused edit or a stale
		// input is. There is no rejection message in the contract and no acknowledgement
		// of any kind: a swing that did not land is a swing the client sees nothing from.
		if aErr := player.Attack(*msg.Attack); aErr != nil {
			log.Debug("refusing attack",
				"reason", aErr.Error(),
				"slot", msg.Attack.Slot,
				"client_tick", msg.Attack.ClientTick,
			)
		}
		return nil

	case vnet.PayloadPlaceStructureRequest:
		if player == nil || msg.PlaceStructure == nil {
			// Unreachable for the reason the cases above are, and stated for the same one:
			// the alternative to a log line is a nil dereference in the goroutine holding a
			// socket.
			log.Debug("structure placement arrived with no player to attribute it to; discarding")
			return nil
		}

		request := *msg.PlaceStructure
		state, reason, pErr := player.PlaceStructure(request)
		if pErr != nil {
			// **The log line stays, and the message beside it is not a copy of it.** The
			// log is for the operator and names the exact cell in prose; the frame is for
			// the player and carries a code. Two audiences, two outputs — and the reason
			// the sentence below was worth keeping is that it is the only one that says
			// *which* of nine footprint cells was air.
			log.Debug("refusing structure placement",
				"reason", pErr.Error(),
				"code", reason.String(),
				"slot", request.Slot,
				"anchor", request.Anchor,
				"facing", request.Facing.String(),
				"client_tick", request.ClientTick,
			)

			// The blocking send, for the reason the inventory below uses it: a refusal is
			// not superseded by the next tick the way a snapshot is, and a dropped one
			// leaves the player exactly where they were before this message existed —
			// looking at a click that vanished.
			refusal := protocol.ActionRefused{
				Action:    vnet.RefusedActionPlaceStructure,
				Reason:    reason,
				Anchor:    request.Anchor,
				HasAnchor: request.HasAnchor,
			}
			if sErr := send(protocol.EncodeActionRefused(refusal)); sErr != nil {
				return fmt.Errorf("session: send structure placement refusal: %w", sErr)
			}
			return nil
		}

		// Unlike a snapshot, an inventory state is not superseded on the next tick. Use
		// the blocking session send so a full queue cannot leave the client holding an
		// item the server has already spent.
		if sErr := send(protocol.EncodeInventoryState(state)); sErr != nil {
			return fmt.Errorf("session: send inventory after structure placement: %w", sErr)
		}
		log.Debug("structure placed", "slot", request.Slot, "anchor", request.Anchor, "facing", request.Facing.String())
		return nil

	case vnet.PayloadRemoveStructureRequest:
		if player == nil || msg.RemoveStructure == nil {
			log.Debug("structure removal arrived with no player to attribute it to; discarding")
			return nil
		}

		request := *msg.RemoveStructure
		// No inventory frame to send: a removed structure becomes a drop lying at its
		// anchor, and what the player ends up carrying is decided by walking over it.
		if rErr := player.RemoveStructure(request); rErr != nil {
			log.Debug("refusing structure removal",
				"reason", rErr.Error(),
				"structure_id", request.StructureID,
				"client_tick", request.ClientTick,
			)
			return nil
		}
		log.Debug("structure removed", "structure_id", request.StructureID)
		return nil

	case vnet.PayloadCraftRequest:
		if player == nil || msg.Craft == nil {
			// Unreachable for the reason the cases above are, and stated for the same one:
			// the alternative to a log line is a nil dereference in the goroutine holding a
			// socket.
			log.Debug("craft arrived with no player to attribute it to; discarding")
			return nil
		}

		request := *msg.Craft
		state, cErr := player.Craft(request)
		if cErr != nil {
			// Refusal is silence plus a debug line, exactly as a refused edit is. A craft
			// that did not happen is a pack that did not change, and that is the whole of
			// the answer the client gets.
			log.Debug("refusing craft",
				"reason", cErr.Error(),
				"recipe", request.Recipe.String(),
				"client_tick", request.ClientTick,
			)
			return nil
		}

		// Unlike a snapshot, an inventory state is not superseded on the next tick. Use
		// the blocking session send so a full queue cannot leave the client holding
		// materials the server has already spent.
		if sErr := send(protocol.EncodeInventoryState(state)); sErr != nil {
			return fmt.Errorf("session: send inventory after craft: %w", sErr)
		}
		log.Debug("craft applied", "recipe", request.Recipe.String())
		return nil

	case vnet.PayloadRepairRequest:
		if player == nil || msg.Repair == nil {
			// Unreachable for the reason the cases above are, and stated for the same one:
			// the alternative to a log line is a nil dereference in the goroutine holding a
			// socket.
			log.Debug("repair arrived with no player to attribute it to; discarding")
			return nil
		}

		request := *msg.Repair
		state, rErr := player.Repair(request)
		if rErr != nil {
			// Refusal is silence plus a debug line, exactly as a refused craft is. A repair
			// that did not happen is durability that did not move, and that is the whole of
			// the answer the client gets.
			log.Debug("refusing repair",
				"reason", rErr.Error(),
				"kit_slot", request.KitSlot,
				"target_slot", request.TargetSlot,
				"client_tick", request.ClientTick,
			)
			return nil
		}

		// Unlike a snapshot, an inventory state is not superseded on the next tick. Use the
		// blocking session send so a full queue cannot leave the client holding a kit the
		// server has already spent, or a blade still showing wear it no longer has.
		if sErr := send(protocol.EncodeInventoryState(state)); sErr != nil {
			return fmt.Errorf("session: send inventory after repair: %w", sErr)
		}
		log.Debug("repair applied", "kit_slot", request.KitSlot, "target_slot", request.TargetSlot)
		return nil

	case vnet.PayloadDropItemRequest:
		if player == nil || msg.DropItem == nil {
			// Unreachable for the reason the cases above are, and stated for the same one:
			// the alternative to a log line is a nil dereference in the goroutine holding a
			// socket.
			log.Debug("drop arrived with no player to attribute it to; discarding")
			return nil
		}

		request := *msg.DropItem
		state, dErr := player.DropItem(request)
		if dErr != nil {
			// Refusal is silence plus a debug line, exactly as a refused repair is: a pack
			// that did not change and a ground with nothing new on it.
			log.Debug("refusing drop",
				"reason", dErr.Error(),
				"slot", request.Slot,
				"client_tick", request.ClientTick,
			)
			return nil
		}

		// Unlike a snapshot, an inventory state is not superseded on the next tick. Use the
		// blocking session send so a full queue cannot leave the client still holding a stack
		// that is now on the ground. The drop needs no frame of its own — it is an entity,
		// and the next snapshot carries it like every other one.
		if sErr := send(protocol.EncodeInventoryState(state)); sErr != nil {
			return fmt.Errorf("session: send inventory after drop: %w", sErr)
		}
		log.Debug("drop applied", "slot", request.Slot)
		return nil

	case vnet.PayloadLeaveRequest:
		if player == nil || msg.LeaveRequest == nil {
			return fmt.Errorf("session: %w: LeaveRequest has no admitted player or payload", protocol.ErrMalformed)
		}
		return errLeaveRequested

	case vnet.PayloadClientHello:
		return fmt.Errorf("session: %w: second %s on an admitted session", protocol.ErrMalformed, msg.Kind)
	default:
		return fmt.Errorf("session: %w: client sent %s", protocol.ErrMalformed, msg.Kind)
	}
}

// errLeaveRequested is control flow inside Serve, not a failure. Keeping it distinct
// lets the shared post-handshake router stay exhaustive while Serve owns the session
// lifecycle, acknowledgement and timer.
var errLeaveRequested = errors.New("session: leave requested")

// Registry tracks live connections so shutdown can close them all, hands out the
// identities that name them, and knows which sessions hold which chunks.
//
// That last job is what makes it the home of the broadcast. "Every session holding this
// chunk" is a different question from the per-session snapshot fan-out the tick loop
// performs: the tick knows where each player *is* and can derive the cube it is
// streaming, but only the streamer knows which chunks have actually reached the client.
// The answer therefore lives beside the connections rather than inside the simulation,
// and the tick loop stays out of it.
type Registry struct {
	nextID atomic.Uint64

	mu    sync.Mutex
	conns map[uint64]transport.Conn
	peers map[uint64]peer
}

// peer is an admitted session as a broadcast sees it: what it holds, and how to reach it.
type peer struct {
	view *View
	send func(frame []byte) bool
}

// NewRegistry returns an empty registry.
func NewRegistry() *Registry {
	return &Registry{
		conns: make(map[uint64]transport.Conn),
		peers: make(map[uint64]peer),
	}
}

// NextID mints one identity.
//
// Exported because a connection is no longer the only thing that needs a name: the
// simulation owns entities of its own — an item lying on the ground is the first —
// and a snapshot addresses all of them in one space. One counter is what makes "an id
// names one thing" a fact rather than a coincidence, so game.Sim is handed this method
// instead of counting for itself. The direction is the usual one: session depends on
// game, and game is handed a function.
func (r *Registry) NextID() uint64 { return r.nextID.Add(1) }

// Add registers conn and returns the entity id the server assigns it.
//
// Identities are minted here and never read from the wire. An id a client can
// choose is an id a client can claim from someone else, and no amount of
// validation downstream fixes that.
func (r *Registry) Add(conn transport.Conn) uint64 {
	id := r.NextID()

	r.mu.Lock()
	defer r.mu.Unlock()
	r.conns[id] = conn
	return id
}

// Remove forgets a session. Calling it for an unknown id is a no-op, so a
// caller's cleanup path never needs to know whether Add succeeded.
func (r *Registry) Remove(id uint64) {
	r.mu.Lock()
	defer r.mu.Unlock()
	delete(r.conns, id)
	delete(r.peers, id)
}

// Subscribe makes an admitted session a broadcast target: view is what it holds, and send
// is how a frame reaches it.
//
// send must be the non-blocking path. It is called with this registry's lock held, from
// whichever session's goroutine resolved an edit, so a send that waited for room in
// somebody else's queue would stall an unrelated player's connection until a client that
// had stopped reading started again.
func (r *Registry) Subscribe(id uint64, view *View, send func(frame []byte) bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.peers[id] = peer{view: view, send: send}
}

// Unsubscribe stops broadcasts to a session.
//
// **When it returns, no broadcast is part-way through sending to that session and no
// later one will start.** It takes the lock BroadcastChunk holds while it sends, so the
// two are mutually exclusive by construction.
//
// That is the same guarantee Sim.Leave gives for snapshots, and it is needed for the same
// reason: a session's outbound channel may only be closed once nothing can still send to
// it, because a send on a closed channel is a panic in a goroutine and takes the process
// with it. Serve calls this before close(out).
func (r *Registry) Unsubscribe(id uint64) {
	r.mu.Lock()
	defer r.mu.Unlock()
	delete(r.peers, id)
}

// BroadcastChunk sends frame to every session that holds the chunk at coord, and reports
// how many it reached.
//
// **Every session holding it, and no others.** A voxel update for a chunk a client does
// not have describes terrain it cannot place, and one withheld from a client that does
// have it leaves that client rendering a world the server has already changed. The editor
// is included by the same rule as everyone else — it holds the chunk it just edited — and
// not as a special case, because a BlockUpdate is a statement about the world rather than
// a reply to a request.
func (r *Registry) BroadcastChunk(coord world.Coord, frame []byte) int {
	r.mu.Lock()
	defer r.mu.Unlock()

	sent := 0
	for _, p := range r.peers {
		if !p.view.Holds(coord) {
			continue
		}
		if p.send(frame) {
			sent++
			continue
		}
		// The frame is gone and no later one replaces it, so the chunk has to go too:
		// forgetting it makes the next view diff re-send the whole composed chunk, which is
		// the recovery a failed chunk send already relies on.
		p.view.Forget(coord)
	}
	return sent
}

// Count reports how many sessions are live.
func (r *Registry) Count() int {
	r.mu.Lock()
	defer r.mu.Unlock()
	return len(r.conns)
}

// CloseAll closes every live connection. This is what turns a cancelled context
// into unblocked reads: a session sitting in ReadFrame notices a shutdown only
// because its connection went away underneath it.
func (r *Registry) CloseAll() {
	r.mu.Lock()
	conns := make([]transport.Conn, 0, len(r.conns))
	for _, conn := range r.conns {
		conns = append(conns, conn)
	}
	r.mu.Unlock()

	for _, conn := range conns {
		// Best effort by design: a connection that is already gone is exactly the
		// state we want it in.
		_ = conn.Close()
	}
}
