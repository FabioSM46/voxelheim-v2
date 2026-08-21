package session_test

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"log/slog"
	"strings"
	"sync"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
)

// knownIdentities is a claim set over a real player store that already holds a
// record for each token given, so those tokens resume rather than mint.
func knownIdentities(t *testing.T, known ...identity.Token) (*session.Identities, *persist.Store) {
	t.Helper()

	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	for _, token := range known {
		if err := store.Save(identity.IDOf(token), livingRecord("Eivor")); err != nil {
			t.Fatalf("seeding a record: %v", err)
		}
	}
	return session.NewIdentities(store, nil), store
}

// livingRecord is the smallest record this build will resume: a living player, at a
// position, holding nothing.
//
// The health is the part a seeded fixture cannot leave out. A record always describes
// somebody alive — a player who dies and quits is written as their respawn would have
// left them — so game.Life.Validate refuses a health of zero, and a record seeded
// without one is refused as unreadable rather than resumed.
func livingRecord(name string) persist.Record {
	return persist.Record{
		Name:     name,
		LastSeen: time.Unix(1, 0),
		Pos:      [3]float64{0.5, 64, 0.5},
		Health:   game.PlayerMaxHealth,
	}
}

// helloWith decodes a hello carrying token, which is what Resolve takes.
func helloWith(t *testing.T, token []byte) *protocol.ClientHello {
	t.Helper()

	msg, err := protocol.Decode(protocol.EncodeClientHelloWithToken(vnet.ProtocolVersionCurrent, "Eivor", token))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	return msg.ClientHello
}

// refusalReason is the wire code a resolution failure carries, or a fatal test
// failure when it carries none — a server-side error and a refusal are deliberately
// different things, and a test that conflated them would pass for the wrong reason.
func refusalReason(t *testing.T, err error) vnet.RejectReason {
	t.Helper()

	var refused *session.Refused
	if !errors.As(err, &refused) {
		t.Fatalf("error %v is not a refusal with a reason code", err)
	}
	return refused.Reason
}

