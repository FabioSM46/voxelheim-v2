package ticket

import (
	"bytes"
	"crypto/ed25519"
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
	// so, rather than silently refusing every ticket it is ever shown.
	if _, err := Verify(pair.Public(), minted[:], WorldID{}, now); !errors.Is(err, ErrWrongWorld) {
		t.Errorf("a verifier with no world answered %v, want ErrWrongWorld", err)
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

// A body this service would not sign is refused on the way back in as well, and the only
// way to reach that branch is from inside this package: [Mint] will not produce one.
//
// Written as an internal test for exactly that reason. It is the halves-of-a-format
// rule internal/auth keeps — a record this build would refuse to write is one this build
// must refuse to read — and without it a bug in encodeBody would be answered by a
// verifier handing a caller a ticket naming nobody.
func TestASignedBodyThisServiceWouldNotWriteIsStillRefused(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	world := midgard(t)
	now := time.Now()

	for name, body := range map[string][]byte{
		"a body naming no account": bodyWith(t, AccountID{}, world, now.Add(Lifetime)),
		"a body naming no world":   bodyWith(t, anAccount(), WorldID{}, now.Add(Lifetime)),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			var forged Ticket
			copy(forged[:BodySize], body)
			copy(forged[BodySize:], ed25519.Sign(pair.signing.key, body))

			// The signature is genuine, so this is not ErrBadSignature: the refusal has
			// to come from reading the body.
			if _, err := Verify(pair.Public(), forged[:], world, now); !errors.Is(err, ErrMalformedBody) {
				t.Errorf("a signed but unwritable body answered %v, want ErrMalformedBody", err)
			}
		})
	}
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
	if first.IsZero() {
		t.Error("a world name hashed to the zero id, which Mint refuses")
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
func renderings(secret []byte) map[string]string {
	decimal := make([]string, len(secret))
	for i, b := range secret {
		decimal[i] = strconv.Itoa(int(b))
	}
	return map[string]string{
		"raw bytes": string(secret),
		"hex":       hex.EncodeToString(secret),
		"base64":    base64.StdEncoding.EncodeToString(secret),
		"base64url": base64.RawURLEncoding.EncodeToString(secret),
		"decimal":   strings.Join(decimal, " "),
	}
}
