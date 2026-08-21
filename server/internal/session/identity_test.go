package session_test

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"log/slog"
	"strings"
	"sync"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
)

// knownIdentities is a claim set over a real player store that already holds a
// character with a life for each account given, so those accounts resume rather than
// arrive with nothing.
//
// The first account named gets "Eivor", which is the name [helloCarrying] presents, so
// a hello resolves to it by name; the rest get names of their own, because a name is
// unique within a world and two seeded accounts cannot both have that one.
func knownIdentities(t *testing.T, known ...ticket.AccountID) (*session.Identities, *persist.Store) {
	t.Helper()

	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	for i, account := range known {
		seedCharacter(t, store, account, seededName(i))
	}
	return identitiesOver(store), store
}

// seededName is a name per seeded account: "Eivor" for the first, distinct for the
// rest.
func seededName(i int) string {
	if i == 0 {
		return "Eivor"
	}
	return fmt.Sprintf("Eivor the %dth", i+1)
}

// seedCharacter mints a character for account and gives it a life to come back to.
func seedCharacter(t *testing.T, store *persist.Store, account ticket.AccountID, name string) persist.Character {
	t.Helper()

	character, err := store.Create(testPlayerID(account), name)
	if err != nil {
		t.Fatalf("creating the seeded character: %v", err)
	}
	if err := store.Save(character.ID, livingRecord()); err != nil {
		t.Fatalf("seeding a record: %v", err)
	}
	return character
}

// onlyCharacter is the one character an account holds here, and a fatal failure when it
// holds none or several — every use below seeded exactly one.
func onlyCharacter(t *testing.T, store *persist.Store, account ticket.AccountID) persist.Character {
	t.Helper()

	held := store.Characters(testPlayerID(account))
	if len(held) != 1 {
		t.Fatalf("the account holds %d characters, want exactly 1", len(held))
	}
	return held[0]
}

// identitiesWith is a claim set admitting whatever verifier describes, for the tests
// that are about a *different* account service: another key, another world, another
// clock.
func identitiesWith(t *testing.T, verifier *session.Verifier) *session.Identities {
	t.Helper()

	identities, err := session.NewIdentities(nil, verifier, nil)
	if err != nil {
		t.Fatalf("NewIdentities: %v", err)
	}
	return identities
}

// verifierAt is a verifier for the package's own key and world, reading a clock the
// caller chose.
func verifierAt(t *testing.T, now time.Time) *session.Verifier {
	t.Helper()

	verifier, err := session.NewVerifier(testPair.Public(), testWorld, func() time.Time { return now })
	if err != nil {
		t.Fatalf("NewVerifier: %v", err)
	}
	return verifier
}

// livingRecord is the smallest record this build will resume: a living player, at a
// position, holding nothing.
//
// The health is the part a seeded fixture cannot leave out, and it now carries a second
// meaning: a record always describes somebody alive — a player who dies and quits is
// written as their respawn would have left them — so game.Life.Validate refuses a health
// of zero, *and* zero is what persist.Record.Unplayed reads as "this character exists
// and has never had a session". A record seeded without one is a character that has
// never played rather than a life to come back to.
//
// The character, its owner and its name are deliberately absent: persist.Store.Save
// fills all three from its own index and ignores what a caller puts here.
func livingRecord() persist.Record {
	return persist.Record{
		LastSeen: time.Unix(1, 0),
		Pos:      [3]float64{0.5, 64, 0.5},
		Health:   game.PlayerMaxHealth,
	}
}

// helloCarrying decodes a hello presenting raw as its session ticket, which is what
// Resolve takes. raw may be any length, including none.
func helloCarrying(t *testing.T, raw []byte) *protocol.ClientHello {
	t.Helper()

	return helloAsking(t, "Eivor", raw)
}

// helloAsking is helloCarrying with the display name chosen, for the tests where two
// accounts meet in one world: a character name is unique there, so two connections that
// both ask for "Eivor" is itself one of the things under test.
func helloAsking(t *testing.T, name string, raw []byte) *protocol.ClientHello {
	t.Helper()

	msg, err := protocol.Decode(protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, name, raw))
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

// refusalDetail is what the client is told, which is the half of a refusal that must
// be the same sentence whatever went wrong.
func refusalDetail(t *testing.T, err error) string {
	t.Helper()

	var refused *session.Refused
	if !errors.As(err, &refused) {
		t.Fatalf("error %v is not a refusal with a reason code", err)
	}
	return refused.Detail
}