func TestResolveIdentity(t *testing.T) {
	t.Parallel()

	t.Run("an empty token mints a new identity", func(t *testing.T) {
		t.Parallel()

		identities, _ := knownIdentities(t)

		resolved, err := identities.Resolve(helloWith(t, nil))
		if err != nil {
			t.Fatalf("Resolve: %v", err)
		}
		if resolved.Returning {
			t.Error("a client that presented nothing was reported as returning")
		}
		if resolved.Token == (identity.Token{}) {
			t.Fatal("the minted token is the zero token")
		}
		if resolved.ID != identity.IDOf(resolved.Token) {
			t.Error("the resolved id is not the hash of the resolved token")
		}
	})

	t.Run("a known token resumes that identity", func(t *testing.T) {
		t.Parallel()

		token := testToken(2)
		identities, _ := knownIdentities(t, token)

		resolved, err := identities.Resolve(helloWith(t, token[:]))
		if err != nil {
			t.Fatalf("Resolve: %v", err)
		}
		if !resolved.Returning {
			t.Error("a token the store knows was not reported as returning")
		}
		if !resolved.Token.Equal(token) {
			t.Error("a resumed identity was answered with a different token")
		}
		if resolved.ID != identity.IDOf(token) {
			t.Error("a resumed identity has a different player id")
		}
	})

	t.Run("an unknown token mints a new token rather than adopting it", func(t *testing.T) {
		t.Parallel()

		// The rule that keeps every token in circulation one this server minted: a
		// client cannot choose who it is by inventing 32 bytes.
		presented := testToken(3)
		identities, _ := knownIdentities(t)

		resolved, err := identities.Resolve(helloWith(t, presented[:]))
		if err != nil {
			t.Fatalf("Resolve: %v", err)
		}
		if resolved.Returning {
			t.Error("a token the store has never seen was reported as returning")
		}
		if resolved.Token.Equal(presented) {
			t.Fatal("the presented token was adopted as this identity's key")
		}
		if resolved.ID == identity.IDOf(presented) {
			t.Error("the identity is the one the client asked for")
		}
	})

	t.Run("a wrong-length token is BAD_REQUEST", func(t *testing.T) {
		t.Parallel()

		for _, size := range []int{1, 7, 31, 33, 64} {
			identities, _ := knownIdentities(t)

			_, err := identities.Resolve(helloWith(t, make([]byte, size)))
			if err == nil {
				t.Fatalf("a %d-byte token was accepted", size)
			}
			if got := refusalReason(t, err); got != vnet.RejectReasonBAD_REQUEST {
				t.Errorf("a %d-byte token was refused with %s, want BAD_REQUEST", size, got)
			}
			// Decided before any identity is looked up, so nothing was claimed.
			if identities.Count() != 0 {
				t.Errorf("a %d-byte token left an identity claimed", size)
			}
		}
	})

	t.Run("an identity that is already playing is ALREADY_CONNECTED", func(t *testing.T) {
		t.Parallel()

		token := testToken(4)
		identities, _ := knownIdentities(t, token)

		first, err := identities.Resolve(helloWith(t, token[:]))
		if err != nil {
			t.Fatalf("the first Resolve: %v", err)
		}

		_, err = identities.Resolve(helloWith(t, token[:]))
		if err == nil {
			t.Fatal("a second session on one identity was admitted")
		}
		if got := refusalReason(t, err); got != vnet.RejectReasonALREADY_CONNECTED {
			t.Errorf("the second hello was refused with %s, want ALREADY_CONNECTED", got)
		}
		if identities.Count() != 1 {
			t.Errorf("%d identities are live, want the one that was admitted", identities.Count())
		}

		// And it is free again once the session that held it releases.
		identities.Release(first.ID)
		if _, err := identities.Resolve(helloWith(t, token[:])); err != nil {
			t.Fatalf("the identity was not free after Release: %v", err)
		}
	})

	t.Run("two clients presenting nothing get different identities", func(t *testing.T) {
		t.Parallel()

		identities, _ := knownIdentities(t)

		first, err := identities.Resolve(helloWith(t, nil))
		if err != nil {
			t.Fatalf("the first Resolve: %v", err)
		}
		second, err := identities.Resolve(helloWith(t, nil))
		if err != nil {
			t.Fatalf("the second Resolve: %v", err)
		}
		if first.ID == second.ID {
			t.Error("two minted identities collided, so the exclusivity rule could never fire")
		}
	})

	t.Run("an ephemeral world mints and never resumes", func(t *testing.T) {
		t.Parallel()

		// No store: minting and exclusivity still work, nothing is written, and a
		// presented token is therefore never known.
		identities := session.NewIdentities(nil, nil)
		token := testToken(5)

		resolved, err := identities.Resolve(helloWith(t, token[:]))
		if err != nil {
			t.Fatalf("Resolve: %v", err)
		}
		if resolved.Returning || resolved.Token.Equal(token) {
			t.Error("an ephemeral world resumed an identity it cannot have stored")
		}
	})
}

