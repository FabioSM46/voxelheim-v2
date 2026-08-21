package ticket

import (
	"bytes"
	"crypto/ed25519"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"strconv"
	"strings"
	"testing"
	"time"
)

// newPair is a key pair in a directory belonging to this test.
//
// Every key in this package's tests is generated here, at run time, into a temporary
// directory. **No key pair is committed**, not even a public half: a fixture key in a
// public repository is a key somebody eventually signs something with.
func newPair(t *testing.T) *Pair {
	t.Helper()

	pair, err := LoadOrCreate(t.TempDir())
	if err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}
	return pair
}

// midgard is the world these tests issue for, and hel is the one that must refuse those
// tickets.
func midgard(t *testing.T) WorldID { return worldID(t, "midgard") }
func hel(t *testing.T) WorldID     { return worldID(t, "hel") }

func worldID(t *testing.T, name string) WorldID {
	t.Helper()

	id, err := WorldIDFor(name)
	if err != nil {
		t.Fatalf("WorldIDFor(%q): %v", name, err)
	}
	return id
}

// anAccount is an account id that is not the zero one.
func anAccount() AccountID {
	var id AccountID
	for i := range id {
		id[i] = byte(i + 1)
	}
	return id
}

// **The wire contract, and the reason this package exists in this shape.**
//
// schemas/handshake.fbs fixes a session ticket at 96 bytes — a 32-byte body and a
// 64-byte detached Ed25519 signature over it — and this layout is a consequence of that
// number rather than a choice beside it. The numbers are written out literally: deriving
// the expectation from the same constants it is checking would pin nothing.
func TestATicketIsTheBytesTheHandshakeCarries(t *testing.T) {
	t.Parallel()

	if BodySize != 32 {
		t.Errorf("a ticket body is %d bytes, and the contract says 32", BodySize)
	}
	if Size != 96 {
		t.Errorf("a ticket is %d bytes, and the contract says 96", Size)
	}
	if ed25519.SignatureSize != 64 {
		t.Errorf("an Ed25519 signature is %d bytes, and the contract says 64", ed25519.SignatureSize)
	}
	if AccountIDSize+WorldIDSize+expiresAtSize != BodySize {
		t.Error("the body's fields do not add up to the body")
	}

	// And a minted ticket really is that many bytes: TicketFrom is the length rule
	// stated once, so putting one back through it is the check that a mint produced
	// something the wire will carry.
	pair := newPair(t)
	minted, _, err := pair.Mint(anAccount(), midgard(t), time.Now())
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}
	if _, err := TicketFrom(minted[:]); err != nil {
		t.Errorf("a minted ticket is not the length a ticket is: %v", err)
	}
}

// Sign and verify: the whole of the happy path, and the claims that come back are the
// ones that went in.
func TestAMintedTicketVerifiesAndSaysWhatItWasMintedWith(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	account, world := anAccount(), midgard(t)
	now := time.Now()

	minted, claims, err := pair.Mint(account, world, now)
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}

	verified, err := Verify(pair.Public(), minted[:], world, now)
	if err != nil {
		t.Fatalf("Verify: %v", err)
	}
	if verified.Account != account {
		t.Errorf("the ticket names account %s, want %s", verified.Account, account)
	}
	if verified.World != world {
		t.Errorf("the ticket names world %s, want %s", verified.World, world)
	}
	// The claims Mint answered with are the claims a verifier reads back, to the
	// second. A caller tells somebody when the ticket runs out from the first, and a
	// game server decides from the second.
	if !verified.ExpiresAt.Equal(claims.ExpiresAt) {
		t.Errorf("Mint said the ticket expires at %s and the ticket says %s", claims.ExpiresAt, verified.ExpiresAt)
	}
	if want := now.Add(Lifetime).UTC().Truncate(time.Second); !verified.ExpiresAt.Equal(want) {
		t.Errorf("the ticket expires at %s, want %s — one Lifetime from the moment it was minted", verified.ExpiresAt, want)
	}
}

// **A tampered ticket is refused, wherever the tampering is.** Every byte of the body is
// under the signature, so an edit anywhere in the first 32 is caught by the same check
// that catches an edit to the signature itself.
func TestATamperedTicketIsRefused(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	world := midgard(t)
	now := time.Now()

	minted, _, err := pair.Mint(anAccount(), world, now)
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}

	for name, at := range map[string]int{
		"the first byte of the account":   offAccount,
		"the last byte of the account":    offAccount + AccountIDSize - 1,
		"a byte of the world":             offWorld,
		"a byte of the expiry":            offExpires,
		"the first byte of the body":      0,
		"the last byte of the body":       BodySize - 1,
		"the first byte of the signature": BodySize,
		"the last byte of the signature":  Size - 1,
	} {
		t.Run(name+" was flipped", func(t *testing.T) {
			t.Parallel()

			edited := minted
			edited[at] ^= 0x01

			_, err := Verify(pair.Public(), edited[:], world, now)
			if !errors.Is(err, ErrBadSignature) {
				t.Errorf("a tampered ticket answered %v, want ErrBadSignature", err)
			}
		})
	}
}

