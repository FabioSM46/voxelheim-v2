// Internal tests: the encoder, the on-disk layout and the index built over it are what
// is being pinned, and a corrupt file is produced by writing bytes rather than by
// calling an exported function.
package persist

import (
	"encoding/binary"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// testID is a distinct account per seed, derived so a failing test names the same
// owner on every run.
func testID(seed byte) identity.PlayerID {
	var account identity.Account
	for i := range account {
		account[i] = seed*31 + byte(i)
	}
	return identity.IDOf(account)
}

func openStore(t *testing.T) (*Store, string) {
	t.Helper()

	worldDir := t.TempDir()
	store, err := OpenStore(worldDir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	return store, worldDir
}

// newCharacter mints one, failing the test rather than returning a refusal: every use
// below is a set-up step, and the refusals have tests of their own in characters_test.go.
func newCharacter(t *testing.T, store *Store, owner identity.PlayerID, name string) Character {
	t.Helper()

	character, err := store.Create(owner, name, testAppearance())

	if err != nil {
		t.Fatalf("Create(%s, %q): %v", owner.Short(), name, err)
	}
	return character
}

// testAppearance is a face the contract allows: every colour inside 0x00RRGGBB and a
// hair model that is a real member.
//
// This package stores an appearance without judging it — that gate is the handshake's,
// where the value arrives from a client — but the startup scan refuses one the contract
// forbids, so a fixture that wants its character to survive a reopen has to state a
// legal one.
func testAppearance() protocol.Appearance {
	return protocol.Appearance{
		SkinColor:     0xc68642,
		ShirtColor:    0x4b6043,
		TrousersColor: 0x2e2a25,
		ShoesColor:    0x111111,
		HairModel:     vnet.HairModelLoose,
		HairColor:     0x9a3b1b,
	}
}

func TestStoreRoundTripsARecord(t *testing.T) {
	t.Parallel()

	store, worldDir := openStore(t)
	owner := testID(1)
	character := newCharacter(t, store, owner, "Eivor")

	// Seconds, because that is the resolution the format keeps. A test that wrote
	// time.Now() and compared it whole would fail on the nanoseconds the format
	// deliberately does not store.
	want := Record{LastSeen: time.Unix(1_700_000_000, 0).UTC(), Health: 100, Hunger: 73, Experience: 4321, Silver: 2468}
	if err := store.Save(character.ID, want); err != nil {
		t.Fatalf("Save: %v", err)
	}

	got, found, err := store.Load(character.ID)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("the record just written was not found")
	}
	// The three fields that name a character come from the index, never from the
	// caller: a save that could restate them would be a second, unlocked way to rename
	// a character or move it to another account.
	if got.Character != character.ID {
		t.Errorf("Character = %s, want %s", got.Character, character.ID)
	}
	if got.Owner != owner {
		t.Errorf("Owner = %s, want %s", got.Owner.Short(), owner.Short())
	}
	if got.Name != "Eivor" {
		t.Errorf("Name = %q, want %q", got.Name, "Eivor")
	}
	if !got.LastSeen.Equal(want.LastSeen) {
		t.Errorf("LastSeen = %s, want %s", got.LastSeen, want.LastSeen)
	}

	// The file is named for the character id and lives under the world directory. Both
	// are part of the contract with an operator reading a directory listing.
	path := filepath.Join(worldDir, playersDirName, character.ID.String()+recordFileExt)
	if _, err := os.Stat(path); err != nil {
		t.Errorf("the record is not at %s: %v", path, err)
	}

	// A second save replaces the first rather than appending to it.
	later := Record{LastSeen: want.LastSeen.Add(time.Hour), Health: 61, Hunger: 12, Experience: 9876, Silver: 8642}
	if err := store.Save(character.ID, later); err != nil {
		t.Fatalf("the second Save: %v", err)
	}
	got, _, _ = store.Load(character.ID)
	if got.Health != later.Health || got.Hunger != later.Hunger || got.Experience != later.Experience || got.Silver != later.Silver || !got.LastSeen.Equal(later.LastSeen) {
		t.Errorf("the second save did not replace the first: %+v", got)
	}
}

// A record's three naming fields are the store's, whatever a caller puts in them. This
// is the authoritative half of "who owns this character": there is exactly one way to
// answer it, and Save is not it.
func TestSaveIgnoresACallersIdeaOfWhoACharacterIs(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	mine := newCharacter(t, store, testID(1), "Eivor")
	theirs := newCharacter(t, store, testID(2), "Sigrun")

	// A face nobody chose, offered by the caller: an appearance is chosen once, at
	// creation, and a save that could restate one would be a way to come back from a
	// session wearing somebody else's.
	borrowed := testAppearance()
	borrowed.HairModel = vnet.HairModelTopknot
	borrowed.SkinColor = 0x010203

	if err := store.Save(mine.ID, Record{
		Character:  theirs.ID,
		Owner:      theirs.Owner,
		Name:       "Sigrun the Second",
		Appearance: borrowed,
		Health:     100,
	}); err != nil {
		t.Fatalf("Save: %v", err)
	}

	got, _, err := store.Load(mine.ID)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if got.Character != mine.ID || got.Owner != mine.Owner || got.Name != mine.Name {
		t.Errorf("a save restated who the character is: %s/%s/%q", got.Character, got.Owner.Short(), got.Name)
	}
	if got.Appearance != mine.Appearance {
		t.Errorf("a save restated what the character looks like: %+v, want %+v", got.Appearance, mine.Appearance)
	}

	// And the other character is untouched, which is the failure this prevents.
	if theirs2, ok := store.Character(theirs.ID); !ok || theirs2.Name != "Sigrun" {
		t.Errorf("the other character changed: %+v (known %v)", theirs2, ok)
	}
}

// An id no character wears is not a new file. The index says what exists, so a save
// under an unknown id would create a character no lookup could ever find.
func TestSaveRefusesAnUnknownCharacter(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	if err := store.Save(CharacterID(0xdead_beef), Record{Health: 100}); !errors.Is(err, ErrUnknownCharacter) {
		t.Fatalf("Save under an unknown id = %v, want ErrUnknownCharacter", err)
	}
}

func TestStoreReportsAnUnknownCharacterAsNotFound(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)

	rec, found, err := store.Load(CharacterID(0x1234_5678))
	if err != nil {
		t.Fatalf("Load of an unknown character: %v", err)
	}
	if found {
		t.Fatal("a character that was never created was found")
	}
	if rec != (Record{}) {
		t.Errorf("a record came back for an unknown character: %+v", rec)
	}
}

