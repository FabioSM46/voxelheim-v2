package session

import (
	"bytes"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
)

// The account service these internal tests are admitted by.
//
// Built per test rather than shared, unlike the one the external tests keep: there are
// four of them, a pair costs microseconds, and the alternative is a second TestMain in
// a binary that may hold only one.
func internalMint(t *testing.T) (*ticket.Pair, ticket.WorldID) {
	t.Helper()

	pair, err := ticket.LoadOrCreate(t.TempDir())
	if err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}
	world, err := ticket.WorldIDFor("midgard")
	if err != nil {
		t.Fatalf("WorldIDFor: %v", err)
	}
	return pair, world
}

// internalIdentities is a claim set over store, with a hello builder for the account
// service that admits it.
func internalIdentities(t *testing.T, store *persist.Store) (*Identities, func(identity.Account) *protocol.ClientHello) {
	t.Helper()
	return internalIdentitiesExploring(t, store, nil)
}

// internalIdentitiesExploring is internalIdentities with a map ledger behind it too.
func internalIdentitiesExploring(t *testing.T, store *persist.Store, explored *persist.ExplorationStore) (*Identities, func(identity.Account) *protocol.ClientHello) {
	t.Helper()

	pair, world := internalMint(t)
	verifier, err := NewVerifier(pair.Public(), world, nil)
	if err != nil {
		t.Fatalf("NewVerifier: %v", err)
	}
	identities, err := NewIdentities(store, explored, verifier, nil)
	if err != nil {
		t.Fatalf("NewIdentities: %v", err)
	}

	return identities, func(account identity.Account) *protocol.ClientHello {
		t.Helper()

		minted, _, mErr := pair.Mint(ticket.AccountID(account), world, time.Now())
		if mErr != nil {
			t.Fatalf("Mint: %v", mErr)
		}
		msg, dErr := protocol.Decode(protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", minted[:]))
		if dErr != nil {
			t.Fatalf("Decode: %v", dErr)
		}
		return msg.ClientHello
	}
}

// seedCharacter mints the one character an account holds in these tests and answers the
// path its record lives at, which is what a test damages.
//
// The path is derived from the character id rather than from the account, which is the
// change #103 made to this file: a record is a character's now, and the account is a
// field inside it.
func seedCharacter(t *testing.T, store *persist.Store, account identity.Account) (persist.Character, string) {
	t.Helper()

	character, err := store.Create(identity.IDOf(account), "Eivor", testAppearance())
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	return character, filepath.Join(store.Dir(), character.ID.String()+".bin")
}

// testAppearance is a face the contract allows. Every path that stores or plays one
// checks it, so a fixture that skipped it would be testing the refusal instead.
func testAppearance() protocol.Appearance {
	return protocol.Appearance{
		SkinColor:     0x00C68642,
		ShirtColor:    0x00415B76,
		TrousersColor: 0x00332B24,
		ShoesColor:    0x00101010,
		HairModel:     vnet.HairModelCropped,
		HairColor:     0x005A3A22,
	}
}

// play drives the two steps a connection takes: the hello, which admits an account and
// claims its one live session, and the choice, which settles a character.
//
// One helper because the tests below are about what happens to a *record*, and each
// would otherwise repeat a handshake to get there. **The claim it takes is not released
// here**, exactly as it is not released by a failed choice in production: it belongs to
// the connection, and Serve's teardown is what gives it back.
func play(t *testing.T, identities *Identities, hello *protocol.ClientHello, wanted persist.CharacterID) (Resolved, error) {
	t.Helper()

	admitted, err := identities.Admit(hello)
	if err != nil {
		return Resolved{}, err
	}
	return identities.Select(admitted, wanted)
}

// An internal test, because a claim set built without a verifier is a thing only this
// package can attempt: the constructor is the only way in from outside, and it refuses.
//
// The rule is the acceptance criterion put into the type system — a server that cannot
// verify a ticket cannot admit anybody — and the direction that matters is the one in
// which forgetting an argument would have been permissive.
func TestAClaimSetRefusesToExistWithoutAVerifier(t *testing.T) {
	t.Parallel()

	identities, err := NewIdentities(nil, nil, nil, nil)
	if err == nil {
		t.Fatal("a claim set was built with no way to check a ticket")
	}
	if identities != nil {
		t.Error("NewIdentities returned a claim set beside its error")
	}
}

// A player store that cannot be *reached* is a failure, not a refusal, and above all
// not "this player is unknown".
//
// The distinction this pins is the one legacy PR 147 introduced. A corrupt record will never be
// readable, so refusing the connection for ever helps nobody and it is set aside — see
// TestResolveSetsACorruptRecordAsideAndAdmitsThePlayerAsNew. An unreachable one is a
// different animal: a permission, a failing disk, a path that is not a file at all. A
// retry may well succeed, and reading it as "no record" would throw away a perfectly
// good life on a transient fault.
func TestResolveRefusesWhenThePlayerStoreCannotBeRead(t *testing.T) {
	t.Parallel()

	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	identities, hello := internalIdentities(t, store)

	// Something at the record's path that is not missing and cannot be read: a symlink
	// to itself, which resolves to ELOOP. The read fails with neither "not found" nor a
	// corruption, which is exactly the case this test is about — and unlike a
	// permission bit, it fails the same way for a test run as root.
	account := identity.Account{9}
	character, path := seedCharacter(t, store, account)
	if err := os.Remove(path); err != nil {
		t.Fatalf("removing the record the character was created with: %v", err)
	}
	if err := os.Symlink(path, path); err != nil {
		t.Fatalf("creating the unreadable record: %v", err)
	}

	if _, err := play(t, identities, hello(account), character.ID); err == nil {
		t.Fatal("the selection treated an unreadable record as a first connection")
	} else {
		var refused *Refused
		if errors.As(err, &refused) {
			t.Errorf("an unreadable store was reported as the refusal %s", refused.Reason)
		}
	}
	// The claim is still held, and that is the phase this contract added rather than a
	// leak: it is taken when the ticket verifies, one message before a character is
	// chosen, so a refusal here leaves it exactly where a person still reading their
	// character list would. Serve releases it on every path out.
	if identities.Count() != 1 {
		t.Errorf("%d accounts are claimed, want the one whose selection failed", identities.Count())
	}
}

// TestResolveSetsACorruptRecordAsideAndAdmitsThePlayerAsNew is the other half of the
// rule above: a record this build cannot read is refused *whole*, kept, and the player
// joins with nothing.
//
// **What changed with the ticket is which player joins.** While an identity was minted
// from crypto/rand, the player admitted after a corrupt record was a *different* one
// under a different file name, so nothing that session went on to write could land on
// the damaged record. A player is named by their account now, so the same person comes
// back to the same path — which is why the move has to succeed, and why the test below
// this one exists for the case where it does not.
func TestResolveSetsACorruptRecordAsideAndAdmitsThePlayerAsNew(t *testing.T) {
	t.Parallel()

	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	identities, hello := internalIdentities(t, store)

	account := identity.Account{9}
	character, path := seedCharacter(t, store, account)
	damaged := []byte("not a player record")
	if err := os.WriteFile(path, damaged, 0o600); err != nil {
		t.Fatalf("writing the damaged record: %v", err)
	}

	resolved, err := play(t, identities, hello(account), character.ID)
	if err != nil {
		t.Fatalf("the selection: %v", err)
	}
	if resolved.Returning {

		t.Error("a record that could not be read was resumed")
	}
	if resolved.Life != nil {
		t.Error("a record that could not be read produced a life")
	}
	// The player is still the player. Their ticket names the same account, so admitting
	// them as somebody else would mean an unreadable file had silently changed who they
	// are — which is what used to happen, and could only happen while this server was
	// the thing that decided.
	if resolved.ID != identity.IDOf(account) {
		t.Error("the account was admitted as somebody else")
	}
	// And it is still the same character. Quarantining a record takes the life, never
	// the character: the name is still theirs and the account still owns it.
	if resolved.Character != character.ID || resolved.Name != character.Name {
		t.Errorf("the player came back as character %s/%q, want %s/%q",
			resolved.Character, resolved.Name, character.ID, character.Name)
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

// A record that nobody can read and nobody can move refuses the connection, and this is
// the branch the ticket changed.
//
// #147 could survive a failed move because the identity minted next was a different one
// — 32 fresh random bytes, a different file — so nothing that session went on to write
// could land on the damaged record. That is gone: a player id is a digest of the
// account their ticket names, so the session admitted after a failed move writes to
// exactly the path whose contents nobody could read, and its first teardown would
// destroy the evidence and the player's only record together.
//
// So the answer moves to the one the *unreachable* record already gets. A refusal costs
// that player one connection and an operator one look at a directory; the alternative
// costs the record, and nobody can undo it.
func TestResolveRefusesWhenACorruptRecordCannotBeSetAside(t *testing.T) {
	t.Parallel()

	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	identities, hello := internalIdentities(t, store)

	account := identity.Account{13}
	character, path := seedCharacter(t, store, account)

	damaged := []byte("not a player record")
	if err := os.WriteFile(path, damaged, 0o600); err != nil {
		t.Fatalf("writing the damaged record: %v", err)
	}
	// A directory nothing may rename inside of, which is what governs a move: the file
	// itself stays perfectly readable at its own 0600. Restored on the way out so
	// t.TempDir's cleanup can remove it.
	if err := os.Chmod(store.Dir(), 0o500); err != nil {
		t.Fatalf("sealing the players directory: %v", err)
	}
	t.Cleanup(func() { _ = os.Chmod(store.Dir(), 0o700) })
	if err := os.Rename(path, path+".probe"); err == nil {
		// Running as a user a read-only directory does not stop, root being the usual
		// one. Skipped rather than asserted, because the alternative is a test that
		// passes by not testing.
		_ = os.Rename(path+".probe", path)
		t.Skip("this user can rename inside a read-only directory, so a failed move cannot be staged")
	}

	resolved, err := play(t, identities, hello(account), character.ID)
	if err == nil {
		t.Fatal("a record that could not be set aside was admitted; the next teardown would write over it")
	}

	// A failure and not a refusal: RejectReason has no member for "this server cannot
	// keep your record safe", and BAD_REQUEST would blame a client for a directory it
	// has never heard of. The session ends with no reply and Serve returns the error so
	// it reaches a log.
	var refused *Refused
	if errors.As(err, &refused) {
		t.Errorf("a server-side failure was reported as the refusal %s", refused.Reason)
	}
	if resolved != (Resolved{}) {
		t.Error("the selection returned a character beside its error")
	}
	// Still claimed, for the reason the unreachable-record test states: the claim is the
	// account's and it is taken one message earlier than the choice that failed.
	if identities.Count() != 1 {
		t.Errorf("%d accounts are claimed, want the one whose selection failed", identities.Count())
	}

	// And the evidence is exactly where it was.
	kept, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("reading the record that could not be moved: %v", err)
	}
	if !bytes.Equal(kept, damaged) {
		t.Error("the record that could not be moved was changed anyway")
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
	identities, _ := internalIdentities(t, store)

	id := identity.IDOf(identity.Account{11})
	character, err := store.Create(id, "Eivor", testAppearance())
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	// The two steps a connection takes, in the order it takes them: the account is
	// claimed when its ticket verifies, and the character is put on the claim when the
	// choice settles. An autosave that ran between them would have nothing to write.
	if !identities.claim(id) {
		t.Fatal("the player was already claimed")
	}
	self := identities.playing(Admitted{ID: id}, character, false, nil)

	// What the autosave captured, a moment before the player left.
	stale := game.Life{Pos: [3]float64{1, 64, 1}, Health: 40, Hunger: 12, Experience: 50}
	// What the teardown captured after them: the last word.
	final := game.Life{Pos: [3]float64{9, 70, 9}, Health: 100, Hunger: 83, Experience: 725}

	if err := identities.Remember(self, final); err != nil {
		t.Fatalf("Remember: %v", err)
	}
	if err := identities.RememberAll(map[identity.PlayerID]game.Life{id: stale}); err != nil {
		t.Fatalf("RememberAll: %v", err)
	}

	saved, found, err := store.Load(character.ID)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("no record was written at all")
	}
	if saved.Pos != final.Pos || saved.Health != final.Health || saved.Hunger != final.Hunger || saved.Experience != final.Experience {
		t.Errorf("the stored record is %v/%d/%d/%d, want the teardown's %v/%d/%d/%d",
			saved.Pos, saved.Health, saved.Hunger, saved.Experience,
			final.Pos, final.Health, final.Hunger, final.Experience)
	}

	// And once the claim is gone there is nothing for an autosave to write at all: a
	// life for a player with no session is stale by construction.
	identities.Release(id)
	if err := identities.RememberAll(map[identity.PlayerID]game.Life{id: stale}); err != nil {
		t.Fatalf("RememberAll after Release: %v", err)
	}
	saved, _, err = store.Load(character.ID)
	if err != nil {
		t.Fatalf("the second Load: %v", err)
	}
	if saved.Pos != final.Pos {
		t.Errorf("an autosave wrote for a player with no session: %v", saved.Pos)
	}
}

func TestOfflineMobExperienceRaisesOnlyTheCharacterThatOwnedTheTap(t *testing.T) {
	t.Parallel()

	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	identities, _ := internalIdentities(t, store)
	owner := identity.IDOf(identity.Account{31})
	character, err := store.Create(owner, "Eivor", testAppearance())
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	original := persist.Record{
		Pos: [3]float64{4, 65, -4}, Health: 73, Hunger: 29, Experience: 10,
	}
	if err := store.Save(character.ID, original); err != nil {
		t.Fatalf("Save: %v", err)
	}

	award := game.ExperienceAward{PlayerID: owner, CharacterName: "Eivor", Experience: 25}
	if persisted, err := identities.RememberExperience(award); err != nil {
		t.Fatalf("RememberExperience: %v", err)
	} else if !persisted {
		t.Fatal("RememberExperience did not persist the award")
	}
	// The absolute total makes both a retry and an older delayed write harmless.
	if persisted, err := identities.RememberExperience(award); err != nil {
		t.Fatalf("RememberExperience retry: %v", err)
	} else if !persisted {
		t.Fatal("RememberExperience retry did not find the durable award")
	}
	if persisted, err := identities.RememberExperience(game.ExperienceAward{
		PlayerID: owner, CharacterName: "Eivor", Experience: 20,
	}); err != nil {
		t.Fatalf("RememberExperience with an older total: %v", err)
	} else if !persisted {
		t.Fatal("RememberExperience with an older total did not find the newer durable award")
	}

	saved, found, err := store.Load(character.ID)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("the offline award wrote no record")
	}
	if saved.Experience != 25 {
		t.Errorf("stored experience = %d, want 25", saved.Experience)
	}
	if saved.Pos != original.Pos || saved.Health != original.Health || saved.Hunger != original.Hunger {
		t.Errorf("offline experience changed the life to %v/%d/%d, want %v/%d/%d",
			saved.Pos, saved.Health, saved.Hunger, original.Pos, original.Health, original.Hunger)
	}
	if _, err := identities.RememberExperience(game.ExperienceAward{
		PlayerID: identity.PlayerID{99}, CharacterName: "Eivor", Experience: 30,
	}); err == nil {
		t.Error("an award under another owner was accepted")
	}
}

func TestOfflineMobExperienceWithoutARecordIsNotReportedAsPersisted(t *testing.T) {
	t.Parallel()

	store := persist.NewMemoryStore()
	identities, _ := internalIdentities(t, store)
	owner := identity.IDOf(identity.Account{32})
	if _, err := store.Create(owner, "Eivor", testAppearance()); err != nil {
		t.Fatalf("Create: %v", err)
	}

	persisted, err := identities.RememberExperience(game.ExperienceAward{
		PlayerID: owner, CharacterName: "Eivor", Experience: 15,
	})
	if err != nil {
		t.Fatalf("RememberExperience: %v", err)
	}
	if persisted {
		t.Fatal("an ephemeral store with no record reported the award as durable")
	}
}

func TestOfflineMobExperienceWaitsForTheDepartedSessionsFinalRecord(t *testing.T) {
	t.Parallel()

	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	identities, _ := internalIdentities(t, store)
	owner := identity.IDOf(identity.Account{33})
	character, err := store.Create(owner, "Eivor", testAppearance())
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	if !identities.claim(owner) {
		t.Fatal("the player was already claimed")
	}
	self := identities.playing(Admitted{ID: owner}, character, false, nil)
	award := game.ExperienceAward{PlayerID: owner, CharacterName: "Eivor", Experience: 55}

	// The simulation has removed this session, but its teardown still owns one final
	// write at 40. The award must stay queued until that write has landed.
	if persisted, err := identities.RememberExperience(award); err != nil {
		t.Fatalf("RememberExperience before teardown: %v", err)
	} else if persisted {
		t.Fatal("the award persisted before the departed session wrote its final record")
	}
	if err := identities.Remember(self, game.Life{Health: 100, Hunger: 100, Experience: 40}); err != nil {
		t.Fatalf("Remember: %v", err)
	}
	if persisted, err := identities.RememberExperience(award); err != nil {
		t.Fatalf("RememberExperience after teardown: %v", err)
	} else if !persisted {
		t.Fatal("the award did not persist after the teardown finalised")
	}

	saved, found, err := store.Load(character.ID)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found || saved.Experience != award.Experience {
		t.Fatalf("stored experience = %d (found %t), want %d", saved.Experience, found, award.Experience)
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
	identities, _ := internalIdentities(t, store)

	id := identity.IDOf(identity.Account{12})
	character, err := store.Create(id, "Eivor", testAppearance())
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	if !identities.claim(id) {
		t.Fatal("the player was already claimed")
	}
	identities.playing(Admitted{ID: id}, character, false, nil)

	life := game.Life{Pos: [3]float64{4, 65, -4}, Yaw: 1, Health: 73, Hunger: 29, Experience: 330}
	if err := identities.RememberAll(map[identity.PlayerID]game.Life{id: life}); err != nil {
		t.Fatalf("RememberAll: %v", err)
	}

	saved, found, err := store.Load(character.ID)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("the autosave wrote nothing for a live session")
	}
	if saved.Pos != life.Pos || saved.Health != life.Health || saved.Hunger != life.Hunger || saved.Experience != life.Experience {
		t.Errorf("the autosaved record is %v/%d/%d/%d, want %v/%d/%d/%d",
			saved.Pos, saved.Health, saved.Hunger, saved.Experience,
			life.Pos, life.Health, life.Hunger, life.Experience)
	}
	// The character it was written under comes from the claim, because the simulation
	// keys a life by account and an account has several characters.
	if saved.Character != character.ID || saved.Name != "Eivor" || saved.Owner != id {
		t.Errorf("the autosaved record is %s/%q/%s, want the claimed character", saved.Character, saved.Name, saved.Owner.Short())
	}
}

// TestRefuseCharacterMapsEveryStoreRefusal pins the one place a store refusal becomes a
// wire code.
//
// An internal test because refuseCharacter is unexported. All three cases are reachable
// from the wire now — a CreateCharacterRequest can be refused for the name being taken,
// for the name being unacceptable, and for the account being full — which is what the
// character phase changed: while a character was resolved from the hello's display name,
// an account that already held one never created another, so CHARACTER_LIMIT_REACHED was
// a code nothing could produce.
//
// The fourth case is the one that is not a refusal: a store that cannot mint or cannot
// write is this server failing, and RejectReason has no member that says so. Answering
// one anyway would blame a client for a disk it has never heard of.
func TestRefuseCharacterMapsEveryStoreRefusal(t *testing.T) {
	t.Parallel()

	cases := map[error]vnet.RejectReason{
		persist.ErrNameTaken:      vnet.RejectReasonCHARACTER_NAME_TAKEN,
		persist.ErrNameRefused:    vnet.RejectReasonCHARACTER_NAME_REFUSED,
		persist.ErrCharacterLimit: vnet.RejectReasonCHARACTER_LIMIT_REACHED,
	}

	details := make(map[string]bool, len(cases))
	for sentinel, want := range cases {
		// Wrapped, because that is how the store returns them and a mapping that only
		// worked on a bare sentinel would work on nothing real.
		err := refuseCharacter(fmt.Errorf("persist: %w: something specific", sentinel))

		var refused *Refused
		if !errors.As(err, &refused) {
			t.Fatalf("%v became %v, which carries no reason code", sentinel, err)
		}
		if refused.Reason != want {
			t.Errorf("%v became %s, want %s", sentinel, refused.Reason, want)
		}
		// The cause reaches through, so a caller can name the sentinel it means rather
		// than match prose.
		if !errors.Is(err, sentinel) {
			t.Errorf("%v is not reachable through the refusal it became", sentinel)
		}
		// Each says something different, which is the whole reason these are three
		// codes and not one: the player acts on them differently.
		if refused.Detail == "" || details[refused.Detail] {
			t.Errorf("%v is told %q, which is empty or the same as another case", sentinel, refused.Detail)
		}
		details[refused.Detail] = true
	}

	// Anything else is a server failure and never a refusal.
	failure := refuseCharacter(errors.New("the disk is full"))
	var refused *Refused
	if errors.As(failure, &refused) {
		t.Errorf("a store failure was reported as the refusal %s", refused.Reason)
	}
}

// An account that has been admitted and has not chosen yet is a person reading their
// character list, and there is nothing for an autosave to write for one.
//
// It cannot happen through Serve — the autosave asks the *simulation* which lives to
// write, and a session that has not chosen has not joined it — which is exactly why the
// skip is worth a test: what would otherwise stand in the way is a Save refused with an
// error, one layer further down and for a different reason.
func TestAnAutosaveSkipsAnAccountThatHasNotChosen(t *testing.T) {
	t.Parallel()

	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	identities, _ := internalIdentities(t, store)

	id := identity.IDOf(identity.Account{14})
	character, err := store.Create(id, "Eivor", testAppearance())
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	if !identities.claim(id) {
		t.Fatal("the account was already claimed")
	}

	if err := identities.RememberAll(map[identity.PlayerID]game.Life{
		id: {Pos: [3]float64{1, 64, 1}, Health: 40},
	}); err != nil {
		t.Fatalf("RememberAll: %v", err)
	}

	// The record the character was created with, untouched: unplayed, and therefore not
	// a life.
	saved, found, err := store.Load(character.ID)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("the record Create wrote is gone")
	}
	if !saved.Unplayed() {
		t.Errorf("an autosave wrote a life for an account that had not chosen a character: %+v", saved)
	}
}

