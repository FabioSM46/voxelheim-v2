package registry

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// StoreVersion is the on-disk format version of a registered server's record.
//
// Bump it for any change to the layout below, including one that only adds a field: a
// reader of an older build must refuse a newer record rather than parse a prefix of it and
// guess at the rest. Deliberately separate from `auth.StoreVersion`, `ticket.KeyStoreVersion`
// and every version internal/persist keeps — a registered server, an account, a key pair
// and a chunk delta change for entirely unrelated reasons.
//
// **Bumping it is the cheapest of those, and it is worth saying why.** A record here holds
// nothing that cannot be reconstructed: every server re-announces on its own interval, so a
// version this build refuses costs one announce interval of a shorter list rather than
// anybody's data. That is not licence to bump it carelessly; it is the reason this store,
// alone among them, can.
const StoreVersion uint32 = 1

// On-disk layout, little-endian throughout, one file per registered server.
//
//	servers/<name-key-hex>.bin
//	    magic[4] version:u32
//	    last_seen:i64
//	    name_len:u16 display_len:u16 address_len:u16 fingerprint_len:u16
//	    name[name_len] display[display_len] address[address_len] fingerprint[fingerprint_len]
//	    crc32:u32
//
// Everything fixed-width first and every variable-length field last, so the decoder reasons
// about exactly one equation: the header, plus the four declared lengths, plus the CRC, must
// be the file's actual size. A truncated file fails that equality rather than being read as a
// server with a shorter name, and so does a padded one. internal/auth's record has the same
// shape with three lengths, and this is deliberately not a fourth reinvention of it.
//
// The fingerprint is length-declared rather than fixed at [FingerprintHexLen], even though it
// is never anything else. One equation covering four fields is a decoder with no special case
// in it, and the two bytes that costs are two bytes.
//
// **The file name is a digest of the server name, not the name.** The name is this service's
// own vocabulary — lowercase letters, digits and hyphens — so it would in fact be a safe path
// today, and the reason it is hashed anyway is that "safe path" is a property of the
// vocabulary rather than of the code: widening it by one character later is a one-line change
// that nothing here would notice, and `con` is already a file nobody can create on Windows. A
// digest is fixed-length hex whatever the vocabulary becomes. The cost is real and is worth
// naming — `ls servers/` is no longer a list of who is registered — and the answer to it is
// the list endpoint, which is the supported way to read this store.
const (
	serversDirName = "servers"
	serverFileExt  = ".bin"

	offLastSeen   = world.HeaderSize
	offNameLen    = offLastSeen + 8
	offDisplayLen = offNameLen + 2
	offAddressLen = offDisplayLen + 2
	offFPLen      = offAddressLen + 2

	recordHeaderSize = offFPLen + 2
	maxRecordSize    = recordHeaderSize + MaxNameBytes + MaxDisplayNameBytes +
		MaxAddressBytes + FingerprintHexLen + world.ChecksumSize
)

// serverMagic is this store's own four bytes: 'R' for registry, beside internal/world's 'W'
// and 'D', internal/persist's 'P', 'S' and 'C', internal/auth's 'A' and internal/ticket's 'K'
// and 'V'. Distinct so that a file of one kind can never be read as another even when the two
// happen to be the same size.
var serverMagic = [4]byte{'V', 'X', 'H', 'R'}

// nameKeyDomain separates this digest from every other use of SHA-256 in this repository, so
// that a file name can never coincide with a value computed for another purpose over the same
// text. The NUL is the separator `ticket.worldIDDomain` and `auth.accountKey` both use, and
// for the same reason: a server name cannot contain one, so the prefix can never be confused
// with the name.
const nameKeyDomain = "voxelheim/registry/name/v1\x00"

