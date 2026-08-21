package session_test

import (
	"bytes"
	"context"
	"errors"
	"io"
	"log/slog"
	"math"
	"net"
	"os"
	"sync"
	"syscall"
	"testing"
	"time"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/transport"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// payloadTable unwraps an envelope's union payload. Tests read frames the server
// produced, so they may use the generated accessors directly — unlike
// protocol.Decode, whose input is chosen by a client.
func payloadTable(t *testing.T, env *vnet.Envelope) flatbuffers.Table {
	t.Helper()

	var tbl flatbuffers.Table
	if !env.Payload(&tbl) {
		t.Fatal("envelope payload is absent")
	}
	return tbl
}

// encodePlayerInput builds one tick of intent, through the same encoder the client's
// bytes are checked against rather than a second one written here.
func encodePlayerInput(clientTick uint32, moveZ float32) []byte {
	return protocol.EncodePlayerInput(protocol.PlayerInput{ClientTick: clientTick, MoveZ: moveZ})
}

func discard() *slog.Logger { return slog.New(slog.DiscardHandler) }

// serveConfig is testConfig with the view distance turned down to a single chunk:
// the Serve tests are about admission and lifetime, and streaming has its own file.
func serveConfig() session.Config {
	cfg := testConfig()
	cfg.ViewDistance = 0
	return cfg
}

// noTimeouts disables both read deadlines.
//
// Most Serve tests are about admission and lifetime, and a deadline in the middle of
// one would be a clock they never asked for. The deadline has its own tests, which
// pass real windows. A zero duration is what net.Conn's zero Time means — never
// expire — and session.Timeouts.Validate is what stops a server from running this way.
func noTimeouts() session.Timeouts { return session.Timeouts{} }

// ephemeralIdentities is a fresh claim set with no player store behind it — the
// ephemeral world, which is how every test in this package runs unless it says
// otherwise.
//
// Fresh per session rather than shared, because a session that presents no token
// mints a fresh random identity, so two of them never collide however they are
// wired. The tests that are *about* exclusivity build one deliberately and hand it
// to both sessions; that is the whole difference, and it should be visible in the
// test that depends on it.
func ephemeralIdentities() *session.Identities { return session.NewIdentities(nil, nil) }

// testToken is a token whose bytes are chosen rather than minted, so a test can
// present the same one twice and can say what a log line must not contain.
//
// Distinct per seed and never the zero token: the zero token hashes to a real id,
// but a test that accidentally shared one would be asserting about identities that
// are the same by accident.
func testToken(seed byte) identity.Token {
	var token identity.Token
	for i := range token {
		token[i] = seed*17 + byte(i)
	}
	return token
}

// testChunks is a tiny world cache for the session tests.
func testChunks() *world.Cache { return world.NewCache(1234, 1, 8) }

// serveDeps builds the world and the simulation one Serve test needs, over the same
// chunks — so collision reads exactly what streaming sent.
//
// The Serve tests never tick the simulation; Sim.Step has its own tests in the package
// that owns it. What matters here is that a session joins it, hands it input, and
// leaves it without racing its own teardown.
func serveDeps(t *testing.T) (*world.Cache, *game.Sim, *session.Registry) {
	t.Helper()

	chunks := testChunks()
	// The registry is the identity source as well as the broadcast target, exactly as it
	// is in main: one counter names players and drops alike.
	peers := session.NewRegistry()
	sim, err := game.NewSim(20, serveConfig().ViewDistance, serveConfig().WorldSeed, game.NewCacheTerrain(chunks), chunks, peers.NextID, discard())
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	return chunks, sim, peers
}

func testConfig() session.Config {
	return session.Config{
		WorldSeed:    1234,
		TickRate:     20,
		ChunkSize:    32,
		ViewDistance: 3,
		Spawn:        [3]float32{0.5, 64, -0.5},
	}
}

// fakeConn is a transport.Conn backed by channels: no socket, no timing, and a
// closed connection that behaves the way a real one does — reads unblock with an
// error.
type fakeConn struct {
	in  chan []byte
	out chan []byte

	// mu guards the read deadline, which is one channel per arming: closed when that
	// deadline expires, replaced when the session arms the next one. A read holds the
	// generation it started with, so a re-arm can never wake a read that is already
	// waiting on the previous one.
	mu       sync.Mutex
	deadline chan struct{}
	timer    *time.Timer
	expired  bool

	closeOnce sync.Once
	done      chan struct{}
}

func newFakeConn() *fakeConn {
	return &fakeConn{
		in:   make(chan []byte, 4),
		out:  make(chan []byte, 8),
		done: make(chan struct{}),
	}
}

func (f *fakeConn) ReadFrame() ([]byte, error) {
	f.mu.Lock()
	deadline := f.deadline
	f.mu.Unlock()

	// A frame already waiting beats an expired deadline, because that is what a socket
	// with bytes in it does: the deadline bounds the wait, not the data. Without this
	// the two-ready select would pick between them at random.
	select {
	case frame, ok := <-f.in:
		if !ok {
			return nil, io.EOF
		}
		return frame, nil
	default:
	}

	select {
	case frame, ok := <-f.in:
		if !ok {
			return nil, io.EOF
		}
		return frame, nil
	case <-f.done:
		return nil, io.EOF
	case <-deadline:
		// The sentinel a real socket produces, and the one transport.IsTimeout is asked
		// about. A nil channel — nothing armed — blocks for ever, which is the other
		// half of what the zero Time means.
		return nil, os.ErrDeadlineExceeded
	}
}

// SetReadDeadline behaves the way a socket's does: it bounds the reads after it, the
// zero Time clears it again, and a deadline already in the past expires the next read
// rather than waiting for a timer.
func (f *fakeConn) SetReadDeadline(at time.Time) error {
	f.mu.Lock()
	defer f.mu.Unlock()

	if f.timer != nil {
		// The old generation's timer, if it has not fired. If it has, it closed the
		// channel this line is about to replace, and nothing is waiting on it.
		f.timer.Stop()
		f.timer = nil
	}

	ch := make(chan struct{})
	f.deadline = ch
	switch {
	case f.expired:
		// Fired on demand, and every deadline after it is born expired. See
		// expireReadDeadline for why that has to stick.
		close(ch)
	case at.IsZero():
		// No deadline at all: nothing will close ch.
	case time.Until(at) <= 0:
		close(ch)
	default:
		// Guarded and under the mutex: expireReadDeadline can close this same
		// generation's channel first, and Stop cannot un-fire a callback that is
		// already waiting on f.mu. Closing blind here is a "close of closed
		// channel" panic in a goroutine no test can recover.
		f.timer = time.AfterFunc(time.Until(at), func() {
			f.mu.Lock()
			defer f.mu.Unlock()
			select {
			case <-ch:
			default:
				close(ch)
			}
		})
	}
	return nil
}

// expireReadDeadline expires the current read deadline and every later one.
//
// Sticky deliberately. Serve arms a deadline before each read, so a test that fired
// one armed deadline would be racing that: a fire landing in the gap between two
// arms would be discarded by the next one, and the test would wait out a whole real
// window — or hang, when the window is the "no deadline" the other tests use. Sticky
// means a test never has to know which read the session has reached.
func (f *fakeConn) expireReadDeadline() {
	f.mu.Lock()
	defer f.mu.Unlock()

	f.expired = true
	if f.timer != nil {
		// This generation's timer is about to become redundant: its channel is
		// closed below. Stopping it keeps a pending callback from waking to find
		// the work already done — and if it has fired, the guard above is what
		// makes that harmless.
		f.timer.Stop()
		f.timer = nil
	}
	if f.deadline == nil {
		f.deadline = make(chan struct{})
	}
	select {
	case <-f.deadline:
	default:
		close(f.deadline)
	}
}

// WriteFrame answers a closed connection with what a real one answers — net.ErrClosed,
// which transport.IsDisconnect recognises.
//
// It returned io.ErrClosedPipe until #61, and the swap is a decision rather than a tidy-up.
// Serve now asks IsDisconnect about a write failure as well as a read one, and
// io.ErrClosedPipe is not one of the sentinels it lists, so there were two ways to make a
// clean disconnect end cleanly here: add that sentinel to IsDisconnect, or have the double
// return what the thing it doubles returns. Adding it would have widened a production
// predicate about real transports to cover io.Pipe and net.Pipe — nothing this server can
// ever be handed produces ErrClosedPipe — purely so a test would pass, and the note under
// IsClosed ("did the peer go away" versus "did we shut it down") only stays honest while
// both predicates are answering questions about real transports. So the double moved.
//
// Reads keep io.EOF, a peer hanging up; writes get net.ErrClosed, a write on a connection
// that has been closed. Each is what a real net.Conn produces for the side that notices the
// end, and errors.Is sees through the *net.OpError a socket wraps it in.
func (f *fakeConn) WriteFrame(payload []byte) error {
	select {
	case f.out <- bytes.Clone(payload):
		return nil
	case <-f.done:
		return net.ErrClosed
	}
}

func (f *fakeConn) RemoteAddr() string { return "fake" }

func (f *fakeConn) Close() error {
	f.closeOnce.Do(func() { close(f.done) })
	return nil
}

// nextFrame fails the test rather than hanging it: a session that never replies is
// a bug worth a clear message.
func nextFrame(t *testing.T, conn *fakeConn) []byte {
	t.Helper()

	select {
	case frame := <-conn.out:
		return frame
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for a frame from the session")
		return nil
	}
}

func TestHandshake(t *testing.T) {
	t.Parallel()

	cfg := testConfig()

	t.Run("a matching version is admitted with the server's parameters", func(t *testing.T) {
		t.Parallel()

		msg, err := protocol.Decode(protocol.EncodeClientHello(vnet.ProtocolVersionCurrent, "Eivor"))
		if err != nil {
			t.Fatalf("Decode: %v", err)
		}

		token := testToken(1)
		reply, accepted := session.Handshake(msg, cfg, 7, session.Resolved{Token: token})
		if !accepted {
			t.Fatal("a current-version hello was refused")
		}

		env := vnet.GetRootAsEnvelope(reply, 0)
		if env.PayloadType() != vnet.PayloadServerWelcome {
			t.Fatalf("reply is %s, want %s", env.PayloadType(), vnet.PayloadServerWelcome)
		}
		welcome := welcomeFrom(t, env)
		if got := welcome.EntityId(); got != 7 {
			t.Errorf("EntityId = %d, want the server-assigned 7", got)
		}
		if got := welcome.TickRate(); got != cfg.TickRate {
			t.Errorf("TickRate = %d, want %d", got, cfg.TickRate)
		}
		if got := welcome.ChunkSize(); got != cfg.ChunkSize {
			t.Errorf("ChunkSize = %d, want %d", got, cfg.ChunkSize)
		}
		if got := welcome.ViewDistance(); got != cfg.ViewDistance {
			t.Errorf("ViewDistance = %d, want %d", got, cfg.ViewDistance)
		}
		if got := welcome.InventorySlots(); got != protocol.InventorySlots {
			t.Errorf("InventorySlots = %d, want %d", got, protocol.InventorySlots)
		}
		if got := welcome.HotbarSlots(); got != protocol.HotbarSlots {
			t.Errorf("HotbarSlots = %d, want %d", got, protocol.HotbarSlots)
		}
		if got := welcome.WorldSeed(); got != cfg.WorldSeed {
			t.Errorf("WorldSeed = %d, want %d", got, cfg.WorldSeed)
		}
		// The welcome carries the identity the caller resolved, whatever the client
		// presented. Handshake is not what decides which token that is — see
		// TestResolveIdentity — it is what guarantees one is always announced.
		if got := welcome.PlayerTokenBytes(); !bytes.Equal(got, token[:]) {
			t.Errorf("PlayerToken is %d bytes and not the token given (%d bytes)", len(got), len(token))
		}
		// The world's clock, announced from the constants that own it rather than from
		// Config — which is why these are compared against game and not against cfg.
		// Zero is the wire's "this server keeps no clock", so a welcome that forgot to
		// set them would be a legal frame that lied about the world.
		if got := welcome.DayLengthTicks(); got != game.DayLengthTicks {
			t.Errorf("DayLengthTicks = %d, want %d", got, uint32(game.DayLengthTicks))
		}
		if got := welcome.NightStartTicks(); got != game.NightStartTicks {
			t.Errorf("NightStartTicks = %d, want %d", got, uint32(game.NightStartTicks))
		}
		if got := welcome.NightEndTicks(); got != game.NightEndTicks {
			t.Errorf("NightEndTicks = %d, want %d", got, uint32(game.NightEndTicks))
		}
	})

	// The ordering schemas/handshake.fbs requires of a welcome that declares a clock,
	// executed against the frame this server actually emits.
	//
	// **The client refuses a welcome that breaks it** — codec.rs raises WorldClock and
	// drops the connection — so a server able to send one would be a server nobody could
	// join. internal/game pins the same relationship on the constants; what is pinned
	// here is that the welcome is built from those constants and not from something else
	// that merely looks like them.
	t.Run("the announced clock satisfies the contract's ordering", func(t *testing.T) {
		t.Parallel()

		msg, err := protocol.Decode(protocol.EncodeClientHello(vnet.ProtocolVersionCurrent, "Eivor"))
		if err != nil {
			t.Fatalf("Decode: %v", err)
		}

		reply, accepted := session.Handshake(msg, cfg, 7, session.Resolved{Token: testToken(1)})
		if !accepted {
			t.Fatal("a current-version hello was refused")
		}

		welcome := welcomeFrom(t, vnet.GetRootAsEnvelope(reply, 0))
		day, start, end := welcome.DayLengthTicks(), welcome.NightStartTicks(), welcome.NightEndTicks()
		if day == 0 {
			t.Fatal("the welcome declares no clock at all")
		}
		// One clause at a time, so a failure names which half of the ordering broke.
		for _, rule := range []struct {
			holds bool
			what  string
		}{
			{start > 0, "night begins after the first tick of the day"},
			{start < end, "night begins before it ends"},
			{end <= day, "night ends no later than the day does"},
		} {
			if !rule.holds {
				t.Errorf("the welcome announces night %d..%d in a day of %d ticks, which breaks: %s",
					start, end, day, rule.what)
			}
		}
	})

	refusals := map[string]struct {
		frame  []byte
		reason vnet.RejectReason
	}{
		// A hello with no version field decodes as Unknown, so the version check is
		// what catches it — the absent case and the wrong case share one path.
		"absent version": {
			frame:  protocol.EncodeClientHello(vnet.ProtocolVersionUnknown, ""),
			reason: vnet.RejectReasonPROTOCOL_MISMATCH,
		},
		"future version": {
			frame:  protocol.EncodeClientHello(vnet.ProtocolVersion(99), "Eivor"),
			reason: vnet.RejectReasonPROTOCOL_MISMATCH,
		},
		// The point of the V3 break, named rather than left to the general case: a V2
		// client's snapshots would carry no vitals and its inventory no durability, and
		// the handshake is where that has to be discovered. Mid-session it is a decode
		// error on a connection both sides thought was healthy.
		"the previous protocol": {
			frame:  protocol.EncodeClientHello(vnet.ProtocolVersion(2), "Eivor"),
			reason: vnet.RejectReasonPROTOCOL_MISMATCH,
		},
		"first message is not a hello": {
			frame:  protocol.EncodeServerWelcome(protocol.Welcome{TickRate: 20, ChunkSize: 32}),
			reason: vnet.RejectReasonBAD_REQUEST,
		},
	}

	for name, tc := range refusals {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			msg, err := protocol.Decode(tc.frame)
			if err != nil {
				t.Fatalf("Decode: %v", err)
			}

			reply, accepted := session.Handshake(msg, cfg, 7, session.Resolved{Token: testToken(1)})
			if accepted {
				t.Fatal("the handshake was accepted")
			}

			env := vnet.GetRootAsEnvelope(reply, 0)
			if env.PayloadType() != vnet.PayloadServerReject {
				t.Fatalf("reply is %s, want %s", env.PayloadType(), vnet.PayloadServerReject)
			}
			if got := rejectFrom(t, env).Reason(); got != tc.reason {
				t.Errorf("Reason = %s, want %s", got, tc.reason)
			}
		})
	}
}

