// Package ticket is the credential a game server can check without asking anybody.
//
// # The decision this package implements
//
// A game server that called the account service on every join would make a small
// service a hard dependency of play, and its failure mode is the one nobody wants:
// nobody can play a game running on hardware that is perfectly fine. So the game
// server **verifies a signature instead of asking permission**. It reads this
// service's public key once, keeps it, and from then on admitting a player is
// arithmetic — see [Verify], which touches no disk and no socket and takes its idea
// of "now" as a parameter.
//
// # The cost, stated rather than mitigated
//
// There is no revocation. A ticket cannot be withdrawn before it expires, so a
// stolen one dies only by expiring, and [Lifetime] is the whole of the answer to
// that. A grace period for an unreachable verifier would be strictly worse than
// having none: it is a rule an attacker triggers by blocking the service.
//
// # What a ticket is
//
// [Size] bytes — a [BodySize]-byte body and a detached Ed25519 signature over it —
// which are exactly the bytes `ClientHello.session_ticket` carries. schemas/handshake.fbs
// is authoritative for that number and for the split, and `protocol.SessionTicketLen`
// is the game server's copy of it; ticket_test.go pins this package's constants to
// that one so the three cannot drift apart in silence. The body names the account, the
// world, and the moment the ticket stops being good for anything, and there is nothing
// else in it: a ticket is not a place to put state, because every byte of it is
// bounded by a number the wire format has already fixed.
//
// **There is no version field in the body, and that is an argument rather than an
// omission.** A ticket is only ever presented in a `ClientHello`, which carries
// `protocol_version` beside it — so the ticket's version is the protocol's, and the
// contract already says that changing the length, the signature scheme or the split is
// a protocol version bump. A second version number here would be a second thing to keep
// in step with the first.
//
// # Two kinds of ticket, told apart by one value
//
// A ticket whose world id is zero names **no world**: an *account ticket*, which says
// only "the account service knows who this is". Every other ticket is world-scoped and
// says "this account may play on that one world".
//
// The account ticket exists because the trust chain closed in a circle without it. A
// player needs a ticket to read the server list, needs to name a world to be minted one,
// and the list is what tells them which worlds exist. So `POST /v1/signin/discord/finish`
// mints an account ticket when the request names no world, and the server-list endpoint
// takes either kind — it only needs to know which account is asking.
//
// **The zero id is safe to spend on this because [WorldIDFor] cannot produce it**, so
// the two kinds can never be confused for one another. And the boundary that matters is
// unmoved: [Verify] is what a game server calls, it is handed that server's own world,
// and an account ticket names a different one — so it is refused there exactly as a
// ticket for somebody else's world is. Nothing a game server does admits an account
// ticket; [VerifyAnyWorld] is the account service's own check and says so.
//
// Minting one is deliberate rather than incidental: [Pair.Mint] still refuses a zero
// world, and [Pair.MintAccountTicket] is the only way to ask for one. A forgotten field
// is a refusal, never a credential of a shape nobody meant to issue.
//
// # This package is a leaf, deliberately
//
// It imports internal/world for the five record helpers that package exports for the
// purpose ([world.WriteAtomic], [world.CheckHeader], [world.CheckChecksum],
// [world.PutChecksum], [world.SweepTemporaries]) and nothing else of ours. That is not
// tidiness: **the game server is going to import this package** in order to verify, and
// anything this package can reach, the game server can reach. An import of internal/auth
// from here would put the accounts directory back inside the simulation's trust domain,
// which is the boundary internal/auth/imports_test.go exists to hold. imports_test.go
// here holds the other end of it.
//
// This is also why [AccountID] is this package's own 16 bytes rather than auth's:
// cmd/voxelheim-auth converts one to the other, and that conversion stops compiling if
// either side ever changes width.
package ticket

import (
	"crypto/ed25519"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"log/slog"
	"time"
)

// Lifetime is how long a minted ticket is good for.
//
// **Eight hours, and the number is the entire cost of a theft.** There is no
// revocation in this design and none is coming: a ticket that has been copied off a
// machine works until it expires, whatever anybody notices in the meantime. So the
// number is chosen from both ends. Long enough that one sign-in covers one sitting —
// nobody wants a browser round trip in the middle of an evening — and short enough
// that a ticket taken today is worthless tomorrow.
//
// Hours rather than days is the shape the design requires; eight rather than six or
// twelve is a judgement inside that shape, and it is one constant so that changing it
// is one edit and one decision.
const Lifetime = 8 * time.Hour