func TestResolveAPlayer(t *testing.T) {
	t.Parallel()

	t.Run("a valid ticket names the account's player", func(t *testing.T) {
		t.Parallel()

		account := testAccount(1)
		identities, _ := knownIdentities(t)

		resolved, err := identities.Resolve(helloCarrying(t, testTicket(account)))
		if err != nil {
			t.Fatalf("Resolve: %v", err)
		}
		// The whole of the new model in one assertion: who this is was decided by
		// whoever signed the ticket, and this server computed the name from it.
		if resolved.ID != testPlayerID(account) {
			t.Error("the resolved player is not the one the ticket's account names")
		}
		if resolved.Returning {
			t.Error("a player with no stored record was reported as returning")
		}
		if resolved.Life != nil {
			t.Error("a player with no stored record arrived with a life")
		}
	})

	t.Run("a stored record resumes that player", func(t *testing.T) {
		t.Parallel()

		account := testAccount(2)
		identities, _ := knownIdentities(t, account)

		resolved, err := identities.Resolve(helloCarrying(t, testTicket(account)))
		if err != nil {
			t.Fatalf("Resolve: %v", err)
		}
		if !resolved.Returning {
			t.Error("an account the store holds a record for was not reported as returning")
		}
		if resolved.Life == nil {
			t.Fatal("a returning player arrived with no life")
		}
		if resolved.ID != testPlayerID(account) {
			t.Error("a resumed player has a different player id")
		}
	})

	t.Run("a second ticket for one account is ALREADY_CONNECTED", func(t *testing.T) {
		t.Parallel()

		// **The claim moved to the account**, which is what makes this test different
		// from the one it replaces: the two tickets below are *different bytes*, minted
		// separately, exactly as two machines signing in would present. What they share
		// is the account, and that is now the thing that cannot be in two places.
		account := testAccount(3)
		identities, _ := knownIdentities(t, account)

		first, err := identities.Resolve(helloCarrying(t, testTicket(account)))
		if err != nil {
			t.Fatalf("the first Resolve: %v", err)
		}

		// A second ticket minted a minute later, which is what two machines signing in
		// present: different bytes, one account. Identical bytes would let this test
		// pass on a rule about tickets rather than the rule about accounts.
		second := testTicketAt(account, time.Now().Add(time.Minute))
		if bytes.Equal(second, testTicket(account)) {
			t.Fatal("the two tickets are the same bytes, so this test would pass on the bytes rather than the account")
		}
		_, err = identities.Resolve(helloCarrying(t, second))
		if err == nil {
			t.Fatal("a second session on one account was admitted")
		}
		if got := refusalReason(t, err); got != vnet.RejectReasonALREADY_CONNECTED {
			t.Errorf("the second hello was refused with %s, want ALREADY_CONNECTED", got)
		}
		if identities.Count() != 1 {
			t.Errorf("%d players are live, want the one that was admitted", identities.Count())
		}

		// And it is free again once the session that held it releases.
		identities.Release(first.ID)
		if _, err := identities.Resolve(helloCarrying(t, testTicket(account))); err != nil {
			t.Fatalf("the account was not free after Release: %v", err)
		}
	})

	t.Run("two accounts are two players", func(t *testing.T) {
		t.Parallel()

		identities, _ := knownIdentities(t)

		first, err := identities.Resolve(helloAsking(t, "Eivor", testTicket(testAccount(4))))
		if err != nil {
			t.Fatalf("the first Resolve: %v", err)
		}
		// A name of its own: two accounts are two players, and on this world they are
		// also two names.
		second, err := identities.Resolve(helloAsking(t, "Sigrun", testTicket(testAccount(5))))
		if err != nil {
			t.Fatalf("the second Resolve: %v", err)
		}
		if first.ID == second.ID {
			t.Error("two accounts resolved to one player, so the exclusivity rule could never fire")
		}
	})

	t.Run("an ephemeral world admits and never resumes", func(t *testing.T) {
		t.Parallel()

		// No store: verification and exclusivity still work, nothing is written, and no
		// life is ever found. **The account still names the same player**, which is what
		// changed here: an ephemeral world costs a returning player their life, and no
		// longer costs them their name.
		identities := ephemeralIdentities()
		account := testAccount(6)

		resolved, err := identities.Resolve(helloCarrying(t, testTicket(account)))
		if err != nil {
			t.Fatalf("Resolve: %v", err)
		}
		if resolved.Returning || resolved.Life != nil {
			t.Error("an ephemeral world resumed a life it cannot have stored")
		}
		if resolved.ID != testPlayerID(account) {
			t.Error("an ephemeral world gave the account a different player id")
		}
	})
}