func TestConfigValidate(t *testing.T) {
	t.Parallel()

	valid := testConfig()
	if err := valid.Validate(); err != nil {
		t.Fatalf("a valid config was rejected: %v", err)
	}

	invalid := map[string]func(c *session.Config){
		"tick rate 0":  func(c *session.Config) { c.TickRate = 0 },
		"chunk size 0": func(c *session.Config) { c.ChunkSize = 0 },
		"chunk size past the u16 run limit": func(c *session.Config) {
			c.ChunkSize = protocol.MaxChunkSize + 1
		},
		"view distance past the volume limit": func(c *session.Config) {
			c.ViewDistance = protocol.MaxViewDistance + 1
		},
		"NaN spawn":      func(c *session.Config) { c.Spawn[1] = float32(math.NaN()) },
		"infinite spawn": func(c *session.Config) { c.Spawn[2] = float32(math.Inf(1)) },
	}

	for name, mutate := range invalid {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			cfg := testConfig()
			mutate(&cfg)
			if err := cfg.Validate(); err == nil {
				t.Fatalf("Validate accepted %s", name)
			}
		})
	}
}

func TestServeAdmitsAndAcceptsInput(t *testing.T) {
	t.Parallel()

	chunks, sim, peers := serveDeps(t)
	conn := newFakeConn()
	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, serveConfig(), noTimeouts(), chunks, sim, peers, ephemeralIdentities(), 3, discard())
	}()

	conn.in <- protocol.EncodeClientHello(vnet.ProtocolVersionCurrent, "Eivor")
	env := vnet.GetRootAsEnvelope(nextFrame(t, conn), 0)
	if env.PayloadType() != vnet.PayloadServerWelcome {
		t.Fatalf("first reply is %s, want %s", env.PayloadType(), vnet.PayloadServerWelcome)
	}

	// Input on an admitted session is handed to the simulation. What this covers is
	// the routing and the lifetime; that the simulation then *moves* the player is
	// TestSessionWalksThePlayerAndStreamsWhereItWalks.
	conn.in <- encodePlayerInput(1, 1)

	if err := conn.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Serve returned %v, want nil for a clean disconnect", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("Serve did not return after the connection closed")
	}
}