// **A signature has to say what it is a signature of, and until #138 this one did not.**
//
// The key's guarantee was "the account service signed these 32 bytes", which is not the
// claim a game server needs — any *other* 32-byte object this pair ever signed would have
// verified here as a ticket and been decoded into an account, a world and an expiry.
// Nothing signs a second object today, which is exactly what made the gap cheap to close
// now: the fix is [ticketBodyDomain], folded into what the signature covers rather than
// into the body, so `ClientHello.session_ticket` did not move.
//
// **Verified in the direction that fails.** A round trip through the real path proves
// only that the two halves agree with each other, which they did before the domain
// existed too. What has to be shown is that the old shape is refused, and that the
// refusal an operator reads distinguishes "this is not a ticket" from "this is not ours".
func TestASignatureThatDoesNotSayItCoversATicketIsRefused(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	world := midgard(t)
	now := time.Now()

	minted, _, err := pair.Mint(anAccount(), world, now)
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}
	body := minted[:BodySize]

	// The body is byte-for-byte a real ticket's and the key is the real one. The only
	// difference is what the signature covers: the bare body, which is what this service
	// signed before the domain existed.
	var old Ticket
	copy(old[:BodySize], body)
	copy(old[BodySize:], ed25519.Sign(pair.signing.key, body))

	// **Refused by both verifiers**, and by name. VerifyAnyWorld drops the world
	// comparison and nothing else, so a shape that got past it would be a shape the
	// account service's own endpoint accepted.
	for name, got := range map[string]error{
		"Verify": func() error {
			_, err := Verify(pair.Public(), old[:], world, now)
			return err
		}(),
		"VerifyAnyWorld": func() error {
			_, err := VerifyAnyWorld(pair.Public(), old[:], now)
			return err
		}(),
	} {
		if got == nil {
			t.Errorf("%s admitted a ticket signed over the bare body", name)
		}
		if !errors.Is(got, ErrNotATicket) {
			t.Errorf("%s answered %v for a ticket signed over the bare body, want ErrNotATicket", name, got)
		}
	}

	// **The two answers must not be one sentinel.** An operator reading a log has to be
	// able to tell a ticket minted before the domain from a key that does not match, and
	// wrapping one in the other would make errors.Is answer yes to both.
	if errors.Is(ErrNotATicket, ErrBadSignature) || errors.Is(ErrBadSignature, ErrNotATicket) {
		t.Error("ErrNotATicket and ErrBadSignature satisfy each other; an operator cannot tell them apart")
	}

	// **A sibling domain is the object this issue is actually about**: something else this
	// pair signs one day, domain-separated as a ticket now is. It is refused, and with
	// ErrBadSignature rather than ErrNotATicket — which is the honest answer, because
	// nothing here knows that domain and "not this one" is all that can be said about it.
	sibling := sha256.Sum256(append([]byte("voxelheim/not-a-ticket/v1\x00"), body...))
	var other Ticket
	copy(other[:BodySize], body)
	copy(other[BodySize:], ed25519.Sign(pair.signing.key, sibling[:]))
	if _, err := Verify(pair.Public(), other[:], world, now); !errors.Is(err, ErrBadSignature) {
		t.Errorf("a body signed under a sibling domain answered %v, want ErrBadSignature", err)
	}

	// Somebody else's key over the right domain is the other of the two answers, and it
	// stays ErrBadSignature: this is not ours, which is a different sentence from ours
	// and not a ticket.
	stranger := newPair(t)
	var foreign Ticket
	copy(foreign[:BodySize], body)
	copy(foreign[BodySize:], ed25519.Sign(stranger.signing.key, signedMessage(body)))
	if _, err := Verify(pair.Public(), foreign[:], world, now); !errors.Is(err, ErrBadSignature) {
		t.Errorf("a ticket signed by another key answered %v, want ErrBadSignature", err)
	}

	// And the real path is untouched: the ticket verifies, and it is still exactly the
	// bytes the handshake carries. The domain is in the digest and none of it is on the
	// wire — which is the reason this shape was taken over a tag inside the body.
	if _, err := Verify(pair.Public(), minted[:], world, now); err != nil {
		t.Errorf("a freshly minted ticket was refused: %v", err)
	}
	if BodySize != 32 || Size != 96 {
		t.Errorf("the signing domain cost wire bytes: a body is %d and a ticket is %d, and the contract says 32 and 96",
			BodySize, Size)
	}
}

// The signing domain is versioned and separate, in the shape [worldIDDomain] established
// one layer down.
//
// **Written out literally rather than derived from the constant it is checking**, which
// is TestATicketIsTheBytesTheHandshakeCarries's rule and matters more here: the value is
// not an implementation detail anybody may tidy. Changing a byte of it invalidates every
// ticket in existence, and with no revocation in this design that is a decision somebody
// makes on purpose rather than a refactor.
func TestTheSigningDomainIsVersionedAndSeparate(t *testing.T) {
	t.Parallel()

	if ticketBodyDomain != "voxelheim/ticket-body/v1\x00" {
		t.Errorf("the signing domain is %q; changing it stops every ticket in flight from verifying, "+
			"so it is a decision rather than a refactor", ticketBodyDomain)
	}
	if ticketBodyDomain == worldIDDomain {
		t.Error("the signing domain and the world-id domain are the same string, which is the collision both exist to prevent")
	}
	if !strings.HasSuffix(ticketBodyDomain, "\x00") {
		t.Errorf("the signing domain %q does not end in the NUL separator worldIDDomain uses", ticketBodyDomain)
	}
	if !strings.Contains(ticketBodyDomain, "/v1") {
		t.Errorf("the signing domain %q carries no version, so a later change to what a signature covers "+
			"would be a silent reinterpretation of this one", ticketBodyDomain)
	}

	// The signed message is a digest, and its width is why the domain is free: none of
	// these bytes are transmitted.
	body := make([]byte, BodySize)
	if got := len(signedMessage(body)); got != sha256.Size {
		t.Errorf("the signed message is %d bytes, want a %d-byte digest", got, sha256.Size)
	}
	// A function of the body, and only of the body.
	if !bytes.Equal(signedMessage(body), signedMessage(make([]byte, BodySize))) {
		t.Error("the same body hashed to two different messages")
	}
	edited := make([]byte, BodySize)
	edited[0] ^= 0x01
	if bytes.Equal(signedMessage(body), signedMessage(edited)) {
		t.Error("two different bodies hashed to the same message")
	}
	// And it is not the body: the whole point is that the bytes signed are not the bytes
	// somebody can substitute another meaning for.
	if bytes.Equal(signedMessage(body), body) {
		t.Error("the signed message is the bare body, so the domain covers nothing")
	}
	// The concatenation is the domain in front, which is what makes a sibling domain a
	// different message for the same body rather than a longer one.
	if want := sha256.Sum256(append([]byte(ticketBodyDomain), body...)); !bytes.Equal(signedMessage(body), want[:]) {
		t.Error("the signed message is not SHA-256 of the domain followed by the body")
	}
}

