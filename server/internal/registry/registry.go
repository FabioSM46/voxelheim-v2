// Package registry is the list that ends the client's trust chain.
//
// # What this list is for
//
// A player who has to be sent an address and a certificate fingerprint by hand is a
// player who is deciding, on their own, that whoever answered first is the server they
// meant. That is trust on first use, and it is the last manual step in joining a game
// here. This package removes it: the client knows the account service by construction,
// the account service knows the game servers because an operator registered them, and so
// a client can verify a game server it has never met.
//
// The number that makes that work is the **certificate fingerprint** — the SHA-256 of
// the certificate the game server actually presents, which is exactly what
// `certs.Fingerprint` produces and what `voxelheimd` prints at startup as
// `certificate_sha256=…`. This package never computes it. It is text arriving in a
// registration, checked for being a well-formed digest and served back verbatim, because
// a second way of computing the number is a second number to disagree with the first.
//
// # Two credentials, and neither is the other
//
// **Registration is authenticated with an operator-configured key** ([Key]). Anybody
// able to register could otherwise put their own address in the list under a name players
// trust, which is the whole attack this list would otherwise create — and it would be a
// better attack than the one it replaces, because the client would believe the answer.
//
// **The list is read with a session ticket**, so it is not a public directory of
// somebody's home address. A game server registered here is usually a machine in a house,
// and its address is the sort of thing that does not go in a public listing. That check
// lives in the account service's handler rather than here, because it is a question about
// a request; what lives here is that this package never offers an unauthenticated way to
// read anything.
//
// # A changing home address is the point
//
// [Store.Register] replaces the record a name already had, so the address the list serves
// is the one the server last announced. That is the criterion the whole design is for: a
// home connection that gets a new address overnight becomes invisible to players, because
// nobody is holding a copy of the old one.
//
// A server that stops announcing is **shown as offline, never dropped** — see
// [OfflineAfter]. Dropping it would make a server that is briefly unreachable
// indistinguishable from one that was never registered, and the second is what a player
// reads an empty list as.
//
// # What is deliberately not here
//
// **No rate limit and no lockout on a wrong registration key.** The bound on guessing is
// [MinKeyBytes] and nothing else: a counter would be per-process state that a restart
// clears and a second instance never sees, and a lockout on a wrong key is a way for
// anybody to stop an operator's servers registering by guessing wrong on purpose. A key
// that cannot be guessed is the answer; one that could be is not fixed by counting.
//
// **No way to withdraw a server.** Removing a registration is deleting its file, and the
// whole of the ceremony is that an operator does it. An endpoint for it would be the first
// piece of moderation in a service that has none, and the thing it would need to decide —
// who may remove whose server — has no answer yet.
//
// **Nothing is dialled.** A registration is checked for being well-formed, never for being
// reachable, and [Server.Online] reports only whether the server has said anything lately.
// Probing somebody's home connection from this service would make it a scanner, and it
// would answer a question the player's own client answers better by connecting.
//
// # Who imports this
//
// cmd/voxelheim-auth, and nothing else. The game server talks to this over HTTP and
// imports nothing of it; imports_test.go holds both halves of that, in the shape
// internal/auth's does and for its reasons.
//
// # The discipline, reused rather than re-derived
//
// Magic number, format version, trailing CRC-32, temporary-file-and-rename writes,
// temporaries swept on open, unknown versions refused whole — internal/world's, through
// the five helpers it exports for the purpose. internal/auth was the third store to take
// them and this is the fourth. The version number is this package's own, because a
// registered server and an account change for entirely unrelated reasons.
package registry

import (
	"crypto/sha256"
	"crypto/subtle"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
)

