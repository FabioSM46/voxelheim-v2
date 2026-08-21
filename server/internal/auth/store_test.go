// Internal tests: the encoder, the on-disk layout and every way a file can be refused
// are what is being pinned, and a damaged file is produced by writing bytes rather
// than by reaching into the encoder — so what is checked is what a reader on another
// build would actually find on the disk.
package auth

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// created is a fixed moment, because the format keeps whole seconds and a test that
// wrote time.Now() and compared it whole would fail on the nanoseconds the format
// deliberately does not store.
var created = time.Unix(1_700_000_000, 0).UTC()

// testIdentity is a distinct synthetic provider identity per seed, derived rather than
// invented so that a failing test names the same file on every run. The subject is a
// digit string in the shape a real provider issues and belongs to nobody.
func testIdentity(seed byte) ProviderIdentity {
	return ProviderIdentity{Provider: "discord", Subject: fmt.Sprintf("90000000000000%03d", seed)}
}

// testAccountID is derived rather than minted, for the same reason. It can never be
// the zero id: the bytes vary with their index whatever the seed is.
func testAccountID(seed byte) AccountID {
	var id AccountID
	for i := range id {
		id[i] = seed*31 + byte(i)
	}
	return id
}

func testAccount(seed byte, name string) Account {
	return Account{
		ID:          testAccountID(seed),
		Identity:    testIdentity(seed),
		DisplayName: name,
		CreatedAt:   created,
	}
}

func openStore(t *testing.T) (*Store, string) {
	t.Helper()

	authDir := t.TempDir()
	store, err := OpenStore(authDir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	return store, authDir
}

func TestStoreRoundTripsAnAccount(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	want := testAccount(1, "Eivor")

	if err := store.Save(want); err != nil {
		t.Fatalf("Save: %v", err)
	}

	got, found, err := store.Load(want.Identity)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("the account just written was not found")
	}
	if got.ID != want.ID {
		t.Errorf("account id round-tripped as %s, want %s", got.ID, want.ID)
	}
	if got.Identity != want.Identity {
		t.Errorf("identity round-tripped as %+v, want %+v", got.Identity, want.Identity)
	}
	if got.DisplayName != want.DisplayName {
		t.Errorf("display name round-tripped as %q, want %q", got.DisplayName, want.DisplayName)
	}
	if !got.CreatedAt.Equal(want.CreatedAt) {
		t.Errorf("created-at round-tripped as %s, want %s", got.CreatedAt, want.CreatedAt)
	}
}

// An identity nobody has signed in with has no file, and that is not an error — it is
// the answer that lets a first sign-in mint. The distinction from an unreadable file
// is the whole subject of the refusal tests below.
func TestStoreReportsAnUnknownIdentityAsAbsent(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)

	got, found, err := store.Load(testIdentity(2))
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if found {
		t.Error("an identity that was never saved was found")
	}
	if got != (Account{}) {
		t.Errorf("an account came back for an unknown identity: %+v", got)
	}
}

