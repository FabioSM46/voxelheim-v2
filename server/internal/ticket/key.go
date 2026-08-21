package ticket

import (
	"bytes"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"os"
	"path/filepath"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// KeyStoreVersion is the on-disk format version of a key record.
//
// Bump it for any change to the layout below, including one that only adds a field: a
// reader of an older build must refuse a newer record rather than parse a prefix of it
// and guess at the rest. Deliberately separate from `auth.StoreVersion` and from every
// version internal/persist keeps — a key pair, an account, a player record and a chunk
// delta change for entirely unrelated reasons.
//
// **Bumping it is not free the way the others are.** A version this build does not know
// is refused whole, and refusing this pair means the service will not start; there is no
// migration that invents the old key. A format change here is a change an operator has
// to be given a path through.
const KeyStoreVersion uint32 = 1

// Where the key pair lives, under the operator's -auth-dir beside the accounts
// directory. Exported for the reason internal/certs exports its two: an operator asking
// "what do I back up" should be able to find the answer in the package that writes it.
const (
	// SigningKeyFileName holds the private half — the seed, not the expanded key.
	SigningKeyFileName = "signing-key.bin"

	// VerifyingKeyFileName holds the public half.
	//
	// Kept even though the seed determines it, because half a pair has to be a state
	// this package can *see* in order to refuse it, and because an operator reading the
	// public key should not have to open the file that holds the private one.
	VerifyingKeyFileName = "verifying-key.bin"
)

// signingKeyFileMode is what the private key is written as, and it is the one file in
// this service whose mode is load-bearing: whatever can read it can mint a ticket for
// anybody. world.WriteAtomic creates its temporary through os.CreateTemp, which is
// already 0600, and the rename preserves it — this constant is what the test asserts
// against so that a change to that helper cannot loosen it in silence.
const signingKeyFileMode fs.FileMode = 0o600

// On-disk layout, little-endian throughout, one file per half.
//
//	signing-key.bin    magic[4] version:u32 seed[32]      crc32:u32 = 44 bytes
//	verifying-key.bin  magic[4] version:u32 public[32]    crc32:u32 = 44 bytes
//
// The discipline is internal/world's, taken through the five helpers it exports for the
// purpose rather than written down a fourth time: magic number, format version, trailing
// CRC-32, temporary-file-and-rename writes, temporaries swept on open, unknown versions
// refused whole.
//
// **The two magics are different and the two records are the same size, which is why
// they have to be.** A copy of one file onto the other's path is otherwise a pair whose
// public half is a seed — and Ed25519 will happily hold 32 arbitrary bytes as a public
// key, so nothing later in the load would notice. internal/auth writes the identity into
// its record for the same reason; here the discriminator is the magic, because a record
// of 32 opaque bytes has nothing else to compare against.
//
// The **seed** rather than the 64-byte expanded private key: it is the whole of the
// secret, crypto/ed25519 reconstructs the rest from it, and a file holding only the
// minimal secret is one fewer copy of the public half to disagree with the file next to
// it.
const (
	seedRecordSize   = world.HeaderSize + ed25519.SeedSize + world.ChecksumSize
	publicRecordSize = world.HeaderSize + ed25519.PublicKeySize + world.ChecksumSize
)

// The two magics: 'K' for the signing key and 'V' for the verifying key, beside
// internal/world's 'W' and 'D', internal/persist's 'P', 'S' and 'C', and internal/auth's
// 'A'. Distinct so that a file of one kind can never be read as another even when the
// two happen to be the same size — which, here, they are.
var (
	signingMagic   = [4]byte{'V', 'X', 'H', 'K'}
	verifyingMagic = [4]byte{'V', 'X', 'H', 'V'}
)

// redactedSigningKey is what a [SigningKey] renders as, whichever formatter reaches it.
const redactedSigningKey = "ticket.SigningKey(redacted)"

// SigningKey is the private half of this service's key pair, and the one value in this
// repository whose disclosure is unrecoverable: whoever holds it can mint a ticket for
// any account on any world, and there is no revocation to undo it with.
//
// **A struct with an unexported field, which is stronger than the named types this
// repository redacts elsewhere.** `identity.Token` and `discord.Secret` are a named
// array and a named string, so a conversion gets the value back out; this one has no
// conversion, no accessor and no `Reveal`. The only thing anybody can do with it is ask
// it to sign, which is the whole of what a signing key is for. There is deliberately no
// way to obtain the bytes, because there is no legitimate caller for them.
//
// The four defences are four because each covers a route the others do not:
//
//   - [SigningKey.String] covers fmt — %v, %s, and every message built with fmt.Errorf.
//     It is needed rather than decorative: fmt prints unexported struct fields, so
//     without it %v renders the key as a list of numbers.
//   - [SigningKey.GoString] covers %#v, which a Stringer never sees. That is the verb a
//     test failure and a debugging print reach for, which makes it the one most likely
//     to end up pasted somewhere.
//   - [SigningKey.LogValue] covers log/slog, which resolves a LogValuer before either
//     handler formats anything. The text handler formats a struct through fmt, so
//     without this the key reaches a log line as bytes.
//   - [SigningKey.MarshalJSON] covers encoding/json. It is the one of the four that is
//     not load-bearing today — json cannot see an unexported field, so the zero-value
//     `{}` would be safe anyway — and it is here so that the type stays safe if the
//     field is ever exported or the struct ever becomes a named slice.
type SigningKey struct{ key ed25519.PrivateKey }

// String redacts the key, for fmt and for every error message built through it.
func (s SigningKey) String() string { return redactedSigningKey }

// GoString redacts a key printed with %#v. See the type comment: this is not the same
// defence as String.
func (s SigningKey) GoString() string { return redactedSigningKey }

// LogValue redacts a key that reaches a log line. See the type comment: this is not the
// same defence as String either, and the text handler is the reason.
func (s SigningKey) LogValue() slog.Value { return slog.StringValue(redactedSigningKey) }

// MarshalJSON redacts a key that reaches encoding/json.
func (s SigningKey) MarshalJSON() ([]byte, error) { return []byte(`"` + redactedSigningKey + `"`), nil }

// Pair is the account service's Ed25519 key pair: the thing it signs tickets with and
// the thing every game server reads once and keeps.
//
// **There is no ephemeral pair, and the omission is deliberate.** internal/certs offers
// one because a world that keeps nothing cannot keep a key either, and the cost is
// borne by a client that pinned a fingerprint. Here the cost would be borne by every
// game server that had already stored the public key and by every ticket in flight, and
// nobody would find out until a player was refused. So the only way to obtain a pair is
// [LoadOrCreate], which keeps what it makes; generation itself is unexported so that no
// caller can create the mode by accident.
//
// Safe for concurrent use, which is not incidental: this sits behind HTTP handlers, so
// two sign-ins minting at once is the ordinary case. Nothing here is written after
// LoadOrCreate returns and ed25519.Sign holds no state, so there is nothing to
// serialise — and a mutex would have been the wrong answer anyway, since it would have
// implied there was.
type Pair struct {
	signing   SigningKey
	verifying ed25519.PublicKey
}

// LoadOrCreate is the key pair kept under authDir, generated on first start and read
// back on every one after it.
//
// **A pair that exists and cannot be read is an error and stays one.** That is the same
// refusal `auth.Store` makes about an account record, restated where it costs more: the
// tempting thing to do with a key file that will not parse is to make a fresh one, and
// the service that does invalidates every ticket in flight and every copy a game server
// has stored — a fleet of servers refusing every player at once, on the strength of a
// permission problem. Refusing to start is a message an operator can act on.
//
// **Half a pair is refused rather than repaired**, and it is refused even though the
// public half is derivable from the private one. Writing the missing half would mean
// this service deciding, on its own, that the file which survived is the one that is
// correct — and if the survivor is the *public* half there is nothing to derive from at
// all. One rule that always says the same thing beats two that depend on which file went
// missing.
func LoadOrCreate(authDir string) (*Pair, error) {
	if authDir == "" {
		// Refused rather than answered with a pair that is written nowhere. See the
		// [Pair] doc: there is no ephemeral mode here to fall back to.
		return nil, errors.New("ticket: the accounts directory must be named")
	}

	// Whatever a crash left mid-rename, swept on open for the reason every store
	// writing through world.WriteAtomic does. Inert — a reader only ever opens an exact
	// path and a temporary name never is one — so this is housekeeping rather than
	// correctness, and it is a no-op on a directory that does not exist yet.
	world.SweepTemporaries(authDir)

	signingPath := filepath.Join(authDir, SigningKeyFileName)
	verifyingPath := filepath.Join(authDir, VerifyingKeyFileName)

	signingData, signingErr := readKeyFile(signingPath, seedRecordSize)
	verifyingData, verifyingErr := readKeyFile(verifyingPath, publicRecordSize)
	switch {
	case signingErr == nil && verifyingErr == nil:
		return loadPair(signingPath, signingData, verifyingPath, verifyingData)

	case errors.Is(signingErr, fs.ErrNotExist) && errors.Is(verifyingErr, fs.ErrNotExist):
		// The first start. Anything else — one half present, or a read that failed for
		// a reason other than absence — falls through to a refusal below.

	case signingErr != nil && !errors.Is(signingErr, fs.ErrNotExist):
		return nil, signingErr
	case verifyingErr != nil && !errors.Is(verifyingErr, fs.ErrNotExist):
		return nil, verifyingErr
	default:
		// Exactly one of the two is missing, which is not a state this service ever
		// writes: the pair is written together.
		return nil, fmt.Errorf(
			"ticket: %s and %s must both exist or both be absent; restore the missing half from a backup — "+
				"deleting the other mints a new pair, which invalidates every ticket in flight and every copy a game server has stored",
			signingPath, verifyingPath)
	}

	if err := os.MkdirAll(authDir, 0o700); err != nil {
		return nil, fmt.Errorf("ticket: creating %s: %w", authDir, err)
	}

	pub, priv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		// Returned, never swallowed. The alternative is the one value this package must
		// never hold — a zero key, which every service that failed to generate would
		// share, and which would make them all the same signer.
		return nil, fmt.Errorf("ticket: generating a key pair: %w", err)
	}

	// The private half first. A crash between the two writes leaves the pair incomplete
	// either way, and the refusal above turns that into a message rather than into a
	// service running under a public key nobody can sign for.
	if err := world.WriteAtomic(signingPath, encodeKeyRecord(signingMagic, priv.Seed())); err != nil {
		return nil, fmt.Errorf("ticket: writing %s: %w", signingPath, err)
	}
	if err := world.WriteAtomic(verifyingPath, encodeKeyRecord(verifyingMagic, pub)); err != nil {
		return nil, fmt.Errorf("ticket: writing %s: %w", verifyingPath, err)
	}
	return &Pair{signing: SigningKey{key: priv}, verifying: pub}, nil
}