// The two ways a selection is refused leave one answer, and that is the point of it.
//
// A client that could tell "no such character" from "that one is not yours" could map
// the world's characters by asking for ids it does not have — the rule
// game.Player.RemoveStructure keeps for a camp, and the one
// schemas/handshake.fbs states for this message in as many words.
func TestASelectionThisAccountMayNotMakeLeavesOneAnswer(t *testing.T) {
	t.Parallel()

	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	identities, hello := internalIdentities(t, store)

	mine := identity.Account{21}
	seedCharacter(t, store, mine)
	theirs, err := store.Create(identity.IDOf(identity.Account{22}), "Sigrun", testAppearance())
	if err != nil {
		t.Fatalf("Create for the other account: %v", err)
	}

	admitted, err := identities.Admit(hello(mine))
	if err != nil {
		t.Fatalf("Admit: %v", err)
	}

	answers := make([]*Refused, 0, 2)
	for what, wanted := range map[string]persist.CharacterID{
		"a character another account owns": theirs.ID,
		"a character nobody has":           persist.CharacterID(0xdead_beef),
	} {
		resolved, sErr := identities.Select(admitted, wanted)
		var refused *Refused
		if !errors.As(sErr, &refused) {
			t.Fatalf("selecting %s answered %v, which carries no reason code", what, sErr)
		}
		if refused.Reason != vnet.RejectReasonBAD_REQUEST {
			t.Errorf("selecting %s answered %s, want BAD_REQUEST", what, refused.Reason)
		}
		if refused.Cause == nil {
			t.Errorf("selecting %s left nothing in the log to tell it apart by", what)
		}
		if resolved != (Resolved{}) {
			t.Errorf("selecting %s returned a character beside its refusal", what)
		}
		answers = append(answers, refused)
	}

	// Identical on the wire; the causes above are what an operator reads instead.
	if answers[0].Reason != answers[1].Reason || answers[0].Detail != answers[1].Detail {
		t.Errorf("the two refusals differ on the wire: %s/%q and %s/%q",
			answers[0].Reason, answers[0].Detail, answers[1].Reason, answers[1].Detail)
	}
	// A selection this account *can* make is still admitted, so the test above is not
	// passing because everything is refused.
	if _, err := identities.Select(admitted, admitted.Characters[0].ID); err != nil {
		t.Errorf("the account's own character was refused: %v", err)
	}
}