// Algorithm names the signature scheme, for the endpoint that publishes the public key.
//
// Published rather than assumed, so that a game server reading the key is told what to
// do with the bytes instead of inferring it from their length — and so that a future
// scheme is a value that changes rather than a silent reinterpretation of the same
// field.
const Algorithm = "ed25519"

// The body's layout, little-endian throughout, which is what every other record in this
// repository is. Nothing outside this module parses a ticket — the client carries it
// verbatim and the game server hands it to [Verify] — so there is no foreign decoder to
// owe network byte order to, and matching the stores costs nothing and surprises nobody.
//
//	account_id[16] world_id[12] expires_at:u32
//
// Every field is fixed-width and the whole is [BodySize] bytes, so there is no length to
// declare, nothing to truncate and no equation for a decoder to check. That is the
// dividend of the wire format having fixed the size first: a variable-length ticket
// would need the length-versus-actual-size reasoning internal/auth's record does, and a
// fixed one cannot be short.
const (
	// AccountIDSize is how many bytes of a body name the account. Sixteen, the width
	// `auth.AccountID` already is.
	AccountIDSize = 16

	// WorldIDSize is how many bytes of a body name the world.
	//
	// Twelve, and it is what is left after the account and the expiry — which is the
	// honest way round to say it, because the total was fixed by the contract before
	// this layout existed. Ninety-six bits is what that leaves, and it is worth
	// checking that it is enough for what this field defends against, since a world id
	// is a truncated digest.
	//
	// The threat is not a player: any account can ask for a ticket for any world, so
	// naming the world is not an authorisation boundary against whoever holds the
	// ticket. It is a **confusion boundary against the operator of the world the
	// ticket was issued for** — a compromised or dishonest game server collecting the
	// tickets its players present and replaying them somewhere else as those players.
	// To do that it needs a world whose id collides with the target's, and it may
	// choose its own world's name freely, so the work is a second preimage on this
	// many bits. Ninety-six is out of reach; sixty-four, which is what an eight-byte
	// field would have left, is not comfortably so.
	WorldIDSize = 12

	// expiresAtSize is how many bytes carry the expiry: four, as Unix seconds.
	//
	// **Four bytes stop working on 2106-02-07, which is written down here rather than
	// discovered there.** A ticket lives hours, so nothing about it needs a range
	// beyond the moment it is minted — and spending eight bytes on a field that will
	// never hold a large number would have cost four bytes of world id, which is the
	// field where width buys something. [Mint] refuses an expiry it cannot represent
	// rather than wrapping it, so the failure when that date arrives is a refusal an
	// operator can read and not a ticket that expired in 1970.
	expiresAtSize = 4

	// BodySize is the signed part of a ticket: 32 bytes, as schemas/handshake.fbs
	// states it.
	BodySize = AccountIDSize + WorldIDSize + expiresAtSize

	// Size is a whole ticket: the body and the detached signature over it, 96 bytes,
	// which is exactly what `ClientHello.session_ticket` carries.
	Size = BodySize + ed25519.SignatureSize
)

const (
	offAccount = 0
	offWorld   = offAccount + AccountIDSize
	offExpires = offWorld + WorldIDSize
)

// maxExpiresAtUnix is the largest instant [expiresAtSize] bytes of Unix seconds can
// hold: 2106-02-07T06:28:15Z.
const maxExpiresAtUnix = 1<<(8*expiresAtSize) - 1

// MaxWorldNameBytes is the longest world name [WorldIDFor] will hash.
//
// A bound rather than a limit anybody will reach: the name is an identifier an operator
// types into two configurations, and a cap is what keeps this function from being handed
// a megabyte by a request body.
const MaxWorldNameBytes = 64

// worldIDDomain separates this digest from every other use of SHA-256 in this
// repository, so that a world id can never coincide with a value computed for another
// purpose over the same text. The NUL is the same separator internal/auth's account key
// uses, and for the same reason: a world name cannot contain one, so the prefix can
// never be confused with the name.
const worldIDDomain = "voxelheim/world-id/v1\x00"

// redactedTicket is what a [Ticket] renders as, whichever formatter reaches it.
const redactedTicket = "ticket.Ticket(redacted)"