// Public is the verifying half, which is the value this service publishes and every
// game server keeps.
//
// A copy rather than the pair's own slice: ed25519.PublicKey is a []byte, and handing
// out the backing array would let a caller change what this pair verifies against by
// writing into a value it was merely shown.
func (p *Pair) Public() ed25519.PublicKey { return ed25519.PublicKey(bytes.Clone(p.verifying)) }

// PublicHex is the verifying half in lowercase hex: 64 characters.
//
// Hex rather than the base64url a ticket is encoded in, and deliberately the same
// rendering `certs.Fingerprint` uses: this is the one value in this service an operator
// reads with their eyes, comparing what the log said against what the endpoint answered
// against what a game server has stored.
func (p *Pair) PublicHex() string { return hex.EncodeToString(p.verifying) }

// String is the pair, named by its public half. The private half is not in it.
func (p *Pair) String() string { return "ticket.Pair(" + Algorithm + " " + p.PublicHex() + ")" }

// LogValue is what a pair reaching a log line becomes: the public key, which is the
// thing an operator wants, and nothing else.
//
// The deliberate disclosure is therefore the default and the accident is unrepresentable
// — without this, the text handler would format the struct through fmt and reach the
// signing key's own redaction, which is safe but says nothing useful.
func (p *Pair) LogValue() slog.Value { return slog.StringValue(p.String()) }