// Store is one account service's registry directory.
//
// **There is no nil Store**, the deliberate difference from every store under the world
// directory and the rule internal/auth already keeps: an ephemeral registry is one that
// forgets every server on restart, and the list would then be empty until every operator's
// announce interval had come round. [OpenStore] refuses an unnamed directory instead.
//
// Safe for concurrent use, which is not incidental: this sits behind HTTP handlers, and a
// registration arriving while somebody reads the list is the ordinary case rather than the
// exotic one. [Store.List] opens exact paths and world.WriteAtomic renames onto them, so a
// reader sees the whole of the old file or the whole of the new one and needs no exclusion.
// [Store.Register] takes one lock, because it is the pair of a read and a write.
type Store struct {
	dir string

	// write serialises every registration against every other one, and against the read
	// each of them decides `created` from.
	//
	// The lock is not protecting the record — world.WriteAtomic is what makes a replacement
	// all-or-nothing, and two announcements for one server both landing would be fine in
	// whichever order they arrived. What it protects is the answer: without it, two first
	// announcements for one name both find no file and both report `created`, and the log
	// then says a server was registered for the first time twice.
	//
	// **What this does not cover is two processes**, because a mutex cannot. One account
	// service owns its registry directory; running a second against the same directory is a
	// deployment mistake, and the fix for it is a lock in the filesystem rather than a wider
	// one here — internal/auth says the same about the accounts.
	write sync.Mutex
}

// OpenStore opens the registry directory under authDir, creating it if it is not there.
//
// Called before the listener is bound, in the order `auth.OpenStore` and
// `ticket.LoadOrCreate` already are: a service that is going to fail on its storage should
// fail before it has bound a port and told the world it is up.
func OpenStore(authDir string) (*Store, error) {
	if authDir == "" {
		return nil, errors.New("registry: the registry directory must be named")
	}

	dir := filepath.Join(authDir, serversDirName)
	// 0700, as the accounts are. A registered server's address is somebody's home
	// connection, which is the reason the list is behind a credential; a directory anybody
	// on the machine can read would be the same disclosure one layer down.
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, fmt.Errorf("registry: creating %s: %w", dir, err)
	}

	// Whatever a crash left mid-rename. Inert, because a reader only ever opens an exact
	// <key>.bin path and a temporary name never is one — so this is housekeeping rather than
	// correctness, the same sweep every store writing through world.WriteAtomic inherits.
	//
	// The pattern bounds it to temporaries of this store's own records. As in
	// internal/auth, this is the servers directory created here rather than the
	// operator's `-auth-dir` above it (#137).
	world.SweepTemporaries(dir, "*"+serverFileExt)
	return &Store{dir: dir}, nil
}

// Dir is the registry directory this store writes to.
func (s *Store) Dir() string { return s.dir }

// Register records a server, replacing whatever the name held before, and reports whether
// this was the first time that name was seen.
//
// **Replacing rather than merging is the criterion this whole package exists for.** The
// address the list serves is the one the server last announced, so a home connection that
// changes address overnight is invisible to players. There is nothing in a record worth
// keeping across a registration: every field of it came from the announcement.
//
// `srv.LastSeen` is the caller's — internal/auth's rule for internal/auth's reason, and here
// it is also what lets a test hold a server that went quiet an hour ago without waiting one.
//
// **A record that exists and cannot be read is replaced rather than refused**, which is the
// opposite of what `auth.Store.Ensure` does and the opposite for a reason worth reading.
// There, an unreadable record reported as absent mints a *second* account and the person
// loses everything the first one owned. Here the announcer holds every field of the record
// and is in the middle of restating all of them, so overwriting a damaged file loses exactly
// nothing and is the only way the store ever heals itself. [Store.List] is where a damaged
// file is still an error, because that is the caller that cannot repair it.
func (s *Store) Register(srv Server) (created bool, err error) {
	// Before the lock rather than inside it: a registration that is not one is the caller's
	// own mistake, and answering it needs no exclusion and no disk.
	if err := srv.Validate(); err != nil {
		return false, err
	}
	// To the second, because that is the resolution the format keeps: the record this writes
	// is the record a later List reads back, not a description of one.
	srv.LastSeen = srv.LastSeen.UTC().Truncate(time.Second)

	data, err := encodeServer(srv)
	if err != nil {
		return false, err
	}

	s.write.Lock()
	defer s.write.Unlock()

	path := s.serverPath(srv.Name)
	_, statErr := os.Stat(path)
	switch {
	case statErr == nil:
		created = false
	case errors.Is(statErr, fs.ErrNotExist):
		created = true
	default:
		// A path this service cannot even stat is a storage problem, and reporting it as a
		// first registration would put a wrong line in the log about the one event an
		// operator watches for.
		return false, fmt.Errorf("registry: reading %s: %w", path, statErr)
	}

	if err := world.WriteAtomic(path, data); err != nil {
		return false, fmt.Errorf("registry: writing %s: %w", path, err)
	}
	return created, nil
}