// writeErrConn is a fakeConn whose every write fails with a chosen error.
//
// TestServeAdmitsAndAcceptsInput reaches the write-failure path only when Close lands in the
// window where a spawn chunk is still in flight, and the rate is a strong function of the
// configuration: before the fix, 31 failures in 200 runs at GOMAXPROCS=1 under eight
// busy-loops, against 0 to 1 in 200 at GOMAXPROCS 2 and 4 (#61). Enough to catch a regression
// eventually, nowhere near enough to state a rule — so both halves of the rule get a
// connection that always fails.
type writeErrConn struct {
	*fakeConn
	err error
}

func (w *writeErrConn) WriteFrame([]byte) error { return w.err }

// serveWithWriteError admits one session whose writes all fail with wErr, and reports what
// Serve returned.
//
// Nothing here closes the connection: the writer goroutine closes it on the first failure,
// which unblocks the read loop, so wErr is the only thing deciding the outcome.
func serveWithWriteError(t *testing.T, wErr error) error {
	t.Helper()

	chunks, sim, peers := serveDeps(t)
	conn := &writeErrConn{fakeConn: newFakeConn(), err: wErr}

	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, serveConfig(), noTimeouts(), chunks, sim, peers, ephemeralIdentities(), 3, discard())
	}()

	conn.in <- protocol.EncodeClientHello(vnet.ProtocolVersionCurrent, "Eivor")

	select {
	case err := <-done:
		return err
	case <-time.After(2 * time.Second):
		t.Fatal("Serve did not return after the write failed")
		return nil
	}
}