// A ticket the expiry has passed is refused, and the boundary is exclusive: a ticket is
// not good *at* the instant it expires. With no revocation behind it, "valid until"
// should not round in the holder's favour.
func TestAnExpiredTicketIsRefused(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	world := midgard(t)
	minted, claims, err := pair.Mint(anAccount(), world, time.Now())
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}

	if _, err := Verify(pair.Public(), minted[:], world, claims.ExpiresAt.Add(-time.Nanosecond)); err != nil {
		t.Errorf("a ticket one nanosecond before its expiry was refused: %v", err)
	}
	for name, at := range map[string]time.Time{
		"exactly at its expiry": claims.ExpiresAt,
		"a second after":        claims.ExpiresAt.Add(time.Second),
		"a year after":          claims.ExpiresAt.AddDate(1, 0, 0),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			if _, err := Verify(pair.Public(), minted[:], world, at); !errors.Is(err, ErrExpired) {
				t.Errorf("a ticket verified %s answered %v, want ErrExpired", name, err)
			}
		})
	}
}

// **A ticket for one world is refused by another**, which is what stops the operator of
// one game server from collecting the tickets its players present and offering them
// somewhere else as those players.
func TestATicketForOneWorldIsRefusedByAnother(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	now := time.Now()

	minted, _, err := pair.Mint(anAccount(), midgard(t), now)
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}

	_, err = Verify(pair.Public(), minted[:], hel(t), now)
	if !errors.Is(err, ErrWrongWorld) {
		t.Fatalf("a ticket for another world answered %v, want ErrWrongWorld", err)
	}
	// The refusal names both worlds, because an operator staring at it needs to see
	// which two disagreed. Neither is a secret: a world id is a digest of a name an
	// operator publishes.
	if !strings.Contains(err.Error(), midgard(t).String()) || !strings.Contains(err.Error(), hel(t).String()) {
		t.Errorf("the refusal %q does not name both worlds", err)
	}
}

// **A ticket signed by a different key is refused**, which is the whole of what the
// signature buys: a second account service, or anybody who invented a key, cannot mint
// a ticket this world will admit.
func TestATicketSignedByAnotherKeyIsRefused(t *testing.T) {
	t.Parallel()

	mine, theirs := newPair(t), newPair(t)
	world := midgard(t)
	now := time.Now()

	if bytes.Equal(mine.Public(), theirs.Public()) {
		t.Fatal("two generated pairs share a public key, which no two random keys should")
	}

	minted, _, err := theirs.Mint(anAccount(), world, now)
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}
	if _, err := Verify(mine.Public(), minted[:], world, now); !errors.Is(err, ErrBadSignature) {
		t.Errorf("a ticket signed by another key answered %v, want ErrBadSignature", err)
	}
	// And the same ticket is good under the key that signed it, so the refusal above is
	// about the key rather than about the ticket.
	if _, err := Verify(theirs.Public(), minted[:], world, now); err != nil {
		t.Errorf("the ticket was refused by its own signer: %v", err)
	}
}

// A ticket of the wrong length is refused rather than indexed into, and a public key of
// the wrong length is refused rather than handed to crypto/ed25519 — which panics on
// one, and a panic is not an answer a server can give a connection.
func TestVerifyRefusesLengthsItCannotRead(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	world := midgard(t)
	now := time.Now()
	minted, _, err := pair.Mint(anAccount(), world, now)
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}

	for name, raw := range map[string][]byte{
		"nothing at all":     nil,
		"an empty ticket":    {},
		"one byte short":     minted[:Size-1],
		"one byte too many":  append(append([]byte{}, minted[:]...), 0),
		"just the body":      minted[:BodySize],
		"just the signature": minted[BodySize:],
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			if _, err := Verify(pair.Public(), raw, world, now); !errors.Is(err, ErrTicketSize) {
				t.Errorf("a %s answered %v, want ErrTicketSize", name, err)
			}
		})
	}

	for name, pub := range map[string]ed25519.PublicKey{
		"no public key":         nil,
		"a short public key":    pair.Public()[:ed25519.PublicKeySize-1],
		"an oversized key":      append(pair.Public(), 0),
		"a private-key-sized k": make([]byte, ed25519.PrivateKeySize),
	} {
		t.Run(name+" is refused rather than panicking", func(t *testing.T) {
			t.Parallel()

			if _, err := Verify(pub, minted[:], world, now); !errors.Is(err, ErrPublicKeySize) {
				t.Errorf("%s answered %v, want ErrPublicKeySize", name, err)
			}
		})
	}

	// A verifier that does not know which world it is is a misconfiguration and says
	// so, rather than silently refusing every ticket it is ever shown — and it says so
	// with its own sentinel. See TestAMisconfiguredVerifierIsNotACrossWorldTicket.
	if _, err := Verify(pair.Public(), minted[:], WorldID{}, now); !errors.Is(err, ErrVerifierWorld) {
		t.Errorf("a verifier with no world answered %v, want ErrVerifierWorld", err)
	}
}

// **A game server that was never told which world it is must not be reported as somebody
// else's ticket, and until #126 it was.**
//
// Both refusals were [ErrWrongWorld], so the two states that a log has to tell apart
// produced the same sentinel and, near enough, the same sentence. The misconfiguration is
// the one that hides: every player is refused, every line says the ticket names another
// world, and nothing anywhere says that this verifier names none — so the operator reads
// a run of refusals as an attack, or as a client bug, and never as the empty flag it is.
//
// The distinction is asserted in **both** directions, because a sentinel that answered
// [ErrVerifierWorld] for a genuine cross-world ticket would be exactly as useless, and a
// test that only checked the new one would not notice.
func TestAMisconfiguredVerifierIsNotACrossWorldTicket(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	now := time.Now()

	minted, _, err := pair.Mint(anAccount(), midgard(t), now)
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}

	// A configured verifier, shown a ticket for somewhere else. The signature is good and
	// the ticket is simply not for here — an ordinary answer, and nothing about the
	// verifier is wrong.
	_, crossWorld := Verify(pair.Public(), minted[:], hel(t), now)
	if !errors.Is(crossWorld, ErrWrongWorld) {
		t.Errorf("a ticket for another world answered %v, want ErrWrongWorld", crossWorld)
	}
	if errors.Is(crossWorld, ErrVerifierWorld) {
		t.Errorf("a ticket for another world answered %v, which also reads as a misconfigured verifier", crossWorld)
	}

	// A verifier that was never configured, shown the very ticket it should have
	// admitted. The ticket is fine; this server is not.
	_, unconfigured := Verify(pair.Public(), minted[:], WorldID{}, now)
	if !errors.Is(unconfigured, ErrVerifierWorld) {
		t.Errorf("a verifier with no world answered %v, want ErrVerifierWorld", unconfigured)
	}
	if errors.Is(unconfigured, ErrWrongWorld) {
		t.Errorf("a verifier with no world answered %v, which is the answer a cross-world ticket gets", unconfigured)
	}

	// And the refusal says what is wrong with *this* server rather than describing the
	// ticket, which is the whole of what an operator needs from it.
	if !strings.Contains(unconfigured.Error(), "verifier") {
		t.Errorf("the refusal %q does not say that the verifier is the thing that is unconfigured", unconfigured)
	}
}

