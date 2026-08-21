package persist

import (
	"crypto/rand"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"sort"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
)

// MaxCharactersPerAccount is how many characters one account may hold on one world.
//
// **Five, and the number is a judgement rather than a measurement**, so the reasoning
// is what a later issue should argue with. A world is a place a group plays together,
// and the thing this limit exists to bound is not disk — a record is a few hundred
// bytes — but the two costs that are not storage: the character list an account is
// shown on every connection, and the number of names one account may hold out of a
// world's shared namespace. A name is unique per world, so every character an account
// keeps is a name nobody else can have; an unbounded allowance would let one account
// take every name it liked and call it play.
//
// Five is enough for the archetypes a player actually keeps — a main, a second class,
// and a few experiments — and small enough that the list is a screen rather than a
// scroll. It is a ubyte on the wire (`ServerCharacterList.max_characters`), so whatever
// replaces it stays under 256, and the contract requires it to be non-zero.
const MaxCharactersPerAccount = 5

// MaxNameBytes is the longest character name this world accepts, in bytes.
//
// **A refusal now, where it used to be a truncation, and the inversion is the point.**
// While the name was untrusted display text that nothing keyed on, a long one was
// silently cut at a rune boundary, because "a long name is not a reason to turn a
// player away". A character name is an identifier: it is unique within the world, and
// two different names that truncate to the same bytes would be one name by the time
// the uniqueness check saw them. So the cut is gone and the name is refused whole —
// which is what `RejectReason.CHARACTER_NAME_REFUSED` exists to say, and it names "too
// long" first among its reasons.
const MaxNameBytes = 64

// CharacterID names one character on one world.
//
// Server-minted, never zero, and stable for the life of the character. It is the key
// the player store writes under and the value `CharacterSummary.character_id` carries,
// which is why it is the wire's `ulong` rather than a digest: a client is shown it and
// hands it back, so it has to be a number the contract can hold.
//
// **Not a credential and not a secret.** Knowing a character id gains nothing —
// whether it names a character *this account owns* is re-read from this store on every
// selection — which is the property that lets it be minted at random rather than
// derived from anything.
type CharacterID uint64

// String is the id in lowercase hex, zero-padded to sixteen characters: the name of
// the file this character's record lives in.
//
// Fixed width so that a directory listing sorts and so that the name a record is read
// back from is exactly the name it was written under, whatever the leading nibbles are.
func (c CharacterID) String() string { return fmt.Sprintf("%016x", uint64(c)) }

// IsZero reports the id that names no character.
//
// Zero is reserved by schemas/handshake.fbs — "`character_id` is never 0" — so a
// selection carrying it is refused rather than read as the first row. Minting skips it
// for the same reason.
func (c CharacterID) IsZero() bool { return c == 0 }

// Character is one character as the index knows it: who owns it and what it is called.
//
// Deliberately not the life. Where a character stands, what health it has and what is
// in its pack are in its [Record] and are read once a character has been chosen — the
// same split `ServerCharacterList` draws, and for the same reason: a list that carried
// them would hand out world state before an identity had been settled.
type Character struct {
	// ID is what this character is keyed by, everywhere.
	ID CharacterID

	// Owner is the account that owns this character, as the one-way [identity.PlayerID]
	// rather than the account itself. A leaked players directory stays a directory of
	// digests: that is the whole reason the id exists as a separate type, and storing
	// the account here would undo it.
	Owner identity.PlayerID

	// Name is the character's name as the server accepted it — the exact text, not the
	// folded form uniqueness is decided on.
	Name string
}

// The three ways this store refuses to create a character.
//
// Sentinels because the caller has to answer each with a different member of
// `RejectReason`, and matching prose to decide what to put on the wire is how a log
// line becomes a contract. They map one to one:
// [ErrNameTaken] is CHARACTER_NAME_TAKEN, [ErrNameRefused] is CHARACTER_NAME_REFUSED,
// and [ErrCharacterLimit] is CHARACTER_LIMIT_REACHED.
var (
	// ErrNameTaken reports a name a character on this world already wears.
	//
	// Distinct from [ErrNameRefused] on purpose: this name is perfectly acceptable and
	// somebody else has it, so the player retries with another one.
	ErrNameTaken = errors.New("persist: a character on this world already has that name")

	// ErrNameRefused reports a name this world will not accept at all.
	ErrNameRefused = errors.New("persist: that is not a name this world accepts")

	// ErrCharacterLimit reports an account that already holds as many characters as
	// this world allows.
	ErrCharacterLimit = fmt.Errorf("persist: an account may hold at most %d characters on one world", MaxCharactersPerAccount)
)