// The refusals, each verified in the direction that fails, and each one distinguishable
// from the others **only** to whoever is reading the log.
//
// The two halves of that are the two assertions every case makes: the cause is the
// sentinel that names what went wrong, and the detail is the one sentence every case
// shares. A build that told the client which of these five it was would pass the first
// assertion and fail the second, which is the direction that matters — an oracle is
// added by making a message more helpful.
func TestATicketThisServerWillNotAdmitIsRefused(t *testing.T) {
	t.Parallel()

	foreign, err := ticket.LoadOrCreate(t.TempDir())
	if err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}
	foreignTicket, _, err := foreign.Mint(testAccount(7), testWorld, time.Now())
	if err != nil {
		t.Fatalf("Mint with another key: %v", err)
	}

	elsewhere, err := ticket.WorldIDFor("asgard")
	if err != nil {
		t.Fatalf("WorldIDFor: %v", err)
	}
	elsewhereVerifier, err := session.NewVerifier(testPair.Public(), elsewhere, nil)
	if err != nil {
		t.Fatalf("NewVerifier: %v", err)
	}

	accountTicket, _, err := testPair.MintAccountTicket(testAccount(8), time.Now())
	if err != nil {
		t.Fatalf("MintAccountTicket: %v", err)
	}

	tampered := testTicket(testAccount(9))
	tampered[0] ^= 0x01

	cases := map[string]struct {
		identities *session.Identities
		presented  []byte
		want       error
	}{
		// Malformed, in the two shapes a client can produce without a signing key: a
		// ticket that is not the right length at all, and 96 bytes that are not a
		// ticket. The first is refused from a comparison before anything is verified;
		// the second gets as far as the signature and no further.
		"no ticket at all":      {presented: nil, want: session.ErrTicketAbsent},
		"an empty ticket":       {presented: []byte{}, want: session.ErrTicketAbsent},
		"one byte short":        {presented: make([]byte, protocol.SessionTicketLen-1), want: session.ErrTicketLength},
		"one byte too many":     {presented: make([]byte, protocol.SessionTicketLen+1), want: session.ErrTicketLength},
		"ninety-six zeroes":     {presented: make([]byte, protocol.SessionTicketLen), want: ticket.ErrBadSignature},
		"a tampered ticket":     {presented: tampered, want: ticket.ErrBadSignature},
		"signed by another key": {presented: foreignTicket[:], want: ticket.ErrBadSignature},

		// Issued for another world, in both of its shapes. A ticket for somebody else's
		// world is what stops one operator replaying their players' tickets here; an
		// account ticket names *no* world and is a credential for talking to the account
		// service rather than for joining a game.
		"issued for another world": {
			identities: identitiesWith(t, elsewhereVerifier),
			presented:  testTicket(testAccount(10)),
			want:       ticket.ErrWrongWorld,
		},
		"an account ticket, naming no world": {presented: accountTicket[:], want: ticket.ErrWrongWorld},
	}

	// Expired is the one case that needs a clock rather than a fixture: the ticket is
	// perfectly good and this server is reading the time on the far side of its life.
	cases["expired"] = struct {
		identities *session.Identities
		presented  []byte
		want       error
	}{
		identities: identitiesWith(t, verifierAt(t, time.Now().Add(ticket.Lifetime+time.Minute))),
		presented:  testTicket(testAccount(11)),
		want:       ticket.ErrExpired,
	}

	details := make(map[string]string, len(cases))
	var mu sync.Mutex

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			identities := tc.identities
			if identities == nil {
				// A store that holds a record for every account these cases use, so a
				// refusal that ran *after* the lookup would be visible as a resumed
				// session rather than as an equally red test.
				identities, _ = knownIdentities(t,
					testAccount(7), testAccount(8), testAccount(9), testAccount(10), testAccount(11))
			}

			resolved, err := identities.Resolve(helloCarrying(t, tc.presented))
			if err == nil {
				t.Fatal("the ticket was admitted")
			}
			if !errors.Is(err, tc.want) {
				t.Errorf("the refusal is %v, which does not name %v", err, tc.want)
			}
			if got := refusalReason(t, err); got != vnet.RejectReasonBAD_REQUEST {
				t.Errorf("Reason = %s, want BAD_REQUEST", got)
			}
			if resolved != (session.Resolved{}) {
				t.Error("Resolve returned a player beside its refusal")
			}
			// Nothing was claimed, which is the observable half of "nothing is looked up
			// for a ticket nobody vouched for".
			if identities.Count() != 0 {
				t.Error("a refused ticket left a player claimed")
			}

			mu.Lock()
			details[name] = refusalDetail(t, err)
			mu.Unlock()
		})
	}

	t.Cleanup(func() {
		// Collected across the subtests and compared once: every one of them tells the
		// client the same sentence. Skipped rather than failed when a subtest did not
		// record one, because that subtest has already failed and this would only
		// report it twice.
		if len(details) != len(cases) {
			return
		}
		var first, firstName string
		for name, detail := range details {
			if firstName == "" {
				first, firstName = detail, name
				continue
			}
			if detail != first {
				t.Errorf("%q is told %q and %q is told %q; a client that can tell two refusals apart "+
					"can ask this server about tickets it was never shown", name, detail, firstName, first)
			}
		}
	})
}