// Every shape of account record this build will not read, and the rule is the one the
// world's own stores keep: refused whole, never repaired, never partly believed, and
// never reported as an absence.
//
// **An absence is the one answer that costs somebody their account.** Told "no such
// account", the flow above this mints a second one for a person who already has one:
// they sign in successfully, find none of their characters, and the new account's
// first write lands on the record nobody could read.
func TestStoreRefusesARecordItCannotReadExactly(t *testing.T) {
	t.Parallel()

	id := testIdentity(3)
	sound, err := encodeAccount(testAccount(3, "Sigrun"))
	if err != nil {
		t.Fatalf("encodeAccount: %v", err)
	}

	damage := map[string]func([]byte) []byte{
		"a wrong magic number": func(b []byte) []byte {
			out := bytes.Clone(b)
			out[0] = 'X'
			world.PutChecksum(out)
			return out
		},
		// Another store's file, dropped into this directory. The magic is what stops a
		// player record being read as an account that happens to be the same length.
		"another store's magic": func(b []byte) []byte {
			out := bytes.Clone(b)
			copy(out[0:4], []byte{'V', 'X', 'H', 'P'})
			world.PutChecksum(out)
			return out
		},
		// A well-formed record of a version this build does not know. Refused rather
		// than read as the layout it happens to resemble.
		"a version this build does not speak": func(b []byte) []byte {
			out := bytes.Clone(b)
			out[4] = byte(StoreVersion) + 1
			world.PutChecksum(out)
			return out
		},
		// A flipped byte inside a record whose shape is still perfectly valid —
		// exactly what a length check cannot catch, and the whole reason the CRC is
		// there. The checksum is deliberately not recomputed.
		"a flipped byte under the checksum": func(b []byte) []byte {
			out := bytes.Clone(b)
			out[recordHeaderSize] ^= 0xFF
			return out
		},
		"a flipped byte in the account id": func(b []byte) []byte {
			out := bytes.Clone(b)
			out[offAccountID] ^= 0xFF
			return out
		},
		"a truncated file":          func(b []byte) []byte { return bytes.Clone(b[:len(b)-3]) },
		"a file cut to its header":  func(b []byte) []byte { return bytes.Clone(b[:recordHeaderSize]) },
		"a file shorter than magic": func([]byte) []byte { return []byte{'V', 'X'} },
		"an empty file":             func([]byte) []byte { return nil },
		"a longer file": func(b []byte) []byte {
			return append(bytes.Clone(b), 0)
		},
		"a name length that disagrees with the file": func(b []byte) []byte {
			out := bytes.Clone(b)
			out[offNameLen] = 200
			world.PutChecksum(out)
			return out
		},
		"a subject length that disagrees with the file": func(b []byte) []byte {
			out := bytes.Clone(b)
			out[offSubjectLen] = 200
			world.PutChecksum(out)
			return out
		},
		// The three lengths still add up, but the text has been re-cut between the
		// fields: the subject has eaten a byte of the name. Caught because the identity
		// that comes out is no longer the one the file is named for.
		"lengths that add up but describe another identity": func(b []byte) []byte {
			out := bytes.Clone(b)
			out[offSubjectLen]++
			out[offNameLen]--
			world.PutChecksum(out)
			return out
		},
		// The one id a mint can never produce. Written down it would make every record
		// that reached this state the same account.
		"an account id of nothing but zeroes": func(b []byte) []byte {
			out := bytes.Clone(b)
			for i := offAccountID; i < offAccountID+AccountIDSize; i++ {
				out[i] = 0
			}
			world.PutChecksum(out)
			return out
		},
		// A record naming a provider this build would refuse to write. Refusing to read
		// it too is what keeps the two halves of the format describing one thing.
		"a provider name this build would not write": func(b []byte) []byte {
			out := bytes.Clone(b)
			out[recordHeaderSize] = 'D'
			world.PutChecksum(out)
			return out
		},
	}

	for name, break_ := range damage {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			store, _ := openStore(t)
			path := store.accountPath(id)
			broken := break_(sound)
			if err := os.WriteFile(path, broken, 0o600); err != nil {
				t.Fatalf("writing the damaged record: %v", err)
			}

			got, found, err := store.Load(id)
			if !errors.Is(err, world.ErrCorruptStore) {
				t.Fatalf("Load = %v, want ErrCorruptStore", err)
			}
			// The other half of "refused whole": nothing is handed back for a caller to
			// half-believe, and it is not reported as an unknown identity either.
			if found || got != (Account{}) {
				t.Errorf("a corrupt record was reported as found=%v: %+v", found, got)
			}
			// **And the file is kept, byte for byte.** Reading it is what failed;
			// nothing about that is a licence to destroy the evidence.
			kept, err := os.ReadFile(path)
			if err != nil {
				t.Fatalf("the refused record did not survive the read: %v", err)
			}
			if !bytes.Equal(kept, broken) {
				t.Error("the refused record was rewritten by the read that refused it")
			}
		})
	}
}