// The two ids a mint must never sign, and the expiry the format cannot hold. Each is the
// caller's own mistake, refused at the write rather than only at the read: signing
// something this build would reject is the single failure that looks like a success
// until somebody presents it.
func TestMintRefusesWhatItCannotSign(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	world := midgard(t)

	if _, _, err := pair.Mint(AccountID{}, world, time.Now()); !errors.Is(err, ErrUnmintable) {
		t.Errorf("a ticket naming no account answered %v, want ErrUnmintable", err)
	}
	// **This refusal did not move when the account ticket arrived, and it is the reason a
	// forgotten field cannot become one.** The format reads a world-less body back happily
	// now — see TestAnAccountTicketNamesNoWorldAndIsRefusedByEveryGameServer — so this is
	// no longer a rule about what a ticket may be. It is a rule about how one is asked
	// for, and Pair.MintAccountTicket is the way to ask on purpose.
	if _, _, err := pair.Mint(anAccount(), WorldID{}, time.Now()); !errors.Is(err, ErrUnmintable) {
		t.Errorf("a ticket naming no world answered %v, want ErrUnmintable", err)
	}

	// **2106, arriving early.** Four bytes of Unix seconds stop working then, and the
	// refusal is what the failure looks like: an error an operator can read, rather
	// than a ticket that wrapped round to 1970 and expired before it was issued.
	past2106 := time.Unix(maxExpiresAtUnix, 0).UTC().Add(time.Hour)
	if _, _, err := pair.Mint(anAccount(), world, past2106); !errors.Is(err, ErrUnmintable) {
		t.Errorf("a ticket expiring past 2106 answered %v, want ErrUnmintable", err)
	}
	// And the same clock the other way: a machine that believes it is 1970 mints
	// nothing rather than minting something that already expired.
	if _, _, err := pair.Mint(anAccount(), world, time.Unix(0, 0).Add(-Lifetime)); !errors.Is(err, ErrUnmintable) {
		t.Errorf("a ticket expiring before the epoch answered %v, want ErrUnmintable", err)
	}
}

// **A clock this service cannot trust is refused at the mint, and until #126 it was not.**
//
// The guard above it — [encodeBody]'s — is applied to the *expiry*, which is `now` plus
// [Lifetime]. So it fires only once `now` is further back than a whole lifetime, and the
// eight hours between 1969-12-31T16:00:00Z and the epoch were a window in which a host
// that had never set its clock minted happily. What the player got was a 200, a ticket
// that had expired before it was issued, and no way to try again: the sign-in's state is
// spent by the redemption that happens before the mint, so the same request answers
// `sign_in_not_found` from then on.
//
// The window is what makes this a real configuration rather than a contrived one. A host
// with no RTC and no NTP yet reads exactly the epoch, and a container that starts before
// the clock is stepped reads a few seconds either side of it.
//
// **The bound is on `now` rather than on the expiry**, because it is a question about the
// machine and not about the format: an expiry that fits in four bytes is what the record
// can hold, and a clock at zero is a service that does not know what time it is. The two
// guards are both kept for the same reason [encodeBody] refuses what [decodeBody] refuses.
func TestMintRefusesAClockThatHasNotBeenSet(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	world := midgard(t)

	// Every one of these is inside the window the expiry guard leaves open: add
	// Lifetime to any of them and the result is still a positive Unix second, which is
	// why each was minted rather than refused.
	for name, now := range map[string]time.Time{
		"the epoch itself":                                    time.Unix(0, 0),
		"one second before the epoch":                         time.Unix(-1, 0),
		"a second short of a whole lifetime before the epoch": time.Unix(1, 0).Add(-Lifetime),
		"the zero time":                                       {},
		"a whole lifetime before the epoch":                   time.Unix(0, 0).Add(-Lifetime),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			if _, _, err := pair.Mint(anAccount(), world, now); !errors.Is(err, ErrUnmintable) {
				t.Errorf("Mint at %s answered %v, want ErrUnmintable", now.UTC().Format(time.RFC3339), err)
			}
			// The account ticket goes through the same shared mint, so it has to
			// refuse the same clock — a second entry point that did not would be the
			// hole this one closed.
			if _, _, err := pair.MintAccountTicket(anAccount(), now); !errors.Is(err, ErrUnmintable) {
				t.Errorf("MintAccountTicket at %s answered %v, want ErrUnmintable", now.UTC().Format(time.RFC3339), err)
			}
		})
	}

	// One second past the epoch is the first instant this service will sign for, and it
	// signs a ticket that verifies — the refusal is a bound on a clock that is unset, not
	// a general distrust of old ones.
	first := time.Unix(1, 0)
	minted, claims, err := pair.Mint(anAccount(), world, first)
	if err != nil {
		t.Fatalf("Mint one second after the epoch answered %v, want a ticket", err)
	}
	if !claims.ExpiresAt.After(first) {
		t.Errorf("the ticket expires at %s, which is not after the %s it was minted at",
			claims.ExpiresAt.Format(time.RFC3339), first.UTC().Format(time.RFC3339))
	}
	if _, err := Verify(pair.Public(), minted[:], world, first); err != nil {
		t.Errorf("the ticket minted one second after the epoch was refused at that instant: %v", err)
	}
}