// corrupt is what a damaged record looks like on disk, per way of being damaged.
func TestStoreRefusesARecordItCannotReadExactly(t *testing.T) {
	t.Parallel()

	sound := encodeRecord(Record{Name: "Eivor", LastSeen: time.Unix(1_700_000_000, 0).UTC()})

	damage := map[string]func([]byte) []byte{
		// A flipped bit inside a record whose shape is still valid — exactly what a
		// length check cannot catch, and the whole reason the CRC is there.
		"a flipped bit in the name": func(b []byte) []byte {
			out := append([]byte(nil), b...)
			out[recordHeaderSize] ^= 0x20
			return out
		},
		"a wrong magic number": func(b []byte) []byte {
			out := append([]byte(nil), b...)
			out[0] = 'X'
			world.PutChecksum(out)
			return out
		},
		// A well-formed record of a version this build does not know. Refused rather
		// than read as the layout it happens to resemble.
		"a future format version": func(b []byte) []byte {
			out := append([]byte(nil), b...)
			out[4] = byte(StoreVersion + 1)
			world.PutChecksum(out)
			return out
		},
		"a truncated file": func(b []byte) []byte { return b[:len(b)-3] },
		"a name length that disagrees with the file": func(b []byte) []byte {
			out := append([]byte(nil), b...)
			out[offNameLen] = 200
			world.PutChecksum(out)
			return out
		},
		"a file shorter than a header": func([]byte) []byte { return []byte{'V', 'X'} },
	}

	for name, break_ := range damage {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			store, _ := openStore(t)
			character := newCharacter(t, store, testID(3), "Eivor")
			if err := os.WriteFile(store.recordPath(character.ID), break_(sound), 0o600); err != nil {
				t.Fatalf("writing the damaged record: %v", err)
			}

			_, found, err := store.Load(character.ID)
			if !errors.Is(err, world.ErrCorruptStore) {
				t.Fatalf("Load = %v, want ErrCorruptStore", err)
			}
			// The distinction that matters: unreadable is not "absent". Reported as
			// absent, the session would start the character fresh and its teardown
			// would write over the record nobody could read.
			if found {
				t.Error("a corrupt record was reported as found")
			}
		})
	}
}

