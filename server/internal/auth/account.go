// Package auth keeps who the people playing here are, and nothing about how they
// prove it.
//
// # No credential is kept, so there is none to leak
//
// An [Account] is an internal id, the [ProviderIdentity] it was created from, a
// display name and the moment it was made. There is no password, no access token, no
// refresh token and no signing key in this package or in any file it writes. Whatever
// a provider hands this service to show that somebody is who they say they are is
// checked by the flow that receives it and then dropped; what survives is the fact
// that the check once passed, and that is the whole of what an account is. A leaked
// accounts directory is therefore a list of display names and provider ids — an
// embarrassment rather than a way in — and that is a property of the format instead
// of a rule somebody has to keep remembering.
//
// **That claim is about the accounts directory and not about -auth-dir, and the
// distinction became load-bearing when internal/ticket arrived.** The ticket signing key
// is kept *beside* this store rather than in it — under the auth directory itself, while
// every file this package writes is under <auth-dir>/accounts/ — so a leaked accounts
// directory is still the embarrassment described above, and a leaked auth directory is
// somebody who can mint a ticket for any account on any world. The sentence above is
// narrower than it used to be and it is still exactly true; what changed is what sits
// next door.
//
// # The delta store's discipline, reused rather than re-derived
//
// Magic number, format version, trailing CRC-32, temporary-file-and-rename writes,
// temporaries swept on open, unknown versions refused whole. Every one of those comes
// from internal/world through the helpers it exports for the purpose
// ([world.WriteAtomic], [world.CheckHeader], [world.CheckChecksum],
// [world.PutChecksum], [world.SweepTemporaries]) rather than being written a third
// time here — internal/persist was the second. The version number is this package's
// own, because an account and a chunk delta change for entirely unrelated reasons.
//
// **The import of internal/world is for those five helpers and nothing else.** This
// package never opens a world directory, never names a chunk and never learns that
// terrain exists. The accounts directory is its own, under its own flag, and the two
// services share no byte on disk.
//
// # This package judges its keys, and nothing but its keys
//
// internal/persist deliberately judges none of a record's contents, because
// internal/game owns what a life is allowed to say. There is no such layer above this
// one: auth *is* where an account means something. So the line here is drawn
// somewhere else, and it is drawn between keys and description. A [ProviderIdentity]
// is a key — a wrong one names the wrong person — so it is validated on the way in and
// again on the way out, and a record whose identity this build would not have written
// is refused whole. An [Account.ID] is a name and must exist, so a zero one is refused
// too. A display name and a created-at time describe an account rather than finding
// it, and are written down as given.
//
// # One index, deliberately
//
// An account is found by the provider identity it was created from, because that is
// the only lookup this service can perform today: the flow that will call it has a
// provider identity in its hand and nothing else. Finding an account by its
// [AccountID] needs a second index, and it arrives with the thing that needs it.
//
// # Who imports this
//
// cmd/voxelheim-auth, and nothing else. cmd/voxelheimd in particular must not: the
// game server and the account service are separate trust domains that happen to ship
// from one module, and the moment the simulation can open the accounts directory,
// "the account service holds the accounts" stops being true. imports_test.go pins
// both halves of that — who may import this package, and what this package may
// import — because a boundary nothing checks is a boundary that has already moved.
package auth

import (
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"time"
	"unicode/utf8"
)

// AccountIDSize is how many bytes an account id is.
//
// Sixteen — 128 bits from crypto/rand — because an account id is a *name* and not a
// digest of anything: there is nothing here for it to be the hash of. That is
// deliberate rather than incidental. Deriving it from the provider identity would
// make it a one-way function of somebody's Discord id, and then anybody holding an
// account id could confirm a guess about whose it is. Minted at random, it says
// nothing at all about the person it names, which is what lets it be the id the rest
// of the game will carry.
const AccountIDSize = 16