// A body this service would not sign is refused on the way back in as well, and the only
// way to reach that branch is from inside this package: [encodeBody] will not produce one.
//
// Written as an internal test for exactly that reason. It is the halves-of-a-format
// rule internal/auth keeps — a record this build would refuse to write is one this build
// must refuse to read — and without it a bug in encodeBody would be answered by a
// verifier handing a caller a ticket naming nobody.
//
// **The zero account id is the whole of the list, and it used to have the zero world id
// beside it.** That case moved rather than being dropped: a world-less body is a legal
// account ticket now, so refusing it here would put the two halves of the format back into
// disagreement in the other direction — the mint writes one and the reader would refuse it.
// What guards the mistake the old case was guarding is [Pair.Mint], which still will not
// sign a zero world, and TestMintRefusesWhatItCannotSign is where that is asserted.
func TestASignedBodyThisServiceWouldNotWriteIsStillRefused(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	world := midgard(t)
	now := time.Now()

	for name, body := range map[string][]byte{
		"a body naming no account": bodyWith(t, AccountID{}, world, now.Add(Lifetime)),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			forged := signedTicket(t, pair, body)

			// The signature is genuine, so this is not ErrBadSignature: the refusal has
			// to come from reading the body.
			if _, err := Verify(pair.Public(), forged[:], world, now); !errors.Is(err, ErrMalformedBody) {
				t.Errorf("a signed but unwritable body answered %v, want ErrMalformedBody", err)
			}
		})
	}
}

// **The game server is the party that must not have to trust the account service beyond
// its signature, and until #126 nothing on the verify side bounded a ticket's remaining
// life** (defect 10).
//
// A body carrying 0xFFFFFFFF in its expiry word is a legal record — [expiresAtSize] holds
// exactly that value, it is 2106, and [encodeBody] writes it without complaint — so a
// ticket signed with the real key verified with seventy-six years left on it. [Pair.Mint]
// cannot produce one today, which is what makes this defence in depth rather than a live
// hole: it costs one comparison, and what it buys is that a mint which ever *could*
// produce one is caught by the half of the system that admits players rather than by the
// half that signs.
//
// **The bound carries an explicit allowance for clock skew, and that is not slack for its
// own sake.** A ticket's expiry is computed from the account service's clock and this
// comparison is made against the game server's. Without an allowance, a game server whose
// clock is two seconds behind sees every freshly minted ticket as having more than
// [Lifetime] left and refuses all of them — turning a bound meant to catch a forgery into
// the most effective denial of service in the design. The allowance is one-sided: it
// widens what a verifier accepts as *fresh*, and it does not extend any ticket's life,
// because [ErrExpired] is checked against `now` with no allowance at all.
func TestVerifyRefusesATicketWithMoreLifeLeftThanThisServiceEverMints(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	world := midgard(t)
	now := time.Now()

	sign := func(body []byte) Ticket { return signedTicket(t, pair, body) }

	// The issue's own reproduction: the largest expiry the format can hold, signed with
	// the real key. Everything about it is genuine except how long it lasts.
	in2106 := sign(bodyWith(t, anAccount(), world, time.Unix(maxExpiresAtUnix, 0)))
	if _, err := Verify(pair.Public(), in2106[:], world, now); !errors.Is(err, ErrMalformedBody) {
		t.Errorf("a validly signed ticket expiring in 2106 answered %v, want ErrMalformedBody", err)
	}
	// The account service's own verifier drops the world comparison and nothing else, so
	// it has to make this check too — it is the endpoint a ticket is presented to.
	if _, err := VerifyAnyWorld(pair.Public(), in2106[:], now); !errors.Is(err, ErrMalformedBody) {
		t.Errorf("VerifyAnyWorld answered %v for a ticket expiring in 2106, want ErrMalformedBody", err)
	}

	// The edge, from both sides, so the bound is pinned rather than merely present.
	atTheBound := sign(bodyWith(t, anAccount(), world, now.Add(Lifetime+verifierClockSkew)))
	if _, err := Verify(pair.Public(), atTheBound[:], world, now); err != nil {
		t.Errorf("a ticket expiring exactly at the bound was refused: %v", err)
	}
	pastTheBound := sign(bodyWith(t, anAccount(), world, now.Add(Lifetime+verifierClockSkew+2*time.Second)))
	if _, err := Verify(pair.Public(), pastTheBound[:], world, now); !errors.Is(err, ErrMalformedBody) {
		t.Errorf("a ticket expiring past the bound answered %v, want ErrMalformedBody", err)
	}

	// **And a real ticket is unaffected by any of it, including on a verifier whose clock
	// is behind.** This is the assertion that would have caught a bound written without
	// an allowance, which is the version of this fix that breaks every join.
	minted, _, err := pair.Mint(anAccount(), world, now)
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}
	for name, at := range map[string]time.Time{
		"a verifier whose clock agrees":             now,
		"a verifier a second behind":                now.Add(-time.Second),
		"a verifier the whole allowance behind":     now.Add(-verifierClockSkew),
		"a verifier an hour into the ticket's life": now.Add(time.Hour),
	} {
		if _, err := Verify(pair.Public(), minted[:], world, at); err != nil {
			t.Errorf("%s refused a freshly minted ticket: %v", name, err)
		}
	}
}

// signedTicket is a whole ticket over body, signed the way a mint signs it: over
// [signedMessage] of the body rather than over the bare body.
//
// **Every test that lays a body out by hand goes through here**, so that "what a real
// signature covers" is written down once in the tests as it is written down once in the
// package. A test that signed the bare body would be constructing the one thing
// TestASignatureThatDoesNotSayItCoversATicketIsRefused exists to reject, and would report
// it as a failure of whatever it was actually testing.
func signedTicket(t *testing.T, pair *Pair, body []byte) Ticket {
	t.Helper()

	var whole Ticket
	copy(whole[:BodySize], body)
	copy(whole[BodySize:], ed25519.Sign(pair.signing.key, signedMessage(body)))
	return whole
}

