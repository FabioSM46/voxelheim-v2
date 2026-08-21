package persist

import (
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// One account, several characters, and every one of them keyed by its own id rather
// than by the account. Keyed by the account there is one character per world, which is
// the thing #103 exists to remove.
func TestAnAccountHoldsSeveralCharacters(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	owner := testID(1)

	names := []string{"Eivor", "Sigrun", "Halvar"}
	minted := make(map[CharacterID]string, len(names))
	for _, name := range names {
		character := newCharacter(t, store, owner, name)
		if _, repeated := minted[character.ID]; repeated {
			t.Fatalf("two characters were minted the same id %s", character.ID)
		}
		minted[character.ID] = name
	}

	held := store.Characters(owner)
	if len(held) != len(names) {
		t.Fatalf("the account holds %d characters, want %d", len(held), len(names))
	}
	for _, character := range held {
		if minted[character.ID] != character.Name {
			t.Errorf("character %s came back as %q, want %q", character.ID, character.Name, minted[character.ID])
		}
		if character.Owner != owner {
			t.Errorf("character %s is owned by %s, want %s", character.ID, character.Owner.Short(), owner.Short())
		}
	}
	// Lowest id first, and the order is stable: a resolution that picks "the first one"
	// has to settle the same way on every connection.
	for i := 1; i < len(held); i++ {
		if held[i-1].ID >= held[i].ID {
			t.Errorf("Characters is not ordered by id: %s before %s", held[i-1].ID, held[i].ID)
		}
	}
}

// None of one account's characters is visible to another. The owner is a field on the
// record and the filter is over the index, so this is the whole of "whose is it".
func TestOneAccountsCharactersAreNotAnothers(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	mine, theirs := testID(1), testID(2)

	newCharacter(t, store, mine, "Eivor")
	newCharacter(t, store, mine, "Sigrun")
	only := newCharacter(t, store, theirs, "Halvar")

	held := store.Characters(theirs)
	if len(held) != 1 || held[0].ID != only.ID {
		t.Fatalf("the other account holds %+v, want just %s", held, only.ID)
	}
	for _, character := range store.Characters(mine) {
		if character.ID == only.ID {
			t.Error("one account can see another's character")
		}
	}
	if store.Characters(testID(3)) != nil {
		t.Error("an account that has never played here holds characters")
	}
}

// A name is unique within a world, and the fold is what makes that mean what a player
// would expect: "Eivor" and "eivor" are one name, not two that nobody could tell apart
// in a chat line.
func TestANameIsWornByOneCharacter(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	first := newCharacter(t, store, testID(1), "Eivor")

	for _, attempt := range []string{"Eivor", "eivor", "EIVOR", "  Eivor  "} {
		if _, err := store.Create(testID(2), attempt); !errors.Is(err, ErrNameTaken) {
			t.Errorf("Create(%q) = %v, want ErrNameTaken", attempt, err)
		}
	}
	// The same account cannot take it twice either: the rule is about the world.
	if _, err := store.Create(testID(1), "Eivor"); !errors.Is(err, ErrNameTaken) {
		t.Errorf("the owner retook its own name: %v", err)
	}
	if store.Count() != 1 {
		t.Errorf("the world holds %d characters after five refusals, want 1", store.Count())
	}
	// And the one that has it can still be found by any of those spellings.
	for _, spelling := range []string{"Eivor", "eivor", "EIVOR"} {
		got, known := store.Named(spelling)
		if !known || got.ID != first.ID {
			t.Errorf("Named(%q) = %+v (known %v), want %s", spelling, got, known, first.ID)
		}
	}
}

// **The acceptance criterion, and the one a -race run cannot answer.** Two creations
// racing for one name resolve to exactly one winner and one refusal. A check followed
// by a separate insert passes -race perfectly and fails this: both goroutines see the
// name free, both write, and the loser is whichever rename landed second.
func TestTwoCreationsRacingForOneNameLeaveOneWinner(t *testing.T) {
	t.Parallel()

	// Enough contenders that a window between a check and an insert is hit rather than
	// hoped for, each from a different account so nothing but the name is contended.
	const contenders = 16

	store, _ := openStore(t)

	var start sync.WaitGroup
	start.Add(1)

	var mu sync.Mutex
	var winners []Character
	var refusals int
	var other []error

	var done sync.WaitGroup
	for i := range contenders {
		done.Add(1)
		go func() {
			defer done.Done()
			start.Wait()

			character, err := store.Create(testID(byte(i+1)), "Eivor")
			mu.Lock()
			defer mu.Unlock()
			switch {
			case err == nil:
				winners = append(winners, character)
			case errors.Is(err, ErrNameTaken):
				refusals++
			default:
				other = append(other, err)
			}
		}()
	}
	start.Done()
	done.Wait()

	if len(other) > 0 {
		t.Fatalf("a creation failed for a reason that is not the race: %v", other)
	}
	if len(winners) != 1 {
		t.Fatalf("%d creations won the name, want exactly 1", len(winners))
	}
	if refusals != contenders-1 {
		t.Errorf("%d creations were refused, want %d", refusals, contenders-1)
	}

	// The index agrees with the outcome, and so does the directory: one character
	// wearing the name, one record on disk. A second file would be the losing write
	// that a split critical section leaves behind.
	won := winners[0]
	named, known := store.Named("Eivor")
	if !known || named.ID != won.ID {
		t.Errorf("the name belongs to %+v (known %v), want the winner %s", named, known, won.ID)
	}
	if store.Count() != 1 {
		t.Errorf("the world holds %d characters, want 1", store.Count())
	}
	records, err := filepath.Glob(filepath.Join(store.Dir(), "*"+recordFileExt))
	if err != nil {
		t.Fatalf("listing the records: %v", err)
	}
	if len(records) != 1 || filepath.Base(records[0]) != won.ID.String()+recordFileExt {
		t.Errorf("the directory holds %v, want just the winner's %s", records, won.ID)
	}
}

// An account holds at most MaxCharactersPerAccount characters on one world, and the
// refusal is its own so the caller can answer CHARACTER_LIMIT_REACHED rather than
// guessing from prose.
func TestAnAccountIsHeldToTheMaximum(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	owner := testID(1)

	for i := range MaxCharactersPerAccount {
		newCharacter(t, store, owner, fmt.Sprintf("Eivor%d", i))
	}
	if _, err := store.Create(owner, "OneTooMany"); !errors.Is(err, ErrCharacterLimit) {
		t.Fatalf("Create past the limit = %v, want ErrCharacterLimit", err)
	}
	if held := store.Characters(owner); len(held) != MaxCharactersPerAccount {
		t.Errorf("the account holds %d characters, want %d", len(held), MaxCharactersPerAccount)
	}
	// The refused name was not taken on the way out: a limit is not a way to reserve
	// names for nobody.
	if _, known := store.Named("OneTooMany"); known {
		t.Error("a creation refused by the limit still took the name")
	}
	// Another account is unaffected — the allowance is per account, not per world.
	newCharacter(t, store, testID(2), "OneTooMany")
}

// The limit is a ubyte on the wire (ServerCharacterList.max_characters) and the
// contract requires it to be non-zero. A change that broke either would be invisible
// until a client read a list.
func TestTheMaximumFitsTheContract(t *testing.T) {
	t.Parallel()

	if MaxCharactersPerAccount == 0 {
		t.Error("max_characters is required to be non-zero")
	}
	if MaxCharactersPerAccount > 255 {
		t.Errorf("max_characters is a ubyte on the wire; %d does not fit", MaxCharactersPerAccount)
	}
}

// What names this world accepts is the server's decision and the contract deliberately
// does not state it. Each refusal below is one of the reasons CHARACTER_NAME_REFUSED
// names.
func TestANameThisWorldWillNotAccept(t *testing.T) {
	t.Parallel()

	refused := map[string]string{
		"empty":                "",
		"only spaces":          "   ",
		"longer than the cap":  strings.Repeat("a", MaxNameBytes+1),
		"a newline":            "Eivor\nSigrun",
		"a terminal escape":    "Eivor\x1b[31m",
		"a NUL":                "Eivor\x00",
		"invalid utf-8":        "Eivor\xff\xfe",
		"a multi-byte overrun": strings.Repeat("ᛁ", MaxNameBytes/3+1),
	}

	for what, name := range refused {
		t.Run(what, func(t *testing.T) {
			t.Parallel()

			store, _ := openStore(t)
			if _, err := store.Create(testID(1), name); !errors.Is(err, ErrNameRefused) {
				t.Fatalf("Create(%q) = %v, want ErrNameRefused", name, err)
			}
			if store.Count() != 0 {
				t.Error("a refused name still created a character")
			}
		})
	}

	// Surrounding whitespace is trimmed rather than refused — almost always a paste —
	// and what is stored is the trimmed text.
	store, _ := openStore(t)
	character := newCharacter(t, store, testID(1), "  Eivor \t ")
	if character.Name != "Eivor" {
		t.Errorf("the stored name is %q, want the trimmed %q", character.Name, "Eivor")
	}
	// A name exactly at the cap is accepted: the bound is inclusive.
	newCharacter(t, store, testID(2), strings.Repeat("a", MaxNameBytes))
}

// A lookup is a map hit and never a walk of the directory. Proved the only way it can
// be from outside: the record files are deleted after the store is open, and every
// answer is unchanged.
func TestALookupIsAMapHitAndNotADirectoryScan(t *testing.T) {
	t.Parallel()

	worldDir := t.TempDir()
	store, err := OpenStore(worldDir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	owner := testID(1)
	character := newCharacter(t, store, owner, "Eivor")

	records, err := filepath.Glob(filepath.Join(store.Dir(), "*"+recordFileExt))
	if err != nil {
		t.Fatalf("listing the records: %v", err)
	}
	for _, path := range records {
		if err := os.Remove(path); err != nil {
			t.Fatalf("removing %s: %v", path, err)
		}
	}

	if got, known := store.Named("Eivor"); !known || got.ID != character.ID {
		t.Errorf("Named after the directory was emptied = %+v (known %v)", got, known)
	}
	if held := store.Characters(owner); len(held) != 1 || held[0].ID != character.ID {
		t.Errorf("Characters after the directory was emptied = %+v", held)
	}
	if _, known := store.Character(character.ID); !known {
		t.Error("Character after the directory was emptied is unknown")
	}
}

// **The set-aside, and what proves it did not delete.** A players directory written
// before characters is moved whole and a fresh one is opened. Every byte that was in it
// is still there, under a name nothing will write to; the world starts with no
// characters at all.
func TestAPreCharacterDirectoryIsSetAsideAndNotDeleted(t *testing.T) {
	t.Parallel()

	worldDir := t.TempDir()
	players := filepath.Join(worldDir, playersDirName)
	if err := os.MkdirAll(players, 0o755); err != nil {
		t.Fatalf("creating the old players directory: %v", err)
	}

	// What a build before characters left: one file per identity, named for a
	// 64-character player id, carrying this magic and format version 2.
	was := map[string][]byte{}
	for seed := byte(1); seed <= 3; seed++ {
		name := testID(seed).String() + recordFileExt
		body := preCharacterRecord(seed)
		if err := os.WriteFile(filepath.Join(players, name), body, 0o600); err != nil {
			t.Fatalf("writing %s: %v", name, err)
		}
		was[name] = body
	}
	// And whatever else was in there, including a crash leftover, because "nothing was
	// deleted" has to be true of every byte rather than of the records alone.
	leftover := "stray.txt"
	was[leftover] = []byte("an operator's note")
	if err := os.WriteFile(filepath.Join(players, leftover), was[leftover], 0o600); err != nil {
		t.Fatalf("writing the stray file: %v", err)
	}

	store, err := OpenStore(worldDir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}

	// A fresh directory, with nothing in it and nobody in the index.
	if store.Count() != 0 {
		t.Errorf("the world started with %d characters, want none", store.Count())
	}
	fresh, err := os.ReadDir(players)
	if err != nil {
		t.Fatalf("reading the fresh players directory: %v", err)
	}
	if len(fresh) != 0 {
		t.Errorf("the fresh players directory holds %d entries, want none", len(fresh))
	}

	// And the old one is where SetAside says, byte for byte.
	aside := store.SetAside()
	if aside == "" {
		t.Fatal("SetAside is empty; nothing was reported as moved")
	}
	if base := filepath.Base(aside); !strings.HasPrefix(base, playersDirName+preAccountsSuffix+".") {
		t.Errorf("the directory was set aside as %q, want a %s%s.<timestamp> name", base, playersDirName, preAccountsSuffix)
	}
	kept, err := os.ReadDir(aside)
	if err != nil {
		t.Fatalf("reading the directory set aside: %v", err)
	}
	if len(kept) != len(was) {
		t.Errorf("the directory set aside holds %d entries, want the %d that were there", len(kept), len(was))
	}
	for _, entry := range kept {
		want, expected := was[entry.Name()]
		if !expected {
			t.Errorf("%s appeared in the directory set aside", entry.Name())
			continue
		}
		got, err := os.ReadFile(filepath.Join(aside, entry.Name()))
		if err != nil {
			t.Fatalf("reading %s: %v", entry.Name(), err)
		}
		if string(got) != string(want) {
			t.Errorf("%s was changed on the way aside", entry.Name())
		}
	}

	// Opening again moves nothing: the fresh directory is this format's, so there is
	// nothing to set aside and nothing to overwrite.
	again, err := OpenStore(worldDir)
	if err != nil {
		t.Fatalf("re-opening: %v", err)
	}
	if again.SetAside() != "" {
		t.Errorf("a second open set %q aside; there was nothing there to move", again.SetAside())
	}
}

// The timestamped suffix is the doctrine Quarantine keeps, one level up: a second
// set-aside must not destroy the first.
func TestASecondSetAsideDoesNotOverwriteTheFirst(t *testing.T) {
	t.Parallel()

	worldDir := t.TempDir()
	players := filepath.Join(worldDir, playersDirName)

	aside := make([]string, 0, 2)
	for round := byte(1); round <= 2; round++ {
		if err := os.MkdirAll(players, 0o755); err != nil {
			t.Fatalf("creating the old players directory: %v", err)
		}
		name := testID(round).String() + recordFileExt
		if err := os.WriteFile(filepath.Join(players, name), preCharacterRecord(round), 0o600); err != nil {
			t.Fatalf("writing %s: %v", name, err)
		}
		store, err := OpenStore(worldDir)
		if err != nil {
			t.Fatalf("OpenStore round %d: %v", round, err)
		}
		if store.SetAside() == "" {
			t.Fatalf("round %d set nothing aside", round)
		}
		aside = append(aside, store.SetAside())
	}

	if aside[0] == aside[1] {
		t.Fatal("both set-asides went to the same path")
	}
	for _, path := range aside {
		entries, err := os.ReadDir(path)
		if err != nil || len(entries) != 1 {
			t.Errorf("%s holds %d entries (%v), want the one that was moved there", path, len(entries), err)
		}
	}
}

// A directory written by a *newer* build is the one case that refuses to start. Moving
// it aside would be this build deciding it knows better than the one that wrote it, and
// an operator who downgraded by accident should find out before a player does.
func TestANewerDirectoryIsRefusedRatherThanMovedAside(t *testing.T) {
	t.Parallel()

	worldDir := t.TempDir()
	players := filepath.Join(worldDir, playersDirName)
	if err := os.MkdirAll(players, 0o755); err != nil {
		t.Fatalf("creating the players directory: %v", err)
	}
	newer := make([]byte, world.HeaderSize)
	copy(newer[0:4], playerMagic[:])
	binary.LittleEndian.PutUint32(newer[4:8], StoreVersion+1)
	path := filepath.Join(players, CharacterID(0x1122_3344_5566_7788).String()+recordFileExt)
	if err := os.WriteFile(path, newer, 0o600); err != nil {
		t.Fatalf("writing the newer record: %v", err)
	}

	if _, err := OpenStore(worldDir); !errors.Is(err, world.ErrCorruptStore) {
		t.Fatalf("OpenStore over a newer directory = %v, want ErrCorruptStore", err)
	}
	if _, err := os.Stat(path); err != nil {
		t.Errorf("the newer record was moved: %v", err)
	}
}

// A record the index cannot use is set aside rather than skipped. Skipped, its id stays
// free for a later mint to write over and its name free for a later character to take —
// and the first of those loses the evidence the whole set-aside doctrine exists to keep.
func TestARecordTheIndexCannotUseIsKept(t *testing.T) {
	t.Parallel()

	broken := map[string]func(t *testing.T, dir string){
		"a name no character is written under": func(t *testing.T, dir string) {
			t.Helper()
			write(t, filepath.Join(dir, "not-a-character-id"+recordFileExt), encodeRecord(Record{
				Character: 1, Owner: testID(1), Name: "Eivor", Health: 100,
			}))
		},
		"a record found under another character's name": func(t *testing.T, dir string) {
			t.Helper()
			write(t, filepath.Join(dir, CharacterID(2).String()+recordFileExt), encodeRecord(Record{
				Character: 1, Owner: testID(1), Name: "Eivor", Health: 100,
			}))
		},
		"a name this world would not accept": func(t *testing.T, dir string) {
			t.Helper()
			write(t, filepath.Join(dir, CharacterID(3).String()+recordFileExt), encodeRecord(Record{
				Character: 3, Owner: testID(1), Name: "", Health: 100,
			}))
		},
		"a broken checksum": func(t *testing.T, dir string) {
			t.Helper()
			body := encodeRecord(Record{Character: 4, Owner: testID(1), Name: "Eivor", Health: 100})
			body[len(body)-1] ^= 0xff
			write(t, filepath.Join(dir, CharacterID(4).String()+recordFileExt), body)
		},
	}

	for what, damage := range broken {
		t.Run(what, func(t *testing.T) {
			t.Parallel()

			worldDir := t.TempDir()
			players := filepath.Join(worldDir, playersDirName)
			if err := os.MkdirAll(players, 0o755); err != nil {
				t.Fatalf("creating the players directory: %v", err)
			}
			damage(t, players)

			store, err := OpenStore(worldDir)
			if err != nil {
				t.Fatalf("OpenStore: %v", err)
			}
			if store.Count() != 0 {
				t.Errorf("the index took a record it could not use: %d characters", store.Count())
			}
			kept := store.Unreadable()
			if len(kept) != 1 {
				t.Fatalf("Unreadable reports %v, want the one file", kept)
			}
			if _, err := os.Stat(kept[0]); err != nil {
				t.Errorf("the file reported as kept is not there: %v", err)
			}
			// And it is out of the directory, so nothing mints or names over it.
			left, err := filepath.Glob(filepath.Join(players, "*"+recordFileExt))
			if err != nil {
				t.Fatalf("listing the records: %v", err)
			}
			if len(left) != 0 {
				t.Errorf("%v is still where the index would read it", left)
			}
		})
	}
}

// Two records wearing one name is a store disagreeing with itself. Only this server
// writes here, so it is tampering or a bug — and either way one of the two has to end up
// somewhere an operator can find it rather than silently deciding a name.
func TestTwoRecordsWearingOneNameLeaveOneIndexed(t *testing.T) {
	t.Parallel()

	worldDir := t.TempDir()
	players := filepath.Join(worldDir, playersDirName)
	if err := os.MkdirAll(players, 0o755); err != nil {
		t.Fatalf("creating the players directory: %v", err)
	}
	for _, id := range []CharacterID{1, 2} {
		write(t, filepath.Join(players, id.String()+recordFileExt), encodeRecord(Record{
			Character: id, Owner: testID(1), Name: "Eivor", Health: 100,
		}))
	}

	store, err := OpenStore(worldDir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	if store.Count() != 1 {
		t.Errorf("the index holds %d characters, want 1", store.Count())
	}
	// Which one wins is deterministic and not arbitrary: os.ReadDir sorts by file name,
	// a file name is the id in fixed-width hex, so the lower id arrives first and keeps
	// the name. A store that decided this by map order would answer differently on
	// different runs, which is the worst shape a resolution can have.
	if got, known := store.Named("Eivor"); !known || got.ID != CharacterID(1) {
		t.Errorf("the name went to %+v (known %v), want the lower id 1", got, known)
	}
	if len(store.Unreadable()) != 1 {
		t.Fatalf("Unreadable reports %v, want the one that lost", store.Unreadable())
	}
	if !strings.HasPrefix(filepath.Base(store.Unreadable()[0]), CharacterID(2).String()) {
		t.Errorf("the record set aside is %s, want character 2's", store.Unreadable()[0])
	}
	// Kept, not deleted.
	if _, err := os.Stat(store.Unreadable()[0]); err != nil {
		t.Errorf("the losing record is gone: %v", err)
	}
}

// A character id is never zero — schemas/handshake.fbs reserves it — and the hex name a
// record is written under round-trips through the parser the startup scan uses.
func TestACharacterIDNamesAFile(t *testing.T) {
	t.Parallel()

	for _, id := range []CharacterID{1, 0x00ff, 0xffff_ffff_ffff_ffff, 0x0123_4567_89ab_cdef} {
		name := id.String()
		if len(name) != 16 {
			t.Errorf("%s is %d characters, want 16", name, len(name))
		}
		got, ok := parseCharacterID(name)
		if !ok || got != id {
			t.Errorf("parseCharacterID(%q) = %s, %v; want %s, true", name, got, ok, id)
		}
	}
	if _, ok := parseCharacterID(CharacterID(0).String()); ok {
		t.Error("zero was read as a character id; the contract reserves it")
	}
	for _, bad := range []string{"", "abc", "00000000000000000", "00000000000000zz", "0123456789ABCDEF"} {
		if _, ok := parseCharacterID(bad); ok {
			t.Errorf("parseCharacterID(%q) accepted a name no record is written under", bad)
		}
	}
	// Minting never produces the reserved id, and never repeats one it has produced.
	store, _ := openStore(t)
	seen := make(map[CharacterID]bool)
	for i := range 64 {
		// A fresh account every MaxCharactersPerAccount characters, so what is under
		// test is the mint rather than the allowance.
		character := newCharacter(t, store, testID(byte(i/MaxCharactersPerAccount+1)), fmt.Sprintf("Eivor%d", i))
		if character.ID.IsZero() {
			t.Fatal("a minted id is zero")
		}
		if seen[character.ID] {
			t.Fatalf("id %s was minted twice", character.ID)
		}
		seen[character.ID] = true
	}
}

// preCharacterRecord is what a build before characters wrote: this magic, format
// version 2, and a body this build has no way to read.
func preCharacterRecord(seed byte) []byte {
	body := make([]byte, world.HeaderSize+32)
	copy(body[0:4], playerMagic[:])
	binary.LittleEndian.PutUint32(body[4:8], StoreVersion-1)
	for i := world.HeaderSize; i < len(body); i++ {
		body[i] = seed
	}
	return body
}

func write(t *testing.T, path string, body []byte) {
	t.Helper()

	if err := os.WriteFile(path, body, 0o600); err != nil {
		t.Fatalf("writing %s: %v", path, err)
	}
}

// One account, many first connections, one character.
//
// **The race this closes is the one the cross-account test above cannot see.** That one
// contends a name; this one contends the *decision to create at all*. Reading the roster
// and then creating let two hellos for a fresh account both find it empty and both
// create — under different names, so `Create`'s per-name lock let both through — and the
// connection that afterwards lost the single-session claim had already written a second
// character to disk. Nothing deletes a character, so the account was left holding one
// nobody asked for, with its roster slot and its name spent for good.
//
// Asserted on the outcome rather than on the detector, for the reason the cross-account
// test records: a check-then-act across two lock acquisitions is not a data race, and
// `-race` reports nothing while the invariant is broken.
func TestManyFirstConnectionsForOneAccountCreateOneCharacter(t *testing.T) {
	t.Parallel()

	const contenders = 16

	store, _ := openStore(t)
	owner := testID(1)

	var start sync.WaitGroup
	start.Add(1)

	var mu sync.Mutex
	var seen []Character
	var creations int
	var failures []error

	var done sync.WaitGroup
	for i := range contenders {
		done.Add(1)
		go func(i int) {
			defer done.Done()
			start.Wait()

			// A different requested name each, which is what made the old shape create
			// several: one name would have collided in Create and hidden the defect.
			character, created, err := store.ResolveOrCreate(owner, fmt.Sprintf("wanderer%d", i))

			mu.Lock()
			defer mu.Unlock()
			if err != nil {
				failures = append(failures, err)
				return
			}
			seen = append(seen, character)
			if created {
				creations++
			}
		}(i)
	}

	start.Done()
	done.Wait()

	if len(failures) != 0 {
		t.Fatalf("%d of %d first connections were refused: %v", len(failures), contenders, failures[0])
	}
	if creations != 1 {
		t.Errorf("%d characters were created for one account, want exactly 1", creations)
	}
	if held := store.Characters(owner); len(held) != 1 {
		names := make([]string, 0, len(held))
		for _, character := range held {
			names = append(names, character.Name)
		}
		t.Errorf("the account holds %d characters (%v), want the one it asked for", len(held), names)
	}
	for _, character := range seen {
		if character.ID != seen[0].ID {
			t.Fatalf("two connections played different characters: %s and %s", seen[0].ID, character.ID)
		}
	}
}