// List is every registered server, ordered by name.
//
// Ordered rather than however the directory happened to be read: a list that reshuffles
// between two requests is a list a player loses their place in, and directory order is not
// something any filesystem promises anyway.
//
// **A record that cannot be read fails the whole call**, and it is worth being explicit
// about why that is the right answer here rather than the tempting one of skipping it. A
// skipped server is a server that has silently vanished from the list — the player sees a
// shorter list, concludes that server is gone, and nobody is told anything. That is the shape
// of failure this repository spends its effort refusing. Failing loudly costs the list until
// somebody looks, and the somebody usually does not have to be a person: the next
// announcement from that server replaces the damaged file, because [Store.Register] repairs
// what this reports.
func (s *Store) List() ([]Server, error) {
	entries, err := os.ReadDir(s.dir)
	if err != nil {
		return nil, fmt.Errorf("registry: reading %s: %w", s.dir, err)
	}

	servers := make([]Server, 0, len(entries))
	for _, entry := range entries {
		name := entry.Name()
		// Exactly the files this store writes. A directory is not one, and neither is a
		// temporary left by a crash mid-rename — world.WriteAtomic names those
		// `<file>.tmp<random>`, so the extension is what tells them apart.
		if entry.IsDir() || !strings.HasSuffix(name, serverFileExt) {
			continue
		}

		srv, err := s.load(filepath.Join(s.dir, name))
		if err != nil {
			return nil, err
		}
		// The record says which name it belongs to, so a file copied or renamed onto
		// another server's path is caught rather than served as that server —
		// internal/world writes a chunk's coordinate into its file and internal/auth writes
		// the provider identity into its record for exactly this reason. Here it matters
		// more than usual: the fields a misplaced record carries are the address a client
		// dials and the certificate it is told to expect.
		if got := s.serverPath(srv.Name); got != filepath.Join(s.dir, name) {
			return nil, fmt.Errorf("%w: %s holds the record for a differently named server",
				world.ErrCorruptStore, filepath.Join(s.dir, name))
		}
		servers = append(servers, srv)
	}

	// No two records can share a name — one file per name, and the misplaced-file check
	// above is what guarantees each record's name maps to its own path — so there are no
	// ties for an unstable sort to reorder.
	slices.SortFunc(servers, func(a, b Server) int { return strings.Compare(a.Name, b.Name) })
	return servers, nil
}

// load reads one record off disk.
func (s *Store) load(path string) (Server, error) {
	info, err := os.Stat(path)
	if err != nil {
		return Server{}, fmt.Errorf("registry: reading %s: %w", path, err)
	}
	// Before the read, not after: a file this large is not one this format wrote, and finding
	// that out by allocating it is how a corrupt directory becomes an out-of-memory.
	if info.Size() > int64(maxRecordSize) {
		return Server{}, fmt.Errorf("%w: %s is %d bytes, more than the %d a server record can need",
			world.ErrCorruptStore, path, info.Size(), maxRecordSize)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		return Server{}, fmt.Errorf("registry: reading %s: %w", path, err)
	}

	srv, err := decodeServer(data)
	if err != nil {
		return Server{}, fmt.Errorf("%s: %w", path, err)
	}
	return srv, nil
}

// serverPath is where one server's record lives. The hex digest is the whole name: fixed
// length, and every character comes from a hash.
func (s *Store) serverPath(name string) string {
	return filepath.Join(s.dir, nameKey(name)+serverFileExt)
}

// nameKey is the file name a server name resolves to: the SHA-256 of a domain prefix, a NUL
// and the name, in lowercase hex.
func nameKey(name string) string {
	sum := sha256.Sum256(append([]byte(nameKeyDomain), name...))
	return hex.EncodeToString(sum[:])
}