// TestServeRefusesASecondSessionOnOneIdentity is the rule end to end, through the
// whole connection lifetime rather than through Resolve alone — because the half
// that is easy to get wrong is not the refusal, it is the release.
func TestServeRefusesASecondSessionOnOneIdentity(t *testing.T) {
	t.Parallel()

	token := testToken(6)
	identities, _ := knownIdentities(t, token)
	chunks, sim, peers := serveDeps(t)

	first := newFakeConn()
	firstDone := make(chan error, 1)
	go func() {
		firstDone <- session.Serve(context.Background(), first, serveConfig(), noTimeouts(), chunks, sim, peers, identities, 1, discard())
	}()

	first.in <- protocol.EncodeClientHelloWithToken(vnet.ProtocolVersionCurrent, "Eivor", token[:])
	if got := vnet.GetRootAsEnvelope(nextFrame(t, first), 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the first session got %s, want a welcome", got)
	}

	second := newFakeConn()
	secondDone := make(chan error, 1)
	go func() {
		secondDone <- session.Serve(context.Background(), second, serveConfig(), noTimeouts(), chunks, sim, peers, identities, 2, discard())
	}()

	second.in <- protocol.EncodeClientHelloWithToken(vnet.ProtocolVersionCurrent, "Eivor", token[:])
	env := vnet.GetRootAsEnvelope(nextFrame(t, second), 0)
	if env.PayloadType() != vnet.PayloadServerReject {
		t.Fatalf("the second session got %s, want a rejection", env.PayloadType())
	}
	if got := rejectFrom(t, env).Reason(); got != vnet.RejectReasonALREADY_CONNECTED {
		t.Errorf("the second session was refused with %s, want ALREADY_CONNECTED", got)
	}

	// A refusal is a clean end, exactly as a protocol mismatch is.
	select {
	case err := <-secondDone:
		if err != nil {
			t.Fatalf("the refused session returned %v, want nil", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("the refused session did not return")
	}

	// **The claim is released last, after the session has gone.** Once Serve has
	// returned, the identity is free — which is what makes a reconnect right after a
	// disconnect work, and what makes an idle session hand its identity back.
	if err := first.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	select {
	case err := <-firstDone:
		if err != nil {
			t.Fatalf("the first session returned %v, want nil for a clean disconnect", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("the first session did not return")
	}
	if identities.Count() != 0 {
		t.Fatalf("%d identities are still claimed after every session ended", identities.Count())
	}

	third := newFakeConn()
	thirdDone := make(chan error, 1)
	go func() {
		thirdDone <- session.Serve(context.Background(), third, serveConfig(), noTimeouts(), chunks, sim, peers, identities, 3, discard())
	}()
	third.in <- protocol.EncodeClientHelloWithToken(vnet.ProtocolVersionCurrent, "Eivor", token[:])
	if got := vnet.GetRootAsEnvelope(nextFrame(t, third), 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the reconnect got %s, want a welcome", got)
	}
	_ = third.Close()
	<-thirdDone
}

func TestServeRefusesAWrongLengthToken(t *testing.T) {
	t.Parallel()

	identities, _ := knownIdentities(t)
	chunks, sim, peers := serveDeps(t)

	conn := newFakeConn()
	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, serveConfig(), noTimeouts(), chunks, sim, peers, identities, 4, discard())
	}()

	conn.in <- protocol.EncodeClientHelloWithToken(vnet.ProtocolVersionCurrent, "Eivor", make([]byte, 7))
	env := vnet.GetRootAsEnvelope(nextFrame(t, conn), 0)
	if env.PayloadType() != vnet.PayloadServerReject {
		t.Fatalf("reply is %s, want a rejection", env.PayloadType())
	}
	reject := rejectFrom(t, env)
	if got := reject.Reason(); got != vnet.RejectReasonBAD_REQUEST {
		t.Errorf("Reason = %s, want BAD_REQUEST", got)
	}
	// The refusal says how long the token was and nothing about what was in it.
	if detail := string(reject.Detail()); !strings.Contains(detail, "7") {
		t.Errorf("Detail %q does not say what was wrong", detail)
	}

	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Serve returned %v, want nil for a refused handshake", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("Serve did not return after refusing the handshake")
	}
	if identities.Count() != 0 {
		t.Error("a refused handshake left an identity claimed")
	}
	if sim.Count() != 0 {
		t.Error("a refused handshake joined the simulation")
	}
}

// TestAPlayerComesBackAsThemselves is the whole point of the issue, in one test: a
// session ends, its record is written, and the token it was given brings the same
// identity back.
func TestAPlayerComesBackAsThemselves(t *testing.T) {
	t.Parallel()

	identities, store := knownIdentities(t)
	chunks, sim, peers := serveDeps(t)

	first := newFakeConn()
	firstDone := make(chan error, 1)
	go func() {
		firstDone <- session.Serve(context.Background(), first, serveConfig(), noTimeouts(), chunks, sim, peers, identities, 1, discard())
	}()

	// A first connection presents nothing and is answered with a minted token.
	first.in <- protocol.EncodeClientHello(vnet.ProtocolVersionCurrent, "Eivor")
	minted := welcomeToken(t, nextFrame(t, first))

	if err := first.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if err := <-firstDone; err != nil {
		t.Fatalf("the first session returned %v", err)
	}

	// The record is written in teardown, before the claim is released — so by the time
	// Serve has returned it is readable.
	token, err := identity.TokenFrom(minted)
	if err != nil {
		t.Fatalf("the welcome carried a token of the wrong length: %v", err)
	}
	rec, found, err := store.Load(identity.IDOf(token))
	if err != nil || !found {
		t.Fatalf("the player's record was not written: %v (found %v)", err, found)
	}
	if rec.Name != "Eivor" {
		t.Errorf("the record holds the name %q, want %q", rec.Name, "Eivor")
	}
	if rec.LastSeen.IsZero() {
		t.Error("the record holds no last-seen time")
	}

	second := newFakeConn()
	secondDone := make(chan error, 1)
	go func() {
		secondDone <- session.Serve(context.Background(), second, serveConfig(), noTimeouts(), chunks, sim, peers, identities, 2, discard())
	}()

	second.in <- protocol.EncodeClientHelloWithToken(vnet.ProtocolVersionCurrent, "Eivor", minted)
	if got := welcomeToken(t, nextFrame(t, second)); !bytes.Equal(got, minted) {
		t.Error("the returning client was answered with a different token")
	}

	_ = second.Close()
	<-secondDone
}

// syncWriter is an io.Writer several goroutines may log through at once. A session
// logs from its read loop, its streamer and its mining worker, so an unguarded
// bytes.Buffer here would be a data race rather than a test.
type syncWriter struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (w *syncWriter) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.buf.Write(p)
}