// The size is checked before the file is read, not after: finding out that a file is
// too large by allocating it is how a corrupt directory becomes an out-of-memory.
//
// The assertion is on the message because that is what distinguishes the two refusals.
// Every check below the size test would also reject this file, so a passing
// errors.Is would prove nothing about the ordering; only the size test names the
// number of bytes the file has against the number a record can need.
func TestStoreRefusesAFileTooLargeToBeAnAccountBeforeReadingIt(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	id := testIdentity(4)

	if err := os.WriteFile(store.accountPath(id), make([]byte, maxRecordSize+1), 0o600); err != nil {
		t.Fatalf("writing the oversized record: %v", err)
	}

	_, found, err := store.Load(id)
	if !errors.Is(err, world.ErrCorruptStore) {
		t.Fatalf("Load = %v, want ErrCorruptStore", err)
	}
	if found {
		t.Error("an oversized record was reported as found")
	}
	if want := "an account record can need"; !strings.Contains(err.Error(), want) {
		t.Errorf("Load failed with %q, which is not the size check that must run first (looking for %q)", err, want)
	}
}

// A record moved or copied onto another identity's path is caught rather than answered
// as that identity's account. internal/world writes a chunk's coordinate into its file
// for the same reason; internal/persist deliberately cannot, because its file name is
// the hash of a secret and there is nothing in the record to compare against.
func TestARecordOnAnotherIdentitysPathIsRefused(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	mine, theirs := testIdentity(5), testIdentity(6)

	if err := store.Save(testAccount(5, "Eivor")); err != nil {
		t.Fatalf("Save: %v", err)
	}
	sound, err := os.ReadFile(store.accountPath(mine))
	if err != nil {
		t.Fatalf("reading the sound record: %v", err)
	}
	if err := os.WriteFile(store.accountPath(theirs), sound, 0o600); err != nil {
		t.Fatalf("copying the record onto another identity's path: %v", err)
	}

	got, found, err := store.Load(theirs)
	if !errors.Is(err, world.ErrCorruptStore) {
		t.Fatalf("Load = %v, want ErrCorruptStore", err)
	}
	if found || got != (Account{}) {
		t.Errorf("a misplaced record was answered as %+v", got)
	}
	// The error names the file and neither identity: an error string reaches a log,
	// and both of these are somebody's provider id.
	for _, secret := range []string{mine.Subject, theirs.Subject} {
		if strings.Contains(err.Error(), secret) {
			t.Error("the refusal wrote a provider subject into its error message")
		}
	}
}

// Ensure mints exactly once for an identity and recalls it forever after — which is
// the sentence the whole store exists to make true.
func TestEnsureMintsOnceAndThenRecalls(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	id := testIdentity(7)

	first, minted, err := store.Ensure(id, "Halvar", created)
	if err != nil {
		t.Fatalf("first Ensure: %v", err)
	}
	if !minted {
		t.Error("the first Ensure for an identity did not report a new account")
	}
	if first.ID.IsZero() {
		t.Error("the minted account has no id")
	}
	if !first.CreatedAt.Equal(created) {
		t.Errorf("created-at is %s, want the moment it was handed %s", first.CreatedAt, created)
	}

	// A later sign-in, with a different name and at a different time. Neither may
	// produce a second account.
	again, minted, err := store.Ensure(id, "Halvar the Elder", created.Add(72*time.Hour))
	if err != nil {
		t.Fatalf("second Ensure: %v", err)
	}
	if minted {
		t.Error("a returning identity was reported as a new account")
	}
	if again.ID != first.ID {
		t.Errorf("a returning identity got account %s, want the original %s", again.ID, first.ID)
	}
	if !again.CreatedAt.Equal(first.CreatedAt) {
		t.Errorf("a returning identity's created-at moved to %s", again.CreatedAt)
	}

	// A different identity is a different person, however similar the rest.
	other, minted, err := store.Ensure(testIdentity(8), "Halvar", created)
	if err != nil {
		t.Fatalf("Ensure for a second identity: %v", err)
	}
	if !minted || other.ID == first.ID {
		t.Error("two provider identities were answered with one account")
	}
}