func TestStoreRefusesAFileTooLargeToBeARecord(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	character := newCharacter(t, store, testID(4), "Eivor")

	// Checked before the read, not after: finding this out by allocating it is how a
	// corrupt directory becomes an out-of-memory.
	if err := os.WriteFile(store.recordPath(character.ID), make([]byte, maxRecordSize+1), 0o600); err != nil {
		t.Fatalf("writing the oversized record: %v", err)
	}
	if _, _, err := store.Load(character.ID); !errors.Is(err, world.ErrCorruptStore) {
		t.Fatalf("Load = %v, want ErrCorruptStore", err)
	}
}

func TestOpenStoreSweepsTemporaries(t *testing.T) {
	t.Parallel()

	worldDir := t.TempDir()
	store, err := OpenStore(worldDir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	dir := store.Dir()

	// What a crash between the write and the rename leaves behind. Inert — a reader
	// only ever opens an exact <character-id>.bin path — so this is housekeeping, and
	// the housekeeping is the store's rather than an operator's.
	leftover := filepath.Join(dir, CharacterID(0x51_92_af_00_11_22_33_44).String()+recordFileExt+".tmp3141592")
	if err := os.WriteFile(leftover, []byte("half a record"), 0o600); err != nil {
		t.Fatalf("writing the leftover: %v", err)
	}

	if _, err := OpenStore(worldDir); err != nil {
		t.Fatalf("re-opening: %v", err)
	}
	if _, err := os.Stat(leftover); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("the temporary file survived the sweep: %v", err)
	}
}

func TestAnEphemeralWorldWritesNothing(t *testing.T) {
	t.Parallel()

	// The ephemeral world is a store with an index and no directory: the rules about
	// names and allowances are the world's, and only the disk is missing.
	store := NewMemoryStore()

	if store.Dir() != "" {
		t.Errorf("a memory store names a directory: %q", store.Dir())
	}
	character := newCharacter(t, store, testID(7), "Eivor")
	if err := store.Save(character.ID, Record{LastSeen: time.Now(), Health: 100}); err != nil {
		t.Fatalf("Save on a memory store: %v", err)
	}
	rec, found, err := store.Load(character.ID)
	if err != nil {
		t.Fatalf("Load on a memory store: %v", err)
	}
	if found || rec != (Record{}) {
		t.Error("a memory store remembered something")
	}
	// The rules still hold: the name is taken, by the account that took it.
	if _, err := store.Create(testID(8), "eivor", testAppearance()); !errors.Is(err, ErrNameTaken) {
		t.Errorf("a second account took the name on an ephemeral world: %v", err)
	}

	// And the world directory an ephemeral server was pointed at stays empty.
	dir := t.TempDir()
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatalf("ReadDir: %v", err)
	}
	if len(entries) != 0 {
		t.Errorf("an ephemeral run left %d entries behind", len(entries))
	}
}

func TestOpenStoreRefusesAnUnnamedWorldDirectory(t *testing.T) {
	t.Parallel()

	// An empty -world-dir is the ephemeral world, and choosing it is main's decision.
	// Answering with a store that writes nowhere would hide that decision here.
	if _, err := OpenStore(""); err == nil {
		t.Fatal("OpenStore accepted an empty world directory")
	}
}