// OfflineAfter is how long a server may go unheard-from before the list reports it as
// offline.
//
// **Offline, not absent.** The record stays exactly where it is and the address it last
// announced is still served; what changes is one boolean a player reads as "you probably
// cannot join this right now". Deleting the record instead would make a server that is
// briefly unreachable look like one nobody ever registered, and an empty list is what a
// player concludes the whole game is broken from.
//
// Five minutes is chosen against the interval an announcer repeats on rather than on its
// own: it has to be a comfortable multiple of that interval, so that one missed announce
// — a dropped packet, an account service restart, a home connection blinking — does not
// flap a healthy server to offline and back. An announcer repeating every minute has four
// tries. **Whoever writes the announcing side reads this constant rather than picking a
// second number**; that is why it is exported from the package that decides what it means.
//
// The cost of the other direction is stated too: a server that has genuinely died looks
// alive for up to five minutes, and a player clicking it gets a connection failure. That
// is the cheaper of the two mistakes, and it is the one that fixes itself.
const OfflineAfter = 5 * time.Minute

// The bounds on what a record can be asked to hold. They are what make [maxRecordSize] a
// number, which is what lets an oversized file be refused before a byte of it is read.
const (
	// MaxNameBytes is the longest server name a record keeps.
	//
	// It is `ticket.MaxWorldNameBytes` and it must stay so — see [Server.Name]: the name
	// is the world name, and a registration this store accepted but no ticket could be
	// minted for would be a server in the list that nobody can join.
	MaxNameBytes = ticket.MaxWorldNameBytes

	// MaxDisplayNameBytes is the longest display name a record keeps.
	MaxDisplayNameBytes = 64

	// MaxAddressBytes is the longest address a record keeps. Generous against the 253
	// bytes a fully-qualified domain name can be, plus a port.
	MaxAddressBytes = 260

	// FingerprintHexLen is how long the fingerprint is: a SHA-256 digest in lowercase
	// hex, which is 64 characters and never anything else.
	FingerprintHexLen = sha256.Size * 2

	// MinKeyBytes is the shortest registration key this service will start with.
	//
	// The key is the only thing standing between a stranger and an entry in this list
	// under a name players trust, and it is checked by a single HTTP request that anybody
	// can repeat. Thirty-two characters is what a `head -c 24 /dev/urandom | base64`
	// produces, and refusing anything shorter is the one thing this package can do about
	// an operator who would otherwise use a word.
	//
	// It bounds guessing rather than preventing it. There is deliberately no rate limit
	// and no lockout here — see the package comment on what is not built.
	MinKeyBytes = 32

	// MaxKeyBytes is the longest registration key this service will start with, and the
	// longest credential [Key.Matches] will hash.
	//
	// **It is one bound doing two jobs, and it has to be, which is the whole reason it is
	// a constant rather than a check at the handler.** A key is presented in an
	// `Authorization` header by somebody nobody has authenticated yet — the credential has
	// to be read to be refused — and a header is as long as whoever sent it chose, up to
	// net/http's MaxHeaderBytes, a megabyte by default. Without a bound, an unauthenticated
	// request makes this process SHA-256 a megabyte to learn what a length comparison knows
	// immediately. That is the argument cmd/voxelheim-auth's maxRegistrationRequestBytes
	// makes about the request body, one field earlier.
	//
	// Bounding only the presentation would be worse than not bounding it: an operator whose
	// key was longer than this would watch every registration fail with nothing in any log
	// to explain it, which is the failure [ParseKey]'s whitespace rule exists to avoid. So
	// [ParseKey] refuses to build a key this long in the first place, and a credential
	// [Key.Matches] turns away on length is one no configuration could have made correct.
	//
	// Two hundred and fifty-six is far past what anybody needs — a 192-byte random value is
	// 256 characters of base64, and [MinKeyBytes] is what an operator is actually held to.
	// The point of the number is that there is one, not that it is close to anything.
	MaxKeyBytes = 256
)

