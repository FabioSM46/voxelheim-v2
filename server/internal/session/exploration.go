package session

import (
	"cmp"
	"errors"
	"log/slog"
	"slices"
	"sync"

	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Exploration is where one character has been, for as long as that character exists.
//
// # What "explored" means here
//
// **A column is explored once it has been streamed to this character**, and that is the
// only definition this server can enforce. It is not "looked at" and not "walked
// through": the client is where a camera lives and where a pair of feet is drawn, and
// neither is something the server may be told about. What the server knows on its own
// is which chunks it has put on the wire — [View.MarkLoaded] is the moment — and a
// chunk it has sent is terrain the player had in front of them. It also costs one map
// insert on a path that was already doing a map insert.
//
// The unit is the chunk column rather than the chunk, because a character who has been
// somewhere has been there at every height; schemas/world.fbs says the same thing about
// `MapColumn`, and it is why the whole vertical stack of a view cube adds one entry
// rather than seven.
//
// # Why it lives on the session
//
// The dependency direction here is session → persist → world, and internal/game never
// imports persist. A ledger in game.Life would either break that or make the tick loop
// learn about map state; a ledger in persist alone would have nowhere to be revealed
// from. The code that knows a chunk reached a client is in this package, so this is
// where the set is.
//
// # Concurrency
//
// Two goroutines reach one of these. Columns are revealed from the streaming goroutine,
// through the callback [View.MarkLoaded] runs; the ledger is read from the session's
// own goroutine, both to send the pages that follow the welcome and — from #450 on — to
// answer whether a tile's columns may be drawn. So it carries its own mutex, and
// [View.MarkLoaded] deliberately calls out to it with the view's lock released.
//
// A nil *Exploration is inert in every method, which is what a test that is about
// streaming rather than about the map gets.
type Exploration struct {
	// store is the directory this ledger is written to, or nil in an ephemeral world.
	store *persist.ExplorationStore

	// character names the file. A character id is stable for that character's life, so
	// this is the same path on every connection.
	character persist.CharacterID

	// log is where the one operational event this type has is reported: the cap.
	log *slog.Logger

	mu sync.Mutex

	// columns is the whole ledger. A set rather than a slice because the question asked
	// of it thousands of times is membership, and because a column revealed twice must
	// cost nothing.
	columns map[world.Column]struct{}

	// revealed is what has been added since the last time anything was sent — the
	// "newly revealed" batch, drained by [Exploration.TakeRevealed] after each view
	// diff. Append-only between drains, and never holding a column twice, because a
	// column is appended only on the insert that added it to columns.
	revealed []world.Column

	// dirty says the set has changed since the last successful save. An unchanged
	// ledger costs no write, which matters because the autosave visits every connected
	// player and a character standing still explores nothing.
	dirty bool

	// sealed says nothing may be written to this character's file for the rest of the
	// session, because the bytes already in it are evidence. It is set when a ledger
	// could not be read *and* could not be moved out of the way: the session then plays
	// with an empty set, and the one thing it must not do is have its first save
	// overwrite the file nobody could read. See [recallExploration].
	sealed bool

	// warned says the cap has already been reported. Once per session, because the
	// alternative is a Warn line per chunk crossing for the rest of that session.
	warned bool
}

// newExploration builds a live ledger over a stored one.
//
// stored is taken as given: it is the file's own contents, deduplicated into the set by
// construction, and nothing here judges a column. A nil logger discards.
func newExploration(store *persist.ExplorationStore, character persist.CharacterID, stored []world.Column, sealed bool, log *slog.Logger) *Exploration {
	if log == nil {
		log = slog.New(slog.DiscardHandler)
	}

	columns := make(map[world.Column]struct{}, len(stored))
	for _, column := range stored {
		if len(columns) >= persist.MaxExploredColumns {
			// Unreachable through a file this build wrote — the writer refuses to
			// exceed the cap and the reader refuses a file that declares more — and
			// checked anyway, because the alternative is that a way past the bound
			// exists and it is the one path nobody looks at.
			break
		}
		columns[column] = struct{}{}
	}

	return &Exploration{
		store:     store,
		character: character,
		log:       log,
		columns:   columns,
		sealed:    sealed,
	}
}

// Reveal records that a chunk column has been streamed to this character.
//
// Idempotent, and that is what makes it safe on the path it is on: a view diff re-sends
// a chunk whose delivery could not be confirmed, and a column already in the ledger adds
// nothing to the set and nothing to the batch. The vertical chunks of one column
// collapse the same way.
//
// **At the cap nothing more is recorded and the session says so once.** The character
// keeps playing and keeps being streamed terrain; what stops is the map growing. See
// persist.MaxExploredColumns for why the number is far past any history this game
// produces.
func (e *Exploration) Reveal(column world.Column) {
	if e == nil {
		return
	}

	e.mu.Lock()
	defer e.mu.Unlock()

	if _, known := e.columns[column]; known {
		return
	}
	if len(e.columns) >= persist.MaxExploredColumns {
		if !e.warned {
			e.warned = true
			e.log.Warn("this character has explored as many chunk columns as one ledger can hold; the map stops growing here",
				"character", e.character.String(),
				"columns", len(e.columns),
				"max_columns", persist.MaxExploredColumns)
		}
		return
	}

	e.columns[column] = struct{}{}
	e.revealed = append(e.revealed, column)
	e.dirty = true
}

// Explored reports whether this character has been sent the chunk column at col.
//
// The question #450's tile drawing asks, one column at a time, on the session
// goroutine: a pixel inside a column this answers false for carries nothing at all in
// the tile, because a client is not where a secret is kept.
func (e *Exploration) Explored(col world.Column) bool {
	if e == nil {
		return false
	}

	e.mu.Lock()
	defer e.mu.Unlock()
	_, known := e.columns[col]
	return known
}

// Count is how many columns the ledger holds.
func (e *Exploration) Count() int {
	if e == nil {
		return 0
	}

	e.mu.Lock()
	defer e.mu.Unlock()
	return len(e.columns)
}

// Snapshot is the whole ledger, in the one order this package produces.
//
// The whole of it rather than a page, because paging is [sendExplored]'s job and an
// order is easier to be right about in one place. Allocated fresh, so a caller can hold
// it while the streaming goroutine goes on revealing columns.
func (e *Exploration) Snapshot() []world.Column {
	if e == nil {
		return nil
	}

	e.mu.Lock()
	defer e.mu.Unlock()
	return e.sorted()
}

// TakeRevealed drains the batch of columns added since the last drain, in the same
// order [Exploration.Snapshot] uses. Empty when nothing was revealed, which is the
// ordinary case for a player standing still.
func (e *Exploration) TakeRevealed() []world.Column {
	if e == nil {
		return nil
	}

	e.mu.Lock()
	defer e.mu.Unlock()

	if len(e.revealed) == 0 {
		return nil
	}
	batch := e.revealed
	// Released rather than truncated: the batch is handed to a caller that keeps it
	// until the frames are built, so reusing the array would let the next reveal write
	// into a slice somebody is still encoding.
	e.revealed = nil

	slices.SortFunc(batch, compareColumns)
	return batch
}

// sorted is the ledger as a sorted slice. The caller holds the lock.
func (e *Exploration) sorted() []world.Column {
	columns := make([]world.Column, 0, len(e.columns))
	for column := range e.columns {
		columns = append(columns, column)
	}
	slices.SortFunc(columns, compareColumns)
	return columns
}

// compareColumns orders by cz, then cx — rows of the map, north to south, west to east.
//
// Deterministic because a map's iteration order is not, and everything downstream is
// easier to be exact about for it: a page boundary falls in the same place twice, a
// stored file is byte-identical for an unchanged ledger, and a test can assert the
// frames rather than their contents as a set.
//
// cmp.Compare rather than subtraction, for the reason compareCoords uses it: the
// difference of two int32 coordinates can overflow, and the sort would then order them
// backwards.
func compareColumns(a, b world.Column) int {
	if d := cmp.Compare(a.CZ, b.CZ); d != 0 {
		return d
	}
	return cmp.Compare(a.CX, b.CX)
}

// Save writes the ledger down, and writes nothing when nothing has changed.
//
// Called from exactly where a player record is written — the teardown, and the autosave
// pass — so that "the map is as durable as the life" needs no second schedule and no
// second decision about when it is safe to touch a file. Both of those already run off
// the tick goroutine, which is the constraint that matters: no I/O on the tick.
//
// **The dirty flag is cleared before the write rather than after**, which is
// world.Cache.takeDirty's ordering and is chosen for the same reason: a reveal landing
// in between re-marks the ledger, so the worst case is writing the same bytes twice. The
// other order has a window in which a column is in neither the file being written nor
// the set of things still to write. A failed write re-marks it too, so the next save
// retries rather than believing a write that did not land.
func (e *Exploration) Save() error {
	if e == nil {
		return nil
	}

	e.mu.Lock()
	if e.sealed || !e.dirty {
		e.mu.Unlock()
		return nil
	}
	columns := e.sorted()
	e.dirty = false
	e.mu.Unlock()

	if err := e.store.Save(e.character, columns); err != nil {
		e.mu.Lock()
		e.dirty = true
		e.mu.Unlock()
		return err
	}
	return nil
}

// recallExploration loads a character's ledger and answers with the live set built over
// it. It never fails: a character whose map cannot be read plays with a blank one.
//
// **The asymmetry with [Identities.recall] is deliberate and is about what is at
// stake.** An unreadable *life* refuses the connection when it cannot be set aside,
// because the session that followed would write its own life over the only evidence of
// the one that was lost. An unreadable *map* costs the player the fog they had lifted
// and nothing else — no items, no position, no progress — and refusing to let somebody
// into the world over it would be a larger loss than the one being protected against.
// So the connection proceeds, and the evidence is protected the other way: the file is
// set aside if it can be, and the ledger is sealed against writing if it cannot.
//
// That is the structures precedent read one level down. `restoreStructures` survives a
// camp it cannot read and leaves the file where it is because nothing rewrites it; a
// ledger *is* rewritten by the session that could not read it, so "leave it alone" has
// to be said explicitly rather than being a consequence of nobody touching it.
func (i *Identities) recallExploration(character persist.Character) *Exploration {
	stored, found, err := i.exploration.Load(character.ID)
	switch {
	case err == nil:
		if !found {
			// A character who has walked nowhere, or one from before this file existed.
			// Both are an empty map and neither is an event.
			return newExploration(i.exploration, character.ID, nil, false, i.log)
		}
		return newExploration(i.exploration, character.ID, stored, false, i.log)

	case !errors.Is(err, world.ErrCorruptStore):
		// A permission, a failing disk: the file may well be readable later, and this
		// session must not be the thing that replaces it. Sealed, blank, and loud.
		i.log.Error("this character's exploration ledger could not be read; the map starts blank and nothing will be written over the file",
			"player_id", character.Owner.Short(), "character", character.ID.String(), "error", err)
		return newExploration(i.exploration, character.ID, nil, true, i.log)
	}

	aside, moveErr := i.exploration.Quarantine(character.ID)
	if moveErr != nil {
		i.log.Error("this character's exploration ledger could not be read and could not be set aside; the map starts blank and nothing will be written over the file",
			"player_id", character.Owner.Short(), "character", character.ID.String(),
			"reason", err.Error(), "error", moveErr)
		return newExploration(i.exploration, character.ID, nil, true, i.log)
	}

	// Warn rather than Error: the file is safe, the character keeps everything that
	// matters, and what was lost is a map they can walk again.
	i.log.Warn("this character's exploration ledger could not be read; it has been kept and the map starts blank",
		"player_id", character.Owner.Short(), "character", character.ID.String(),
		"reason", err.Error(), "kept_at", aside)
	return newExploration(i.exploration, character.ID, nil, false, i.log)
}

// sendExplored puts a ledger on the wire as MapExplored pages, and sends nothing at all
// for an empty one.
//
// **Empty is not a message.** schemas/world.fbs requires `MapExplored.columns` to be
// present and non-empty, and says why: the ledger is additive, so an empty page states
// nothing and a client that read one as "the ledger is empty" would erase its own map.
//
// The page size is protocol.MaxExploredColumns, which is the contract's bound on one
// frame rather than on the ledger — persist.MaxExploredColumns is the other number, and
// it is sixteen times larger. Paging exists because of exactly that gap: a character
// with a long history has more columns than one frame should carry, and the last page is
// deliberately not an end marker, because a character who keeps walking keeps producing
// them.
func sendExplored(send func([]byte) error, columns []world.Column) error {
	for start := 0; start < len(columns); start += protocol.MaxExploredColumns {
		end := min(start+protocol.MaxExploredColumns, len(columns))

		page := make([]protocol.MapColumn, 0, end-start)
		for _, column := range columns[start:end] {
			page = append(page, protocol.MapColumn{CX: column.CX, CZ: column.CZ})
		}
		if err := send(protocol.EncodeMapExplored(protocol.MapExplored{Columns: page})); err != nil {
			return err
		}
	}
	return nil
}
