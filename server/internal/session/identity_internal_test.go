package session

import (
	"bytes"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// An internal test, because the mint is not injectable from outside and should not
// become so: replacing it is a thing exactly one test needs, and an exported seam
// for it would be an exported way to make every session the same player.
//
// The branch cannot be reached in production — crypto/rand does not fail on any
// platform this server runs on — which is precisely why it needs a test. What must
// never happen is the alternative to a refusal: a zero token, shared by every
// session that failed to mint one.
func TestResolveRefusesWhenItCannotMint(t *testing.T) {
	t.Parallel()

	broken := errors.New("no entropy")
	identities := NewIdentities(nil, nil)
	identities.mint = func() (identity.Token, error) { return identity.Token{}, broken }

	msg, err := protocol.Decode(protocol.EncodeClientHello(vnet.ProtocolVersionCurrent, "Eivor"))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}

	resolved, err := identities.Resolve(msg.ClientHello)
	if err == nil {
		t.Fatal("Resolve admitted a session it could not mint an identity for")
	}
	if !errors.Is(err, broken) {
		t.Errorf("Resolve returned %v, which does not wrap the mint's failure", err)
	}

	// Not a Refused: RejectReason has no member for "the server broke", and answering
	// SERVER_FULL would tell the client something false about why. The session ends
	// with no reply, and Serve returns the error so it reaches a log.
	var refused *Refused
	if errors.As(err, &refused) {
		t.Errorf("a server-side failure was reported as the refusal %s", refused.Reason)
	}

	if resolved != (Resolved{}) {
		t.Error("Resolve returned an identity beside its error")
	}
	if identities.Count() != 0 {
		t.Error("a failed mint left an identity claimed")
	}
}

// The same shape one layer out: a player store that cannot be *reached* is a failure,
// not a refusal, and above all not "this identity is unknown".
//
// The distinction this pins is the one #147 introduced. A corrupt record will never be
// readable, so refusing the connection for ever helps nobody and it is set aside — see
// TestResolveSetsACorruptRecordAsideAndAdmitsANewPlayer. An unreachable one is a
// different animal: a permission, a failing disk, a path that is not a file at all. A
// retry may well succeed, and reading it as "no record" would throw away a perfectly
// good life on a transient fault.
func TestResolveRefusesWhenThePlayerStoreCannotBeRead(t *testing.T) {
	t.Parallel()

	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	identities := NewIdentities(store, nil)

	// Something at the record's path that is not missing and cannot be read: a symlink
	// to itself, which resolves to ELOOP. The read fails with neither "not found" nor a
	// corruption, which is exactly the case this test is about — and unlike a
	// permission bit, it fails the same way for a test run as root.
	token := identity.Token{9}
	path := filepath.Join(store.Dir(), identity.IDOf(token).String()+".bin")
	if err := os.Symlink(path, path); err != nil {
		t.Fatalf("creating the unreadable record: %v", err)
	}

	msg, err := protocol.Decode(protocol.EncodeClientHelloWithToken(vnet.ProtocolVersionCurrent, "Eivor", token[:]))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}

	if _, err := identities.Resolve(msg.ClientHello); err == nil {
		t.Fatal("Resolve treated an unreadable record as a first connection")
	} else {
		var refused *Refused
		if errors.As(err, &refused) {
			t.Errorf("an unreadable store was reported as the refusal %s", refused.Reason)
		}
	}
	if identities.Count() != 0 {
		t.Error("an unreadable store left an identity claimed")
	}
}

// TestResolveSetsACorruptRecordAsideAndAdmitsANewPlayer is the other half of the rule
// above, and the one the acceptance criterion names: a record this build cannot read is
// refused *whole*, kept, and the player joins as new.
//
// Two things are asserted about the file, and the second is the one that matters. The
// original bytes still exist somewhere — nothing deletes a player's only record — and
// the identity that gets minted is a different one, so nothing this session goes on to
// write can land on the file nobody could read.
func TestResolveSetsACorruptRecordAsideAndAdmitsANewPlayer(t *testing.T) {
	t.Parallel()

	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	identities := NewIdentities(store, nil)

	token := identity.Token{9}
	damaged := []byte("not a player record")
	path := filepath.Join(store.Dir(), identity.IDOf(token).String()+".bin")
	if err := os.WriteFile(path, damaged, 0o600); err != nil {
		t.Fatalf("writing the damaged record: %v", err)
	}

	msg, err := protocol.Decode(protocol.EncodeClientHelloWithToken(vnet.ProtocolVersionCurrent, "Eivor", token[:]))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}

	resolved, err := identities.Resolve(msg.ClientHello)
	if err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	if resolved.Returning {
		t.Error("a record that could not be read was resumed")
	}
	if resolved.Life != nil {
		t.Error("a record that could not be read produced a life")
	}
	if resolved.ID == identity.IDOf(token) {
		t.Error("the corrupt record's own identity was minted again; its file is now writable over")
	}

	if _, err := os.Stat(path); !errors.Is(err, fs.ErrNotExist) {
		t.Errorf("the corrupt record is still at its own path (Stat error %v), so the next save would replace it", err)
	}
	kept, err := filepath.Glob(path + ".corrupt.*")
	if err != nil {
		t.Fatalf("Glob: %v", err)
	}
	if len(kept) != 1 {
		t.Fatalf("found %d files set aside, want exactly 1", len(kept))
	}
	preserved, err := os.ReadFile(kept[0])
	if err != nil {
		t.Fatalf("reading the record set aside: %v", err)
	}
	if !bytes.Equal(preserved, damaged) {
		t.Error("the record set aside is not the bytes that could not be read")
	}
}