// bodyWith lays out a body directly, bypassing encodeBody's refusals — which is what
// makes the test above able to reach a state Mint cannot produce.
func bodyWith(t *testing.T, account AccountID, world WorldID, expiresAt time.Time) []byte {
	t.Helper()

	body := make([]byte, BodySize)
	copy(body[offAccount:], account[:])
	copy(body[offWorld:], world[:])
	// Written through the same encoder the real path uses, so the layout under test is
	// the layout in use.
	whole, err := encodeBody(Claims{Account: anAccount(), World: midgard(t), ExpiresAt: expiresAt})
	if err != nil {
		t.Fatalf("encodeBody: %v", err)
	}
	copy(body[offExpires:], whole[offExpires:])
	return body
}

// A world id is a function of the name and nothing else — two configurations agreeing on
// a string agree on the id, which is the whole reason it is derived rather than chosen.
func TestAWorldIDIsTheNameAndNothingElse(t *testing.T) {
	t.Parallel()

	first, second := worldID(t, "midgard"), worldID(t, "midgard")
	if first != second {
		t.Error("the same world name produced two ids")
	}
	if first == worldID(t, "midgard-2") {
		t.Error("two world names produced one id")
	}
	// **This is the property the account ticket rests on**, not merely a curiosity about
	// the digest. The zero id means "this ticket names no world"; a name that hashed to it
	// would be a real world whose tickets every verifier read as world-less, and
	// ticket.Verify would then admit an account ticket at that one game server.
	if first.IsZero() {
		t.Error("a world name hashed to the zero id, which is the value an account ticket claims")
	}
	if len(first.String()) != WorldIDSize*2 {
		t.Errorf("a world id renders as %d characters, want %d hex characters", len(first.String()), WorldIDSize*2)
	}
}

// **The name is constrained rather than normalised**, which is internal/auth's rule for
// a provider name. Lowercasing or trimming would quietly accept two spellings as one
// world, and the day something compares them before they reach here is the day one
// spelling becomes a world the game server has never heard of.
func TestAWorldNameIsRefusedRatherThanNormalised(t *testing.T) {
	t.Parallel()

	for name, world := range map[string]string{
		"an empty name":       "",
		"a capital letter":    "Midgard",
		"a leading space":     " midgard",
		"a trailing space":    "midgard ",
		"a space inside":      "mid gard",
		"an underscore":       "mid_gard",
		"a slash":             "mid/gard",
		"a dot dot":           "..",
		"a NUL":               "mid\x00gard",
		"something not ASCII": "midgård",
		"far too long":        strings.Repeat("a", MaxWorldNameBytes+1),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			if _, err := WorldIDFor(world); !errors.Is(err, ErrWorldName) {
				t.Errorf("%s was accepted as a world name (err %v)", name, err)
			}
		})
	}

	for _, world := range []string{"a", "midgard", "world-2", "9", strings.Repeat("a", MaxWorldNameBytes)} {
		if _, err := WorldIDFor(world); err != nil {
			t.Errorf("the world name %q was refused: %v", world, err)
		}
	}
}

// The refusal states the rule and never quotes the name back. A world name arrives in a
// request body, and an error string ends up in a log — this is the same reason
// internal/auth's identity validation quotes nothing.
func TestAWorldNameRefusalDoesNotQuoteTheName(t *testing.T) {
	t.Parallel()

	const hostile = "MiDgArD\x00<script>"
	_, err := WorldIDFor(hostile)
	if err == nil {
		t.Fatal("a hostile world name was accepted")
	}
	if strings.Contains(err.Error(), hostile) || strings.Contains(err.Error(), "<script>") {
		t.Errorf("the refusal %q quotes the name it was given", err)
	}
}

// A ticket is a bearer credential, and the schema's rule is never logged, never
// displayed, on either side. Four formatters, because each reaches the value by a route
// the others do not — and the JSON handler is the one a Stringer would not have saved.
func TestATicketRedactsItselfWhateverFormatterReachesIt(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	minted, _, err := pair.Mint(anAccount(), midgard(t), time.Now())
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}

	var text, jsonOut bytes.Buffer
	slog.New(slog.NewTextHandler(&text, nil)).Info("a ticket", "ticket", minted)
	slog.New(slog.NewJSONHandler(&jsonOut, nil)).Info("a ticket", "ticket", minted)
	marshalled, err := json.Marshal(struct {
		Ticket Ticket `json:"ticket"`
	}{minted})
	if err != nil {
		t.Fatalf("json.Marshal: %v", err)
	}

	rendered := map[string]string{
		"%v":              fmt.Sprintf("%v", minted),
		"%s":              fmt.Sprintf("a ticket: %s", minted),
		"%#v":             fmt.Sprintf("%#v", minted),
		"an error":        fmt.Errorf("a ticket: %v", minted).Error(),
		"the text log":    text.String(),
		"the JSON log":    jsonOut.String(),
		"encoding/json":   string(marshalled),
		"a struct holder": fmt.Sprintf("%v", struct{ T Ticket }{minted}),
	}
	for where, got := range rendered {
		if !strings.Contains(got, redactedTicket) {
			t.Errorf("%s rendered a ticket as %q, want the redaction", where, got)
		}
		for encoding, leaked := range renderings(minted[:]) {
			if strings.Contains(got, leaked) {
				t.Errorf("%s carries the ticket's bytes as %s", where, encoding)
			}
		}
	}

	// And the one deliberate way out still works, because a client has to be handed the
	// thing.
	back, err := Decode(minted.Encode())
	if err != nil {
		t.Fatalf("Decode(Encode()): %v", err)
	}
	if back != minted {
		t.Error("a ticket did not survive Encode and Decode")
	}
}

// Decode refuses what is not a ticket, and never quotes what it was given.
func TestDecodeRefusesWhatIsNotATicket(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	minted, _, err := pair.Mint(anAccount(), midgard(t), time.Now())
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}

	for name, encoded := range map[string]string{
		"an empty string":    "",
		"not base64 at all":  "not a ticket!!",
		"padded base64":      base64.StdEncoding.EncodeToString(minted[:]),
		"standard base64":    base64.StdEncoding.WithPadding(base64.NoPadding).EncodeToString(bytes.Repeat([]byte{0xFF}, Size)),
		"a ticket cut short": minted.Encode()[:len(minted.Encode())-4],
		"hex instead":        hex.EncodeToString(minted[:]),
		"a 32-byte token":    base64.RawURLEncoding.EncodeToString(make([]byte, 32)),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			if _, err := Decode(encoded); err == nil {
				t.Errorf("%s was decoded as a ticket", name)
			} else if encoded != "" && strings.Contains(err.Error(), encoded) {
				t.Errorf("the refusal %q quotes what it was given", err)
			}
		})
	}
}