// The headline refusal, at the level where it costs something. A damaged record must
// stop Ensure, and it must not become a fresh account written over the damaged one.
func TestEnsureRefusesToMintOverADamagedRecord(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	id := testIdentity(9)

	sound, err := encodeAccount(testAccount(9, "Sigrun"))
	if err != nil {
		t.Fatalf("encodeAccount: %v", err)
	}
	broken := bytes.Clone(sound)
	broken[recordHeaderSize+2] ^= 0xFF // a flipped byte the CRC catches
	path := store.accountPath(id)
	if err := os.WriteFile(path, broken, 0o600); err != nil {
		t.Fatalf("writing the damaged record: %v", err)
	}

	got, minted, err := store.Ensure(id, "Sigrun", created)
	if !errors.Is(err, world.ErrCorruptStore) {
		t.Fatalf("Ensure = %v, want ErrCorruptStore", err)
	}
	if minted || got != (Account{}) {
		t.Fatalf("Ensure minted over a damaged record: %+v", got)
	}

	kept, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("the damaged record did not survive Ensure: %v", err)
	}
	if !bytes.Equal(kept, broken) {
		t.Error("Ensure wrote over the damaged record; the only evidence of that account is gone")
	}
}

// Two requests for one person arriving together is the ordinary case for an HTTP
// service, not the exotic one. Unguarded, both find no account and both mint: the
// second write lands on the first, and whichever account id the loser handed back
// names nothing.
func TestEnsureMintsOneAccountUnderConcurrentCallers(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	id := testIdentity(10)

	const callers = 8
	var (
		wg      sync.WaitGroup
		mu      sync.Mutex
		ids     = map[AccountID]int{}
		mints   int
		failure error
	)
	for range callers {
		wg.Add(1)
		go func() {
			defer wg.Done()
			acct, minted, err := store.Ensure(id, "Eivor", created)

			mu.Lock()
			defer mu.Unlock()
			if err != nil {
				failure = err
				return
			}
			ids[acct.ID]++
			if minted {
				mints++
			}
		}()
	}
	wg.Wait()

	if failure != nil {
		t.Fatalf("Ensure failed under concurrent callers: %v", failure)
	}
	if len(ids) != 1 {
		t.Errorf("%d callers were handed %d different accounts, want 1", callers, len(ids))
	}
	if mints != 1 {
		t.Errorf("%d callers minted %d accounts, want exactly 1", callers, mints)
	}
}

// This store never writes a file it would then refuse to read: the refusal is at the
// write, not only at the next read, because that is the one failure that looks like a
// success until a restart.
func TestSaveRefusesAnAccountItCouldNotReadBack(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)

	unwritable := map[string]Account{
		"an account that was built rather than minted": {Identity: testIdentity(11), CreatedAt: created},
		"no provider":                          {ID: testAccountID(11), Identity: ProviderIdentity{Subject: "9000"}, CreatedAt: created},
		"no subject":                           {ID: testAccountID(11), Identity: ProviderIdentity{Provider: "discord"}, CreatedAt: created},
		"a provider name with a capital in it": {ID: testAccountID(11), Identity: ProviderIdentity{Provider: "Discord", Subject: "9000"}, CreatedAt: created},
		"a subject past the cap": {ID: testAccountID(11),
			Identity: ProviderIdentity{Provider: "discord", Subject: strings.Repeat("9", MaxSubjectBytes+1)}, CreatedAt: created},
	}

	for name, acct := range unwritable {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			if err := store.Save(acct); err == nil {
				t.Fatal("Save accepted an account this build could not read back")
			}
		})
	}
}

// A display name is description rather than a key, so it is truncated to what the
// format keeps and never refused — and the cut is at a rune boundary, because a cut
// through the middle of a multi-byte rune stores text that no longer decodes.
func TestSaveTruncatesADisplayNameAtARuneBoundary(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	acct := testAccount(12, strings.Repeat("á", MaxDisplayNameBytes)) // two bytes per rune
	if err := store.Save(acct); err != nil {
		t.Fatalf("Save: %v", err)
	}

	got, found, err := store.Load(acct.Identity)
	if err != nil || !found {
		t.Fatalf("Load = (%+v, %v, %v)", got, found, err)
	}
	if len(got.DisplayName) > MaxDisplayNameBytes {
		t.Errorf("the stored display name is %d bytes, more than the %d the format keeps",
			len(got.DisplayName), MaxDisplayNameBytes)
	}
	if !strings.HasPrefix(acct.DisplayName, got.DisplayName) {
		t.Error("the stored display name is not a prefix of the one that was given")
	}
	if strings.ContainsRune(got.DisplayName, '�') {
		t.Error("the display name was cut through the middle of a rune")
	}
}

