package auth

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"sync"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// StoreVersion is the on-disk format version of an account record.
//
// Bump it for any change to the layout below, including one that only adds a field:
// a reader of an older build must refuse a newer record rather than parse a prefix of
// it and guess at the rest. Deliberately separate from world.StoreVersion and from
// every version internal/persist keeps, because an account, a player record, a camp,
// a clock and a chunk delta change for unrelated reasons — and world.StoreVersion in
// particular must never be reached for, since bumping it invalidates every stored
// chunk delta in every existing world.
const StoreVersion uint32 = 1

// DefaultAuthDir is where the account service keeps its accounts unless told
// otherwise, in the shape world.DefaultWorldDir has for the game server.
const DefaultAuthDir = "auth"

// On-disk layout, little-endian throughout, one file per account.
//
//	accounts/<provider-key-hex>.bin
//	    magic[4] version:u32
//	    account_id[16] created_at:i64
//	    provider_len:u16 subject_len:u16 name_len:u16
//	    provider[provider_len] subject[subject_len] name[name_len]
//	    crc32:u32
//
// Everything fixed-width first and every variable-length field last, so the decoder
// reasons about exactly one equation: header + the three declared lengths + the CRC
// must be the file's actual size. A truncated file fails that equality rather than
// being read as an account with a shorter name, and so does a padded one.
//
// Three lengths rather than internal/persist's one is the only place this layout is
// less simple than that one, and the alternative was worse: leaving the identity out
// of the record, on the grounds that the file name already encodes it. The file name
// is a *hash* of the identity and cannot be read back, so the record would then be
// unable to say who it belongs to — which the account is required to hold — and the
// misplaced-file check below would be impossible.
//
// **The file name is a hash of the identity, not the identity.** A provider subject
// is a third party's text and must never reach the filesystem: it can hold a slash,
// a NUL, a "..", or four hundred characters of anything. A digest is fixed-length hex
// and carries none of that. It also means the accounts directory is not a listing of
// everybody's Discord ids — see [accountKey] for what the digest is taken over.
const (
	accountsDirName = "accounts"
	accountFileExt  = ".bin"

	offAccountID   = world.HeaderSize
	offCreatedAt   = offAccountID + AccountIDSize
	offProviderLen = offCreatedAt + 8
	offSubjectLen  = offProviderLen + 2
	offNameLen     = offSubjectLen + 2

	recordHeaderSize = offNameLen + 2
	maxRecordSize    = recordHeaderSize + MaxProviderBytes + MaxSubjectBytes + MaxDisplayNameBytes + world.ChecksumSize
)

// accountMagic is this store's own four bytes: 'A' for account, beside internal/world's
// 'W' and 'D' and internal/persist's 'P', 'S' and 'C'. Distinct so that a file of one
// kind can never be read as another even when the two happen to be the same size.
var accountMagic = [4]byte{'V', 'X', 'H', 'A'}

// Store is one account service's accounts directory.
//
// **There is no nil Store, and that is the deliberate difference from every sibling
// store in this repository.** A nil world.Store, a nil persist.Store and a nil
// persist.ClockStore are all the ephemeral world: a mode in which nothing is written
// down, chosen by an operator who accepts losing an evening's digging. An account
// service has no such mode. An account nobody kept is a person who cannot get back
// in, so "run without a store" is not a trade anybody would knowingly take — and a
// no-op nil receiver here would create that mode by accident, for the convenience of
// one branch. [OpenStore] refuses an unnamed directory instead.
//
// Safe for concurrent use. [Store.Load] touches the path of exactly one identity and
// world.WriteAtomic renames onto it, so a reader sees the whole of the old file or the
// whole of the new one and needs no exclusion. Every *write* takes one lock: the pair
// [Store.Ensure] makes out of a read and a write, and [Store.Save] on its own.
type Store struct {
	dir string

	// write serialises every write against every other one, and against the read
	// [Store.Ensure] decides from.
	//
	// Ensure is check-then-create, and this service is an HTTP server: two requests
	// for one person arriving together is the ordinary case rather than the exotic
	// one, and unguarded they would both find no account and both mint one — the
	// second write landing on the first, so one person ends up with two account ids
	// and whichever the loser was carrying is gone.
	//
	// **[Store.Save] holds it too, and the first version of this comment explained
	// why it would not need to.** Save does not read before it writes, so nothing
	// about Save alone is check-then-create — but Ensure is, and a Save that lands
	// between Ensure's "no account here" and Ensure's own write is overwritten by an
	// account minted on the strength of a directory that had already stopped looking
	// like that. The exclusion belongs to the *pair*, so it cannot be held by only
	// the operation that reads. Save is exported, so this is a caller's race and not
	// merely an internal one (found in review on #98).
	//
	// **What this does not cover is two processes**, because a mutex cannot. One
	// account service owns its accounts directory; running a second one against the
	// same directory is a deployment mistake, and the fix for it is a lock in the
	// filesystem rather than a wider lock in here.
	write sync.Mutex
}