// ErrUnknownCharacter reports an id this store has never minted.
//
// Its own sentinel rather than a not-found bool, because every caller that reaches it
// is asking about a character it was handed by this store — so it is a bug on this
// side rather than a client naming something that does not exist.
var ErrUnknownCharacter = errors.New("persist: no character on this world has that id")

// Create mints a character for owner and writes its first record, or refuses.
//
// **The whole of it is one critical section, and that is the acceptance criterion
// rather than an implementation detail.** The name is checked against the index, the
// account's allowance is checked, an id is minted, the record is written and the index
// is updated, all under one lock — the shape `game.Sim.PlaceStructure` uses to validate
// the ground and insert the structure together. Split into a check and a later insert,
// two creations racing for one name both see it free and both take it, and the loser is
// whichever rename happened to land second. Held together, one wins and the other is
// told [ErrNameTaken].
//
// The disk write is inside the section deliberately. A character is created once in its
// life, so the lock is held for one atomic write on a path nothing else contends for —
// and moving the write out would put the index and the directory briefly out of step,
// which is the same window in another shape. What that costs is bounded: [Store.Load]
// never takes this lock, and [Store.Save] takes it only for the lookup that says which
// character it is writing, never across its own write.
//
// On an ephemeral world the index is real and the write is a no-op: the name is still
// taken, the allowance is still spent, and nothing survives the process.
func (s *Store) Create(owner identity.PlayerID, name string) (Character, error) {
	if s == nil {
		return Character{}, errors.New("persist: this world has no character store")
	}

	accepted, folded, err := acceptName(name)
	if err != nil {
		return Character{}, err
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	return s.createLocked(owner, accepted, folded)
}

// createLocked is [Store.Create] with s.mu already held, so that a caller which has
// *decided* to create under this lock can do it without releasing one.
//
// It exists for [Store.ResolveOrCreate], and the reason is the whole of #156's review:
// a decision made outside the lock that ends in a write inside it is a check-then-act
// race however carefully the write itself is guarded.
func (s *Store) createLocked(owner identity.PlayerID, accepted, folded string) (Character, error) {
	if _, taken := s.byName[folded]; taken {
		return Character{}, fmt.Errorf("%w: %q", ErrNameTaken, accepted)
	}
	if held := len(s.byOwner[owner]); held >= MaxCharactersPerAccount {
		return Character{}, fmt.Errorf("%w: %s already holds %d", ErrCharacterLimit, owner.Short(), held)
	}

	id, err := s.mintLocked()
	if err != nil {
		return Character{}, err
	}

	character := Character{ID: id, Owner: owner, Name: accepted}
	if wErr := s.writeRecord(character, Record{}); wErr != nil {
		return Character{}, wErr
	}

	s.insertLocked(character)
	return character, nil
}

// ResolveOrCreate answers with the character this account plays, creating one only if
// the account holds none — and it decides **and** writes under one lock.
//
// **The lock is the whole point, and the version of this that read the roster first was
// wrong in a way no test caught** (found in review on #156). Two hellos for one fresh
// account both saw an empty roster, both created — [Store.Create] serialises per *name*,
// so two different names both succeed — and the connection that then lost the
// single-session claim had already written a second character to disk. The account was
// left holding a character nobody asked for, permanently: there is no deletion here, so
// the roster slot and the name were gone for good.
//
// The resolution rule is unchanged and is documented where the caller used to hold it:
// an account with characters plays the one wearing the requested name, or the lowest id
// it holds when none does; an account with none has one created under that name, which
// is the only way a first connection becomes a character. It never *creates* from a
// name an account already has characters for — a second character is made through the
// wire exchange or not at all.
//
// The bool reports whether a character was created, which is what lets a caller say
// "welcome" differently from "welcome back" without comparing timestamps.
func (s *Store) ResolveOrCreate(owner identity.PlayerID, requested string) (Character, bool, error) {
	if s == nil {
		return Character{}, false, errors.New("persist: this world has no character store")
	}

	// Validated before the lock, and its refusal is deliberately not returned here: a
	// name this world would not accept cannot be worn, so on the resolve path it simply
	// matches nothing, exactly as [Store.Named] treats it. The create path below asks
	// again and *does* return the refusal, because there it is the answer.
	accepted, folded, nameErr := acceptName(requested)

	s.mu.Lock()
	defer s.mu.Unlock()

	if ids := s.byOwner[owner]; len(ids) > 0 {
		if nameErr == nil {
			if id, taken := s.byName[folded]; taken {
				if character, known := s.byID[id]; known && character.Owner == owner {
					return character, false, nil
				}
			}
		}
		// The lowest id, found rather than assumed. **byOwner is insertion order, not
		// sorted** — [Store.Characters] sorts a copy, which is where the "lowest id
		// first" the caller relies on actually comes from. Taking ids[0] here made an
		// unknown name play whichever character happened to be created first, and
		// `a hello names which of several characters plays` caught it in eight runs out
		// of eight. The determinism is the property: two connections naming nothing
		// this account wears must settle on the same character.
		lowest, found := Character{}, false
		for _, id := range ids {
			character, known := s.byID[id]
			if !known {
				continue
			}
			if !found || character.ID < lowest.ID {
				lowest, found = character, true
			}
		}
		if found {
			return lowest, false, nil
		}
	}

	if nameErr != nil {
		return Character{}, false, nameErr
	}
	created, err := s.createLocked(owner, accepted, folded)
	if err != nil {
		return Character{}, false, err
	}
	return created, true, nil
}

// Characters is every character owner holds on this world, lowest id first.
//
// **A map hit and a copy, never a walk of the directory.** The index is built once when
// the store is opened; a join that scanned the players directory would cost an account
// with one character a read of every character in the world.
//
// The order is by id and is this store's own: it is not creation order, not recency and
// not anything a caller may read meaning into — it exists so that two calls answer the
// same way, which is what makes a resolution deterministic.
func (s *Store) Characters(owner identity.PlayerID) []Character {
	if s == nil {
		return nil
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	ids := s.byOwner[owner]
	if len(ids) == 0 {
		return nil
	}
	held := make([]Character, 0, len(ids))
	for _, id := range ids {
		if character, known := s.byID[id]; known {
			held = append(held, character)
		}
	}
	sort.Slice(held, func(a, b int) bool { return held[a].ID < held[b].ID })
	return held
}

// Character reports the character id names, and whether this world has one.
func (s *Store) Character(id CharacterID) (Character, bool) {
	if s == nil {
		return Character{}, false
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	character, known := s.byID[id]
	return character, known
}

// Named reports the character wearing name, under the same fold uniqueness is decided
// on — so a lookup finds the character whether or not the caller matched its case.
func (s *Store) Named(name string) (Character, bool) {
	if s == nil {
		return Character{}, false
	}

	_, folded, err := acceptName(name)
	if err != nil {
		// A name this world would refuse cannot be worn by a character, so nothing can
		// match it. Reported as absent rather than as an error: this is a lookup, and
		// the refusal belongs to Create, which is where it can be acted on.
		return Character{}, false
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	id, taken := s.byName[folded]
	if !taken {
		return Character{}, false
	}
	character, known := s.byID[id]
	return character, known
}

// Count is how many characters this world holds, across every account.
func (s *Store) Count() int {
	if s == nil {
		return 0
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.byID)
}

// mintLocked returns an id no character on this world has. The caller holds s.mu.
//
// Random rather than a counter, and the reason is what a counter would cost: a
// monotonic id is a second piece of state that has to survive a restart, be written
// atomically, and be right after a crash — a whole store to keep a number in. Sixty-four
// random bits collide with a probability nothing here will ever meet, and the re-roll
// below makes "will never" into "cannot", because the index is what decides.
//
// **A failing entropy source is an error and never a fallback.** A minted id that
// repeats is a character written over another character.
func (s *Store) mintLocked() (CharacterID, error) {
	var raw [8]byte
	for attempt := 0; attempt < mintAttempts; attempt++ {
		if _, err := rand.Read(raw[:]); err != nil {
			return 0, fmt.Errorf("persist: minting a character id: %w", err)
		}
		id := CharacterID(binary.LittleEndian.Uint64(raw[:]))
		if id.IsZero() {
			// Reserved by the contract: zero names no character anywhere.
			continue
		}
		if _, taken := s.byID[id]; !taken {
			return id, nil
		}
	}
	// Unreachable short of a broken entropy source, which is exactly the case where
	// carrying on would overwrite somebody.
	return 0, fmt.Errorf("persist: no free character id after %d attempts", mintAttempts)
}

// mintAttempts bounds the re-roll above. It is not a tuning knob: one attempt is
// overwhelmingly enough, and the only way to reach the last one is an entropy source
// that has stopped varying.
const mintAttempts = 8

// insertLocked puts a character into all three indexes. The caller holds s.mu.
func (s *Store) insertLocked(character Character) {
	s.byID[character.ID] = character
	s.byName[fold(character.Name)] = character.ID
	s.byOwner[character.Owner] = append(s.byOwner[character.Owner], character.ID)
}

// acceptName decides whether a name may be worn on this world, returning the text that
// is stored and the folded form uniqueness is decided on.
//
// **This is authoritative logic and the rule is deliberately not in the contract.**
// schemas/handshake.fbs says so in as many words: what names a server accepts is a
// decision, and `ServerReject.detail` is where a server says which rule was broken.
// A client may offer any bytes it likes; this is what decides.
//
// The rules, and why each one:
//
//   - Surrounding whitespace is trimmed rather than refused. It is almost always a
//     paste, and refusing it would be a rule about typing rather than about names.
//   - Empty is refused. "A character with no name is a store that has lost one."
//   - Longer than [MaxNameBytes] is refused whole. See that constant.
//   - Invalid UTF-8 is refused: a name is display text, and text that does not decode
//     is not a name anybody chose.
//   - Control characters are refused anywhere in the name, the space rune included
//     only where it is doubled — a name carrying a newline or a terminal escape is a
//     name that rewrites the log line an operator reads it in.
//
// The fold is [strings.ToLower], so "Eivor" and "eivor" are one name. Uniqueness that
// was case-sensitive would let a second character stand beside the first wearing a
// name nobody could tell apart in a chat line, which is impersonation with extra steps.
func acceptName(name string) (accepted, folded string, err error) {
	accepted = strings.TrimSpace(name)

	switch {
	case accepted == "":
		return "", "", fmt.Errorf("%w: a character needs a name", ErrNameRefused)
	case len(accepted) > MaxNameBytes:
		return "", "", fmt.Errorf("%w: %d bytes is longer than the %d a name may be", ErrNameRefused, len(accepted), MaxNameBytes)
	case !utf8.ValidString(accepted):
		return "", "", fmt.Errorf("%w: a name has to be text", ErrNameRefused)
	}
	for _, r := range accepted {
		if unicode.IsControl(r) {
			// The rune itself is never quoted back: it is what would break the line this
			// is about to be logged on.
			return "", "", fmt.Errorf("%w: a name may not contain control characters", ErrNameRefused)
		}
	}
	return accepted, fold(accepted), nil
}

// fold is the form two names are compared in. One function rather than a ToLower at
// each call site, because the property being kept is that the index and every lookup
// agree — and two spellings of the fold is one bug away from a name that is taken and
// cannot be found.
func fold(name string) string { return strings.ToLower(name) }

// parseCharacterID reads the id a record file is named for, and reports whether the
// name is one this store could have written.
//
// Exactly sixteen lowercase hex characters, because that is what [CharacterID.String]
// produces. Anything else is a file this store did not create, and the startup scan
// sets it aside rather than guessing at it.
func parseCharacterID(base string) (CharacterID, bool) {
	if len(base) != 16 || strings.ToLower(base) != base {
		// Lowercase, explicitly: hex.DecodeString takes either case, and a record
		// written under an uppercase name is a second name for the same id — one file
		// on a case-insensitive filesystem and two on this one.
		return 0, false
	}
	raw, err := hex.DecodeString(base)
	if err != nil || len(raw) != 8 {
		return 0, false
	}
	id := CharacterID(binary.BigEndian.Uint64(raw))
	return id, !id.IsZero()
}