// Mint signs a ticket for account on world, good for [Lifetime] from now.
//
// **The lifetime is not a parameter**, which is the difference between a stated constant
// and a default: there is no caller that may ask for a longer one, so there is no
// argument for one to pass. What is a parameter is `now` — internal/auth's rule, for
// internal/auth's reason, and it is also what lets a test hold an expired ticket without
// waiting eight hours for one.
//
// The claims come back beside the ticket because the caller has to tell somebody when it
// runs out, and computing that a second time from [Lifetime] would be a second place for
// the truncation to disagree. They are the ticket's own, read from what was signed.
//
// **A zero world is refused here, and the refusal did not move when the account ticket
// arrived.** The format reads one back happily — see [decodeBody] — so this is not a rule
// about what a ticket may be. It is a rule about how one is asked for: a caller that
// forgot to fill the field in must get an error rather than a credential of a shape
// nobody meant to issue. [Pair.MintAccountTicket] is the way to ask on purpose, and it is
// deliberately a different method name rather than a sentinel value for this parameter.
func (p *Pair) Mint(account AccountID, world WorldID, now time.Time) (Ticket, Claims, error) {
	if world.IsZero() {
		return Ticket{}, Claims{}, fmt.Errorf(
			"%w: it names no world; MintAccountTicket is how a ticket for no world is asked for", ErrUnmintable)
	}
	return p.mint(account, world, now)
}