// A write that fails because the peer went away ends the session the way a read that fails
// for the same reason does: cleanly. Which goroutine noticed the disconnect is an accident of
// scheduling, and it must not decide whether the operator sees a warning.
//
// syscall.EPIPE rather than the fake's own net.ErrClosed, so that the classification is shown
// to be a question put to IsDisconnect and not a comparison against one sentinel.
func TestServeEndsCleanlyWhenAWriteFindsThePeerGone(t *testing.T) {
	t.Parallel()

	if err := serveWithWriteError(t, syscall.EPIPE); err != nil {
		t.Fatalf("Serve returned %v, want nil for a write that found the peer gone", err)
	}
}

// The other half, and what keeps the half above from being a licence to swallow: an error the
// transport does not recognise as a disconnect is a genuine failure and is still reported.
// Making the test above pass by dropping writeFailure altogether would leave this one red,
// which is the whole reason both exist.
func TestServeReportsAWriteFailureThatIsNotADisconnect(t *testing.T) {
	t.Parallel()

	boom := errors.New("the write went wrong for a reason of its own")
	err := serveWithWriteError(t, boom)
	if !errors.Is(err, boom) {
		t.Fatalf("Serve returned %v, want an error wrapping %v", err, boom)
	}
}

func TestServeRefusesAndExplains(t *testing.T) {
	t.Parallel()

	chunks, sim, peers := serveDeps(t)
	conn := newFakeConn()
	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, serveConfig(), noTimeouts(), chunks, sim, peers, ephemeralIdentities(), 3, discard())
	}()

	conn.in <- protocol.EncodeClientHello(vnet.ProtocolVersion(42), "Eivor")

	// The refusal must reach the client before the session ends: being told why is
	// the whole point of ServerReject.
	env := vnet.GetRootAsEnvelope(nextFrame(t, conn), 0)
	if env.PayloadType() != vnet.PayloadServerReject {
		t.Fatalf("reply is %s, want %s", env.PayloadType(), vnet.PayloadServerReject)
	}
	if got := rejectFrom(t, env).Reason(); got != vnet.RejectReasonPROTOCOL_MISMATCH {
		t.Errorf("Reason = %s, want %s", got, vnet.RejectReasonPROTOCOL_MISMATCH)
	}

	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Serve returned %v, want nil for a refused handshake", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("Serve did not return after refusing the handshake")
	}
}