// OpenStore opens the accounts directory under authDir, creating it if it is not
// there.
//
// Unlike the stores under the world directory, nothing has run before this to decide
// whether the directory is the right one: there is no seed to check an account
// against. What this does instead is what it can — create the directory, refuse a path
// it cannot, and sweep whatever a crash left behind — so that a service which is going
// to fail on its storage fails here, before it has bound a port and told the world it
// is up.
func OpenStore(authDir string) (*Store, error) {
	if authDir == "" {
		// Refused rather than answered with a store that writes nowhere. See the
		// [Store] doc: an account service has no ephemeral mode to fall back to.
		return nil, errors.New("auth: the accounts directory must be named")
	}

	dir := filepath.Join(authDir, accountsDirName)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, fmt.Errorf("auth: creating %s: %w", dir, err)
	}

	// Whatever a crash left mid-rename. Inert, because a reader only ever opens an
	// exact <key>.bin path and a temporary name never is one, so this is housekeeping
	// rather than correctness — the same sweep every store writing through
	// world.WriteAtomic inherits.
	//
	// The pattern bounds it to temporaries of this store's own records. Note which
	// directory this is: the accounts directory *under* `-auth-dir`, created here, not
	// `-auth-dir` itself — which is the operator's and is swept by name in
	// internal/ticket (#137).
	world.SweepTemporaries(dir, "*"+accountFileExt)
	return &Store{dir: dir}, nil
}

// Dir is the accounts directory this store writes to.
func (s *Store) Dir() string { return s.dir }

// Load reads the account stored for a provider identity.
//
// Three answers, and the middle one is the entire point of this file: found, absent,
// or unreadable. An identity with no file is not an error — nobody with that provider
// identity has ever signed in — and the caller mints. **A file that exists and cannot
// be read is an error and must stay one.** Reported as "no such account", a damaged
// file would mint a second account for a person who already has one: they would sign
// in successfully, find none of their characters, and the first thing the new account
// does is write itself over the record of the old one.
func (s *Store) Load(id ProviderIdentity) (Account, bool, error) {
	if err := id.Validate(); err != nil {
		return Account{}, false, err
	}
	path := s.accountPath(id)

	info, err := os.Stat(path)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return Account{}, false, nil
	case err != nil:
		return Account{}, false, fmt.Errorf("auth: reading %s: %w", path, err)
	}
	// Before the read, not after: a file this large is not one this format wrote, and
	// finding that out by allocating it is how a corrupt directory becomes an
	// out-of-memory.
	if info.Size() > int64(maxRecordSize) {
		return Account{}, false, fmt.Errorf("%w: %s is %d bytes, more than the %d an account record can need",
			world.ErrCorruptStore, path, info.Size(), maxRecordSize)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		return Account{}, false, fmt.Errorf("auth: reading %s: %w", path, err)
	}

	acct, err := decodeAccount(data)
	if err != nil {
		return Account{}, false, fmt.Errorf("%s: %w", path, err)
	}
	// The record says who it belongs to, so a file copied or renamed onto another
	// identity's path is caught here rather than answered as that identity's account —
	// internal/world writes a chunk's coordinate into its file and checks it for the
	// same reason. internal/persist deliberately cannot: its file name is the hash of
	// a secret, so there is nothing in the record for it to compare against.
	//
	// **Neither identity is named in the error**, only the file. An error string
	// reaches a log, and both of these are somebody's provider id.
	if acct.Identity != id {
		return Account{}, false, fmt.Errorf("%w: %s holds an account for a different provider identity",
			world.ErrCorruptStore, path)
	}
	return acct, true, nil
}