// Text limits, each a bound on what a record can be asked to hold. They are what make
// [maxRecordSize] a number, which is what lets an oversized file be refused before a
// byte of it is read.
//
// The two halves of an identity and the display name are capped for opposite reasons,
// and the difference decides what happens when the cap is exceeded. **A provider name
// and a subject are keys**: shortening one would silently name a different person, so
// an over-long one is refused. **A display name describes**: it is truncated at a rune
// boundary and never refused, because a long name is not a reason to turn somebody
// away — internal/persist truncates a player name for the same reason and in the same
// way.
const (
	// MaxProviderBytes is the longest provider name a record keeps. Generous for a
	// vocabulary this service defines itself; see [ProviderIdentity].
	MaxProviderBytes = 32

	// MaxSubjectBytes is the longest provider subject a record keeps. Sized for the
	// opaque identifiers real providers issue — a Discord snowflake is under 20
	// characters, and an OIDC `sub` is bounded by 255 in the specification but is a
	// UUID or a numeric string in practice.
	MaxSubjectBytes = 128

	// MaxDisplayNameBytes is the longest display name a record keeps.
	MaxDisplayNameBytes = 64
)

// ErrInvalidIdentity reports a provider identity this service will not key on.
//
// A sentinel because the caller branches on it: a malformed identity is the caller's
// mistake and not a failure of the store, so the flow that will sit in front of this
// answers it differently from an unreadable disk. Without a sentinel that distinction
// would have to be made by matching on a string.
var ErrInvalidIdentity = errors.New("auth: that is not a provider identity this service can key on")

// AccountID is the internal name of one account: minted once, never derived, and the
// value the rest of the game will use to say that a character belongs to a person.
//
// A fixed-size array rather than a slice, deliberately: it is copied by assignment,
// two of them cannot alias, and no caller can hand around a 15-byte one.
type AccountID [AccountIDSize]byte

// NewAccountID mints a fresh account's id.
//
// A failed read from crypto/rand is returned, never swallowed. The alternative is the
// one value this package must never produce — a zero id, which every account that
// failed to mint would share, and which would make them all the same person.
func NewAccountID() (AccountID, error) { return newAccountID(rand.Reader) }

// newAccountID is [NewAccountID] over an injectable source, so the refusal above can
// be tested. crypto/rand does not fail on any platform this service runs on, which is
// exactly why the branch needs a test to exist at all.
func newAccountID(r io.Reader) (AccountID, error) {
	var id AccountID
	if _, err := io.ReadFull(r, id[:]); err != nil {
		return AccountID{}, fmt.Errorf("auth: minting an account id: %w", err)
	}
	return id, nil
}

// String is the id in lowercase hex: 32 characters.
func (id AccountID) String() string { return hex.EncodeToString(id[:]) }

// IsZero reports the one id no mint can produce and no record may hold.
func (id AccountID) IsZero() bool { return id == AccountID{} }

// ProviderIdentity is who an identity provider says a person is.
//
// Two strings, and they are read very differently. Provider is **this service's own
// vocabulary** — the name it uses for a provider it supports — so it is constrained
// to lowercase letters, digits and hyphens. Subject is **the provider's**: an opaque
// identifier this service stores and compares and never interprets, so nothing about
// its shape is assumed beyond a length bound and valid UTF-8.
//
// The provider name is constrained rather than normalised on purpose. Lowercasing
// whatever arrived would quietly accept "Discord" and "discord" as one provider,
// which is right until the day something compares them before they reach here — and
// then two spellings are two accounts for one person, which is the exact failure this
// store exists to prevent, arriving with nothing to notice it by. A refusal is loud
// and happens once, in a test.
type ProviderIdentity struct {
	// Provider names the provider itself: "discord", and in time whatever else this
	// service learns to speak to.
	Provider string

	// Subject is that provider's own id for this person — stable across their name
	// changes, which is the whole reason an account is keyed on it rather than on
	// anything a person can edit.
	//
	// Not a secret and not a credential: knowing somebody's Discord user id is not a
	// way to become them. It is personal data all the same, so it is never written
	// into an error message or a log line here.
	Subject string
}