// MintAccountTicket signs a ticket that names **no world**: an account ticket, good for
// [Lifetime] from now.
//
// It says one thing — the account service knows who this is — and it is what the server
// list is read with. A player cannot name a world before they have seen the list, and the
// list is what tells them the worlds exist, so a credential that has to name one up front
// closes the trust chain in a circle. This is the way out of it.
//
// **It is not a way into a game.** [Verify] is what a game server calls, it is handed
// that server's own world, and a ticket naming none fails that comparison exactly as a
// ticket for somebody else's world does. Nothing about this method widens what a game
// server admits; the account service's own [VerifyAnyWorld] is the only check that reads
// one.
//
// A separate method rather than `Mint(account, WorldID{}, now)` on purpose. The zero
// value is what a caller gets from a forgotten field, and the difference between "I meant
// no world" and "I forgot the world" cannot be recovered from the argument — so it is
// carried by which function was called, where it cannot be lost.
func (p *Pair) MintAccountTicket(account AccountID, now time.Time) (Ticket, Claims, error) {
	return p.mint(account, WorldID{}, now)
}

// mint is the signing both public mints share, after each has decided whether the world
// it was given is one it will sign for.
func (p *Pair) mint(account AccountID, world WorldID, now time.Time) (Ticket, Claims, error) {
	claims := Claims{
		Account: account,
		World:   world,
		// To the second, because that is the resolution the format keeps: the claims
		// returned here are the claims a later [Verify] reads back, not a description
		// of them.
		ExpiresAt: now.Add(Lifetime).UTC().Truncate(time.Second),
	}
	body, err := encodeBody(claims)
	if err != nil {
		return Ticket{}, Claims{}, err
	}

	var t Ticket
	copy(t[:BodySize], body)
	copy(t[BodySize:], ed25519.Sign(p.signing.key, body))
	return t, claims, nil
}