// The refusals this package makes, as sentinels because the callers branch on them.
//
// **[ErrInvalidServer] is wrapped alongside one of the four field sentinels, never on its
// own**, so a caller can ask either question: "is this a registration this store would
// refuse" and "which field is wrong". The endpoint in front of this store answers the
// second, because the operator on the other end of a refused announcement has one
// configuration to fix and needs to be told which line of it — and a closed set of refusal
// codes is how this service says that without echoing a request back.
var (
	// ErrInvalidServer reports a registration this store will not write down. The
	// operator's own mistake rather than an attack: registration is authenticated, so
	// whoever reached this line already proved they hold the key.
	ErrInvalidServer = errors.New("registry: that is not a server registration this service can record")

	// ErrServerName reports a name this store will not key on — which is exactly a name no
	// ticket could be minted for. See [Server.Name].
	ErrServerName = errors.New("registry: that is not a server name this service can key on")

	// ErrDisplayName reports display text a record cannot hold.
	ErrDisplayName = errors.New("registry: that is not a display name a record can hold")

	// ErrAddress reports something that is not an address a player could be sent to.
	ErrAddress = errors.New("registry: that is not an address a player can be sent to")

	// ErrFingerprint reports something that is not a well-formed certificate digest.
	ErrFingerprint = errors.New("registry: that is not a SHA-256 certificate fingerprint")

	// ErrInvalidKey reports a registration key this service will not start with. It is
	// read once, at startup, from a configuration only the operator can change.
	ErrInvalidKey = errors.New("registry: that is not a registration key this service will accept")
)

// redactedKey is what a [Key] renders as, whichever formatter reaches it.
const redactedKey = "registry.Key(redacted)"

// Key is the operator-configured credential a game server registers with.
//
// **It holds the SHA-256 of the key and never the key**, which is what makes "the
// registration key is never logged" a property of the type instead of a rule every call
// site has to remember: after [ParseKey] returns there is no key in this process for
// anything to print. The digest is not credential-equivalent — presenting it does not
// authenticate, because [Key.Matches] hashes whatever it is shown — so the worst a leaked
// one costs is an offline guessing target, which [MinKeyBytes] is the answer to.
//
// It is redacted through all four routes anyway, in the shape `ticket.SigningKey` is: a
// value that is nearly harmless in a log is still a value nobody gains anything by
// logging, and the type stays safe if the field is ever changed.
//
// A struct with an unexported field, so there is no conversion out of it and no accessor.
// The only thing anybody can do with a Key is ask it whether something matches.
type Key struct{ digest [sha256.Size]byte }

// ParseKey reads the operator's registration key and keeps only its digest.
//
// **The value is never quoted back**, not in an error and not anywhere else: it is a
// credential, and the refusals below state the rule and the length instead, which is all
// an operator needs to fix one.
//
// Surrounding whitespace is removed first, and that is a usability decision with a
// consequence worth stating: `echo key > key-file` leaves a newline, and an operator who
// had to notice that would notice it as an authentication failure with nothing in any log
// to explain it. **Both sides trim**, so the announcing side does the same to whatever it
// reads. Whitespace *inside* the key is refused rather than removed — see below.
func ParseKey(raw string) (Key, error) {
	key := strings.TrimSpace(raw)
	switch {
	case key == "":
		return Key{}, fmt.Errorf("%w: it is empty", ErrInvalidKey)
	case len(key) < MinKeyBytes:
		return Key{}, fmt.Errorf("%w: it is %d bytes, and a registration key must be at least %d",
			ErrInvalidKey, len(key), MinKeyBytes)
	case len(key) > MaxKeyBytes:
		// The length is named and the key is not, exactly as above. See [MaxKeyBytes]:
		// refusing here is what lets [Key.Matches] refuse on length without ever turning
		// away a key an operator actually configured.
		return Key{}, fmt.Errorf("%w: it is %d bytes, and a registration key must be at most %d",
			ErrInvalidKey, len(key), MaxKeyBytes)
	case !printableASCII(key):
		// A key is presented in an `Authorization` header, which is bytes on one line.
		// A key holding a newline, a tab or a space cannot be presented at all — so this
		// refusal at startup is the difference between an operator reading a message
		// that names the rule and an operator watching every registration fail. The
		// commonest cause is a key file that picked up a line break when it was pasted,
		// which trimming the ends does not fix.
		return Key{}, fmt.Errorf("%w: a registration key is printable ASCII with no spaces", ErrInvalidKey)
	}
	return Key{digest: sha256.Sum256([]byte(key))}, nil
}