// The refusal names no part of the ticket, in any encoding.
//
// A ticket is a bearer credential and a refusal is the first thing anybody reads out of
// a log, so this is the same rule the whole handshake is held to, asked of the one
// string this server composes about a ticket it turned away.
func TestATicketRefusalCarriesNothingOfTheTicket(t *testing.T) {
	t.Parallel()

	identities, _ := knownIdentities(t)
	presented := testTicket(testAccount(12))

	_, err := identities.Resolve(helloCarrying(t, presented[:protocol.SessionTicketLen-1]))
	if err == nil {
		t.Fatal("a wrong-length ticket was admitted")
	}

	var refused *session.Refused
	if !errors.As(err, &refused) {
		t.Fatalf("error %v is not a refusal with a reason code", err)
	}
	// Both halves: what reaches the client, and what reaches the log.
	for what, text := range map[string]string{"the detail": refused.Detail, "the cause": refused.Cause.Error()} {
		for encoding, rendered := range map[string]string{
			"hex":       hex.EncodeToString(presented),
			"base64":    base64.StdEncoding.EncodeToString(presented),
			"base64url": base64.RawURLEncoding.EncodeToString(presented),
			"raw bytes": string(presented),
		} {
			if strings.Contains(text, rendered) {
				t.Errorf("%s carries the ticket as %s", what, encoding)
			}
		}
	}
	// The cause is allowed to say how long it was, and it is the only thing about the
	// ticket it may say. Asserted so that a cause which said nothing at all — leaving an
	// operator with five identical lines — would fail here.
	if !strings.Contains(refused.Cause.Error(), "95") {
		t.Errorf("the cause %q does not say what was wrong", refused.Cause.Error())
	}
}

// The wire half of the same rule, driven through a whole connection: five reasons to be
// turned away, one frame.
//
// Through Serve rather than through Resolve because the frame is what a client sees, and
// the frame is composed one layer out from the refusal. A detail that leaked the reason
// would have to leak it here.
func TestEveryRefusedTicketLeavesTheSameFrame(t *testing.T) {
	t.Parallel()

	foreign, err := ticket.LoadOrCreate(t.TempDir())
	if err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}
	foreignTicket, _, err := foreign.Mint(testAccount(13), testWorld, time.Now())
	if err != nil {
		t.Fatalf("Mint with another key: %v", err)
	}
	stale, _, err := testPair.Mint(testAccount(14), testWorld, time.Now().Add(-ticket.Lifetime-time.Minute))
	if err != nil {
		t.Fatalf("Mint an expired ticket: %v", err)
	}
	otherWorld, err := ticket.WorldIDFor("asgard")
	if err != nil {
		t.Fatalf("WorldIDFor: %v", err)
	}
	wrongWorld, _, err := testPair.Mint(testAccount(15), otherWorld, time.Now())
	if err != nil {
		t.Fatalf("Mint for another world: %v", err)
	}

	presented := map[string][]byte{
		"absent":        nil,
		"malformed":     make([]byte, protocol.SessionTicketLen-1),
		"another key":   foreignTicket[:],
		"expired":       stale[:],
		"another world": wrongWorld[:],
		"not a ticket":  make([]byte, protocol.SessionTicketLen),
	}

	frames := make(map[string][]byte, len(presented))
	for name, raw := range presented {
		chunks, sim, peers := serveDeps(t)
		conn := newFakeConn()
		done := make(chan error, 1)
		go func() {
			done <- session.Serve(context.Background(), conn, serveConfig(), noTimeouts(),
				chunks, sim, peers, ephemeralIdentities(), 1, discard())
		}()

		conn.in <- protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", raw)
		frame := nextFrame(t, conn)
		env := vnet.GetRootAsEnvelope(frame, 0)
		if env.PayloadType() != vnet.PayloadServerReject {
			t.Fatalf("%s got %s, want a rejection", name, env.PayloadType())
		}
		if got := rejectFrom(t, env).Reason(); got != vnet.RejectReasonBAD_REQUEST {
			t.Errorf("%s was refused with %s, want BAD_REQUEST", name, got)
		}
		frames[name] = bytes.Clone(frame)

		select {
		case err := <-done:
			if err != nil {
				t.Fatalf("%s: Serve returned %v, want nil for a refused handshake", name, err)
			}
		case <-time.After(2 * time.Second):
			t.Fatalf("%s: Serve did not return", name)
		}
	}

	var reference, referenceName string
	for name, frame := range frames {
		if referenceName == "" {
			reference, referenceName = string(frame), name
			continue
		}
		if string(frame) != reference {
			t.Errorf("the frame for %q differs from the one for %q; a client can tell the two refusals apart",
				name, referenceName)
		}
	}
}