// The refusals this package makes, as sentinels because every caller branches on them.
//
// A game server answering a handshake has to tell a ticket that is the wrong shape from
// one that is for another world from one that has simply run out, because those are
// three different things to tell a player — and matching on a string to do it is how
// that distinction gets lost in the first refactor.
var (
	// ErrWorldName reports a world name [WorldIDFor] will not hash.
	ErrWorldName = errors.New("ticket: that is not a world name this service can issue a ticket for")

	// ErrUnmintable reports a ticket this service will not sign. The caller's own
	// mistake rather than anything about a request.
	ErrUnmintable = errors.New("ticket: that is not a ticket this service will sign")

	// ErrTicketSize reports a presented ticket whose length is not [Size]. The
	// handshake refuses a wrong-length ticket before any of this is reached, so a
	// caller seeing this has skipped that check.
	ErrTicketSize = fmt.Errorf("ticket: a session ticket is exactly %d bytes", Size)

	// ErrPublicKeySize reports a verifying key that is not an Ed25519 public key.
	// Checked because crypto/ed25519 panics on one, and a panic is not an answer a
	// server can give a connection.
	ErrPublicKeySize = fmt.Errorf("ticket: an Ed25519 public key is exactly %d bytes", ed25519.PublicKeySize)

	// ErrBadSignature reports a ticket nobody holding this key signed: tampered with,
	// signed by a different service, or invented.
	ErrBadSignature = errors.New("ticket: the ticket is not signed by that key")

	// ErrWrongWorld reports a ticket issued for another world. The signature is good;
	// the ticket is simply not for here.
	ErrWrongWorld = errors.New("ticket: the ticket names another world")

	// ErrExpired reports a ticket that has run out. The only way a ticket ever stops
	// working, which is the design's stated cost.
	ErrExpired = errors.New("ticket: the ticket has expired")

	// ErrMalformedBody reports a signed body this build would not have produced.
	//
	// Unreachable through [Mint], which refuses to sign such a body in the first
	// place — so reaching it means this service's own key signed something this
	// service would not write, and the refusal exists so that stays impossible rather
	// than becoming a ticket naming nobody.
	ErrMalformedBody = errors.New("ticket: the ticket's body is not one this service would sign")
)

// AccountID is the account a ticket names: the same sixteen bytes `auth.AccountID` is,
// carried here so that this package never has to import the accounts.
//
// A fixed-size array rather than a slice, deliberately: it is copied by assignment, two
// of them cannot alias, and no caller can hand around a 15-byte one.
type AccountID [AccountIDSize]byte

// String is the id in lowercase hex: 32 characters.
func (id AccountID) String() string { return hex.EncodeToString(id[:]) }

// IsZero reports the one id no mint produces and no ticket may carry.
func (id AccountID) IsZero() bool { return id == AccountID{} }

// WorldID names one world in a ticket, and is what makes a ticket for one world useless
// at another.
//
// Derived from a name rather than chosen, so that two configurations agreeing on a
// string agree on the id without anybody copying a blob between them. See [WorldIDFor].
//
// **The zero id is the one exception, and it reads "this ticket names no world".** See
// [Pair.MintAccountTicket] for what that is for, and [WorldID.IsZero] for why the value
// is safe to spend on it.
type WorldID [WorldIDSize]byte

// String is the id in lowercase hex: 24 characters. Not a secret — it is a digest of a
// name an operator publishes — so it is safe in a log and in an error.
func (w WorldID) String() string { return hex.EncodeToString(w[:]) }

// IsZero reports the world id that names no world: what a caller gets from
// `var w WorldID`, what [Pair.Mint] refuses, and what [Pair.MintAccountTicket] produces
// deliberately.
//
// **[WorldIDFor] never produces it, and that is the property the account ticket rests
// on.** A name that hashed to the zero id would be a world whose tickets every verifier
// read as world-less; the id is 96 bits of SHA-256, so it does not happen, and
// TestAWorldIDIsTheNameAndNothingElse pins that rather than leaving it an assumption.
// The consequence is that the two states cannot be confused: a world-scoped ticket
// always names a real world, and an account ticket always names none.
func (w WorldID) IsZero() bool { return w == WorldID{} }