// Matches reports whether presented is this key.
//
// **Constant time, over two digests rather than over the two strings.** Hashing first is
// what makes the comparison fixed-length: `subtle.ConstantTimeCompare` returns early for
// operands of different lengths, so comparing the raw values would leak the key's length
// through timing — a small leak, and a free one to close.
func (k Key) Matches(presented string) bool {
	// **Refused on length before it is copied or hashed.** [ParseKey] will not build a key
	// longer than [MaxKeyBytes], so nothing this rejects could ever have matched, and
	// returning early leaks nothing: the bound is an exported constant, so a caller learns
	// from it only what it already says out loud.
	if len(presented) > MaxKeyBytes {
		return false
	}
	sum := sha256.Sum256([]byte(presented))
	return subtle.ConstantTimeCompare(k.digest[:], sum[:]) == 1
}

// String redacts the key, for fmt and for every error message built through it.
func (k Key) String() string { return redactedKey }

// GoString redacts a key printed with %#v, which String never sees.
func (k Key) GoString() string { return redactedKey }

// LogValue redacts a key that reaches a log line. Not the same defence as String: slog
// resolves a LogValuer before either handler formats anything, and the text handler
// formats a struct through fmt.
func (k Key) LogValue() slog.Value { return slog.StringValue(redactedKey) }

// MarshalJSON redacts a key that reaches encoding/json.
func (k Key) MarshalJSON() ([]byte, error) { return []byte(`"` + redactedKey + `"`), nil }

// Server is one game server, as the registry writes it down and the list serves it.
//
// Every field but [Server.LastSeen] comes from the announcement and is replaced whole by
// the next one. There is no history here and nothing accumulates: a record is the last
// thing a server said about itself.
type Server struct {
	// Name is the registry's key and **the world name a ticket is minted for**.
	//
	// One string doing both jobs is the thing that closes the trust chain: the client
	// reads a name out of this list and hands that name to
	// `POST /v1/signin/discord/finish`, which resolves it through `ticket.WorldIDFor`. A
	// registry that accepted a name the ticket service would not is a server a player can
	// see and cannot join — so [Server.Validate] asks that function rather than restating
	// its rule, and [MaxNameBytes] is that package's constant rather than a copy of it.
	//
	// Lowercase letters, digits and hyphens, constrained rather than normalised: "Midgard"
	// is refused, not lowercased, because two spellings of one world is the failure the
	// name exists to prevent.
	Name string

	// DisplayName is the title a player reads, and it is display text: it names nothing,
	// nothing keys on it, and two servers may share one.
	//
	// **Refused when it is too long rather than truncated**, which is the opposite of what
	// internal/auth does with a display name, and the difference is who wrote it. A
	// provider's idea of somebody's name arrives from a third party and shortening it is
	// better than turning that person away; this arrives from the operator's own
	// configuration, and an operator can be told the name is too long and shorten it
	// themselves. It also means this package needs no rune-boundary truncation, which is
	// eleven lines internal/auth and internal/persist each keep a copy of.
	DisplayName string

	// Address is where a player connects, as host:port.
	//
	// **This is the field the whole design exists to keep current.** A home connection
	// that changes address overnight is invisible to players precisely because the next
	// announcement replaces this and nobody is holding a copy.
	//
	// It is deliberately not logged anywhere in this service. It locates somebody's house,
	// which is the reason the list is behind a credential at all, and a value that must not
	// be published is a value that must not be in a log line either.
	Address string

	// Fingerprint is the SHA-256 of the certificate the game server presents, in lowercase
	// hex: **the same 64 characters `certs.Fingerprint` produces** and `voxelheimd` logs at
	// startup, which is what lets an operator compare one string against one string.
	//
	// It is checked for being a well-formed digest and stored verbatim. Nothing here
	// computes a digest — a second way of arriving at this number is a second number.
	Fingerprint string

	// LastSeen is when this server last announced itself, to the second and in UTC. It is
	// the only field the registry decides rather than records, and [Server.Online] is the
	// whole of what reads it.
	LastSeen time.Time
}