// TestServeRefusesASecondSessionOnOneAccount is the rule end to end, through the whole
// connection lifetime rather than through Resolve alone — because the half that is easy
// to get wrong is not the refusal, it is the release.
func TestServeRefusesASecondSessionOnOneAccount(t *testing.T) {
	t.Parallel()

	account := testAccount(16)
	identities, _ := knownIdentities(t, account)
	chunks, sim, peers := serveDeps(t)

	first := newFakeConn()
	firstDone := make(chan error, 1)
	go func() {
		firstDone <- session.Serve(context.Background(), first, serveConfig(), noTimeouts(), chunks, sim, peers, identities, 1, discard())
	}()

	first.in <- protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", testTicket(account))
	if got := vnet.GetRootAsEnvelope(nextFrame(t, first), 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the first session got %s, want a welcome", got)
	}

	second := newFakeConn()
	secondDone := make(chan error, 1)
	go func() {
		secondDone <- session.Serve(context.Background(), second, serveConfig(), noTimeouts(), chunks, sim, peers, identities, 2, discard())
	}()

	// A second ticket, freshly minted for the same account: different bytes, same
	// person, and it is the person the claim is about.
	second.in <- protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", testTicket(account))
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
	// returned, the account is free — which is what makes a reconnect right after a
	// disconnect work, and what makes an idle session hand its place back.
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
		t.Fatalf("%d players are still claimed after every session ended", identities.Count())
	}

	third := newFakeConn()
	thirdDone := make(chan error, 1)
	go func() {
		thirdDone <- session.Serve(context.Background(), third, serveConfig(), noTimeouts(), chunks, sim, peers, identities, 3, discard())
	}()
	third.in <- protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", testTicket(account))
	if got := vnet.GetRootAsEnvelope(nextFrame(t, third), 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the reconnect got %s, want a welcome", got)
	}
	_ = third.Close()
	<-thirdDone
}

func TestServeRefusesAConnectionWithNoTicket(t *testing.T) {
	t.Parallel()

	identities, _ := knownIdentities(t)
	chunks, sim, peers := serveDeps(t)

	conn := newFakeConn()
	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, serveConfig(), noTimeouts(), chunks, sim, peers, identities, 4, discard())
	}()

	// The hello a V6 client sends: a display name and nothing else. It is a legal
	// message and this server admits nobody it cannot name.
	conn.in <- protocol.EncodeClientHello(vnet.ProtocolVersionCurrent, "Eivor")
	env := vnet.GetRootAsEnvelope(nextFrame(t, conn), 0)
	if env.PayloadType() != vnet.PayloadServerReject {
		t.Fatalf("reply is %s, want a rejection", env.PayloadType())
	}
	if got := rejectFrom(t, env).Reason(); got != vnet.RejectReasonBAD_REQUEST {
		t.Errorf("Reason = %s, want BAD_REQUEST", got)
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
		t.Error("a refused handshake left a player claimed")
	}
	if sim.Count() != 0 {
		t.Error("a refused handshake joined the simulation")
	}
}

// The retired field is ignored on the way in, whatever is in it.
//
// schemas/handshake.fbs retires `player_token` at V7 and says a V7 server reads past
// it; this is that sentence as a test. The lengths below include the ones a V6 server
// refused as BAD_REQUEST, because "reads past it" has to mean the length too — a rule
// that survived would be a V6 rule refusing a V7 client for a field neither of them
// uses.
func TestTheRetiredTokenFieldIsIgnored(t *testing.T) {
	t.Parallel()

	account := testAccount(17)
	for name, token := range map[string][]byte{
		"no token":              nil,
		"an empty token":        {},
		"a V6-length token":     bytes.Repeat([]byte{0x5C}, protocol.PlayerTokenLen),
		"a wrong-length token":  bytes.Repeat([]byte{0x5C}, 7),
		"a ticket-length token": bytes.Repeat([]byte{0x5C}, protocol.SessionTicketLen),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			identities, _ := knownIdentities(t, account)
			msg, err := protocol.Decode(protocol.EncodeClientHelloFull(
				vnet.ProtocolVersionCurrent, "Eivor", token, testTicket(account)))
			if err != nil {
				t.Fatalf("Decode: %v", err)
			}

			resolved, err := identities.Resolve(msg.ClientHello)
			if err != nil {
				t.Fatalf("Resolve: %v", err)
			}
			if resolved.ID != testPlayerID(account) {
				t.Error("the retired token changed which player resolved")
			}
			if !resolved.Returning {
				t.Error("the retired token changed whether the stored life was found")
			}
		})
	}
}