func TestServeEndsOnUndecodableFrame(t *testing.T) {
	t.Parallel()

	chunks, sim, peers := serveDeps(t)
	conn := newFakeConn()
	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, serveConfig(), noTimeouts(), chunks, sim, peers, ephemeralIdentities(), 3, discard())
	}()

	conn.in <- bytes.Repeat([]byte{0xFF}, 32)

	select {
	case err := <-done:
		if !errors.Is(err, protocol.ErrMalformed) {
			t.Fatalf("Serve returned %v, want an error wrapping protocol.ErrMalformed", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("Serve did not return on an undecodable frame")
	}
}

// A client that sends a server-only payload has broken the protocol: direction is
// a rule the shared union cannot express, so the session enforces it.
func TestServeEndsWhenClientSendsAServerPayload(t *testing.T) {
	t.Parallel()

	chunks, sim, peers := serveDeps(t)
	conn := newFakeConn()
	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, serveConfig(), noTimeouts(), chunks, sim, peers, ephemeralIdentities(), 3, discard())
	}()

	conn.in <- protocol.EncodeClientHello(vnet.ProtocolVersionCurrent, "Eivor")
	_ = nextFrame(t, conn) // the welcome
	conn.in <- protocol.EncodeServerWelcome(protocol.Welcome{TickRate: 20, ChunkSize: 32})

	select {
	case err := <-done:
		if err == nil {
			t.Fatal("Serve accepted a ServerWelcome from a client")
		}
	case <-time.After(2 * time.Second):
		t.Fatal("Serve did not return on a misdirected payload")
	}
}

