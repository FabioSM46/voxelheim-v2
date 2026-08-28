package session

import (
	"errors"
	"log/slog"
	"slices"
	"sync"
	"unicode/utf8"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Markers is the sixty-four marks one character may put on their own map.
//
// # Why it lives on the session
//
// Nothing in internal/game reads a mark and no outcome depends on one: a mark changes
// no position, no health and no pack, and a character with sixty-four of them plays
// exactly like a character with none. So the dependency direction here is
// session → persist → world and game is untouched, which is the same place the
// exploration ledger landed and for the same reason — the state has a file and a wire
// message and no simulation half at all.
//
// # Concurrency
//
// It carries a mutex, like [Exploration], and for a narrower reason: every mutation
// arrives on the session's own goroutine, but [Markers.Save] is called from the autosave
// pass, which runs on the worker that visits every connected player. One writer and one
// occasional reader is still two goroutines.
//
// A nil *Markers is inert in every method — an empty list, every placement refused as a
// map that is full — which is what a test about something else gets.
type Markers struct {
	// store is the directory this map is written to, or nil in an ephemeral world.
	store *persist.MarkerStore

	// character names the file. A character id is stable for that character's life, so
	// this is the same path on every connection.
	character persist.CharacterID

	// log is where the operational events this type has are reported: a save that could
	// not happen, and a file that could not be read.
	log *slog.Logger

	mu sync.Mutex

	// markers is the whole map, in placement order.
	//
	// A slice rather than a map keyed by id, because sixty-four is small and every
	// question asked of it — the whole list, for the wire — wants an order. Placement
	// order is the one this package produces and the one the file preserves; a client
	// that draws them in the order it was given draws the oldest mark first.
	markers []protocol.Marker

	// nextID is the id the next placement takes, and it only ever goes up.
	//
	// **Never derived from the marks it holds.** max(id)+1 falls back the moment the
	// highest-numbered mark is removed, and the next placement would then mint an id the
	// client has already been told means something else — the removal and the placement
	// racing in a client's own list. Stored in the file's header for exactly that reason.
	nextID uint64

	// dirty says the map has changed since the last successful save. An unchanged map
	// costs no write, which matters because the autosave visits every connected player
	// and marks are placed a handful of times in a session at most.
	dirty bool

	// sealed says nothing may be written to this character's file for the rest of the
	// session, because the bytes already in it are evidence. Set when a map could not be
	// read *and* could not be moved out of the way: the session then plays with an empty
	// list, and the one thing it must not do is have its first save overwrite the file
	// nobody could read. The shape [Exploration] settled — see [recallMarkers].
	sealed bool
}

// newMarkers builds a live map over a stored one.
//
// stored is taken as given: persist.MarkerStore.Load has already refused a file that
// could not produce a legal `MarkerList`, so what arrives here is a list of marks with
// unique non-zero ids, known kinds and notes that fit. A nil logger discards.
//
// **The counter is floored at one past the highest id present**, which is belt and
// braces over a check the decoder already makes: a stored counter that was somehow
// behind a stored id would mint a duplicate on the very first placement, and this is the
// last place that can notice before it does.
func newMarkers(store *persist.MarkerStore, character persist.CharacterID, stored persist.StoredMarkers, sealed bool, log *slog.Logger) *Markers {
	if log == nil {
		log = slog.New(slog.DiscardHandler)
	}

	markers := slices.Clone(stored.Markers)
	if len(markers) > persist.MaxMarkers {
		// Unreachable through a file this build wrote — the writer refuses to exceed the
		// cap and the reader refuses a file that declares more — and truncated anyway,
		// because the alternative is that a way past the bound exists and it is the one
		// path nobody looks at.
		markers = markers[:persist.MaxMarkers]
	}

	nextID := max(stored.NextID, 1)
	for _, marker := range markers {
		nextID = max(nextID, marker.MarkerID+1)
	}

	return &Markers{
		store:     store,
		character: character,
		log:       log,
		markers:   markers,
		nextID:    nextID,
		sealed:    sealed,
	}
}

// List is the whole map, in placement order, as the wire carries it.
//
// Allocated fresh on every call, because the caller hands it to an encoder while this
// session's next message may already be placing another mark. Sixty-four small structs
// is a cost worth paying not to have to reason about that.
func (m *Markers) List() protocol.MarkerList {
	if m == nil {
		return protocol.MarkerList{}
	}

	m.mu.Lock()
	defer m.mu.Unlock()
	return protocol.MarkerList{Markers: slices.Clone(m.markers)}
}

// Count is how many marks the character holds.
func (m *Markers) Count() int {
	if m == nil {
		return 0
	}

	m.mu.Lock()
	defer m.mu.Unlock()
	return len(m.markers)
}

// Place puts one mark on this character's map and answers with the whole list.
//
// **Every bound is checked here regardless of what the client checked, and two of them
// are checked again rather than for the first time.** protocol.Decode already refuses a
// note over 120 bytes, a note that is not valid UTF-8 and a kind this contract does not
// name — at the decode boundary, by closing the session, which schemas/player.fbs names
// as the stricter of the two answers it allows. So `NoteTooLong` and a refused kind are
// unreachable over the wire and are still the answer here: this method is the authority
// on what may go into the file, and an authority that trusts its caller is not one. The
// refusal reason is what a client would be told if the decoder ever stopped closing the
// session over it.
//
// The reason is [vnet.RefusalReasonUnknown] when the refusal has no member in the
// contract, which is the vocabulary every other admission in this server uses for "no
// frame, a debug line, and nothing else". Placing a mark outside the world is the one
// refusal in that shape — see [markerOutsideWorld].
func (m *Markers) Place(request protocol.MarkerPlaceRequest) (protocol.MarkerList, vnet.RefusalReason, error) {
	if m == nil {
		// Unreachable: Resolved.Marks is never nil, for the reason Resolved.Explored is
		// never nil. Stated rather than dereferenced, and silent rather than refused with
		// a code, because there is no true sentence to send a client about a map this
		// server failed to build.
		return protocol.MarkerList{}, vnet.RefusalReasonUnknown, errMarkersUnavailable
	}

	if !protocol.MarkerKindOK(request.Kind) {
		return protocol.MarkerList{}, vnet.RefusalReasonMalformedKind, errMarkerKind
	}
	if len(request.Note) > persist.MaxMarkerNote {
		return protocol.MarkerList{}, vnet.RefusalReasonNoteTooLong, errMarkerNoteTooLong
	}
	if !utf8.ValidString(request.Note) {
		// One reason for both, and the contract says so: `NoteTooLong` is what a client
		// is told about a note it may not store, and "not valid UTF-8" is not a length.
		// The distinction is one no correct client can produce and no player can act on
		// differently, so a second member would be a wire distinction with no sentence
		// behind it.
		return protocol.MarkerList{}, vnet.RefusalReasonNoteTooLong, errMarkerNoteEncoding
	}
	if markerOutsideWorld(request.X, request.Z) {
		return protocol.MarkerList{}, vnet.RefusalReasonUnknown, errMarkerOutsideWorld
	}

	m.mu.Lock()
	defer m.mu.Unlock()

	if len(m.markers) >= persist.MaxMarkers {
		return protocol.MarkerList{Markers: slices.Clone(m.markers)}, vnet.RefusalReasonTooManyMarkers, errTooManyMarkers
	}

	m.markers = append(m.markers, protocol.Marker{
		MarkerID: m.nextID,
		X:        request.X,
		Z:        request.Z,
		Kind:     request.Kind,
		// Copied verbatim, exactly as the decoder handed it over. What a player typed is
		// theirs; this server stores it and never edits it.
		Note: request.Note,
	})
	m.nextID++
	m.dirty = true

	return protocol.MarkerList{Markers: slices.Clone(m.markers)}, vnet.RefusalReasonUnknown, nil
}

// Remove takes one of this character's marks off the map and answers with the whole
// list.
//
// **A mark that never existed and a mark somebody else owns are one answer**, and there
// is nothing here that could tell them apart in the first place: this list is one
// character's and no other character's marks are reachable from it. That is the design
// schemas/player.fbs records for `MarkerUnknown` — a client that could distinguish the
// two would learn which ids exist by naming ids it was never given.
func (m *Markers) Remove(markerID uint64) (protocol.MarkerList, vnet.RefusalReason, error) {
	if m == nil {
		// Unreachable for the reason Place's guard is, and silent for the same one.
		return protocol.MarkerList{}, vnet.RefusalReasonUnknown, errMarkersUnavailable
	}

	m.mu.Lock()
	defer m.mu.Unlock()

	at := slices.IndexFunc(m.markers, func(marker protocol.Marker) bool {
		return marker.MarkerID == markerID
	})
	if at < 0 {
		return protocol.MarkerList{Markers: slices.Clone(m.markers)}, vnet.RefusalReasonMarkerUnknown, errMarkerUnknown
	}

	// The counter is deliberately untouched: an id is never reused within a character,
	// so removing the newest mark does not make its id available again.
	m.markers = slices.Delete(m.markers, at, at+1)
	m.dirty = true

	return protocol.MarkerList{Markers: slices.Clone(m.markers)}, vnet.RefusalReasonUnknown, nil
}

// Save writes the map down, and writes nothing when nothing has changed.
//
// Called from exactly where a player record and an exploration ledger are written — the
// teardown, and the autosave pass — so that "the map is as durable as the life" needs no
// third schedule and no third decision about when it is safe to touch a file. Both of
// those already run off the tick goroutine, which is the constraint that matters: no I/O
// on the tick.
//
// **The dirty flag is cleared before the write rather than after**, which is
// [Exploration.Save]'s ordering and is chosen for the same reason: a placement landing in
// between re-marks the map, so the worst case is writing the same bytes twice. The other
// order has a window in which a mark is in neither the file being written nor the set of
// things still to write. A failed write re-marks it too, so the next save retries rather
// than believing a write that did not land.
func (m *Markers) Save() error {
	if m == nil {
		return nil
	}

	m.mu.Lock()
	if m.sealed || !m.dirty {
		m.mu.Unlock()
		return nil
	}
	stored := persist.StoredMarkers{NextID: m.nextID, Markers: slices.Clone(m.markers)}
	m.dirty = false
	m.mu.Unlock()

	if err := m.store.Save(m.character, stored); err != nil {
		m.mu.Lock()
		m.dirty = true
		m.mu.Unlock()
		return err
	}
	return nil
}

// markerOutsideWorld reports whether a mark's coordinates are outside the world this
// server owns.
//
// **This is the first coordinate a client has ever chosen**, which is why the check did
// not exist until there were marks. Everything a client had previously named came from
// terrain the server streamed it — a block edit's target, a structure's anchor, a chunk
// resend's coordinate — so "inside the world" was a property of the input rather than a
// question about it. `MarkerPlaceRequest` carries a bare x and z that nothing produced,
// and schemas/player.fbs states the bound it must satisfy without naming a number,
// because the number belongs to the server: world.BlockLimit, which internal/game reads
// under the name worldLimit and applies to the vertical too.
//
// Inclusive of the limit itself, which is the comparison game.Life.Validate makes about
// a stored position: the edge is where the world ends, and a mark exactly on it is a
// place a body may stand.
func markerOutsideWorld(x, z int32) bool {
	return int64(x) < -world.BlockLimit || int64(x) > world.BlockLimit ||
		int64(z) < -world.BlockLimit || int64(z) > world.BlockLimit
}

// The refusals, as sentences an operator reads in a debug line. Sentinels rather than
// formatted strings because every one of them is a fixed fact about the request, and
// because the caller logs the reason code beside them.
var (
	errMarkersUnavailable = errors.New("session: this character has no map to mark")
	errMarkerKind         = errors.New("session: a mark's kind must be one this contract names, and never Unknown")
	errMarkerNoteTooLong  = errors.New("session: a mark's note is longer than the bytes one may carry")
	errMarkerNoteEncoding = errors.New("session: a mark's note is not valid UTF-8")
	errMarkerOutsideWorld = errors.New("session: a mark must name a place inside the world")
	errTooManyMarkers     = errors.New("session: this character already holds as many marks as one map may carry")
	errMarkerUnknown      = errors.New("session: no mark of this character carries that id")
)

// recallMarkers loads a character's marks and answers with the live map built over them.
// It never fails: a character whose marks cannot be read plays with none.
//
// **The asymmetry with [Identities.recall] is [recallExploration]'s, and the stake is a
// little larger.** An unreadable *life* refuses the connection when it cannot be set
// aside, because the session that followed would write its own life over the only
// evidence of the one that was lost. An unreadable *map* costs the player fog they had
// lifted and, here, a page of their own writing — real, and still not a life, not an item
// and not a position. So the connection proceeds, and the evidence is protected the other
// way: the file is set aside if it can be, and the map is sealed against writing if it
// cannot.
func (i *Identities) recallMarkers(character persist.Character) *Markers {
	stored, found, err := i.markers.Load(character.ID)
	switch {
	case err == nil:
		if !found {
			// A character who has marked nothing, or one from before this file existed.
			// Both are an empty map and neither is an event.
			return newMarkers(i.markers, character.ID, persist.StoredMarkers{NextID: 1}, false, i.log)
		}
		return newMarkers(i.markers, character.ID, stored, false, i.log)

	case !errors.Is(err, world.ErrCorruptStore):
		// A permission, a failing disk: the file may well be readable later, and this
		// session must not be the thing that replaces it. Sealed, blank, and loud.
		i.log.Error("this character's marks could not be read; the map starts unmarked and nothing will be written over the file",
			"player_id", character.Owner.Short(), "character", character.ID.String(), "error", err)
		return newMarkers(i.markers, character.ID, persist.StoredMarkers{NextID: 1}, true, i.log)
	}

	aside, moveErr := i.markers.Quarantine(character.ID)
	if moveErr != nil {
		i.log.Error("this character's marks could not be read and could not be set aside; the map starts unmarked and nothing will be written over the file",
			"player_id", character.Owner.Short(), "character", character.ID.String(),
			"reason", err.Error(), "error", moveErr)
		return newMarkers(i.markers, character.ID, persist.StoredMarkers{NextID: 1}, true, i.log)
	}

	// Warn rather than Error: the file is safe, the character keeps everything that
	// matters, and what was lost is sixty-four lines they can type again.
	i.log.Warn("this character's marks could not be read; they have been kept and the map starts unmarked",
		"player_id", character.Owner.Short(), "character", character.ID.String(),
		"reason", err.Error(), "kept_at", aside)
	return newMarkers(i.markers, character.ID, persist.StoredMarkers{NextID: 1}, false, i.log)
}