// The NUL between the two halves of an identity is load-bearing: without it these two
// identities hash to one file, which is two people sharing an account.
func TestTwoIdentitiesThatConcatenateAlikeAreDifferentAccounts(t *testing.T) {
	t.Parallel()

	one := ProviderIdentity{Provider: "disc", Subject: "ord9000"}
	two := ProviderIdentity{Provider: "discord", Subject: "9000"}
	if accountKey(one) == accountKey(two) {
		t.Fatal("two provider identities resolve to one file; the separator is not doing its job")
	}
}

// The leftovers of a crash mid-rename are swept on open, for the reason every store
// writing through world.WriteAtomic sweeps: they are inert, so this is housekeeping,
// and a store that never swept would accumulate them for the life of the service.
func TestOpenStoreSweepsTemporaries(t *testing.T) {
	t.Parallel()

	authDir := t.TempDir()
	accounts := filepath.Join(authDir, accountsDirName)
	if err := os.MkdirAll(accounts, 0o700); err != nil {
		t.Fatalf("preparing the accounts directory: %v", err)
	}
	leftover := filepath.Join(accounts, "deadbeef"+accountFileExt+".tmp1234")
	if err := os.WriteFile(leftover, []byte("half an account"), 0o600); err != nil {
		t.Fatalf("writing the leftover: %v", err)
	}

	if _, err := OpenStore(authDir); err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	if _, err := os.Stat(leftover); !os.IsNotExist(err) {
		t.Errorf("the temporary file survived the open: %v", err)
	}
}

// There is no ephemeral account service, so an unnamed directory is a refusal rather
// than a store that quietly writes nowhere. See the Store doc.
func TestOpenStoreRefusesAnUnnamedDirectory(t *testing.T) {
	t.Parallel()

	if _, err := OpenStore(""); err == nil {
		t.Fatal("an empty accounts directory was accepted; this service has no ephemeral mode")
	}
}

// A directory that cannot be created is a refusal at startup, which is where a service
// about to fail on its storage should fail — before it has bound a port.
func TestOpenStoreRefusesADirectoryItCannotCreate(t *testing.T) {
	t.Parallel()

	blocked := filepath.Join(t.TempDir(), "blocked")
	if err := os.WriteFile(blocked, []byte("not a directory"), 0o600); err != nil {
		t.Fatalf("writing the blocking file: %v", err)
	}

	if _, err := OpenStore(blocked); err == nil {
		t.Fatal("OpenStore accepted a path it cannot create a directory under")
	}
}

// A record is a fixed header plus its three pieces of text, and the format's own bound
// is what stops a corrupt directory becoming an allocation. Pinned because both
// numbers are derived from the layout and would move silently if a field were added
// without StoreVersion moving with it.
func TestAnAccountRecordIsTheSizeTheFormatSaysItIs(t *testing.T) {
	t.Parallel()

	smallest, err := encodeAccount(Account{
		ID:        testAccountID(13),
		Identity:  ProviderIdentity{Provider: "d", Subject: "9"},
		CreatedAt: created,
	})
	if err != nil {
		t.Fatalf("encodeAccount: %v", err)
	}
	if want := recordHeaderSize + 2 + world.ChecksumSize; len(smallest) != want {
		t.Errorf("the smallest record is %d bytes, want %d", len(smallest), want)
	}

	largest, err := encodeAccount(Account{
		ID: testAccountID(13),
		Identity: ProviderIdentity{
			Provider: strings.Repeat("d", MaxProviderBytes),
			Subject:  strings.Repeat("9", MaxSubjectBytes),
		},
		DisplayName: strings.Repeat("n", MaxDisplayNameBytes),
		CreatedAt:   created,
	})
	if err != nil {
		t.Fatalf("encodeAccount: %v", err)
	}
	if len(largest) != maxRecordSize {
		t.Errorf("the largest record is %d bytes, want the %d the read checks against", len(largest), maxRecordSize)
	}

	// Every store under this repository keeps its own four bytes, so a file of one kind
	// can never be read as another even at the same size. The others are named as
	// literals rather than imported: internal/persist is a different trust domain and
	// this package does not import it.
	for _, other := range [][4]byte{
		{'V', 'X', 'H', 'W'}, // internal/world, the world file
		{'V', 'X', 'H', 'D'}, // internal/world, a chunk's deltas
		{'V', 'X', 'H', 'P'}, // internal/persist, a player record
		{'V', 'X', 'H', 'S'}, // internal/persist, the structures file
		{'V', 'X', 'H', 'C'}, // internal/persist, the clock file
	} {
		if accountMagic == other {
			t.Errorf("the account magic %q collides with another store's", accountMagic[:])
		}
	}
}