// EncodedSize is what Encode actually produces, checked rather than trusted.
//
// The constant is arithmetic a const expression has to spell out, because it cannot call
// base64's own EncodedLen. This is where the two are held together — and where the number
// in Encode's doc comment is held to the type it describes, so that a change to Size
// cannot leave either of them saying 128.
func TestEncodedSizeIsExactlyWhatEncodeProduces(t *testing.T) {
	t.Parallel()

	if want := base64.RawURLEncoding.EncodedLen(Size); EncodedSize != want {
		t.Errorf("EncodedSize is %d, and unpadded base64url of %d bytes is %d", EncodedSize, Size, want)
	}

	pair := newPair(t)
	minted, _, err := pair.Mint(anAccount(), midgard(t), time.Now())
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}
	if got := len(minted.Encode()); got != EncodedSize {
		t.Errorf("Encode produced %d characters, want EncodedSize (%d)", got, EncodedSize)
	}
}

// Decode refuses an oversized credential **on its length, before it decodes any of it**.
//
// A ticket is presented in an `Authorization` header by somebody nobody has authenticated
// yet, and a header is as long as whoever sent it chose — a megabyte, by net/http's
// default. Decoding first would sell an unauthenticated request a base64 pass and three
// quarters of a megabyte of allocation for the price of a header.
//
// **The check is structural rather than a match on the message.** Reaching ErrTicketSize
// means TicketFrom was handed decoded bytes, which means the decode ran; a refusal on
// length cannot produce it. So this test fails against a Decode that checks the length
// afterwards, which is exactly the version it was written against.
func TestDecodeRefusesAnOversizedCredentialWithoutDecodingIt(t *testing.T) {
	t.Parallel()

	// Valid unpadded base64url throughout, so nothing but the length can refuse it.
	oversized := strings.Repeat("A", 1<<20)

	_, err := Decode(oversized)
	if err == nil {
		t.Fatal("a megabyte of base64 was decoded as a ticket")
	}
	if errors.Is(err, ErrTicketSize) {
		t.Error("Decode decoded the whole credential before refusing it; the length is checked first")
	}
	// The refusal still quotes nothing of what it was given — it is somebody's bearer
	// credential whether or not it turned out to be a ticket.
	if strings.Contains(err.Error(), oversized[:64]) {
		t.Errorf("the refusal quotes what it was given: %v", err)
	}
}

// **Hours, not days.** The number is the entire cost of a theft, because there is no
// revocation: a stolen ticket dies only by expiring. The bounds are what the issue
// states, checked so that a later edit has to disagree with them out loud.
func TestTheLifetimeIsHoursAndNotDays(t *testing.T) {
	t.Parallel()

	if Lifetime < time.Hour {
		t.Errorf("Lifetime is %s, which is not long enough to be measured in hours", Lifetime)
	}
	if Lifetime >= 24*time.Hour {
		t.Errorf("Lifetime is %s, which is a day or more; there is no revocation, so this is how long a stolen ticket works", Lifetime)
	}
}

// renderings is one secret in every encoding a leak could take.
//
// The decimal form is the one that catches a value reaching a formatter as bytes rather
// than as text — which is exactly what a [96]byte or an ed25519 key does when nothing
// redacts it — and it is spelled out here rather than taken from fmt so that it is the
// digits and not the brackets around them.
//
// **The Go-syntax form is the one this map was missing, and its absence is the reason a
// leak sat inside a green test for three releases.** `%#v` prints a byte slice as
// `0x9c, 0x1f, …`, and none of the four forms below contains a substring of that — so a
// signing key printed with the one verb a Stringer never sees was being searched for in
// four encodings it does not take (#126). It is spelled out here for the reason the
// decimal form is: what a leak looks like is the digits and the separator, not the type
// name and the braces fmt puts around them, and a key reached through an unexported field
// is printed by the same walker whatever the enclosing type happens to be called.
func renderings(secret []byte) map[string]string {
	decimal := make([]string, len(secret))
	goSyntax := make([]string, len(secret))
	for i, b := range secret {
		decimal[i] = strconv.Itoa(int(b))
		// The exact spelling fmt gives one element of a byte slice under %#v:
		// lowercase, 0x-prefixed and NOT zero-padded, so 0 renders as `0x0`.
		goSyntax[i] = fmt.Sprintf("%#x", b)
	}
	return map[string]string{
		"raw bytes": string(secret),
		"hex":       hex.EncodeToString(secret),
		"base64":    base64.StdEncoding.EncodeToString(secret),
		"base64url": base64.RawURLEncoding.EncodeToString(secret),
		"decimal":   strings.Join(decimal, " "),
		"Go syntax": strings.Join(goSyntax, ", "),
	}
}