func TestARecordSurvivesReopening(t *testing.T) {
	t.Parallel()

	worldDir := t.TempDir()
	store, err := OpenStore(worldDir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}

	owner := testID(8)
	character := newCharacter(t, store, owner, "Sigrun")
	want := Record{LastSeen: time.Unix(1_600_000_000, 0).UTC(), Health: 77}
	if err := store.Save(character.ID, want); err != nil {
		t.Fatalf("Save: %v", err)
	}

	reopened, err := OpenStore(worldDir)
	if err != nil {
		t.Fatalf("re-opening: %v", err)
	}
	got, found, err := reopened.Load(character.ID)
	if err != nil || !found {
		t.Fatalf("Load after reopening: %v (found %v)", err, found)
	}
	if got.Name != "Sigrun" || got.Owner != owner || !got.LastSeen.Equal(want.LastSeen) {
		t.Errorf("the record came back as %+v, want Sigrun/%s/%s", got, owner.Short(), want.LastSeen)
	}
	// The index came back with it, rebuilt from the records and from nothing else.
	if held := reopened.Characters(owner); len(held) != 1 || held[0].ID != character.ID {
		t.Errorf("the reopened index holds %+v, want just %s", held, character.ID)
	}
}

// TestStoreRoundTripsTheLife is the life half of the round trip: everything the record
// keeps about where a character stood, written and read back to the bit.
//
// The position is checked for exact equality rather than within a tolerance, and that
// is the assertion. Position is a float64 on the way in and a float64 on the way out —
// no narrowing anywhere in the format — so a save is not allowed to move a player by
// so much as a rounding, however many times they reconnect.
func TestStoreRoundTripsTheLife(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	character := newCharacter(t, store, testID(7), "Eivor")

	want := Record{
		LastSeen: time.Unix(1_700_000_000, 0).UTC(),
		// Values a float32 could not hold exactly, so a narrowing anywhere in the
		// format shows up as a failure rather than as a rounding nobody notices.
		Pos:        [3]float64{-1234.5678901234567, 70.100000000000001, 4096.3333333333333},
		Yaw:        -2.7182818284590452,
		Health:     61,
		Hunger:     37,
		Experience: 1234,
		Silver:     987654,
	}
	// Every shape a slot can take: a worn durable item, a partial stack, the last slot
	// occupied, and empties everywhere else.
	want.Slots[0] = protocol.InventoryStack{ItemID: 7, Count: 1, Durability: 37, MaxDurability: 100}
	want.Slots[5] = protocol.InventoryStack{ItemID: 1, Count: 23}
	want.Slots[protocol.InventorySlots-1] = protocol.InventoryStack{ItemID: 6, Count: 2}

	if err := store.Save(character.ID, want); err != nil {
		t.Fatalf("Save: %v", err)
	}
	got, found, err := store.Load(character.ID)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("the record just written was not found")
	}

	if got.Pos != want.Pos {
		t.Errorf("Pos = %v, want %v", got.Pos, want.Pos)
	}
	if got.Yaw != want.Yaw {
		t.Errorf("Yaw = %v, want %v", got.Yaw, want.Yaw)
	}
	if got.Health != want.Health {
		t.Errorf("Health = %d, want %d", got.Health, want.Health)
	}
	if got.Hunger != want.Hunger {
		t.Errorf("Hunger = %d, want %d", got.Hunger, want.Hunger)
	}
	if got.Experience != want.Experience {
		t.Errorf("Experience = %d, want %d", got.Experience, want.Experience)
	}
	if got.Silver != want.Silver {
		t.Errorf("Silver = %d, want %d", got.Silver, want.Silver)
	}
	if got.Slots != want.Slots {
		for slot := range want.Slots {
			if got.Slots[slot] != want.Slots[slot] {
				t.Errorf("slot %d = %+v, want %+v", slot, got.Slots[slot], want.Slots[slot])
			}
		}
	}
}