// encodeServer lays one record out, refusing anything the format cannot describe.
func encodeServer(srv Server) ([]byte, error) {
	// The same rule at the write as at the read, which is the halves-of-a-format discipline
	// internal/auth and internal/ticket both keep: writing a file this build would then
	// refuse is the single failure that looks like a success until a restart.
	if err := srv.Validate(); err != nil {
		return nil, err
	}

	buf := make([]byte, recordHeaderSize+len(srv.Name)+len(srv.DisplayName)+
		len(srv.Address)+len(srv.Fingerprint)+world.ChecksumSize)
	copy(buf[0:4], serverMagic[:])
	binary.LittleEndian.PutUint32(buf[4:world.HeaderSize], StoreVersion)
	// Seconds, in UTC, because that is the resolution [OfflineAfter] is compared at and
	// because a whole second round-trips through Unix time unambiguously.
	binary.LittleEndian.PutUint64(buf[offLastSeen:offLastSeen+8], uint64(srv.LastSeen.UTC().Unix()))

	binary.LittleEndian.PutUint16(buf[offNameLen:offNameLen+2], uint16(len(srv.Name)))
	binary.LittleEndian.PutUint16(buf[offDisplayLen:offDisplayLen+2], uint16(len(srv.DisplayName)))
	binary.LittleEndian.PutUint16(buf[offAddressLen:offAddressLen+2], uint16(len(srv.Address)))
	binary.LittleEndian.PutUint16(buf[offFPLen:offFPLen+2], uint16(len(srv.Fingerprint)))

	at := recordHeaderSize
	at += copy(buf[at:], srv.Name)
	at += copy(buf[at:], srv.DisplayName)
	at += copy(buf[at:], srv.Address)
	copy(buf[at:], srv.Fingerprint)

	world.PutChecksum(buf)
	return buf, nil
}

// decodeServer parses one record, refusing anything it cannot read exactly.
//
// Validate-everything-then-return, the shape world.decodeChunkFile, persist.decodeRecord and
// auth.decodeAccount all use: nothing is assembled until every check has passed, so a
// half-valid server is never a value a caller can hold — and in this store a half-valid one
// would be an address a client dials.
func decodeServer(data []byte) (Server, error) {
	if len(data) < recordHeaderSize+world.ChecksumSize {
		return Server{}, fmt.Errorf("%w: %d bytes is shorter than an empty server record",
			world.ErrCorruptStore, len(data))
	}
	if err := world.CheckHeader(data, serverMagic, StoreVersion); err != nil {
		return Server{}, err
	}
	if err := world.CheckChecksum(data); err != nil {
		return Server{}, err
	}

	// The declared lengths are checked against the length the file actually has before
	// anything indexes into it. A truncated record fails here, which is the case this check
	// exists for: a shorter address is a perfectly plausible one.
	nameLen := uint64(binary.LittleEndian.Uint16(data[offNameLen : offNameLen+2]))
	displayLen := uint64(binary.LittleEndian.Uint16(data[offDisplayLen : offDisplayLen+2]))
	addressLen := uint64(binary.LittleEndian.Uint16(data[offAddressLen : offAddressLen+2]))
	fpLen := uint64(binary.LittleEndian.Uint16(data[offFPLen : offFPLen+2]))
	want := uint64(recordHeaderSize) + nameLen + displayLen + addressLen + fpLen + world.ChecksumSize
	if want != uint64(len(data)) {
		return Server{}, fmt.Errorf("%w: the record claims %d, %d, %d and %d bytes of text, needing %d bytes in all, but the file is %d",
			world.ErrCorruptStore, nameLen, displayLen, addressLen, fpLen, want, len(data))
	}

	at := uint64(recordHeaderSize)
	srv := Server{
		Name:        string(data[at : at+nameLen]),
		DisplayName: string(data[at+nameLen : at+nameLen+displayLen]),
		Address:     string(data[at+nameLen+displayLen : at+nameLen+displayLen+addressLen]),
		Fingerprint: string(data[at+nameLen+displayLen+addressLen : at+nameLen+displayLen+addressLen+fpLen]),
		LastSeen:    time.Unix(int64(binary.LittleEndian.Uint64(data[offLastSeen:offLastSeen+8])), 0).UTC(),
	}

	// A record this build would refuse to write is one this build must refuse to read, or
	// the two halves of the format disagree about what a registered server is.
	if err := srv.Validate(); err != nil {
		return Server{}, fmt.Errorf("%w: the record does not describe a server this build would write: %w",
			world.ErrCorruptStore, err)
	}
	return srv, nil
}