// Validate reports whether this is an identity the store can key on.
//
// **The values are never quoted back.** A flag validation quotes what the operator
// typed, because that is the operator's own input on the operator's own terminal; a
// provider subject is somebody else's identity arriving from a third party, and an
// error string ends up in a log. So these messages state the rule and the length and
// nothing else — which is also what stops arbitrary remote text from being pasted
// into a log line.
func (p ProviderIdentity) Validate() error {
	switch {
	case p.Provider == "":
		return fmt.Errorf("%w: it names no provider", ErrInvalidIdentity)
	case len(p.Provider) > MaxProviderBytes:
		return fmt.Errorf("%w: the provider name is %d bytes, more than the %d a record keeps",
			ErrInvalidIdentity, len(p.Provider), MaxProviderBytes)
	case !validProviderName(p.Provider):
		return fmt.Errorf("%w: a provider name is lowercase letters, digits and hyphens", ErrInvalidIdentity)
	case p.Subject == "":
		return fmt.Errorf("%w: it names no subject", ErrInvalidIdentity)
	case len(p.Subject) > MaxSubjectBytes:
		return fmt.Errorf("%w: the subject is %d bytes, more than the %d a record keeps",
			ErrInvalidIdentity, len(p.Subject), MaxSubjectBytes)
	case !utf8.ValidString(p.Subject):
		return fmt.Errorf("%w: the subject is not valid UTF-8", ErrInvalidIdentity)
	}
	return nil
}

// validProviderName reports whether name is drawn from this service's own vocabulary.
//
// Ranging over the string yields runes, so a non-ASCII one fails the switch rather
// than being examined a byte at a time — which is what makes this a check on the
// characters instead of on the encoding.
func validProviderName(name string) bool {
	for _, r := range name {
		switch {
		case r >= 'a' && r <= 'z', r >= '0' && r <= '9', r == '-':
		default:
			return false
		}
	}
	return true
}

// Account is one person, as this service writes them down.
//
// Four fields, and no fifth is coming quietly: anything that would let somebody prove
// they are this person belongs to the flow that checks it and not to this file. See
// the package comment.
type Account struct {
	// ID is the internal name of this account: minted once, at creation, and never
	// derived from anything about the person.
	ID AccountID

	// Identity is the provider identity the account was created from, and the key it
	// is found by. It is written into the record as well as hashed into the file
	// name, so a record that has been copied or renamed onto the wrong path is caught
	// rather than answered — internal/world writes a chunk's coordinate into its file
	// for exactly that reason.
	Identity ProviderIdentity

	// DisplayName is what to call this person, as their provider last reported it.
	// Untrusted display text: truncated to [MaxDisplayNameBytes] on the way to disk,
	// never unique, and nothing keys on it.
	DisplayName string

	// CreatedAt is when the account was made, to the second. It describes the account
	// rather than finding it, so the store writes it down and forms no opinion — see
	// the package comment on keys and description.
	CreatedAt time.Time
}

// truncateName cuts name to at most [MaxDisplayNameBytes] without splitting a rune.
//
// The rune boundary is the whole subtlety: a display name is UTF-8 of the provider's
// choosing, and a cut through the middle of a multi-byte rune stores text that no
// longer decodes — a replacement character in an operator's log, from a name that was
// fine.
//
// internal/persist has the same eleven lines for a player name, and they are
// deliberately not shared. Importing that package from here would tie the account
// service to the game's player store, which is the one coupling the two-command split
// exists to prevent; hoisting it into internal/world would widen a package that
// imports nothing of ours, for a string helper. Two copies of eleven lines is the
// cheaper of the three.
func truncateName(name string) string {
	if len(name) <= MaxDisplayNameBytes {
		return name
	}
	cut := MaxDisplayNameBytes
	for cut > 0 && !utf8.RuneStart(name[cut]) {
		cut--
	}
	return name[:cut]
}