func (w *syncWriter) String() string {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.buf.String()
}

// TestTheTokenNeverReachesTheLog captures everything a full handshake logs, at every
// level, and looks for the token in every encoding a leak could take.
//
// It is a test about a rule rather than about a line: the token is a bearer
// credential on a transport that does not protect it, so a log file naming one is a
// log file that hands the identity to whoever can read it. Both handlers, because
// the JSON one is the one a Stringer would not have saved — it would have written a
// [32]byte out as an array of numbers.
func TestTheTokenNeverReachesTheLog(t *testing.T) {
	t.Parallel()

	presented := testToken(7)
	identities, _ := knownIdentities(t, presented)
	chunks, sim, peers := serveDeps(t)

	for name, handler := range map[string]func(*syncWriter) slog.Handler{
		"text": func(w *syncWriter) slog.Handler {
			return slog.NewTextHandler(w, &slog.HandlerOptions{Level: slog.LevelDebug})
		},
		"json": func(w *syncWriter) slog.Handler {
			return slog.NewJSONHandler(w, &slog.HandlerOptions{Level: slog.LevelDebug})
		},
	} {
		t.Run(name, func(t *testing.T) {
			out := &syncWriter{}
			conn := newFakeConn()
			done := make(chan error, 1)
			go func() {
				done <- session.Serve(context.Background(), conn, serveConfig(), noTimeouts(),
					chunks, sim, peers, identities, 9, slog.New(handler(out)))
			}()

			conn.in <- protocol.EncodeClientHelloWithToken(vnet.ProtocolVersionCurrent, "Eivor", presented[:])
			welcomed := welcomeToken(t, nextFrame(t, conn))
			conn.in <- encodePlayerInput(1, 1)

			if err := conn.Close(); err != nil {
				t.Fatalf("Close: %v", err)
			}
			select {
			case err := <-done:
				if err != nil {
					t.Fatalf("Serve returned %v", err)
				}
			case <-time.After(2 * time.Second):
				t.Fatal("Serve did not return")
			}

			logged := out.String()
			if logged == "" {
				t.Fatal("the handshake logged nothing, so this test proves nothing")
			}

			// Every shape a token could take on its way into a log line, for both the
			// token the client presented and the one the server answered with. The raw
			// bytes are checked too: a handler that wrote the array through fmt would put
			// them there verbatim.
			for label, token := range map[string][]byte{"presented": presented[:], "welcomed": welcomed} {
				for encoding, rendered := range map[string]string{
					"hex":       hex.EncodeToString(token),
					"base64":    base64.StdEncoding.EncodeToString(token),
					"base64url": base64.RawURLEncoding.EncodeToString(token),
					"raw bytes": string(token),
				} {
					if strings.Contains(logged, rendered) {
						t.Errorf("the %s token appears in the log as %s", label, encoding)
					}
				}
			}

			// The line that is supposed to be there, naming the identity rather than the
			// credential: eight hex characters of a digest nobody can reverse.
			short := identity.IDOf(presented).Short()
			if !strings.Contains(logged, short) {
				t.Errorf("the log does not name the player id %q", short)
			}
			if !strings.Contains(logged, "session admitted") {
				t.Error("the log has no admission line")
			}
		})
	}
}

// welcomeToken reads the identity token out of a ServerWelcome frame.
func welcomeToken(t *testing.T, frame []byte) []byte {
	t.Helper()

	env := vnet.GetRootAsEnvelope(frame, 0)
	if env.PayloadType() != vnet.PayloadServerWelcome {
		t.Fatalf("frame is %s, want a welcome", env.PayloadType())
	}
	token := welcomeFrom(t, env).PlayerTokenBytes()
	if len(token) != identity.TokenSize {
		t.Fatalf("the welcome carries a %d-byte token, want %d", len(token), identity.TokenSize)
	}
	return token
}