// An appearance the contract forbids is refused **before it is stored**, which is the
// obligation schemas/common.fbs puts on whatever accepts a CreateCharacterRequest: a
// character written down wearing a hair model no member names is one every client is
// afterwards required to refuse, and the person who cannot get in is not the person who
// sent it.
func TestACreationWithAForbiddenAppearanceIsRefusedBeforeItIsStored(t *testing.T) {
	t.Parallel()

	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	identities, hello := internalIdentities(t, store)

	admitted, err := identities.Admit(hello(identity.Account{23}))
	if err != nil {
		t.Fatalf("Admit: %v", err)
	}

	forbidden := testAppearance()
	forbidden.HairModel = vnet.HairModelUnknown

	resolved, err := identities.Create(admitted, "Eivor", forbidden)
	var refused *Refused
	if !errors.As(err, &refused) {
		t.Fatalf("a forbidden appearance answered %v, which carries no reason code", err)
	}
	if refused.Reason != vnet.RejectReasonBAD_REQUEST {
		t.Errorf("a forbidden appearance answered %s, want BAD_REQUEST — the name is fine and a name code would say otherwise", refused.Reason)
	}
	if !errors.Is(err, protocol.ErrAppearance) {
		t.Error("the refusal does not reach through to what was wrong with the appearance")
	}
	if resolved != (Resolved{}) {
		t.Error("a forbidden appearance returned a character beside its refusal")
	}

	// Nothing was written and nothing was spent: not the name, not a roster slot.
	if got := store.Count(); got != 0 {
		t.Errorf("the world holds %d characters after a refused creation, want none", got)
	}
	if _, taken := store.Named("Eivor"); taken {
		t.Error("a refused creation took the name anyway")
	}
}