// Online reports whether this server has been heard from inside [OfflineAfter] of now.
//
// `now` is a parameter, which is internal/auth's rule for internal/auth's reason: what a
// test writes down is what a test reads back, and there is no clock in this package for a
// caller to be surprised by.
func (s Server) Online(now time.Time) bool {
	return now.Sub(s.LastSeen) < OfflineAfter
}

// Validate reports whether this is a registration the store will write down.
//
// Every field is checked, which is a wider rule than internal/auth's "keys, and nothing
// but its keys" — and the reason the line is drawn elsewhere here is that there is no
// layer above this one that owns what a server may say. The list endpoint serves these
// four strings to a client that acts on them: the address is dialled and the fingerprint
// is compared against a certificate, so a record holding something the format could not
// have meant is a refusal a player sees rather than a description nobody reads.
//
// **The values are quoted back, and that is the deliberate opposite of the rule
// internal/auth and internal/ticket keep.** Those refuse text that arrived unauthenticated
// from a third party, where an error message is a log line an attacker writes. A
// registration is authenticated: whoever reached this line holds the operator's key, the
// text is the operator's own configuration, and a message naming the field and the value
// is the difference between a mistake they can fix and one they have to guess at. The
// address is the exception — see below.
func (s Server) Validate() error {
	// Asked of internal/ticket rather than restated, so that a name this store accepts is
	// always a name a ticket can be minted for. See [Server.Name]. Its own sentinel is
	// wrapped alongside, because `ticket.ErrWorldName` is not a name the operator of a
	// game server would recognise as being about their server's name.
	if _, err := ticket.WorldIDFor(s.Name); err != nil {
		return fmt.Errorf("%w: %w: %w", ErrInvalidServer, ErrServerName, err)
	}

	switch {
	case s.DisplayName == "":
		return fmt.Errorf("%w: %w: it is empty", ErrInvalidServer, ErrDisplayName)
	case len(s.DisplayName) > MaxDisplayNameBytes:
		return fmt.Errorf("%w: %w: it is %d bytes, more than the %d a record keeps",
			ErrInvalidServer, ErrDisplayName, len(s.DisplayName), MaxDisplayNameBytes)
	case !utf8.ValidString(s.DisplayName):
		return fmt.Errorf("%w: %w: it is not valid UTF-8", ErrInvalidServer, ErrDisplayName)
	case !printableText(s.DisplayName):
		// A control character in a display name is a name that renders as something other
		// than what it says — and this string is served to every client that reads the list.
		return fmt.Errorf("%w: %w: it holds a control character", ErrInvalidServer, ErrDisplayName)
	}

	if err := validateAddress(s.Address); err != nil {
		return err
	}
	if err := validateFingerprint(s.Fingerprint); err != nil {
		return err
	}

	if s.LastSeen.IsZero() {
		// The one instant a registration cannot carry, and the one refusal here that no
		// request can cause: [Store.Register] fills this field from what its caller was
		// given. Written down, a zero would make the server permanently offline in every
		// list that ever read it — a silent wrong answer rather than a refusal, which is
		// the shape of failure this repository spends its effort on. It carries no field
		// sentinel because there is no field of a registration to point an operator at.
		return fmt.Errorf("%w: it says nothing about when the server was last heard from", ErrInvalidServer)
	}
	return nil
}