// A record is a fixed size plus its name, and the format's own bound is what stops a
// corrupt directory becoming an allocation. Pinned because both numbers are derived
// from the slot table and would move silently if a field were added without a version
// bump.
func TestARecordIsTheSizeTheFormatSaysItIs(t *testing.T) {
	t.Parallel()

	empty := encodeRecord(Record{})
	if len(empty) != recordHeaderSize+world.ChecksumSize {
		t.Errorf("an empty record is %d bytes, want %d", len(empty), recordHeaderSize+world.ChecksumSize)
	}
	named := encodeRecord(Record{Name: strings.Repeat("a", MaxNameBytes)})
	if len(named) != maxRecordSize {
		t.Errorf("the largest record is %d bytes, want %d", len(named), maxRecordSize)
	}
	// The slot table is the bulk of it, and it is the whole table whatever is in it: a
	// record with one item and a record with none are the same length.
	oneItem := Record{}
	oneItem.Slots[3] = protocol.InventoryStack{ItemID: 1, Count: 1}
	if len(encodeRecord(oneItem)) != len(empty) {
		t.Error("a record with an item is a different size from an empty one; the slot table is not fixed")
	}
}

// V9 keeps the v8 slot table and appends the purse before the variable name. Pin
// every relationship and the bytes themselves: changing only the struct field would
// otherwise round trip through an equally wrong encoder and decoder.
func TestV9StoresSilverAfterFortySlots(t *testing.T) {
	t.Parallel()

	if StoreVersion != 9 {
		t.Fatalf("StoreVersion = %d, want 9", StoreVersion)
	}
	if offHunger != offHealth+2 {
		t.Fatalf("offHunger = %d, want offHealth+2 = %d", offHunger, offHealth+2)
	}
	if offExperience != offHunger+2 {
		t.Fatalf("offExperience = %d, want offHunger+2 = %d", offExperience, offHunger+2)
	}
	if offSlots != offExperience+4 {
		t.Fatalf("offSlots = %d, want offExperience+4 = %d", offSlots, offExperience+4)
	}
	if slotsSize != 40*slotSize {
		t.Fatalf("slotsSize = %d, want 40 slots × %d bytes", slotsSize, slotSize)
	}
	if offSilver != offSlots+slotsSize {
		t.Fatalf("offSilver = %d, want offSlots+slotsSize = %d", offSilver, offSlots+slotsSize)
	}

	rec := Record{Health: 0x1234, Hunger: 0x5678, Experience: 0x9abcdef0, Silver: 0x12345678}
	rec.Slots[0] = protocol.InventoryStack{ItemID: 0x9abc, Count: 1}
	encoded := encodeRecord(rec)
	if got := binary.LittleEndian.Uint16(encoded[offHealth : offHealth+2]); got != rec.Health {
		t.Errorf("health bytes = %#x, want %#x", got, rec.Health)
	}
	if got := binary.LittleEndian.Uint16(encoded[offHunger : offHunger+2]); got != rec.Hunger {
		t.Errorf("hunger bytes = %#x, want %#x", got, rec.Hunger)
	}
	if got := binary.LittleEndian.Uint32(encoded[offExperience : offExperience+4]); got != rec.Experience {
		t.Errorf("experience bytes = %#x, want %#x", got, rec.Experience)
	}
	if got := binary.LittleEndian.Uint16(encoded[offSlots : offSlots+2]); got != rec.Slots[0].ItemID {
		t.Errorf("first slot item bytes = %#x, want %#x", got, rec.Slots[0].ItemID)
	}
	if got := binary.LittleEndian.Uint32(encoded[offSilver : offSilver+4]); got != rec.Silver {
		t.Errorf("silver bytes = %#x, want %#x", got, rec.Silver)
	}
}