// **The property that makes the account ticket safe to issue, and the one to break this
// test on if anybody ever moves it.**
//
// An account ticket names no world. It says only "the account service knows who this is",
// and it exists because the trust chain closed in a circle without it: a player needs a
// ticket to read the server list, needs to name a world to be minted one, and the list is
// what tells them the worlds exist.
//
// What keeps that from widening anything is [Verify], which is the function a *game server*
// calls. It is handed that server's own world and compares — so an account ticket fails
// there exactly as a ticket for somebody else's world does, and it fails for every world a
// game server could possibly be configured with, including the one no game server may have.
func TestAnAccountTicketNamesNoWorldAndIsRefusedByEveryGameServer(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	now := time.Now()

	minted, claims, err := pair.MintAccountTicket(anAccount(), now)
	if err != nil {
		t.Fatalf("MintAccountTicket: %v", err)
	}
	if !claims.World.IsZero() {
		t.Errorf("an account ticket names world %s, want the zero id", claims.World)
	}
	if claims.Account != anAccount() {
		t.Error("an account ticket does not name the account it was minted for")
	}

	// The account service's own check reads it, because that is what it is for.
	read, err := VerifyAnyWorld(pair.Public(), minted[:], now)
	if err != nil {
		t.Fatalf("the account service could not verify its own account ticket: %v", err)
	}
	if read.World != claims.World || read.Account != claims.Account {
		t.Error("VerifyAnyWorld answered claims that are not the ones that were signed")
	}

	// And no game server does. Both a real world and the misconfiguration a verifier can be
	// in: the second is the one that would be a hole, because a game server that did not
	// know its own world would otherwise be asking exactly the question an account ticket
	// answers yes to.
	//
	// The two answers are different sentinels and both are refusals, which is the point:
	// a game server that names a world turns the ticket away because it names another,
	// and one that names none turns it away because it is misconfigured. See
	// TestAMisconfiguredVerifierIsNotACrossWorldTicket for why those must not be the same
	// error. What matters here is that neither of them admits it.
	for name, world := range map[string]WorldID{
		"the world it was not issued for": midgard(t),
		"another world again":             hel(t),
	} {
		if _, err := Verify(pair.Public(), minted[:], world, now); !errors.Is(err, ErrWrongWorld) {
			t.Errorf("%s answered %v, want ErrWrongWorld", name, err)
		}
	}
	if _, err := Verify(pair.Public(), minted[:], WorldID{}, now); !errors.Is(err, ErrVerifierWorld) {
		t.Errorf("a verifier with no world at all answered %v, want ErrVerifierWorld", err)
	}
}

// A world-scoped ticket is unchanged by the account ticket's arrival: the world it names is
// the world it verifies at, and nowhere else.
//
// The half of the pair above that would still pass if [Verify] had been loosened to accept
// anything, which is why it is here rather than left to the older tests.
func TestAWorldTicketIsUnaffectedByTheAccountTicket(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	now := time.Now()

	minted, _, err := pair.Mint(anAccount(), midgard(t), now)
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}
	if _, err := Verify(pair.Public(), minted[:], midgard(t), now); err != nil {
		t.Errorf("the world it was issued for refused it: %v", err)
	}
	if _, err := Verify(pair.Public(), minted[:], hel(t), now); !errors.Is(err, ErrWrongWorld) {
		t.Errorf("another world answered %v, want ErrWrongWorld", err)
	}
	// And the account service can read it too — somebody already holding a world ticket
	// should not have to sign in again to read the list.
	if _, err := VerifyAnyWorld(pair.Public(), minted[:], now); err != nil {
		t.Errorf("VerifyAnyWorld refused a world-scoped ticket: %v", err)
	}
}

// **VerifyAnyWorld drops exactly one check and keeps every other one.** It is the account
// service's own verifier, so the temptation it has to be tested against is the one where
// "any world" quietly became "any ticket".
func TestVerifyAnyWorldStillRefusesEverythingButTheWorld(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	now := time.Now()

	minted, claims, err := pair.MintAccountTicket(anAccount(), now)
	if err != nil {
		t.Fatalf("MintAccountTicket: %v", err)
	}

	// A key of the wrong length is a question about this service's own configuration, and
	// crypto/ed25519 panics on one — a panic is not an answer a server can give a request.
	if _, err := VerifyAnyWorld(nil, minted[:], now); !errors.Is(err, ErrPublicKeySize) {
		t.Errorf("no public key answered %v, want ErrPublicKeySize", err)
	}
	if _, err := VerifyAnyWorld(pair.Public(), minted[:BodySize], now); !errors.Is(err, ErrTicketSize) {
		t.Errorf("a short ticket answered %v, want ErrTicketSize", err)
	}

	// Signed by somebody else's key, which is what an invented ticket looks like.
	if _, err := VerifyAnyWorld(newPair(t).Public(), minted[:], now); !errors.Is(err, ErrBadSignature) {
		t.Errorf("another service's key answered %v, want ErrBadSignature", err)
	}

	// A genuine ticket with one bit of its body changed. The signature is over the body, so
	// this is the edit an attacker holding a real ticket would make.
	tampered := minted
	tampered[0] ^= 1
	if _, err := VerifyAnyWorld(pair.Public(), tampered[:], now); !errors.Is(err, ErrBadSignature) {
		t.Errorf("a tampered body answered %v, want ErrBadSignature", err)
	}

	// **Expiry is checked, and it is the only thing that ever ends a ticket**: there is no
	// revocation in this design. Exclusive at the instant, which is what Verify does too.
	if _, err := VerifyAnyWorld(pair.Public(), minted[:], claims.ExpiresAt); !errors.Is(err, ErrExpired) {
		t.Errorf("a ticket at its expiry answered %v, want ErrExpired", err)
	}
	if _, err := VerifyAnyWorld(pair.Public(), minted[:], claims.ExpiresAt.Add(time.Hour)); !errors.Is(err, ErrExpired) {
		t.Errorf("an expired ticket answered %v, want ErrExpired", err)
	}
	if _, err := VerifyAnyWorld(pair.Public(), minted[:], claims.ExpiresAt.Add(-time.Second)); err != nil {
		t.Errorf("a ticket a second before its expiry was refused: %v", err)
	}
}

// A body naming no account is still refused by both verifiers, which is the one refusal
// [decodeBody] kept. Reachable only from inside this package, exactly as before.
func TestAnAccountlessBodyIsRefusedByBothVerifiers(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	now := time.Now()
	body := bodyWith(t, AccountID{}, WorldID{}, now.Add(Lifetime))
	forged := signedTicket(t, pair, body)

	if _, err := VerifyAnyWorld(pair.Public(), forged[:], now); !errors.Is(err, ErrMalformedBody) {
		t.Errorf("VerifyAnyWorld answered %v, want ErrMalformedBody", err)
	}
	if _, err := Verify(pair.Public(), forged[:], midgard(t), now); !errors.Is(err, ErrMalformedBody) {
		t.Errorf("Verify answered %v, want ErrMalformedBody", err)
	}
}