// Save writes an account, atomically, at the path its identity names.
//
// It refuses an account this build could not read back — an unminted id, an identity
// that is not one — and it refuses it *here*, at the write, rather than only at the
// next read. Writing a file this build would then reject is the single failure that
// looks like a success until a restart; internal/persist's structures file refuses to
// exceed its own cap for exactly that reason.
//
// Serialised against a mint in flight; the write mutex's own comment carries the
// interleaving that makes that necessary.
func (s *Store) Save(acct Account) error {
	s.write.Lock()
	defer s.write.Unlock()
	return s.save(acct)
}

// save is [Store.Save] without the lock, for the one caller that is already holding it.
//
// Split rather than made re-entrant, because a Go mutex is not: [Store.Ensure] calling
// the exported method would deadlock on its own lock. The split makes that
// unrepresentable instead of something each future caller has to remember.
func (s *Store) save(acct Account) error {
	data, err := encodeAccount(acct)
	if err != nil {
		return err
	}
	return world.WriteAtomic(s.accountPath(acct.Identity), data)
}

// Ensure answers with the account this provider identity already has, or mints one.
//
// The bool reports whether an account was created, so a caller can tell a first sign-in
// from a returning one without comparing timestamps.
//
// **An unreadable record stops here, and does not become a new account.** That is the
// same refusal [Store.Load] makes, restated at the level where it costs something: the
// tempting thing for a service to do with a file it cannot read is to carry on and
// make a fresh one, and the person that happens to loses everything the old account
// owned. A damaged file is a call for a human, not a reason to mint.
//
// now is a parameter rather than a call to time.Now, so that what a test writes down
// is what a test reads back. It is truncated to the second the format keeps, so the
// account returned here and the account a later Load produces are the same value.
//
// displayName is truncated to what the format keeps, for the same reason: the account
// this returns is the account that is now on disk, not a description of one.
func (s *Store) Ensure(id ProviderIdentity, displayName string, now time.Time) (Account, bool, error) {
	// Before the lock rather than inside it: an identity that is not one is the
	// caller's own mistake, and answering it needs no exclusion and no disk.
	if err := id.Validate(); err != nil {
		return Account{}, false, err
	}

	s.write.Lock()
	defer s.write.Unlock()

	existing, found, err := s.Load(id)
	if err != nil {
		return Account{}, false, err
	}
	if found {
		// Returned exactly as stored. Whether a display name that has changed at the
		// provider should be written through is a decision about what this service
		// does with a sign-in, and it belongs to the flow that has the sign-in.
		return existing, false, nil
	}

	accountID, err := NewAccountID()
	if err != nil {
		return Account{}, false, err
	}
	acct := Account{
		ID:          accountID,
		Identity:    id,
		DisplayName: truncateName(displayName),
		CreatedAt:   now.UTC().Truncate(time.Second),
	}
	if err := s.save(acct); err != nil {
		return Account{}, false, err
	}
	return acct, true, nil
}

// accountPath is where one identity's account lives. The hex digest is the whole
// name: fixed length, and every character comes from a hash, so nothing a provider
// sends reaches the filesystem.
func (s *Store) accountPath(id ProviderIdentity) string {
	return filepath.Join(s.dir, accountKey(id)+accountFileExt)
}

// accountKey is the file name an identity resolves to: the SHA-256 of the provider
// name, a NUL, and the subject, in lowercase hex.
//
// **The NUL is load-bearing.** Concatenating the two strings without a separator
// would let ("disc", "ordX") and ("discord", "X") hash to one file, which is two
// people sharing an account. A provider name is lowercase letters, digits and hyphens
// by [ProviderIdentity.Validate], so it can never contain a NUL itself: the first NUL
// in the digest's input is therefore always the separator, and the mapping from an
// identity to a key is one-to-one.
//
// The hash is for a filename that is safe and fixed-length, not for secrecy — a
// provider subject is not a secret, and anybody who can read this directory can read
// the subject out of the record inside. What it does buy is that the *directory
// listing* is not a roster of provider ids.
func accountKey(id ProviderIdentity) string {
	sum := sha256.Sum256(append(append([]byte(id.Provider), 0), id.Subject...))
	return hex.EncodeToString(sum[:])
}