// There is deliberately no v8 migration. It may hold silver in an inventory slot, so
// the bytes stay with their old world rather than being repaired into the purse.
func TestV9RefusesAV8RecordRatherThanMigratingIt(t *testing.T) {
	t.Parallel()

	old := encodeRecord(Record{Health: 100, Hunger: 100, Experience: 200})
	binary.LittleEndian.PutUint32(old[4:8], 8)
	world.PutChecksum(old)
	if _, err := decodeRecord(old); !errors.Is(err, world.ErrCorruptStore) {
		t.Fatalf("decodeRecord(v8) = %v, want ErrCorruptStore", err)
	}
}

// Quarantine keeps the bytes and frees the path, which is the pair of properties the
// corrupt-record rule rests on: nothing a player had is deleted, and the next save has
// somewhere to go that is not on top of it.
func TestQuarantineKeepsTheRecordAndFreesThePath(t *testing.T) {
	t.Parallel()

	store, _ := openStore(t)
	character := newCharacter(t, store, testID(8), "Eivor")

	damaged := []byte("not a player record")
	path := store.recordPath(character.ID)
	if err := os.WriteFile(path, damaged, 0o600); err != nil {
		t.Fatalf("writing the damaged record: %v", err)
	}

	aside, err := store.Quarantine(character.ID)
	if err != nil {
		t.Fatalf("Quarantine: %v", err)
	}
	if aside == path {
		t.Fatal("the record was left where it was")
	}
	kept, err := os.ReadFile(aside)
	if err != nil {
		t.Fatalf("reading the record set aside: %v", err)
	}
	if string(kept) != string(damaged) {
		t.Error("the bytes set aside are not the bytes that were there")
	}
	if _, _, err := store.Load(character.ID); err != nil {
		t.Errorf("Load after Quarantine: %v, want the path to be free", err)
	}
	// The character itself survives: only its life is gone. Dropping it from the index
	// would free a name a record on disk is still wearing.
	if got, known := store.Character(character.ID); !known || got.Name != "Eivor" {
		t.Errorf("quarantining a record took the character with it: %+v (known %v)", got, known)
	}

	// A second corrupt record for the same character does not overwrite the first —
	// which is the same silent overwrite this whole path exists to prevent, one turn
	// further round.
	if err := os.WriteFile(path, []byte("also not a record"), 0o600); err != nil {
		t.Fatalf("writing the second damaged record: %v", err)
	}
	again, err := store.Quarantine(character.ID)
	if err != nil {
		t.Fatalf("the second Quarantine: %v", err)
	}
	if again == aside {
		t.Error("the second corrupt record was written over the first")
	}
	if _, err := os.Stat(aside); err != nil {
		t.Errorf("the first record set aside is gone: %v", err)
	}
}

// A nil store keeps nothing, and every path is a no-op rather than a branch at each
// call site. It is not the ephemeral world — NewMemoryStore is — but session hands one
// in and this is what it costs.
func TestANilStoreKeepsNothing(t *testing.T) {
	t.Parallel()

	var store *Store
	id := CharacterID(0x9)

	if err := store.Save(id, Record{Health: 100}); err != nil {
		t.Errorf("Save on a nil store: %v", err)
	}
	rec, found, err := store.Load(id)
	if err != nil {
		t.Errorf("Load on a nil store: %v", err)
	}
	if found || rec != (Record{}) {
		t.Error("a nil store answered with a record")
	}
	aside, err := store.Quarantine(id)
	if err != nil || aside != "" {
		t.Errorf("Quarantine on a nil store = %q, %v; want the empty no-op", aside, err)
	}
	if store.Characters(testID(9)) != nil || store.Count() != 0 {
		t.Error("a nil store holds characters")
	}
	if _, known := store.Character(id); known {
		t.Error("a nil store knows a character")
	}
	if _, known := store.Named("Eivor"); known {
		t.Error("a nil store knows a name")
	}
	// Create is the one method that is an error rather than a no-op: a caller that
	// asked for a character and got the zero value would go on to save under id 0.
	if _, err := store.Create(testID(9), "Eivor", testAppearance()); err == nil {
		t.Error("a nil store minted a character")
	}
}