// WorldIDFor is the world id a world's name resolves to.
//
// **The name is constrained rather than normalised, which is internal/auth's rule for a
// provider name and it is here for the same reason.** Lowercasing or trimming whatever
// arrived would quietly accept "Midgard", "midgard" and " midgard" as one world — right
// until something compares two of them before they reach here, at which point one
// spelling is a world the game server has never heard of and the failure is a player
// being turned away with nothing to look at. A refusal is loud and happens once, in a
// test.
//
// This is the identifier an operator gives both this service and the game server. It is
// not the title a player reads on a server list, which is display text and belongs
// nowhere near a signature.
func WorldIDFor(name string) (WorldID, error) {
	switch {
	case name == "":
		return WorldID{}, fmt.Errorf("%w: it names no world", ErrWorldName)
	case len(name) > MaxWorldNameBytes:
		return WorldID{}, fmt.Errorf("%w: the name is %d bytes, more than the %d a world name may be",
			ErrWorldName, len(name), MaxWorldNameBytes)
	case !validWorldName(name):
		return WorldID{}, fmt.Errorf("%w: a world name is lowercase letters, digits and hyphens", ErrWorldName)
	}
	sum := sha256.Sum256(append([]byte(worldIDDomain), name...))
	return WorldID(sum[:WorldIDSize]), nil
}

// validWorldName reports whether name is drawn from the vocabulary above.
//
// Ranging over the string yields runes, so a non-ASCII one fails the switch rather than
// being examined a byte at a time — which makes this a check on the characters instead
// of on the encoding. Copied in shape from internal/auth's provider-name check, and not
// shared with it, because sharing would mean this package importing the accounts.
func validWorldName(name string) bool {
	for _, r := range name {
		switch {
		case r >= 'a' && r <= 'z', r >= '0' && r <= '9', r == '-':
		default:
			return false
		}
	}
	return true
}

// Claims is what a ticket says: who, where, and until when.
//
// **There are exactly two ways to hold one — mint it or verify it** — and that is the
// point of not offering a method that reads a ticket's body without checking the
// signature. Such a method would be the obvious thing for a handshake to call, and it
// would be reading attacker-supplied bytes as though somebody had vouched for them.
type Claims struct {
	// Account is who the ticket is for.
	Account AccountID

	// World is the world it is for, and no other — or the zero id, which is an
	// **account ticket**: a ticket that names no world at all.
	//
	// The two are told apart with [WorldID.IsZero] and nothing else, because
	// [WorldIDFor] cannot produce the zero id. [Verify] refuses an account ticket for
	// any world a game server could be configured with, which is what makes the second
	// state safe to add: it is a credential for talking to the account service, never
	// one for joining a game.
	World WorldID

	// ExpiresAt is the moment it stops being good for anything, to the second and in
	// UTC. Exclusive: a ticket is refused at exactly this instant, because "valid
	// until" with no revocation behind it should not round in the holder's favour.
	ExpiresAt time.Time
}

// Ticket is one signed ticket: the [Size] bytes `ClientHello.session_ticket` carries.
//
// A fixed-size array rather than a slice, for the reason `identity.Token` is one: it is
// copied by assignment, two of them cannot alias, and no caller can hand around a
// 95-byte one.
//
// **A bearer credential, and treated as one here.** Whatever holds these bytes can make
// the claim they carry — a signature proves who issued a ticket, not who is presenting
// it — so the schema's rule is never logged, never displayed, on either side, and the
// four methods below are what make that structural instead of a habit. [Ticket.Encode]
// is the one deliberate way out.
type Ticket [Size]byte

// Encode is the ticket as a client receives it: unpadded base64url, 128 characters.
//
// A named method so that every place a ticket leaves the type is one grep away — the
// same move `discord.Secret.Reveal` makes, and for the same reason: slicing does this
// too and cannot be prevented, so what this buys is that the deliberate uses are
// findable and the accidental ones are the only ones that look like a slice.
//
// base64url without padding rather than hex: it is what internal/discord already encodes
// a 32-byte secret as, it is safe in a URL and a JSON string without further escaping,
// and it is 128 characters where hex would be 192. Nobody reads a ticket by eye, so the
// readability hex buys elsewhere in this repository buys nothing here.
func (t Ticket) Encode() string { return base64.RawURLEncoding.EncodeToString(t[:]) }

// Decode reads a ticket back from [Ticket.Encode], refusing anything that is not one.
func Decode(encoded string) (Ticket, error) {
	raw, err := base64.RawURLEncoding.DecodeString(encoded)
	if err != nil {
		// The input is not echoed. It is a bearer credential, and an error message
		// reaches a log — encoding/base64 quotes the offending byte and nothing more,
		// which is why the wrap drops even that.
		return Ticket{}, errors.New("ticket: the ticket is not unpadded base64url")
	}
	return TicketFrom(raw)
}