// encodeAccount lays one account out, refusing anything the format cannot describe.
func encodeAccount(acct Account) ([]byte, error) {
	if acct.ID.IsZero() {
		// The one id a mint cannot produce. Written down, it would make every account
		// that reached this line the same account.
		return nil, errors.New("auth: an account has no id; it was built rather than minted")
	}
	if err := acct.Identity.Validate(); err != nil {
		return nil, err
	}

	provider, subject := acct.Identity.Provider, acct.Identity.Subject
	name := truncateName(acct.DisplayName)

	buf := world.NewRecord(recordHeaderSize, len(provider)+len(subject)+len(name),
		accountMagic, StoreVersion)
	copy(buf[offAccountID:offAccountID+AccountIDSize], acct.ID[:])
	// Seconds, in UTC, because an account's age is read by a person rather than by
	// anything needing sub-second resolution — and because a whole second round-trips
	// through Unix time unambiguously.
	binary.LittleEndian.PutUint64(buf[offCreatedAt:offCreatedAt+8], uint64(acct.CreatedAt.UTC().Unix()))

	binary.LittleEndian.PutUint16(buf[offProviderLen:offProviderLen+2], uint16(len(provider)))
	binary.LittleEndian.PutUint16(buf[offSubjectLen:offSubjectLen+2], uint16(len(subject)))
	binary.LittleEndian.PutUint16(buf[offNameLen:offNameLen+2], uint16(len(name)))

	at := recordHeaderSize
	at += copy(buf[at:], provider)
	at += copy(buf[at:], subject)
	copy(buf[at:], name)

	world.PutChecksum(buf)
	return buf, nil
}

// decodeAccount parses one account record, refusing anything it cannot read exactly.
//
// Validate-everything-then-return, the shape world.decodeChunkFile and
// persist.decodeRecord both use: nothing is assembled until every check has passed, so
// a half-valid account is never a value a caller can hold.
func decodeAccount(data []byte) (Account, error) {
	if len(data) < recordHeaderSize+world.ChecksumSize {
		return Account{}, fmt.Errorf("%w: %d bytes is shorter than an empty account record",
			world.ErrCorruptStore, len(data))
	}
	if err := world.CheckHeader(data, accountMagic, StoreVersion); err != nil {
		return Account{}, err
	}
	if err := world.CheckChecksum(data); err != nil {
		return Account{}, err
	}

	// The declared lengths are checked against the length the file actually has before
	// anything indexes into it. A truncated record fails here, which is the case this
	// check exists for: a shorter name is a perfectly plausible one.
	providerLen := uint64(binary.LittleEndian.Uint16(data[offProviderLen : offProviderLen+2]))
	subjectLen := uint64(binary.LittleEndian.Uint16(data[offSubjectLen : offSubjectLen+2]))
	nameLen := uint64(binary.LittleEndian.Uint16(data[offNameLen : offNameLen+2]))
	want := uint64(recordHeaderSize) + providerLen + subjectLen + nameLen + world.ChecksumSize
	if want != uint64(len(data)) {
		return Account{}, fmt.Errorf("%w: the record claims %d, %d and %d bytes of text, needing %d bytes in all, but the file is %d",
			world.ErrCorruptStore, providerLen, subjectLen, nameLen, want, len(data))
	}

	at := uint64(recordHeaderSize)
	acct := Account{
		ID: AccountID(data[offAccountID : offAccountID+AccountIDSize]),
		Identity: ProviderIdentity{
			Provider: string(data[at : at+providerLen]),
			Subject:  string(data[at+providerLen : at+providerLen+subjectLen]),
		},
		DisplayName: string(data[at+providerLen+subjectLen : at+providerLen+subjectLen+nameLen]),
		CreatedAt:   time.Unix(int64(binary.LittleEndian.Uint64(data[offCreatedAt:offCreatedAt+8])), 0).UTC(),
	}

	// The keys, and only the keys. A record whose identity this build would refuse to
	// write is one this build must refuse to read, or the two halves of the format
	// disagree about what an account is — see the package comment on keys and
	// description, and note that the created-at time is deliberately not judged here.
	if acct.ID.IsZero() {
		return Account{}, fmt.Errorf("%w: the record holds no account id", world.ErrCorruptStore)
	}
	if err := acct.Identity.Validate(); err != nil {
		// The identity itself is not repeated into the message; Validate's own text
		// states the rule that was broken and no value.
		return Account{}, fmt.Errorf("%w: the record does not name a provider identity this build would write: %w",
			world.ErrCorruptStore, err)
	}
	return acct, nil
}