// Two saves of one account are the same bytes, which is what lets a test compare files
// rather than parse them — and what makes a rewrite that changes nothing visible.
func TestSavingTheSameAccountTwiceWritesTheSameBytes(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	acct := testAccount(14, "Eivor")

	if err := store.Save(acct); err != nil {
		t.Fatalf("first Save: %v", err)
	}
	first, err := os.ReadFile(store.accountPath(acct.Identity))
	if err != nil {
		t.Fatalf("reading the first save: %v", err)
	}
	if err := store.Save(acct); err != nil {
		t.Fatalf("second Save: %v", err)
	}
	second, err := os.ReadFile(store.accountPath(acct.Identity))
	if err != nil {
		t.Fatalf("reading the second save: %v", err)
	}

	if !bytes.Equal(first, second) {
		t.Errorf("two saves of one account differ: %d bytes then %d", len(first), len(second))
	}
}

// Nothing this store writes holds a credential, and the check is on the bytes rather
// than on anybody's memory of the struct: whatever is handed in, what reaches the disk
// is an id, an identity, a name and a time.
func TestNoCredentialEverReachesTheDisk(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	acct := testAccount(15, "Eivor")
	if err := store.Save(acct); err != nil {
		t.Fatalf("Save: %v", err)
	}

	data, err := os.ReadFile(store.accountPath(acct.Identity))
	if err != nil {
		t.Fatalf("reading the record: %v", err)
	}
	// The record is exactly its four fields plus the framing. Any field an Account
	// grew that could stand in for a credential would make this number move, and the
	// version bump that has to accompany a layout change is the reminder to look.
	want := recordHeaderSize + len(acct.Identity.Provider) + len(acct.Identity.Subject) +
		len(acct.DisplayName) + world.ChecksumSize
	if len(data) != want {
		t.Errorf("the record is %d bytes, want the %d its four fields need; something else is being written",
			len(data), want)
	}
}

// A save that does not wait for a mint in flight is a save that gets overwritten.
//
// The interleaving is the one the review on #98 named: [Store.Ensure] reads, finds no
// account, and then writes the one it mints. A [Store.Save] landing inside that window
// is discarded by a write decided on a directory that had already changed, and both
// calls return nil — so the loss is invisible from either side.
//
// Pinned by *blocking* rather than by racing, because the failure is a race and a race
// reproduces when it feels like it. Holding the store's own write mutex is exactly the
// state Ensure is in between its read and its write; a Save that returns while it is
// held is a Save that would have landed in that window. The wait can only make a broken
// build pass, never a correct one fail: it is the goroutine getting no chance to run.
func TestSaveWaitsForAMintInFlight(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	acct := testAccount(17, "Halvar")

	store.write.Lock()
	saved := make(chan error, 1)
	go func() { saved <- store.Save(acct) }()

	select {
	case err := <-saved:
		store.write.Unlock()
		t.Fatalf("Save returned (%v) while a write was in flight; a mint would overwrite it", err)
	case <-time.After(50 * time.Millisecond):
	}

	store.write.Unlock()
	if err := <-saved; err != nil {
		t.Fatalf("Save after the lock was released: %v", err)
	}

	got, found, err := store.Load(acct.Identity)
	if err != nil || !found {
		t.Fatalf("Load after the waiting Save: %+v found=%v err=%v", got, found, err)
	}
	if got != acct {
		t.Errorf("Load returned %+v, want the account the waiting Save wrote %+v", got, acct)
	}
}