func TestRegistryAssignsIdentitiesAndClosesEverything(t *testing.T) {
	t.Parallel()

	reg := session.NewRegistry()
	if got := reg.Count(); got != 0 {
		t.Fatalf("a new registry holds %d sessions", got)
	}

	conns := make([]*fakeConn, 3)
	ids := make(map[uint64]bool, 3)
	for i := range conns {
		conns[i] = newFakeConn()
		id := reg.Add(conns[i])
		if id == 0 {
			t.Fatal("entity id 0 was assigned; ids must be non-zero so that a zero value is never a valid identity")
		}
		if ids[id] {
			t.Fatalf("entity id %d was assigned twice", id)
		}
		ids[id] = true
	}
	if got := reg.Count(); got != 3 {
		t.Fatalf("Count = %d, want 3", got)
	}

	reg.CloseAll()
	for i, conn := range conns {
		select {
		case <-conn.done:
		default:
			t.Errorf("connection %d was left open by CloseAll", i)
		}
	}

	for id := range ids {
		reg.Remove(id)
	}
	if got := reg.Count(); got != 0 {
		t.Fatalf("Count = %d after removing every session, want 0", got)
	}
	reg.Remove(9999) // unknown ids are a no-op so cleanup paths stay simple
}

func welcomeFrom(t *testing.T, env *vnet.Envelope) *vnet.ServerWelcome {
	t.Helper()

	tbl := payloadTable(t, env)
	welcome := new(vnet.ServerWelcome)
	welcome.Init(tbl.Bytes, tbl.Pos)
	return welcome
}