// TestAPlayerComesBackAsThemselves is the whole point of the issue, in one test: a
// session ends, its record is written, and the **account** brings the same player back
// — with a ticket this server has never seen before.
func TestAPlayerComesBackAsThemselves(t *testing.T) {
	t.Parallel()

	account := testAccount(18)
	identities, store := knownIdentities(t)
	chunks, sim, peers := serveDeps(t)

	first := newFakeConn()
	firstDone := make(chan error, 1)
	go func() {
		firstDone <- session.Serve(context.Background(), first, serveConfig(), noTimeouts(), chunks, sim, peers, identities, 1, discard())
	}()

	first.in <- protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", testTicket(account))
	if got := vnet.GetRootAsEnvelope(nextFrame(t, first), 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the first session got %s, want a welcome", got)
	}

	if err := first.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if err := <-firstDone; err != nil {
		t.Fatalf("the first session returned %v", err)
	}

	// The record is written in teardown, before the claim is released — so by the time
	// Serve has returned it is readable, under the character the account resolved to.
	// **That ordering is what #102 settled and what a change of key must not weaken**:
	// the write happens after sim.Leave and before Release, and the character it writes
	// under travels with the resolution rather than being read back out of the claim.
	character := onlyCharacter(t, store, account)
	rec, found, err := store.Load(character.ID)
	if err != nil || !found {
		t.Fatalf("the character's record was not written: %v (found %v)", err, found)
	}
	if rec.Name != "Eivor" {
		t.Errorf("the record holds the name %q, want %q", rec.Name, "Eivor")
	}
	if rec.Owner != testPlayerID(account) {
		t.Errorf("the record is owned by %s, want the account's player id", rec.Owner.Short())
	}
	if rec.LastSeen.IsZero() {
		t.Error("the record holds no last-seen time")
	}

	second := newFakeConn()
	secondDone := make(chan error, 1)
	go func() {
		secondDone <- session.Serve(context.Background(), second, serveConfig(), noTimeouts(), chunks, sim, peers, identities, 2, discard())
	}()

	// A different ticket, the same person. Nothing the first session handed the client
	// is presented here, because there no longer is anything: what makes this the same
	// player is the account the account service signed.
	second.in <- protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", testTicket(account))
	if got := vnet.GetRootAsEnvelope(nextFrame(t, second), 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the returning client got %s, want a welcome", got)
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

// TestTheCredentialNeverReachesTheLog captures everything a full handshake logs, at
// every level, and looks for the credential in every encoding a leak could take.
//
// **Extended rather than replaced**, which matters because what it looks for changed
// while what it proves did not. It used to search for the identity token in both
// directions — presented and welcomed — and both of those are gone with the minting. It
// now searches for the three things a V7 handshake handles that must never be written
// down: the ticket, the account it names, and the signature over it. The two positive
// assertions are the originals: the log still names the player id, and it still says a
// session was admitted, because a secrecy test that captured nothing would pass while
// proving nothing.
//
// Both handlers, because the JSON one is the one a Stringer would not have saved — it
// would write a [16]byte out as an array of numbers.
func TestTheCredentialNeverReachesTheLog(t *testing.T) {
	t.Parallel()

	account := testAccount(19)
	presented := testTicket(account)

	for name, handler := range map[string]func(*syncWriter) slog.Handler{
		"text": func(w *syncWriter) slog.Handler {
			return slog.NewTextHandler(w, &slog.HandlerOptions{Level: slog.LevelDebug})
		},
		"json": func(w *syncWriter) slog.Handler {
			return slog.NewJSONHandler(w, &slog.HandlerOptions{Level: slog.LevelDebug})
		},
	} {
		t.Run(name, func(t *testing.T) {
			identities, _ := knownIdentities(t, account)
			chunks, sim, peers := serveDeps(t)

			out := &syncWriter{}
			conn := newFakeConn()
			done := make(chan error, 1)
			go func() {
				done <- session.Serve(context.Background(), conn, serveConfig(), noTimeouts(),
					chunks, sim, peers, identities, 9, slog.New(handler(out)))
			}()

			conn.in <- protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", presented)
			if got := vnet.GetRootAsEnvelope(nextFrame(t, conn), 0).PayloadType(); got != vnet.PayloadServerWelcome {
				t.Fatalf("the session got %s, want a welcome", got)
			}
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

			// The three secrets of a V7 handshake, and the signature separately from the
			// whole ticket: a log line that carried only the last 64 bytes would still be
			// signature material, and searching for the ticket alone would not find it.
			for label, secret := range map[string][]byte{
				"ticket":    presented,
				"account":   account[:],
				"signature": presented[len(presented)-ed25519.SignatureSize:],
			} {
				for encoding, rendered := range map[string]string{
					"hex":       hex.EncodeToString(secret),
					"base64":    base64.StdEncoding.EncodeToString(secret),
					"base64url": base64.RawURLEncoding.EncodeToString(secret),
					"raw bytes": string(secret),
				} {
					if strings.Contains(logged, rendered) {
						// The value is not quoted back: a failure means the log holds a
						// credential, and this repository's CI log is public.
						t.Errorf("the %s appears in the log as %s", label, encoding)
					}
				}
			}

			// The line that is supposed to be there, naming the player rather than the
			// credential: eight hex characters of a digest nobody can reverse.
			short := testPlayerID(account).Short()
			if !strings.Contains(logged, short) {
				t.Errorf("the log does not name the player id %q", short)
			}
			if !strings.Contains(logged, "session admitted") {
				t.Error("the log has no admission line")
			}
		})
	}
}

// A refused handshake writes the same three secrets nowhere either, and it is the path
// where one would most easily arrive: the refusal is the thing somebody investigates.
func TestARefusedTicketNeverReachesTheLogEither(t *testing.T) {
	t.Parallel()

	account := testAccount(20)
	stale, _, err := testPair.Mint(account, testWorld, time.Now().Add(-ticket.Lifetime-time.Minute))
	if err != nil {
		t.Fatalf("Mint an expired ticket: %v", err)
	}

	identities, _ := knownIdentities(t, account)
	chunks, sim, peers := serveDeps(t)

	out := &syncWriter{}
	conn := newFakeConn()
	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, serveConfig(), noTimeouts(),
			chunks, sim, peers, identities, 9, slog.New(slog.NewJSONHandler(out, &slog.HandlerOptions{Level: slog.LevelDebug})))
	}()

	conn.in <- protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", stale[:])
	if got := vnet.GetRootAsEnvelope(nextFrame(t, conn), 0).PayloadType(); got != vnet.PayloadServerReject {
		t.Fatalf("the session got %s, want a rejection", got)
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
	for label, secret := range map[string][]byte{
		"ticket":    stale[:],
		"account":   account[:],
		"signature": stale[len(stale)-ed25519.SignatureSize:],
	} {
		for encoding, rendered := range map[string]string{
			"hex":       hex.EncodeToString(secret),
			"base64":    base64.StdEncoding.EncodeToString(secret),
			"base64url": base64.RawURLEncoding.EncodeToString(secret),
			"raw bytes": string(secret),
		} {
			if strings.Contains(logged, rendered) {
				t.Errorf("the %s appears in the refusal log as %s", label, encoding)
			}
		}
	}

	// And the operator is told which refusal it was, which is the whole reason the
	// cause is logged at all. Without this the test above could be satisfied by a
	// server that logged nothing.
	if !strings.Contains(logged, "handshake refused") {
		t.Error("the log has no refusal line")
	}
	if !strings.Contains(logged, "expired") {
		t.Error("the refusal line does not say which of the five refusals this was")
	}
}

// TestResolveACharacter is the character half of a resolution: which of an account's
// characters is playing, and the three ways asking for a new one is refused.
//
// **Every refusal here is authoritative and belongs to no other layer.** The client
// offers a name; the server decides whether it may be worn, whether this account may
// hold another character at all, and which reason code says so. A client that decided
// any of it would be deciding who else exists on this world.
func TestResolveACharacter(t *testing.T) {
	t.Parallel()

	t.Run("a first connection creates the character it asked for", func(t *testing.T) {
		t.Parallel()

		identities, store := knownIdentities(t)
		account := testAccount(30)

		resolved, err := identities.Resolve(helloAsking(t, "Halvar", testTicket(account)))
		if err != nil {
			t.Fatalf("Resolve: %v", err)
		}
		if resolved.Character.IsZero() {
			t.Error("the session was admitted with no character")
		}
		if resolved.Name != "Halvar" {
			t.Errorf("the character is named %q, want %q", resolved.Name, "Halvar")
		}
		if resolved.Returning || resolved.Life != nil {
			t.Error("a character that has just been created came back with a life")
		}
		held := store.Characters(testPlayerID(account))
		if len(held) != 1 || held[0].ID != resolved.Character {
			t.Errorf("the store holds %+v, want the character that was created", held)
		}
	})

	t.Run("a name another account is wearing is CHARACTER_NAME_TAKEN", func(t *testing.T) {
		t.Parallel()

		identities, _ := knownIdentities(t, testAccount(31))

		// testAccount(31) was seeded as "Eivor". A different account asking for it is
		// refused, and told which of the two refusals it was — the contract says a
		// client may tell "somebody has it" from "we will not accept it", because the
		// player acts on them differently.
		_, err := identities.Resolve(helloAsking(t, "eivor", testTicket(testAccount(32))))
		if got := refusalReason(t, err); got != vnet.RejectReasonCHARACTER_NAME_TAKEN {
			t.Errorf("the refusal is %s, want CHARACTER_NAME_TAKEN", got)
		}
		if identities.Count() != 0 {
			t.Error("a refused creation left a player claimed")
		}
	})

	t.Run("a name this world will not accept is CHARACTER_NAME_REFUSED", func(t *testing.T) {
		t.Parallel()

		identities, _ := knownIdentities(t)

		for _, name := range []string{"", "   ", strings.Repeat("a", persist.MaxNameBytes+1), "Eivor\nSigrun"} {
			_, err := identities.Resolve(helloAsking(t, name, testTicket(testAccount(33))))
			if got := refusalReason(t, err); got != vnet.RejectReasonCHARACTER_NAME_REFUSED {
				t.Errorf("%q was refused with %s, want CHARACTER_NAME_REFUSED", name, got)
			}
		}
	})

	t.Run("a full roster is not a locked door", func(t *testing.T) {
		t.Parallel()

		identities, store := knownIdentities(t)
		account := testAccount(34)

		// Filled through the store, because the wire has no way to ask for a second
		// character yet — that is the next issue. The rule is the store's either way.
		for i := range persist.MaxCharactersPerAccount {
			seedCharacter(t, store, account, fmt.Sprintf("Halvar%d", i))
		}
		if _, err := store.Create(testPlayerID(account), "OneTooMany"); !errors.Is(err, persist.ErrCharacterLimit) {
			t.Fatalf("the store did not refuse past the limit: %v", err)
		}

		// **CHARACTER_LIMIT_REACHED is not reachable from a hello, and that is a
		// property of the interim resolution rather than of the rule.** An account with
		// characters plays one instead of creating another, so the only path that can
		// reach the limit is the CreateCharacterRequest the next issue puts on the wire.
		// The mapping from the store's refusal to the reason code is tested where it
		// lives — see TestRefuseCharacterMapsEveryStoreRefusal.
		resolved, err := identities.Resolve(helloAsking(t, "Halvar0", testTicket(account)))
		if err != nil {
			t.Fatalf("Resolve for an account at its limit: %v", err)
		}
		if resolved.Name != "Halvar0" {
			t.Errorf("the account resolved to %q, want the character it asked for", resolved.Name)
		}
	})

	t.Run("an account with characters plays one rather than making another", func(t *testing.T) {
		t.Parallel()

		identities, store := knownIdentities(t)
		account := testAccount(35)
		mine := seedCharacter(t, store, account, "Eivor")

		// A name it does not have, and one nobody has: still no creation. An account's
		// second character is made through the wire exchange or not at all.
		resolved, err := identities.Resolve(helloAsking(t, "SomebodyElse", testTicket(account)))
		if err != nil {
			t.Fatalf("Resolve: %v", err)
		}
		if resolved.Character != mine.ID {
			t.Errorf("the session plays %s, want the character the account already had", resolved.Character)
		}
		if held := store.Characters(testPlayerID(account)); len(held) != 1 {
			t.Errorf("the account now holds %d characters; a hello created one", len(held))
		}
		if _, taken := store.Named("SomebodyElse"); taken {
			t.Error("a hello took a name without creating a character")
		}
	})

	t.Run("a hello names which of several characters plays", func(t *testing.T) {
		t.Parallel()

		identities, store := knownIdentities(t)
		account := testAccount(36)
		first := seedCharacter(t, store, account, "Eivor")
		second := seedCharacter(t, store, account, "Sigrun")

		for _, want := range []persist.Character{first, second} {
			resolved, err := identities.Resolve(helloAsking(t, want.Name, testTicket(account)))
			if err != nil {
				t.Fatalf("Resolve for %q: %v", want.Name, err)
			}
			if resolved.Character != want.ID || resolved.Name != want.Name {
				t.Errorf("asking for %q played %s/%q", want.Name, resolved.Character, resolved.Name)
			}
			identities.Release(resolved.ID)
		}

		// A name this account does not hold settles on the lowest id, deterministically:
		// two connections asking the same unknown name must play the same character.
		lowest := first
		if second.ID < lowest.ID {
			lowest = second
		}
		resolved, err := identities.Resolve(helloAsking(t, "Halvar", testTicket(account)))
		if err != nil {
			t.Fatalf("Resolve: %v", err)
		}
		if resolved.Character != lowest.ID {
			t.Errorf("an unknown name played %s, want the lowest id %s", resolved.Character, lowest.ID)
		}
	})

	t.Run("one account's character is not another's to play", func(t *testing.T) {
		t.Parallel()

		identities, store := knownIdentities(t)
		mine := seedCharacter(t, store, testAccount(37), "Eivor")

		// Another account asking for that exact name does not get it — it is refused as
		// taken, because naming somebody else's character is not a way to play it.
		_, err := identities.Resolve(helloAsking(t, "Eivor", testTicket(testAccount(38))))
		if got := refusalReason(t, err); got != vnet.RejectReasonCHARACTER_NAME_TAKEN {
			t.Errorf("the refusal is %s, want CHARACTER_NAME_TAKEN", got)
		}
		if got, _ := store.Named("Eivor"); got.ID != mine.ID {
			t.Error("the character changed hands")
		}
	})
}
