package registry

import (
	"encoding/binary"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// newStore is a registry in a directory belonging to this test.
func newStore(t *testing.T) *Store {
	t.Helper()

	store, err := OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	return store
}

// register puts one server in and fails the test if it could not.
func register(t *testing.T, store *Store, srv Server) bool {
	t.Helper()

	created, err := store.Register(srv)
	if err != nil {
		t.Fatalf("Register(%s): %v", srv.Name, err)
	}
	return created
}

func list(t *testing.T, store *Store) []Server {
	t.Helper()

	servers, err := store.List()
	if err != nil {
		t.Fatalf("List: %v", err)
	}
	return servers
}

// There is no ephemeral registry: a store that wrote nowhere would answer an empty list
// after every restart until every operator's announce interval had come round.
func TestAStoreMustBeGivenADirectory(t *testing.T) {
	t.Parallel()

	if _, err := OpenStore(""); err == nil {
		t.Error("a store with no directory was opened")
	}
}

// Every field survives the round trip, to the second the format keeps.
func TestARegisteredServerComesBackAsItWentIn(t *testing.T) {
	t.Parallel()

	store := newStore(t)
	now := time.Now()
	srv := aServer(now)

	if !register(t, store, srv) {
		t.Error("the first registration of a name did not report itself as new")
	}

	got := list(t, store)
	if len(got) != 1 {
		t.Fatalf("the list holds %d servers, want 1", len(got))
	}
	if got[0].Name != srv.Name || got[0].DisplayName != srv.DisplayName ||
		got[0].Address != srv.Address || got[0].Fingerprint != srv.Fingerprint {
		t.Errorf("the record came back as %+v, want the four fields of %+v", got[0], srv)
	}
	// To the second, and it is the record that is authoritative rather than the argument:
	// Online is computed from this.
	if want := now.UTC().Truncate(time.Second); !got[0].LastSeen.Equal(want) {
		t.Errorf("last seen came back as %s, want %s", got[0].LastSeen, want)
	}
}

// **The criterion the whole package exists for.** A home connection that gets a new address
// overnight is invisible to players, because the address the list serves is the one the
// server last announced and nobody is holding a copy of the old one.
func TestAReregistrationFollowsAChangedAddress(t *testing.T) {
	t.Parallel()

	store := newStore(t)
	now := time.Now()

	register(t, store, aServer(now))

	// The same server, an hour later, at a different address and — because a restart
	// without a kept key does this — presenting a different certificate.
	moved := aServer(now.Add(time.Hour))
	moved.Address = anotherAddress
	moved.Fingerprint = anotherFingerprint
	moved.DisplayName = "Midgard, moved"

	if register(t, store, moved) {
		t.Error("a re-registration of a known name reported itself as new")
	}

	got := list(t, store)
	if len(got) != 1 {
		t.Fatalf("the list holds %d servers after a re-registration, want 1", len(got))
	}
	if got[0].Address != anotherAddress {
		t.Errorf("the list serves address %q, want the one last announced", got[0].Address)
	}
	if got[0].Fingerprint != anotherFingerprint {
		t.Errorf("the list serves fingerprint %q, want the one last announced", got[0].Fingerprint)
	}
	if got[0].DisplayName != "Midgard, moved" {
		t.Errorf("the list serves display name %q, want the one last announced", got[0].DisplayName)
	}
}

// **Shown as offline, never dropped.** A record that has gone quiet is still served with the
// address it last announced; what changes is one boolean. Dropping it would make a server
// that is briefly unreachable look like one nobody ever registered.
func TestAServerThatHasGoneQuietIsStillInTheListAndOffline(t *testing.T) {
	t.Parallel()

	store := newStore(t)
	now := time.Now()

	quiet := aServer(now.Add(-OfflineAfter - time.Minute))
	quiet.Name = "asgard"
	register(t, store, quiet)
	register(t, store, aServer(now))

	got := list(t, store)
	if len(got) != 2 {
		t.Fatalf("the list holds %d servers, want 2", len(got))
	}

	byName := map[string]Server{}
	for _, srv := range got {
		byName[srv.Name] = srv
	}
	if byName["asgard"].Online(now) {
		t.Error("a server unheard from for longer than the window is online")
	}
	if byName["asgard"].Address != anAddress {
		t.Error("an offline server lost the address it last announced")
	}
	if !byName["midgard"].Online(now) {
		t.Error("a server that announced just now is offline")
	}
}

// Ordered by name rather than by whatever order the directory happened to be read in: a list
// that reshuffles between two requests is a list a player loses their place in, and directory
// order is not something any filesystem promises.
func TestTheListIsOrderedByName(t *testing.T) {
	t.Parallel()

	store := newStore(t)
	now := time.Now()

	for _, name := range []string{"vanaheim", "asgard", "midgard", "helheim"} {
		srv := aServer(now)
		srv.Name = name
		register(t, store, srv)
	}

	var got []string
	for _, srv := range list(t, store) {
		got = append(got, srv.Name)
	}
	want := []string{"asgard", "helheim", "midgard", "vanaheim"}
	if strings.Join(got, ",") != strings.Join(want, ",") {
		t.Errorf("the list is %v, want %v", got, want)
	}
}

// An empty registry is an empty list and not an error: nobody has registered yet, which is
// the ordinary state of a service on its first day.
func TestAnEmptyRegistryListsNothing(t *testing.T) {
	t.Parallel()

	if got := list(t, newStore(t)); len(got) != 0 {
		t.Errorf("an empty registry listed %d servers", len(got))
	}
}

// A registration this store would refuse never reaches the disk, which is asserted rather
// than trusted: the store's own directory is what is checked.
func TestARefusedRegistrationWritesNothing(t *testing.T) {
	t.Parallel()

	store := newStore(t)
	srv := aServer(time.Now())
	srv.Fingerprint = "not a digest"

	if _, err := store.Register(srv); !errors.Is(err, ErrFingerprint) {
		t.Fatalf("Register answered %v, want ErrFingerprint", err)
	}
	if files := recordFiles(t, store.Dir()); len(files) != 0 {
		t.Errorf("a refused registration left %d files behind", len(files))
	}
}

// **A record that cannot be read fails the whole list**, rather than quietly shortening it. A
// skipped server is one that has silently vanished — the player sees a shorter list,
// concludes that server is gone, and nobody is told anything.
func TestADamagedRecordFailsTheList(t *testing.T) {
	t.Parallel()

	for name, damage := range map[string]func([]byte) []byte{
		"a flipped byte":        func(b []byte) []byte { b[len(b)-6] ^= 0xff; return b },
		"a truncated record":    func(b []byte) []byte { return b[:len(b)-1] },
		"a padded record":       func(b []byte) []byte { return append(b, 0) },
		"an empty file":         func([]byte) []byte { return nil },
		"another store's magic": func(b []byte) []byte { copy(b[0:4], []byte("VXHA")); return b },
		"a version from the future": func(b []byte) []byte {
			binary.LittleEndian.PutUint32(b[4:8], StoreVersion+1)
			world.PutChecksum(b)
			return b
		},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			store := newStore(t)
			register(t, store, aServer(time.Now()))

			path := recordFiles(t, store.Dir())[0]
			data, err := os.ReadFile(path)
			if err != nil {
				t.Fatalf("reading the record: %v", err)
			}
			if err := os.WriteFile(path, damage(data), 0o600); err != nil {
				t.Fatalf("damaging the record: %v", err)
			}

			if _, err := store.List(); err == nil {
				t.Error("a damaged record was listed rather than reported")
			}
		})
	}
}