// readKeyFile reads one key record, refusing a file too large to be one before a byte of
// it is read.
//
// The size check is before the read rather than after, the way `auth.Store.Load`'s is: a
// file this large is not one this format wrote, and finding that out by allocating it is
// how a corrupt directory becomes an out-of-memory. A file that is too *short* is caught
// by the exact-length check in [decodeKeyRecord], which is the same check.
func readKeyFile(path string, size int) ([]byte, error) {
	info, err := os.Stat(path)
	if err != nil {
		// Returned unwrapped in shape, so errors.Is(err, fs.ErrNotExist) still answers
		// for the caller's first-start branch, and already naming the path because
		// os.Stat's error does.
		if errors.Is(err, fs.ErrNotExist) {
			return nil, err
		}
		return nil, fmt.Errorf("ticket: reading %s: %w", path, err)
	}
	if info.Size() != int64(size) {
		return nil, fmt.Errorf("%w: %s is %d bytes, and a key record is exactly %d",
			world.ErrCorruptStore, path, info.Size(), size)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("ticket: reading %s: %w", path, err)
	}
	return data, nil
}

// encodeKeyRecord wraps one 32-byte half in the record discipline above.
func encodeKeyRecord(magic [4]byte, payload []byte) []byte {
	buf := make([]byte, world.HeaderSize+len(payload)+world.ChecksumSize)
	copy(buf[0:4], magic[:])
	binary.LittleEndian.PutUint32(buf[4:world.HeaderSize], KeyStoreVersion)
	copy(buf[world.HeaderSize:], payload)
	world.PutChecksum(buf)
	return buf
}

// decodeKeyRecord reads one half back, refusing anything it cannot read exactly.
//
// **No part of the payload reaches an error message here, and that is not tidiness.**
// One of the two payloads is the seed, and every message below is built from the path,
// the sizes and the header — world.CheckHeader quotes the magic and the version,
// world.CheckChecksum quotes two checksums, and neither touches what comes between.
func decodeKeyRecord(path string, data []byte, magic [4]byte, payloadSize int) ([]byte, error) {
	want := world.HeaderSize + payloadSize + world.ChecksumSize
	if len(data) != want {
		return nil, fmt.Errorf("%w: %s is %d bytes, and a key record is exactly %d",
			world.ErrCorruptStore, path, len(data), want)
	}
	if err := world.CheckHeader(data, magic, KeyStoreVersion); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	if err := world.CheckChecksum(data); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	return data[world.HeaderSize : world.HeaderSize+payloadSize], nil
}

// loadPair reads a pair back and refuses two halves that are not each other's.
//
// The agreement check is the one internal/certs gets for free from tls.X509KeyPair and
// has to be made explicitly here. Without it, a public key file restored from the wrong
// backup would be a service that signs with one key and publishes another — which every
// gate passes, every log line looks right for, and no player can join under.
func loadPair(signingPath string, signingData []byte, verifyingPath string, verifyingData []byte) (*Pair, error) {
	seed, err := decodeKeyRecord(signingPath, signingData, signingMagic, ed25519.SeedSize)
	if err != nil {
		return nil, err
	}
	stored, err := decodeKeyRecord(verifyingPath, verifyingData, verifyingMagic, ed25519.PublicKeySize)
	if err != nil {
		return nil, err
	}

	priv := ed25519.NewKeyFromSeed(seed)
	// The assertion cannot fail: crypto/ed25519 declares this exact type as the public
	// half of its own private key.
	derived := priv.Public().(ed25519.PublicKey)
	if !derived.Equal(ed25519.PublicKey(stored)) {
		// Neither key is named in the message. The public half would be harmless and
		// the private one would not, and an error that quoted "the two keys" would
		// most likely quote both.
		return nil, fmt.Errorf("%w: %s and %s are not two halves of one pair; restore both from one backup — "+
			"generating a new pair invalidates every ticket in flight and every copy a game server has stored",
			world.ErrCorruptStore, signingPath, verifyingPath)
	}
	return &Pair{signing: SigningKey{key: priv}, verifying: bytes.Clone(stored)}, nil
}