// TicketFrom copies b into a ticket, refusing any length but [Size].
//
// The copy matters: b is usually a decoded frame, and a ticket that aliased the buffer a
// client chose would change underneath whoever held it.
func TicketFrom(b []byte) (Ticket, error) {
	if len(b) != Size {
		return Ticket{}, fmt.Errorf("%w, got %d", ErrTicketSize, len(b))
	}
	return Ticket(b), nil
}

// String redacts the ticket. It is a Stringer so that %v, %s and every message built
// with fmt.Errorf print the redaction rather than the bytes.
func (t Ticket) String() string { return redactedTicket }

// GoString redacts a ticket printed with %#v, which String never sees. The formatter a
// test failure and a debugging print reach for is exactly the one most likely to be
// pasted somewhere afterwards.
func (t Ticket) GoString() string { return redactedTicket }

// LogValue redacts a ticket that reaches a log line, and it is not the same defence as
// String: slog resolves a LogValuer before either handler formats anything, and without
// it -log-format json would hand a [96]byte to encoding/json and write the ticket out as
// an array of 96 numbers. This is the trap `identity.Token` documents, arriving here at
// three times the width.
func (t Ticket) LogValue() slog.Value { return slog.StringValue(redactedTicket) }

// MarshalJSON redacts a ticket that reaches encoding/json — a struct that happens to
// hold one being marshalled into a response or a diagnostic. The one place a ticket is
// deliberately serialised converts it through [Ticket.Encode] into a plain string field,
// so redacting the default costs that path nothing.
func (t Ticket) MarshalJSON() ([]byte, error) { return []byte(`"` + redactedTicket + `"`), nil }

// encodeBody lays a ticket's claims out, refusing anything this service will not sign.
//
// The refusals are at the *write*, not only at the read, for the reason internal/auth
// refuses an account it could not read back: signing something this build would reject
// is the single failure that looks like a success until somebody presents it. Which is
// why the set of refusals here is exactly [decodeBody]'s — the two halves of a format
// that disagree about what a ticket is are worse than either half's rule alone.
//
// **A zero world is not one of them, and the asymmetry is deliberate.** A body naming no
// world is a legal account ticket, so refusing it here would be refusing something this
// build reads back happily. What must not happen is a zero world arriving by *accident*
// — a caller that forgot the field — and that is a rule about the minting API rather
// than about the format, so it lives in [Pair.Mint] where the caller is. There is no way
// to reach this function without going through one of the two mints.
func encodeBody(c Claims) ([]byte, error) {
	if c.Account.IsZero() {
		// The one id a mint cannot produce. Signed, it would make every ticket that
		// reached this line name the same nobody.
		return nil, fmt.Errorf("%w: it names no account", ErrUnmintable)
	}
	unix := c.ExpiresAt.Unix()
	if unix <= 0 || unix > maxExpiresAtUnix {
		// Refused rather than wrapped. See [expiresAtSize]: this is what 2106 looks
		// like when it arrives, and what a clock set to 1970 looks like today.
		return nil, fmt.Errorf("%w: it expires at %s, which is outside the %d..%d this format holds",
			ErrUnmintable, c.ExpiresAt.UTC().Format(time.RFC3339), 1, maxExpiresAtUnix)
	}

	body := make([]byte, BodySize)
	copy(body[offAccount:offAccount+AccountIDSize], c.Account[:])
	copy(body[offWorld:offWorld+WorldIDSize], c.World[:])
	binary.LittleEndian.PutUint32(body[offExpires:offExpires+expiresAtSize], uint32(unix))
	return body, nil
}

// decodeBody reads a body's claims back.
//
// Called only after the signature has been checked, which is what makes it safe to build
// a value from these bytes at all. It refuses exactly what [encodeBody] refuses — the
// zero account id, and nothing else — because the halves of a format that disagree about
// what a ticket is are worse than either half's rule alone.
//
// **A zero world is read rather than refused**, and it is the only field here whose
// meaning depends on the reader: it is an account ticket, which [Verify] turns away from
// every world and [VerifyAnyWorld] admits. Refusing it here would make the account
// ticket unreadable by the service that mints it.
func decodeBody(body []byte) (Claims, error) {
	c := Claims{
		Account:   AccountID(body[offAccount : offAccount+AccountIDSize]),
		World:     WorldID(body[offWorld : offWorld+WorldIDSize]),
		ExpiresAt: time.Unix(int64(binary.LittleEndian.Uint32(body[offExpires:offExpires+expiresAtSize])), 0).UTC(),
	}
	if c.Account.IsZero() {
		return Claims{}, fmt.Errorf("%w: it names no account", ErrMalformedBody)
	}
	return c, nil
}