// **And the next announcement repairs it**, which is why failing loudly costs so little here.
// internal/auth refuses to write over an unreadable account because the person would lose
// everything the old one owned; a registry record holds nothing that is not being restated.
func TestAReregistrationRepairsADamagedRecord(t *testing.T) {
	t.Parallel()

	store := newStore(t)
	now := time.Now()
	register(t, store, aServer(now))

	path := recordFiles(t, store.Dir())[0]
	if err := os.WriteFile(path, []byte("not a record at all"), 0o600); err != nil {
		t.Fatalf("damaging the record: %v", err)
	}
	if _, err := store.List(); err == nil {
		t.Fatal("the damaged record was listed, so this test is not set up")
	}

	// The same name announcing again, as it does on its own interval.
	if created := register(t, store, aServer(now)); created {
		t.Error("a re-registration over a damaged record reported itself as new")
	}
	if got := list(t, store); len(got) != 1 || got[0].Address != anAddress {
		t.Errorf("the list is %+v after a repair, want the one server back", got)
	}
}

// A file copied or renamed onto another server's path is caught rather than served as that
// server. It matters more here than in the stores this rule is borrowed from: the fields a
// misplaced record carries are the address a client dials and the certificate it is told to
// expect.
func TestARecordOnTheWrongPathIsRefused(t *testing.T) {
	t.Parallel()

	store := newStore(t)
	now := time.Now()
	register(t, store, aServer(now))

	data, err := os.ReadFile(recordFiles(t, store.Dir())[0])
	if err != nil {
		t.Fatalf("reading the record: %v", err)
	}
	// Midgard's record, filed under the name another server would be found by.
	elsewhere := filepath.Join(store.Dir(), nameKey("asgard")+serverFileExt)
	if err := os.WriteFile(elsewhere, data, 0o600); err != nil {
		t.Fatalf("misplacing the record: %v", err)
	}

	if _, err := store.List(); !errors.Is(err, world.ErrCorruptStore) {
		t.Errorf("a misplaced record answered %v, want a corrupt-store error", err)
	}
}