// helloWithTicket decodes a hello carrying both a token and a session ticket, which is
// what Resolve takes. Either may be nil.
func helloWithTicket(t *testing.T, token, ticket []byte) *protocol.ClientHello {
	t.Helper()

	msg, err := protocol.Decode(protocol.EncodeClientHelloFull(vnet.ProtocolVersionCurrent, "Eivor", token, ticket))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	return msg.ClientHello
}

// The V7 ticket rule, and the one thing this server does with a ticket today.
//
// schemas/handshake.fbs says a session_ticket is absent, empty or exactly
// SessionTicketLen bytes, and that any other length is BAD_REQUEST "decided before any
// account is looked up and before any signature is checked". **Before** is the half
// that needs a test rather than an assertion: the hello below presents a token the
// store knows perfectly well, so a rule that ran after the lookup would resume that
// identity and admit the session. It is refused instead, and nothing is claimed.
func TestAWrongLengthTicketIsRefusedBeforeAnythingIsLookedUp(t *testing.T) {
	t.Parallel()

	for name, size := range map[string]int{
		"one byte":              1,
		"one byte short":        protocol.SessionTicketLen - 1,
		"one byte too many":     protocol.SessionTicketLen + 1,
		"a token-length ticket": identity.TokenSize,
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			token := testToken(9)
			identities, _ := knownIdentities(t, token)

			resolved, err := identities.Resolve(helloWithTicket(t, token[:], make([]byte, size)))
			if err == nil {
				t.Fatal("a wrong-length ticket was admitted")
			}
			if got := refusalReason(t, err); got != vnet.RejectReasonBAD_REQUEST {
				t.Errorf("Reason = %s, want BAD_REQUEST", got)
			}
			if resolved != (session.Resolved{}) {
				t.Error("Resolve returned an identity beside its refusal")
			}
			// The claim is the observable half of "before anything is looked up": the
			// presented token names a stored identity, so a rule that ran second would
			// have resumed and claimed it.
			if identities.Count() != 0 {
				t.Error("a refused ticket left an identity claimed")
			}
		})
	}
}

// The refusal names the length and never the bytes. A ticket is a bearer credential
// exactly as a token is, and the first thing anybody does with a refusal is read it out
// of a log.
func TestATicketRefusalReportsTheLengthAndNeverTheTicket(t *testing.T) {
	t.Parallel()

	identities, _ := knownIdentities(t)
	ticket := bytes.Repeat([]byte{0xAB}, 7)

	_, err := identities.Resolve(helloWithTicket(t, nil, ticket))
	if err == nil {
		t.Fatal("a wrong-length ticket was admitted")
	}

	var refused *session.Refused
	if !errors.As(err, &refused) {
		t.Fatalf("error %v is not a refusal with a reason code", err)
	}
	if !strings.Contains(refused.Detail, "7") {
		t.Errorf("Detail %q does not say what was wrong", refused.Detail)
	}
	// Hex and raw, the two shapes a leak takes. Not a two-character needle: "ab" is a
	// substring of "absent", which is a word this refusal legitimately contains.
	if strings.Contains(strings.ToLower(refused.Detail), hex.EncodeToString(ticket)) ||
		strings.Contains(refused.Detail, string(ticket)) {
		t.Errorf("Detail %q carries the ticket's bytes", refused.Detail)
	}
}

// A ticket of the stated length, and no ticket at all, both pass the framing rule
// untouched — and neither changes which identity is resolved, because this server has
// not adopted ticket identity. That is the V6 handshake, which is what every consumer
// in this repository still speaks; the account service is a separate issue.
func TestALegalTicketDoesNotChangeWhichIdentityResolves(t *testing.T) {
	t.Parallel()

	for name, ticket := range map[string][]byte{
		"no ticket at all":     nil,
		"an empty ticket":      {},
		"a full-length ticket": bytes.Repeat([]byte{0x5C}, protocol.SessionTicketLen),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			token := testToken(11)
			identities, _ := knownIdentities(t, token)

			resolved, err := identities.Resolve(helloWithTicket(t, token[:], ticket))
			if err != nil {
				t.Fatalf("Resolve: %v", err)
			}
			if !resolved.Returning {
				t.Error("a known token did not resume its identity")
			}
			if resolved.Token != token {
				t.Error("the resumed session is playing under a different token")
			}
		})
	}
}