func rejectFrom(t *testing.T, env *vnet.Envelope) *vnet.ServerReject {
	t.Helper()

	tbl := payloadTable(t, env)
	reject := new(vnet.ServerReject)
	reject.Init(tbl.Bytes, tbl.Pos)
	return reject
}

var _ transport.Conn = (*fakeConn)(nil)

// The deadline tests below use windows an hour long and fire them on demand. That is
// deliberate: a test that waited out a short real window would be asserting that a
// timer works, where what is worth pinning is that Serve *ends the session* when a
// read reports an expired deadline, and how. The one test that does use real time is
// the last, because "it is still open" cannot be shown by firing anything.
func longTimeouts() session.Timeouts {
	return session.Timeouts{Handshake: time.Hour, Idle: time.Hour}
}

// A connection that arrives and says nothing is closed, and told nothing. There is no
// reply to a silence: ServerReject answers a message, and this peer has not sent one.
func TestServeClosesAConnectionThatNeverSaysHello(t *testing.T) {
	t.Parallel()

	chunks, sim, peers := serveDeps(t)
	conn := newFakeConn()

	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, serveConfig(), longTimeouts(), chunks, sim, peers, ephemeralIdentities(), 3, discard())
	}()

	conn.expireReadDeadline()

	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Serve returned %v, want nil for a handshake that timed out", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("Serve did not return after the handshake deadline expired")
	}

	select {
	case frame := <-conn.out:
		t.Errorf("wrote %d bytes to a client that never spoke", len(frame))
	default:
	}

	if got := sim.Count(); got != 0 {
		t.Errorf("simulation holds %d players, want 0; nothing was ever admitted", got)
	}
}

// A session that goes quiet ends the way a session that hangs up ends: through the
// ordinary teardown, with nil. The #61 lesson, arrived at from the other direction —
// there it was a write finding the peer gone, here it is a read finding nobody there
// — and the cost of getting it wrong is the same, an operator whose log warns about
// the most routine thing a connection does.
func TestServeEndsAnIdleSessionCleanly(t *testing.T) {
	t.Parallel()

	chunks, sim, peers := serveDeps(t)
	conn := newFakeConn()

	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, serveConfig(), longTimeouts(), chunks, sim, peers, ephemeralIdentities(), 3, discard())
	}()

	conn.in <- protocol.EncodeClientHello(vnet.ProtocolVersionCurrent, "Eivor")
	if got := vnet.GetRootAsEnvelope(nextFrame(t, conn), 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("first reply is %s, want %s", got, vnet.PayloadServerWelcome)
	}
	// The inventory is sent after Join returns, so receiving it is what proves the
	// player is in the simulation — the welcome is queued before Join is even called.
	if got := vnet.GetRootAsEnvelope(nextFrame(t, conn), 0).PayloadType(); got != vnet.PayloadInventoryState {
		t.Fatalf("second reply is %s, want %s", got, vnet.PayloadInventoryState)
	}
	if got := sim.Count(); got != 1 {
		t.Fatalf("simulation holds %d players after the handshake, want 1", got)
	}

	conn.expireReadDeadline()

	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Serve returned %v, want nil for an idle session", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("Serve did not return after the idle deadline expired")
	}

	// The whole point of the timeout: the identity is released. A session that ended
	// without leaving the simulation would hold its entity id against the reconnect it
	// exists to make possible.
	if got := sim.Count(); got != 0 {
		t.Errorf("simulation holds %d players after an idle session ended, want 0", got)
	}
}