// A file far too large to be a record is refused before a byte of it is read, which is how a
// corrupt directory does not become an out-of-memory.
func TestAnOversizedFileIsRefusedBeforeItIsRead(t *testing.T) {
	t.Parallel()

	store := newStore(t)
	register(t, store, aServer(time.Now()))

	path := recordFiles(t, store.Dir())[0]
	if err := os.WriteFile(path, make([]byte, maxRecordSize+1), 0o600); err != nil {
		t.Fatalf("writing an oversized file: %v", err)
	}
	err := listErr(t, store)
	if !errors.Is(err, world.ErrCorruptStore) {
		t.Errorf("an oversized file answered %v, want a corrupt-store error", err)
	}
	if !strings.Contains(err.Error(), "more than the") {
		t.Errorf("the refusal %q does not say the file is too large to be a record", err)
	}
}

// Whatever a crash left mid-rename is swept on open, and a temporary that is still there is
// never read as a record: world.WriteAtomic names them `<file>.tmp<random>`, so the extension
// is what tells them apart.
func TestTemporariesAreSweptAndNeverListed(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	store, err := OpenStore(dir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	register(t, store, aServer(time.Now()))

	leftover := filepath.Join(store.Dir(), nameKey("asgard")+serverFileExt+".tmp1234")
	if err := os.WriteFile(leftover, []byte("half a record"), 0o600); err != nil {
		t.Fatalf("leaving a temporary behind: %v", err)
	}

	// Not listed, even before anything sweeps it.
	if got := list(t, store); len(got) != 1 {
		t.Errorf("the list holds %d servers with a temporary in the directory, want 1", len(got))
	}

	// And gone after the next open, which is what a restart does.
	if _, err := OpenStore(dir); err != nil {
		t.Fatalf("reopening: %v", err)
	}
	if _, err := os.Stat(leftover); !os.IsNotExist(err) {
		t.Error("a temporary survived the sweep on open")
	}
}

// Two first announcements for one name arriving together must not both report themselves as
// new: the log would then say a server was registered for the first time twice. Worth running
// under -race, which is where the store's mutex earns its place.
func TestConcurrentRegistrationsReportOneFirstTime(t *testing.T) {
	t.Parallel()

	store := newStore(t)
	now := time.Now()

	const attempts = 16
	var (
		wg      sync.WaitGroup
		mu      sync.Mutex
		creates int
	)
	for i := 0; i < attempts; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			created, err := store.Register(aServer(now))
			if err != nil {
				mu.Lock()
				t.Errorf("Register: %v", err)
				mu.Unlock()
				return
			}
			if created {
				mu.Lock()
				creates++
				mu.Unlock()
			}
		}()
	}
	wg.Wait()

	if creates != 1 {
		t.Errorf("%d of %d concurrent registrations reported themselves as new, want 1", creates, attempts)
	}
	if got := list(t, store); len(got) != 1 {
		t.Errorf("the list holds %d servers after concurrent registrations, want 1", len(got))
	}
}

// A list read while a registration is in flight sees the whole of the old record or the whole
// of the new one — world.WriteAtomic's rename is what makes that true, and this is the test
// that would notice if a write ever stopped going through it. Under -race it is also the
// check that the two paths share no unguarded state.
func TestListingWhileRegistrationsRunNeverSeesHalfARecord(t *testing.T) {
	t.Parallel()

	store := newStore(t)
	now := time.Now()
	register(t, store, aServer(now))

	done := make(chan struct{})
	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		for i := 0; ; i++ {
			select {
			case <-done:
				return
			default:
			}
			srv := aServer(now)
			if i%2 == 0 {
				srv.Address = anotherAddress
			}
			if _, err := store.Register(srv); err != nil {
				return
			}
		}
	}()

	for i := 0; i < 200; i++ {
		got, err := store.List()
		if err != nil {
			close(done)
			wg.Wait()
			t.Fatalf("a list read during a registration failed: %v", err)
		}
		if len(got) != 1 {
			close(done)
			wg.Wait()
			t.Fatalf("a list read during a registration saw %d servers, want 1", len(got))
		}
		if got[0].Address != anAddress && got[0].Address != anotherAddress {
			close(done)
			wg.Wait()
			t.Fatalf("a list read saw address %q, which is neither of the two written", got[0].Address)
		}
	}
	close(done)
	wg.Wait()
}

// recordFiles is every record on disk, which is how "nothing was written" is asserted rather
// than assumed.
func recordFiles(t *testing.T, dir string) []string {
	t.Helper()

	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatalf("reading %s: %v", dir, err)
	}
	var found []string
	for _, entry := range entries {
		if !entry.IsDir() && strings.HasSuffix(entry.Name(), serverFileExt) {
			found = append(found, filepath.Join(dir, entry.Name()))
		}
	}
	return found
}

// listErr is List's error, failing the test if there was not one.
func listErr(t *testing.T, store *Store) error {
	t.Helper()

	if _, err := store.List(); err != nil {
		return err
	}
	t.Fatal("List succeeded where it was expected to fail")
	return nil
}
