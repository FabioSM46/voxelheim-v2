package session

import (
	"errors"
	"fmt"
	"log/slog"
	"sync"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Refused is a handshake refusal the contract has a code for.
//
// Resolution has two failure modes and they are not the same thing. A token the
// client got wrong, and an identity that is already playing, are refusals: the
// client is told which, in a ServerReject, and the connection ends politely. A
// server that cannot mint an identity or cannot read its own player store is a
// *failure*, and RejectReason has no member for it — answering SERVER_FULL would
// tell the client something false about why, and BAD_REQUEST would blame it for
// something it did not do. So the first is this type and the second is an ordinary
// error, which ends the session with no reply at all.
type Refused struct {
	Reason vnet.RejectReason
	Detail string
}

func (r *Refused) Error() string {
	return fmt.Sprintf("session: handshake refused: %s: %s", r.Reason, r.Detail)
}

// Resolved is the identity a hello settled on, and how it got there.
type Resolved struct {
	// ID names the identity. It is what the player store keys on and what a log line
	// carries.
	ID identity.PlayerID

	// Token is what ServerWelcome carries back. For a returning player it is the
	// token they presented; for everyone else it is one this server has just minted.
	// Never the value a client sent that this server did not recognise.
	Token identity.Token

	// Returning reports whether the player store already held a usable record for ID.
	Returning bool

	// Life is what this identity left behind — where they stood, their health and their
	// pack — or nil for a player the store has never had a readable record for.
	//
	// **Loaded here, before the welcome is built**, because ServerWelcome.spawn has to
	// carry the position the player is actually placed at and Handshake is a pure
	// function that cannot go and find one. It is also the reason a refused record is
	// decided at this point and not at Join: by Join the client has already been told
	// where it stands.
	Life *game.Life
}

// Identities is the set of identities that have a live session, and the resolution
// that turns the token in a ClientHello into one of them.
//
// It sits beside Registry deliberately: Registry answers "which connections are
// live" one level down, in entity ids minted per session, and this answers the same
// question one level up, in identities that outlive a connection. Two sets rather
// than one because they have different lifetimes — an entity id is forgotten when a
// session ends, an identity is released and then remembered.
//
// The store may be nil, which is the ephemeral world. Minting and exclusivity work
// exactly as they do with one; nothing is written and nothing is ever found, so
// every presented token resolves to a new identity for the life of the process. A
// client cannot distinguish that from a server that has never seen it before, and
// the contract already requires it to accept a token it did not send.
type Identities struct {
	store *persist.Store

	// log is where a refused record is reported. It is the one thing here that has to
	// say something to an operator: a player who joins as new because their file could
	// not be read is indistinguishable, from the outside, from a player who has never
	// connected.
	log *slog.Logger

	// mint is identity.NewToken, replaced only by the test that covers a failed read
	// from crypto/rand — a branch that cannot be reached on any platform this server
	// runs on, and that must never be allowed to become a zero token.
	mint func() (identity.Token, error)

	// live is every identity with a session, and what that session needs remembered
	// about it. One map rather than a set and a table beside it: everything in
	// liveIdentity has exactly the claim's lifetime, and claim and Release are already
	// the two ends of it.
	mu   sync.Mutex
	live map[identity.PlayerID]*liveIdentity

	// writeMu serialises every record write, so that the last write for an identity is
	// the last *decision* about it rather than whichever goroutine's rename happened to
	// land second. See RememberAll.
	writeMu sync.Mutex
}

// liveIdentity is what one live session's identity carries beside the claim itself.
type liveIdentity struct {
	// name is the display name this session is playing under. The autosave needs it:
	// a record written mid-session must carry the same name the session's own teardown
	// would write, not blank it — and the simulation, which is what the autosave asks
	// for lives, has never heard of a display name.
	name string

	// finalised records that this session's teardown has already written its last
	// word. See RememberAll, which is the only reader.
	finalised bool
}

// NewIdentities returns an empty claim set over store, which may be nil for an
// ephemeral world. A nil logger discards, which is what the tests that are about
// admission rather than about operators want.
func NewIdentities(store *persist.Store, log *slog.Logger) *Identities {
	if log == nil {
		log = slog.New(slog.DiscardHandler)
	}
	return &Identities{
		store: store,
		log:   log,
		mint:  identity.NewToken,
		live:  make(map[identity.PlayerID]*liveIdentity),
	}
}

// Resolve settles which identity a hello is claiming, and claims it for this
// session.
//
// **Runs on the session goroutine, before Join, and never under the simulation's
// lock**: it reads the player store, and a tick that waits on a file is a tick every
// connected player misses.
//
// The order is the contract's, and each step is a different kind of answer:
//
//  1. A token of any length but 0 or 32 is BAD_REQUEST. Decided first, before any
//     identity is looked up, because it is a malformed request rather than a claim
//     that failed.
//  2. An empty token is a first connection: mint.
//  3. A 32-byte token whose hash the store knows resumes that identity, and the
//     welcome carries the token back unchanged.
//  4. A 32-byte token the store does not know mints a **new** identity with a **new**
//     token. The presented value is never adopted as a key — every token in
//     circulation is one this server minted, so a client cannot choose who it is by
//     inventing 32 bytes.
//  5. Whatever identity that produced is claimed. An identity that already has a live
//     session is ALREADY_CONNECTED, which only step 3 can reach: a minted token is 32
//     random bytes and collides with nothing.
//
// The caller releases the claim in its teardown, last. See Serve.
func (i *Identities) Resolve(hello *protocol.ClientHello) (Resolved, error) {
	if hello == nil {
		// Unreachable: Serve resolves only a message that decoded as a hello.
		return Resolved{}, errors.New("session: no hello to resolve an identity from")
	}

	var (
		token     identity.Token
		returning bool
		life      *game.Life
	)
	switch len(hello.PlayerToken) {
	case 0:
		// A first connection to this server, or a client that has thrown its token
		// away. Both mint below.

	case identity.TokenSize:
		presented, err := identity.TokenFrom(hello.PlayerToken)
		if err != nil {
			// Unreachable at this length; the case label is the check.
			return Resolved{}, fmt.Errorf("session: reading the presented token: %w", err)
		}

		// The hash is the only thing that leaves this function's sight: the store is
		// keyed by it and never by the token, so what is on disk cannot be replayed as
		// a credential.
		stored, known, err := i.recall(identity.IDOf(presented))
		if err != nil {
			return Resolved{}, err
		}
		if known {
			token, returning, life = presented, true, stored
		}

	default:
		return Resolved{}, &Refused{
			Reason: vnet.RejectReasonBAD_REQUEST,
			// The length and nothing else. A detail carrying any of the bytes would be
			// a token in a log line the first time this refusal was investigated.
			Detail: fmt.Sprintf("player_token must be absent, empty or %d bytes, got %d",
				identity.TokenSize, len(hello.PlayerToken)),
		}
	}

	if !returning {
		minted, err := i.mint()
		if err != nil {
			return Resolved{}, fmt.Errorf("session: minting an identity: %w", err)
		}
		token = minted
	}

	id := identity.IDOf(token)
	if !i.claim(id) {
		return Resolved{}, &Refused{
			Reason: vnet.RejectReasonALREADY_CONNECTED,
			Detail: "that identity already has a live session",
		}
	}
	return Resolved{ID: id, Token: token, Returning: returning, Life: life}, nil
}

// recall loads the life stored for id: the life itself, whether one was found, and any
// reason this session cannot proceed.
//
// Asked as a load rather than as a stat, because the two differ on the case that
// matters: a record that exists and cannot be read is not a first connection.
//
// **Three answers, and the middle one is where the rule changed.** A file that is
// simply absent is a first connection. A file that cannot be *reached* — a permission,
// a failing disk — is an error and refuses the connection, because a retry may well
// succeed and treating it as absent would throw away a readable life on a transient
// fault. A file that is *corrupt* — the wrong magic, a version this build does not
// speak, a broken checksum, or a life whose values game refuses — will never be
// readable, so refusing the connection for ever helps nobody: it is set aside under a
// name of its own and the player is admitted as new.
//
// #146 could not do that, and said so: a corrupt record read as "not found" would have
// been written over by the new identity's first teardown, turning one bad file into a
// lost player. Two things close that. The file is moved before this returns, and the
// identity that gets minted is a *different* one — 32 fresh random bytes, a different
// hash, a different file name — so nothing this session goes on to write can land on
// the record nobody could read, whether or not the move succeeded.
func (i *Identities) recall(id identity.PlayerID) (*game.Life, bool, error) {
	rec, found, err := i.store.Load(id)
	switch {
	case err != nil && !errors.Is(err, world.ErrCorruptStore):
		return nil, false, fmt.Errorf("session: reading the player record for %s: %w", id.Short(), err)
	case err != nil:
		i.refuseRecord(id, err)
		return nil, false, nil
	case !found:
		return nil, false, nil
	}

	// The store wrote these numbers down; it did not vouch for them. What an item id
	// means and how much health is a full bar are game's answers, so they are asked
	// here — once, before anything is built from them, and about the whole record
	// rather than slot by slot.
	life := game.Life{Pos: rec.Pos, Yaw: rec.Yaw, Health: rec.Health, Slots: rec.Slots}
	if vErr := life.Validate(); vErr != nil {
		i.refuseRecord(id, vErr)
		return nil, false, nil
	}
	return &life, true, nil
}

// refuseRecord sets a record this build cannot use aside and says so, loudly.
//
// Error level and not warn: a player is about to lose everything they had, and the
// only reason it is not a refused connection is that refusing would not give it back.
// A failed move is reported and not fatal — see recall for why the identity minted
// next cannot write over the file either way.
func (i *Identities) refuseRecord(id identity.PlayerID, cause error) {
	aside, err := i.store.Quarantine(id)
	if err != nil {
		i.log.Error("a player record could not be read and could not be set aside; the player joins as new",
			"player_id", id.Short(), "reason", cause.Error(), "error", err)
		return
	}
	i.log.Error("a player record could not be read; it has been kept and the player joins as new",
		"player_id", id.Short(), "reason", cause.Error(), "kept_at", aside)
}

// Admitted records the display name an identity's live session is playing under.
//
// Called once, when the handshake is accepted. It exists for the autosave: a record
// written while a session is still running has to carry the same name that session's
// own teardown would write, and the simulation — which is what the autosave asks for
// lives — has never heard of a display name.
func (i *Identities) Admitted(id identity.PlayerID, name string) {
	i.mu.Lock()
	defer i.mu.Unlock()

	if held, live := i.live[id]; live {
		held.name = name
	}
}

// Remember writes the record one identity's session leaves behind, or has reached so
// far. A no-op in an ephemeral world.
//
// Called from Serve's teardown, after the player has left the simulation and before
// the claim is released — so a client reconnecting the instant this returns is never
// served a record that is still being written — and from the autosave, for every
// player still connected.
//
// **No lock the simulation cares about is held while it writes**, which is the whole
// reason it takes a captured life rather than a *game.Player: game.Player.Record does
// the capture under the simulation's lock and returns, and the file is written out here
// with only this type's own write lock held.
//
// **It is the session's last word**, so it marks the identity finalised: an autosave
// that captured this player before they left must not land afterwards and put the older
// life back. See RememberAll.
func (i *Identities) Remember(id identity.PlayerID, life game.Life) error {
	i.writeMu.Lock()
	defer i.writeMu.Unlock()

	err := i.write(id, life)
	i.finalise(id)
	return err
}

// RememberAll writes a record for every identity in lives that still has a session
// running, and reports every failure rather than the first.
//
// **The two skips are what keep an autosave from undoing a disconnect.** A pass
// captures every connected player and then writes them one at a time, so a session can
// end in the middle of one — and the life this pass is holding for that player is then
// older than the one their teardown wrote. Skipping an identity that is no longer live,
// or whose teardown has already written its last word, is what makes the teardown's
// record the final one. The whole pass runs under the same write lock Remember takes,
// so a teardown lands either entirely before it — and is skipped — or entirely after,
// and wins.
//
// Without that, a player who died and quit inside one pass would be restored from a
// life captured before the death: the durability penalty unpaid, which is exactly the
// escape Player.Record exists to close.
//
// One player's disk error is not a reason to skip the others: the whole point of the
// autosave is the crash that has not happened yet, and a run that stopped at the first
// bad write would leave every player after it in map order unsaved.
func (i *Identities) RememberAll(lives map[identity.PlayerID]game.Life) error {
	i.writeMu.Lock()
	defer i.writeMu.Unlock()

	var errs []error
	for id, life := range lives {
		if !i.stillPlaying(id) {
			continue
		}
		if err := i.write(id, life); err != nil {
			errs = append(errs, err)
		}
	}
	return errors.Join(errs...)
}

// write puts one life on disk under the name its session is playing under. The caller
// holds writeMu.
func (i *Identities) write(id identity.PlayerID, life game.Life) error {
	return i.store.Save(id, persist.Record{
		Name: i.nameOf(id),
		// When this record was written, which is the end of the session on the teardown
		// path and the moment of the pass on the autosave's. Both are "the last time
		// this server knew anything about this player", which is what the field means.
		LastSeen: time.Now().UTC(),
		Pos:      life.Pos,
		Yaw:      life.Yaw,
		Health:   life.Health,
		Slots:    life.Slots,
	})
}

// stillPlaying reports whether id has a live session that has not yet written its last
// record.
func (i *Identities) stillPlaying(id identity.PlayerID) bool {
	i.mu.Lock()
	defer i.mu.Unlock()

	held, live := i.live[id]
	return live && !held.finalised
}

// finalise records that id's session has written its last word.
func (i *Identities) finalise(id identity.PlayerID) {
	i.mu.Lock()
	defer i.mu.Unlock()

	if held, live := i.live[id]; live {
		held.finalised = true
	}
}

// nameOf is the display name id's live session is playing under, and empty for an
// identity with none — which includes one whose claim has already been released.
func (i *Identities) nameOf(id identity.PlayerID) string {
	i.mu.Lock()
	defer i.mu.Unlock()

	if held, live := i.live[id]; live {
		return held.name
	}
	return ""
}

// claim adds id to the live set, answering false when it is already there.
func (i *Identities) claim(id identity.PlayerID) bool {
	i.mu.Lock()
	defer i.mu.Unlock()

	if _, live := i.live[id]; live {
		return false
	}
	i.live[id] = &liveIdentity{}
	return true
}

// Release ends a session's exclusive hold on its identity.
//
// **Last in Serve's teardown, and that is the whole of the ordering rule**: after
// sim.Leave, so a reconnect is never refused for a session that has already gone,
// and after the record write, so it is never served a record that is still being
// written. Releasing an id that was never claimed is a no-op, so a teardown path
// never needs to know how far the handshake got.
func (i *Identities) Release(id identity.PlayerID) {
	i.mu.Lock()
	defer i.mu.Unlock()
	delete(i.live, id)
}

// Count reports how many identities have a live session.
func (i *Identities) Count() int {
	i.mu.Lock()
	defer i.mu.Unlock()
	return len(i.live)
}