// TestAnAutosaveDoesNotUndoATeardown pins the ordering between the two write paths.
//
// An autosave captures every connected player and then writes them one at a time, so a
// session can end in the middle of a pass — and the life that pass is still holding for
// that player is then older than the one their teardown wrote. Landing it afterwards
// would put the older life back, and the worst version of that is a player who died and
// quit inside the pass being restored from before the death, with the durability penalty
// unpaid.
//
// The write lock is what makes it an ordering rather than a race, so this test drives
// the two calls in the order that would lose the teardown's record if the skip were not
// there: the teardown writes, and only then does the autosave get to its write.
func TestAnAutosaveDoesNotUndoATeardown(t *testing.T) {
	t.Parallel()

	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	identities := NewIdentities(store, nil)

	id := identity.IDOf(identity.Token{11})
	if !identities.claim(id) {
		t.Fatal("the identity was already claimed")
	}
	identities.Admitted(id, "Eivor")

	// What the autosave captured, a moment before the player left.
	stale := game.Life{Pos: [3]float64{1, 64, 1}, Health: 40}
	// What the teardown captured after them: the last word.
	final := game.Life{Pos: [3]float64{9, 70, 9}, Health: 100}

	if err := identities.Remember(id, final); err != nil {
		t.Fatalf("Remember: %v", err)
	}
	if err := identities.RememberAll(map[identity.PlayerID]game.Life{id: stale}); err != nil {
		t.Fatalf("RememberAll: %v", err)
	}

	saved, found, err := store.Load(id)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("no record was written at all")
	}
	if saved.Pos != final.Pos || saved.Health != final.Health {
		t.Errorf("the stored record is %v/%d, want the teardown's %v/%d",
			saved.Pos, saved.Health, final.Pos, final.Health)
	}

	// And once the claim is gone there is nothing for an autosave to write at all: a
	// life for an identity with no session is stale by construction.
	identities.Release(id)
	if err := identities.RememberAll(map[identity.PlayerID]game.Life{id: stale}); err != nil {
		t.Fatalf("RememberAll after Release: %v", err)
	}
	saved, _, err = store.Load(id)
	if err != nil {
		t.Fatalf("the second Load: %v", err)
	}
	if saved.Pos != final.Pos {
		t.Errorf("an autosave wrote for an identity with no session: %v", saved.Pos)
	}
}

// The autosave does write for a session that is still running, which is the other half
// of the rule above — a skip that skipped everybody would pass that test too.
func TestAnAutosaveWritesForALiveSession(t *testing.T) {
	t.Parallel()

	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	identities := NewIdentities(store, nil)

	id := identity.IDOf(identity.Token{12})
	if !identities.claim(id) {
		t.Fatal("the identity was already claimed")
	}
	identities.Admitted(id, "Eivor")

	life := game.Life{Pos: [3]float64{4, 65, -4}, Yaw: 1, Health: 73}
	if err := identities.RememberAll(map[identity.PlayerID]game.Life{id: life}); err != nil {
		t.Fatalf("RememberAll: %v", err)
	}

	saved, found, err := store.Load(id)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("the autosave wrote nothing for a live session")
	}
	if saved.Pos != life.Pos || saved.Health != life.Health {
		t.Errorf("the autosaved record is %v/%d, want %v/%d", saved.Pos, saved.Health, life.Pos, life.Health)
	}
	// The name comes from the claim, because the simulation does not carry one.
	if saved.Name != "Eivor" {
		t.Errorf("the autosaved record names %q, want the session's display name", saved.Name)
	}
}
