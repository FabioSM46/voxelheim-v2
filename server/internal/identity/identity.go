// Package identity names a player across connections.
//
// Two values, and the distance between them is the whole point.
//
// An [Account] is who the account service says a player is: the sixteen bytes a
// verified session ticket names, arriving already decided. This server does not issue
// one, cannot choose one, and never has to check one — by the time an account reaches
// this package somebody holding the account service's key has vouched for it. It is
// never logged and never displayed, so an operator's log file is not a list of who
// plays here.
//
// A [PlayerID] is the SHA-256 of an account under a domain of this package's own. It
// names the same player and gives nothing away: it is what the player store keys
// records by, what a log line carries, and what a file under <world-dir>/players/ is
// called. A leaked players directory is therefore a list of digests rather than a list
// of accounts, and the digest cannot be turned back into the account it names.
//
// # What this package stopped being, and why the distance survived it
//
// A [PlayerID] used to be the SHA-256 of a **token this server minted**: 32 bytes from
// crypto/rand, handed to one client in a ServerWelcome and presented again in the next
// ClientHello. That model is gone. It made a player a property of a file on one
// machine — whoever held the token *was* that player, on that one server and nowhere
// else — and it is what a session ticket replaces: an account the client proves with a
// signature, recognised by a server that has never seen it before.
//
// So this package no longer mints anything, and there is no credential here at all: a
// ticket is the credential, and it belongs to internal/ticket, which is where it is
// verified. What survived the change is the *shape* — a value that names a player, and
// a one-way function to the value everything else keys on — because the reason for it
// never depended on where the first value came from. A log line and a file name should
// name a player without naming the person.
//
// The package is a leaf — it imports nothing from this module — which is what lets
// game, session and the player store all name a player without importing each other,
// and what keeps the account service's packages out of the simulation's reach. That is
// also why [Account] is this package's own sixteen bytes rather than ticket's:
// internal/session converts one to the other in one line, and that line stops
// compiling if either width ever moves.
package identity

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"log/slog"
)

// AccountIDSize is how many bytes name an account: sixteen, the width
// `ticket.AccountID` is and the width `auth.AccountID` is under it.
//
// Stated here rather than imported, because importing either would stop this package
// being a leaf. The conversion in internal/session is what pins the two together: it
// converts one array type into the other, so it stops compiling the day either is a
// different size.
const AccountIDSize = 16

// IDSize is how many bytes a player id is. It is SHA-256's output size, because a
// player id is a SHA-256 digest and nothing else.
const IDSize = sha256.Size

// shortIDBytes is how much of a player id a log line carries: enough to follow one
// session through a log, far too little to be an identifier anything keys on.
const shortIDBytes = 4

// playerIDDomain separates this digest from every other use of SHA-256 in this
// repository, so a player id can never coincide with a value computed for another
// purpose over the same sixteen bytes.
//
// The shape `ticket.worldIDDomain` and `ticket.ticketBodyDomain` use, and versioned the
// way they are: changing what a player id is made of is a new domain rather than a
// silent reinterpretation of the old one. **A sibling is a new constant, never a suffix
// on this one** — two domains that share a prefix are two domains, and the property
// being bought is that nothing else hashed anywhere in this repository can produce this
// digest.
//
// The NUL is kept for consistency and is honestly doing less work here than it does in
// front of a world name: an account is a fixed [AccountIDSize] bytes, so this
// concatenation is unambiguous with or without it.
const playerIDDomain = "voxelheim/player-id/v1\x00"

// redacted is what an account renders as, whichever formatter reaches it.
const redacted = "identity.Account(redacted)"

// Account is the account a verified session ticket names.
//
// A fixed-size array rather than a slice, deliberately: it is copied by assignment, two
// of them cannot alias, and no caller can hand around a 15-byte one.
//
// **Not a credential, and redacted anyway.** Holding these bytes proves nothing — the
// ticket carries the proof, and it is signed — so the reason this type redacts is not
// theft, it is that a log file naming accounts is a record of who plays here and of
// when they were online, which is nobody's business but theirs. That rule is easier to
// keep as a property of the type than as a habit at every call site, and the four
// methods below are what make it one. They take value receivers for the reason
// `ticket.Pair` had to learn: a method set on *T leaves a T value implementing neither
// fmt.Stringer nor slog.LogValuer, which a caller reaches by a dereference.
type Account [AccountIDSize]byte

// IsZero reports the account no ticket may name.
//
// `ticket.Verify` already refuses a body naming nobody, so nothing on the admission
// path has to ask; this exists so that a caller which wants to assert it can, without
// comparing against a composite literal.
func (a Account) IsZero() bool { return a == Account{} }

// String redacts the account. It is a Stringer so that %v, %s and fmt.Sprint print the
// redaction rather than the bytes.
func (a Account) String() string { return redacted }

// GoString redacts the account under %#v, which a Stringer never sees.
//
// The route `ticket.Pair` had to learn twice: fmt's reflection walker prints an array's
// elements as `0x9c, 0x1f, …` unless the value itself declares this method.
func (a Account) GoString() string { return redacted }

// LogValue redacts an account that reaches a log line, and it is not the same defence
// as String.
//
// slog resolves a LogValuer before either handler formats anything; without this,
// -log-format json would hand a [16]byte to encoding/json and write the account out as
// an array of 16 numbers, which String never sees.
func (a Account) LogValue() slog.Value { return slog.StringValue(redacted) }

// MarshalJSON redacts the account for anything that encodes it as JSON, which is the
// fourth route out and the one neither of the two above covers.
func (a Account) MarshalJSON() ([]byte, error) { return json.Marshal(redacted) }

// PlayerID is the SHA-256 of an account: the name of a player, safe to store, log and
// put in a file name.
type PlayerID [IDSize]byte

// IDOf is the player id an account names.
//
// One-way by construction: the store can be handed this and never the account, so a
// directory of player records is a directory of digests. Deterministic, so the same
// account is the same player on every connection and after every restart — which is
// all that "recognised by a server that has never seen you" needs from this side.
func IDOf(account Account) PlayerID {
	return PlayerID(sha256.Sum256(append([]byte(playerIDDomain), account[:]...)))
}

// String is the id in lowercase hex: 64 characters, and the name of the file the
// player store keeps this player's record in.
func (id PlayerID) String() string { return hex.EncodeToString(id[:]) }

// Short is the first 8 hex characters, for a log line.
//
// A prefix rather than the whole digest because a log is read by a person: eight
// characters are enough to follow one session and to tell two apart, and nothing keys
// on it, so the collisions a prefix invites cost nothing.
//
// **It is a prefix of a digest and not of an account**, which is the property that lets
// it be logged at all. Eight characters of the account itself would be eight characters
// of the one value this package exists not to write down.
func (id PlayerID) Short() string { return hex.EncodeToString(id[:shortIDBytes]) }