// validateAddress reports whether addr is somewhere a player could be sent.
//
// It is host:port and the port is numeric, which is `cmd/voxelheim-auth`'s rule for
// `-listen` and it is here for the same reason: a named port resolves against whatever
// /etc/services says on the machine that reads it, and a client on a different machine
// would dial somewhere else. Nothing is resolved and nothing is dialled — this asks
// whether the string is an address, not whether anybody is at it.
//
// **The address is not quoted back into the refusal**, alone among the fields here. It
// locates somebody's house and this error reaches a log; the field name and the rule are
// what an operator needs, and they carry no address.
func validateAddress(addr string) error {
	switch {
	case addr == "":
		return fmt.Errorf("%w: %w: it is empty", ErrInvalidServer, ErrAddress)
	case len(addr) > MaxAddressBytes:
		return fmt.Errorf("%w: %w: it is %d bytes, more than the %d a record keeps",
			ErrInvalidServer, ErrAddress, len(addr), MaxAddressBytes)
	case !printableASCII(addr):
		return fmt.Errorf("%w: %w: an address is printable ASCII with no spaces", ErrInvalidServer, ErrAddress)
	}

	host, port, err := net.SplitHostPort(addr)
	if err != nil {
		return fmt.Errorf("%w: %w: it is not host:port", ErrInvalidServer, ErrAddress)
	}
	if host == "" {
		// A bare ":7777" is what a server listening on every interface announces if nobody
		// told it what to announce instead, and it is not somewhere a client can dial.
		return fmt.Errorf("%w: %w: it names no host", ErrInvalidServer, ErrAddress)
	}
	number, err := strconv.Atoi(port)
	if err != nil {
		return fmt.Errorf("%w: %w: it needs a numeric port", ErrInvalidServer, ErrAddress)
	}
	if number < 1 || number > 65535 {
		// Zero is excluded as well as the out-of-range values: :0 means "any free port" to
		// whoever binds it and means nothing at all to whoever dials it.
		return fmt.Errorf("%w: %w: the port must be in 1..65535, got %d", ErrInvalidServer, ErrAddress, number)
	}
	return nil
}

// validateFingerprint reports whether fp is a well-formed SHA-256 digest in the rendering
// `certs.Fingerprint` produces.
//
// **Lowercase hex, refused rather than normalised.** Uppercase would be the same digest
// and this could fold it, and folding is exactly the mistake this repository declines to
// make with a provider name and with a world name: one value with two spellings is one
// value that eventually gets compared before it reaches the folding. The sanctioned way to
// obtain this number is `certs.Fingerprint`, which is lowercase, so nothing on the
// intended path ever meets this refusal — and something arriving in another case did not
// come from there, which is worth being told about.
//
// The value is quoted back: a certificate fingerprint is public. It is in the list this
// service serves and in the game server's own startup line.
func validateFingerprint(fp string) error {
	if len(fp) != FingerprintHexLen {
		return fmt.Errorf("%w: %w: it is %d characters, and a SHA-256 digest in hex is exactly %d",
			ErrInvalidServer, ErrFingerprint, len(fp), FingerprintHexLen)
	}
	for i := 0; i < len(fp); i++ {
		c := fp[i]
		if (c < '0' || c > '9') && (c < 'a' || c > 'f') {
			return fmt.Errorf("%w: %w: %q is not lowercase hex", ErrInvalidServer, ErrFingerprint, fp)
		}
	}
	return nil
}

// printableASCII reports whether s is entirely printable ASCII with no spaces: every byte
// in 0x21..0x7e.
//
// Indexed by byte rather than ranged over as runes, deliberately — the question is about
// the encoding here, not about the characters. A multi-byte rune fails it, which is the
// intent for a key and for an address.
func printableASCII(s string) bool {
	for i := 0; i < len(s); i++ {
		if s[i] < 0x21 || s[i] > 0x7e {
			return false
		}
	}
	return true
}

// printableText reports whether s holds no control character.
//
// Ranged over as runes, which is the opposite choice from [printableASCII] and the right
// one here: a display name is UTF-8 of the operator's choosing and the question is about
// the characters. Spaces are allowed — it is a title people read.
func printableText(s string) bool {
	for _, r := range s {
		if r < 0x20 || r == 0x7f {
			return false
		}
	}
	return true
}
