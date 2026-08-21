// Package identity names a player across connections.
//
// Two values, and the distance between them is the whole point.
//
// A [Token] is a bearer credential: 32 bytes from crypto/rand, minted by the
// server, kept by one client, and presented in ClientHello to resume the identity
// it names. Whatever holds it *is* that player, so it is never logged, never
// displayed, and never written to disk.
//
// A [PlayerID] is the SHA-256 of a token. It names the same identity and gives
// nothing away: it is what the player store keys records by, what a log line
// carries, and what a file under <world-dir>/players/ is called. A leaked players
// directory is therefore a list of hashes rather than a list of credentials, and
// the hash cannot be turned back into the token that opens it.
//
// The package is a leaf — it imports nothing from this module — which is what lets
// game, session and the player store all name an identity without importing each
// other.
package identity

import (
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"log/slog"
)

// TokenSize is how many bytes a token is, on the wire and in memory.
//
// The contract states the same number in schemas/handshake.fbs, where it is a
// decoder invariant: a `player_token` of any other non-zero length is refused
// before any identity is looked up.
const TokenSize = 32

// IDSize is how many bytes a player id is. It is SHA-256's output size, because a
// player id is a SHA-256 digest and nothing else.
const IDSize = sha256.Size

// shortIDBytes is how much of a player id a log line carries: enough to follow one
// session through a log, far too little to be an identifier anything keys on.
const shortIDBytes = 4

// redacted is what a token renders as, whichever formatter reaches it.
const redacted = "identity.Token(redacted)"

// ErrTokenSize reports a presented token whose length is not [TokenSize].
//
// A sentinel because the handshake branches on it: a wrong-length token is
// RejectReason.BAD_REQUEST, decided before any identity is looked up, and the
// caller should not have to match on a string to know that.
var ErrTokenSize = errors.New("identity: a token is exactly 32 bytes")

// Token is the secret a client presents to be recognised as the same player.
//
// A fixed-size array rather than a slice, deliberately: it is copied by assignment,
// two of them cannot alias, and no caller can hand around a 31-byte one.
type Token [TokenSize]byte

// PlayerID is the SHA-256 of a token: the name of an identity, safe to store, log
// and put in a file name.
type PlayerID [IDSize]byte

// NewToken mints a fresh identity's token.
//
// A failed read from crypto/rand is returned, never swallowed. The alternative is
// the one value this package must never produce — a zero token, which every
// session that failed to mint would share, and which would make them all the same
// player.
func NewToken() (Token, error) { return newToken(rand.Reader) }

// newToken is NewToken over an injectable source, so the refusal above can be
// tested. crypto/rand does not fail on any platform this server runs on, which is
// exactly why the branch needs a test to exist at all.
func newToken(r io.Reader) (Token, error) {
	var t Token
	if _, err := io.ReadFull(r, t[:]); err != nil {
		return Token{}, fmt.Errorf("identity: minting a token: %w", err)
	}
	return t, nil
}

// TokenFrom copies b into a token, refusing any length but [TokenSize].
//
// The copy matters: b is usually a decoded frame, and a token that aliased the
// buffer a client chose would change underneath whoever held it.
func TokenFrom(b []byte) (Token, error) {
	if len(b) != TokenSize {
		return Token{}, fmt.Errorf("%w, got %d", ErrTokenSize, len(b))
	}
	return Token(b), nil
}

// IDOf is the identity a token names.
//
// One-way by construction: the store can be handed this and never the token, so
// what it holds cannot be replayed as a credential.
func IDOf(t Token) PlayerID { return PlayerID(sha256.Sum256(t[:])) }

// Equal reports whether two tokens are the same, in constant time.
//
// Nothing on the resolution path needs it — an identity is found by hashing the
// presented token and looking that up, so tokens are never compared — and this
// exists so that a comparison someone does add is the right one from the start.
func (t Token) Equal(other Token) bool {
	return subtle.ConstantTimeCompare(t[:], other[:]) == 1
}

// String redacts the token. It is a Stringer so that %v, %s and fmt.Sprint print
// the redaction rather than the bytes.
func (t Token) String() string { return redacted }

// LogValue redacts a token that reaches a log line, and it is not the same defence
// as String.
//
// slog resolves a LogValuer before either handler formats anything; without this,
// -log-format json would hand a [32]byte to encoding/json and write the token out
// as an array of 32 numbers, which String never sees. The rule this enforces is
// stated in schemas/handshake.fbs: never logged, never displayed, on either side.
func (t Token) LogValue() slog.Value { return slog.StringValue(redacted) }

// String is the id in lowercase hex: 64 characters, and the name of the file the
// player store keeps this identity's record in.
func (id PlayerID) String() string { return hex.EncodeToString(id[:]) }

// Short is the first 8 hex characters, for a log line.
//
// A prefix rather than the whole digest because a log is read by a person: eight
// characters are enough to follow one session and to tell two apart, and nothing
// keys on it, so the collisions a prefix invites cost nothing.
func (id PlayerID) Short() string { return hex.EncodeToString(id[:shortIDBytes]) }