// The other half, and the one that decides whether the idle default is a timeout or a
// disconnect generator: a client sending input at the tick rate is never closed by it.
//
// PlayerInput is the heartbeat, so this is what a healthy connection looks like — the
// real client sends one every tick whether the player is moving, standing still or
// dead. Real time and a real window here, because "it did not fire" is not something a
// fired deadline can show.
func TestServeKeepsASessionThatKeepsTalking(t *testing.T) {
	t.Parallel()

	const (
		idle       = 300 * time.Millisecond
		tickPeriod = 50 * time.Millisecond
		windows    = 4
	)

	chunks, sim, peers := serveDeps(t)
	conn := newFakeConn()

	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, serveConfig(),
			session.Timeouts{Handshake: idle, Idle: idle}, chunks, sim, peers, ephemeralIdentities(), 3, discard())
	}()

	conn.in <- protocol.EncodeClientHello(vnet.ProtocolVersionCurrent, "Eivor")
	if got := vnet.GetRootAsEnvelope(nextFrame(t, conn), 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("first reply is %s, want %s", got, vnet.PayloadServerWelcome)
	}

	until := time.Now().Add(windows * idle)
	for tick := uint32(1); time.Now().Before(until); tick++ {
		select {
		case conn.in <- encodePlayerInput(tick, 0):
		case err := <-done:
			t.Fatalf("Serve returned %v after %d frames; a talking client was closed", err, tick-1)
		case <-time.After(2 * time.Second):
			t.Fatal("the session stopped reading input")
		}
		time.Sleep(tickPeriod)
	}

	select {
	case err := <-done:
		t.Fatalf("Serve returned %v while the client was still sending input", err)
	default:
	}
	if got := sim.Count(); got != 1 {
		t.Errorf("simulation holds %d players, want 1; the session should still be open", got)
	}

	// And it still ends when the client actually goes: the deadline being re-armed
	// must not have replaced the ordinary way out.
	if err := conn.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Serve returned %v, want nil for a clean disconnect", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("Serve did not return after the connection closed")
	}
}

// The deadline policy's own rules, checked where they live. main's flag validation
// asks this same function rather than restating it, so these cases cover both.
func TestTimeoutsValidate(t *testing.T) {
	t.Parallel()

	if err := session.DefaultTimeouts().Validate(); err != nil {
		t.Fatalf("the defaults are invalid: %v", err)
	}

	refused := map[string]session.Timeouts{
		"no handshake window":     {Handshake: 0, Idle: 20 * time.Second},
		"no idle window":          {Handshake: 5 * time.Second, Idle: 0},
		"neither":                 {},
		"negative idle window":    {Handshake: 5 * time.Second, Idle: -time.Second},
		"handshake beyond idle":   {Handshake: 21 * time.Second, Idle: 20 * time.Second},
		"handshake far beyond it": {Handshake: time.Hour, Idle: time.Second},
	}
	for name, timeouts := range refused {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			if err := timeouts.Validate(); err == nil {
				t.Fatalf("Validate accepted %s: %+v", name, timeouts)
			}
		})
	}

	// Equal is the boundary and it is allowed: a handshake window the same length as
	// the idle window means every read gets the same budget, which is a policy rather
	// than a mistake.
	same := session.Timeouts{Handshake: 20 * time.Second, Idle: 20 * time.Second}
	if err := same.Validate(); err != nil {
		t.Errorf("Validate rejected the boundary %+v: %v", same, err)
	}
}
