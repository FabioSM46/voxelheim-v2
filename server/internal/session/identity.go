package session

import (
	"bytes"
	"crypto/ed25519"
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
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Refused is a handshake refusal the contract has a code for.
//
// Resolution has two failure modes and they are not the same thing. A ticket this
// server will not admit, and an account that is already playing, are refusals: the
// client is told which, in a ServerReject, and the connection ends politely. A server
// that cannot read its own player store is a *failure*, and RejectReason has no member
// for it — answering SERVER_FULL would tell the client something false about why, and
// BAD_REQUEST would blame it for something it did not do. So the first is this type
// and the second is an ordinary error, which ends the session with no reply at all.
type Refused struct {
	// Reason is the code the client is answered with.
	Reason vnet.RejectReason

	// Detail is what the client is told, and for a ticket it is deliberately not the
	// reason. See [refusedTicketDetail].
	Detail string

	// Cause is the refusal an operator reads, and the client never does.
	//
	// **The two halves of "distinguishable in the log, indistinguishable to the
	// client" are these two fields**, which is the whole reason the second one exists:
	// with one field the choice is between an oracle on the wire and a log that cannot
	// say which of five things went wrong. Nil for a refusal whose Detail already says
	// everything there is to say — the account-already-playing one, and the two that
	// belong to Handshake rather than here.
	//
	// It is unwrapped, so `errors.Is(err, ticket.ErrExpired)` reaches through a
	// refusal and a caller can name the sentinel it means rather than match prose.
	Cause error
}

func (r *Refused) Error() string {
	if r.Cause == nil {
		return fmt.Sprintf("session: handshake refused: %s: %s", r.Reason, r.Detail)
	}
	return fmt.Sprintf("session: handshake refused: %s: %s: %v", r.Reason, r.Detail, r.Cause)
}

// Unwrap exposes the cause, so errors.Is and errors.As see through a refusal.
func (r *Refused) Unwrap() error { return r.Cause }

// refusedTicketDetail is what the client is told about **every** ticket this server
// will not admit: absent, the wrong length, signed by another key, expired, or issued
// for another world.
//
// **The same sentence for all of them, deliberately.** `game.Player.RemoveStructure`
// draws the same line and gives the reason in a word — every refusal is silence,
// because a client that could tell "no such structure" from "not yours" from "too far
// away" could map somebody else's camp by asking. The same shape applies here with a
// credential in place of a camp: a handshake that distinguishes "expired" from "signed
// by another key" from "issued for another world" is answering questions about tickets
// nobody presented, on a connection nobody has authenticated. [Refused.Cause] tells an
// operator which of the five it was; the wire tells a player the one thing they can act
// on, which is the same thing in every case — sign in again.
const refusedTicketDetail = "the session ticket was not accepted; sign in again"

// The two refusals this package makes about a ticket before anything is verified.
//
// Sentinels because a caller should name the case it means rather than match prose, and
// because these two are the only ticket refusals that are not internal/ticket's own.
// They are framing rather than cryptography, which is why they live here: both are
// decided from the length of a field, before a signature is checked and before an
// account is looked up, exactly as schemas/handshake.fbs requires.
var (
	// ErrTicketAbsent reports a hello that presents no ticket at all.
	//
	// **A legal message this server will not admit**, which is the distinction the
	// contract asks for: absent and empty are a client "claiming no account", and
	// whether such a session is admitted is a server's admission policy rather than a
	// framing question. This server's answer is no. Identity comes from a ticket now,
	// so a connection presenting none is a connection with nobody behind it.
	ErrTicketAbsent = errors.New("session: the hello presents no session ticket")

	// ErrTicketLength reports a ticket that is neither absent nor exactly
	// protocol.SessionTicketLen bytes.
	//
	// The length is named in the wrapped message and the bytes never are: a ticket is
	// a bearer credential, and the first thing anybody does with a refusal is read it
	// out of a log.
	ErrTicketLength = fmt.Errorf("session: a session ticket is exactly %d bytes", protocol.SessionTicketLen)
)

// Verifier is everything this server needs to check a ticket, and it is the whole of
// what admitting a player costs: a public key, the world this server is, and a clock.
//
// **No network, no disk, no lookup.** That is the property the design rests on rather
// than a happy accident of the implementation — internal/ticket's imports_test.go
// asserts that not one file on the verification path can even reach a socket — and it
// is why the account service being down costs nobody a game. The key is read once, at
// startup, by whoever builds this.
//
// Built through [NewVerifier] and never as a literal: the two fields that can be wrong
// are wrong in ways that are invisible afterwards, and one of them is worse than being
// unable to start. See [NewVerifier].
type Verifier struct {
	// pub is the account service's Ed25519 public key.
	pub ed25519.PublicKey

	// world is this server's own world id, and what stops a ticket minted for one
	// server being presented at another.
	world ticket.WorldID

	// now is where this verifier's idea of the current moment comes from.
	//
	// A function rather than a call to time.Now inside Resolve, for the reason
	// NewStreamer takes one: an expiry is a decision about time, and a test that
	// cannot say what time it is can only test expiry by waiting.
	now func() time.Time
}

// NewVerifier settles what this server will admit, refusing a configuration that
// cannot mean anything.
//
// **Both refusals are configuration answers rather than answers about a ticket**, which
// is why they are made here — at startup, where an operator is reading — instead of
// once per join, where the refusal looks exactly like a client's problem. That is #126's
// lesson taken one layer out: a game server given no world refused every player with "the
// ticket names another world", a sentence about the ticket that never once mentions that
// this server names none.
//
// The zero world is the dangerous one and is refused for a reason worth stating plainly:
// a ticket naming no world is an **account ticket**, minted for talking to the account
// service, and a verifier configured with the zero id would ask "does this ticket name
// no world" and admit every account that has ever signed in. internal/ticket refuses it
// too; this is the same refusal moved to the moment somebody can act on it.
//
// A nil clock is time.Now, which is what every caller but a test wants.
func NewVerifier(pub ed25519.PublicKey, world ticket.WorldID, now func() time.Time) (*Verifier, error) {
	if len(pub) != ed25519.PublicKeySize {
		return nil, fmt.Errorf("session: %w, got %d", ticket.ErrPublicKeySize, len(pub))
	}
	if world.IsZero() {
		return nil, fmt.Errorf("session: %w: it was given no world to compare a ticket against", ticket.ErrVerifierWorld)
	}
	if now == nil {
		now = time.Now
	}
	// Cloned, so that a caller which goes on to reuse its buffer cannot change what
	// this server verifies with after it has started admitting players.
	return &Verifier{pub: bytes.Clone(pub), world: world, now: now}, nil
}

// World is the world this verifier admits tickets for. Not a secret — a world id is a
// digest of a name an operator publishes — and it is what the startup line prints.
func (v *Verifier) World() ticket.WorldID { return v.world }

// Resolved is the player a hello settled on, and how it got there.
//
// **Two names, and they answer different questions.** ID is the account, and it is what
// the simulation keys a body on, what the exclusivity claim is held under and what a log
// line carries. Character is which of that account's characters is playing, and it is
// what the player store writes under. One account has one live session; one session
// plays one character; so within a session the two are interchangeable — which is
// exactly why they must not be conflated across one, because an account has several
// characters and only one of them stood where this record says.
type Resolved struct {
	// ID names the player. It is what the simulation keys on and what a log line
	// carries: the one-way name of the account the ticket named.
	ID identity.PlayerID

	// Character is the character this session plays, minted by the store and stable for
	// that character's life. It is what the player store keys a record on.
	Character persist.CharacterID

	// Name is that character's name, as the server accepted it. Unique within this
	// world, which is what makes it a name rather than a label.
	Name string

	// Appearance is what that character looks like, read from the store and from
	// nowhere else. **Not from the message that chose it**: a selection names an id and
	// a creation is the one and only time a client says what a character looks like, so
	// on every connection after the first this value has one source, and it is the same
	// one the world was drawn from last time.
	Appearance protocol.Appearance

	// Returning reports whether the player store already held a life for Character.
	Returning bool

	// Life is what this character left behind — where they stood, their health and their
	// pack — or nil for a character the store has never had a readable life for.
	//
	// **Loaded here, before the welcome is built**, because ServerWelcome.spawn has to
	// carry the position the player is actually placed at and Handshake is a pure
	// function that cannot go and find one. It is also the reason a refused record is
	// decided at this point and not at Join: by Join the client has already been told
	// where it stands.
	Life *game.Life
}

// Identities is the set of accounts that have a live session, and the resolution that
// turns the ticket in a ClientHello into one of them.
//
// It sits beside Registry deliberately: Registry answers "which connections are
// live" one level down, in entity ids minted per session, and this answers the same
// question one level up, in players that outlive a connection. Two sets rather
// than one because they have different lifetimes — an entity id is forgotten when a
// session ends, a player's claim is released and then remembered.
//
// The store is never nil here: a nil one handed to [NewIdentities] becomes
// persist.NewMemoryStore, which is the ephemeral world. Verification, exclusivity, name
// uniqueness and the per-account limit all work exactly as they do with a directory —
// they are rules about the world rather than about the disk — and nothing is written, so
// every character it mints is gone when the process ends. The account still names the
// same player, so what an ephemeral world costs is the life, not the name.
type Identities struct {
	store *persist.Store

	// verifier is how a ticket becomes an account. Never nil: NewIdentities refuses to
	// build a claim set without one, because a server that cannot check a ticket
	// cannot admit anybody and should be visibly broken rather than quietly open.
	verifier *Verifier

	// log is where a refused record is reported. It is the one thing here that has to
	// say something to an operator: a player who joins as new because their file could
	// not be read is indistinguishable, from the outside, from a player who has never
	// connected.
	log *slog.Logger

	// live is every account with a session, and what that session needs remembered
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
	// character is which of this account's characters the session is playing. The
	// autosave needs it and nothing else can supply it: the simulation keys a life by
	// account, which is the one thing that does not say which character stood there.
	//
	// **Zero for a session that has been admitted and has not chosen yet.** The claim is
	// taken before the character list is sent, so this is filled in by
	// [Identities.playing] when a selection or a creation settles it.
	character persist.CharacterID

	// finalised records that this session's teardown has already written its last
	// word. See RememberAll, which is the only reader.
	finalised bool
}

// NewIdentities returns an empty claim set over store. A nil logger discards, which is
// what the tests that are about admission rather than about operators want.
//
// **A nil store becomes persist.NewMemoryStore rather than staying nil**, and the
// substitution is here rather than at every use because of what stopped being optional.
// A nil store used to mean "every read finds nothing and every write goes nowhere",
// which was the whole of an ephemeral world while a player's name was a hash of their
// account. A character has to be *minted* now, and its name has to be unique across the
// world — authoritative logic that an ephemeral world owes its players exactly as much
// as a persistent one does. So the ephemeral world is a store with no directory under
// it, and this type never branches on having one.
//
// **A nil verifier is an error and not a permissive default**, and that is the whole of
// the acceptance criterion in the type system: there is no way to build a claim set that
// admits players without checking them, so "a server that cannot verify a ticket cannot
// admit anybody" cannot be undone by forgetting an argument. The alternative — a nil
// verifier meaning "let everyone in" — is the second way in that this design exists to
// remove, and it would be reached by an omission rather than by a decision.
func NewIdentities(store *persist.Store, verifier *Verifier, log *slog.Logger) (*Identities, error) {
	if verifier == nil {
		return nil, errors.New("session: a claim set needs a verifier; a server that cannot check a ticket cannot admit anybody")
	}
	if log == nil {
		log = slog.New(slog.DiscardHandler)
	}
	if store == nil {
		store = persist.NewMemoryStore()
	}
	return &Identities{
		store:    store,
		verifier: verifier,
		log:      log,
		live:     make(map[identity.PlayerID]*liveIdentity),
	}, nil
}

// Admitted is one account this server has let through the door: its ticket verified, its
// one live session claimed, and its characters read — with nothing yet said about which
// of them is playing.
//
// **It is the half of a handshake that used to be the whole of it.** Through V6 a hello
// was answered with a welcome, so verifying a credential and settling a body were one
// step. V7 puts a choice between them, and the reason is `ServerWelcome.spawn`: where a
// player stands depends on which character they chose, so a welcome sent before the
// choice would carry a spawn the client must not trust and a correction would have to
// follow it. See the header of schemas/handshake.fbs.
//
// The claim is already held by the time one of these exists, which is what makes the
// phase that follows safe to spend a person's time in: a second connection for the same
// account is turned away at the door rather than after somebody has picked a character.
type Admitted struct {
	// ID names the account, one-way. It is what the exclusivity claim is held under,
	// what the simulation keys a body on, and what a log line carries.
	ID identity.PlayerID

	// Characters is every character this account holds **on this world**, lowest id
	// first, as the store's index knows them. An empty one is a legal and expected
	// answer — a new account, or one that has never played here — and it is not a
	// refusal: it says the only way forward is a creation.
	Characters []persist.Character
}

// maxCharactersFitsTheWire is a compile-time reading of the one contract limit this
// package converts rather than copies: `ServerCharacterList.max_characters` is a ubyte,
// and persist's constant is documented as staying under 256. Written as a constant
// conversion so that raising it past the wire's range is a build failure here rather
// than a truncated number a client would believe.
const maxCharactersFitsTheWire = uint8(persist.MaxCharactersPerAccount)

// list is the answer a hello gets: every character this account holds on this world, and
// how many it may hold.
//
// **The limit is announced rather than left for a client to hardcode**, for the reason
// every limit in ServerWelcome is: the number belongs to the server, and a client that
// guessed it would offer or refuse a creation this server disagrees with.
//
// The one case where the announced number is not [persist.MaxCharactersPerAccount] is a
// build whose limit has been **lowered** under an account that already holds more than
// the new allowance. The contract requires `max_characters` to be at least the length of
// the list — a server that says otherwise is disagreeing with itself, and a client is
// required to refuse the frame, which would shut that account out of the world over a
// number nobody could act on. So the larger of the two is announced, which is also the
// field's own reading: it is how many characters this account may hold *including the
// ones above*, and it plainly may hold the ones it already has. Creating another is
// still refused, because [persist.Store.Create] compares against the constant and not
// against this — which is the half that does the enforcing.
func (a Admitted) list() protocol.CharacterList {
	summaries := make([]protocol.CharacterSummary, 0, len(a.Characters))
	for _, character := range a.Characters {
		summaries = append(summaries, protocol.CharacterSummary{
			CharacterID: uint64(character.ID),
			Name:        character.Name,
			// Straight from the store, which is the whole of this issue's rule about a
			// face: it was written down when the character was created and is read from
			// nowhere else.
			Appearance: character.Appearance,
		})
	}

	allowed := maxCharactersFitsTheWire
	// The upper bound is not defensive clutter: this is the one place a count becomes a
	// ubyte, and a silent truncation is the single way this line could announce a number
	// *smaller* than the list beside it — which is the frame the paragraph above exists
	// to avoid. Unreachable, because the constant is documented as staying under 256 and
	// the store refuses a creation past it.
	if held := len(summaries); held > int(allowed) && held <= 0xFF {
		allowed = uint8(held)
	}
	return protocol.CharacterList{Characters: summaries, MaxCharacters: allowed}
}

// Admit verifies the ticket a hello presents, claims the account's one live session, and
// answers with the characters it holds here.
//
// **Runs on the session goroutine, before Join, and never under the simulation's
// lock**: it reads the player store, and a tick that waits on a file is a tick every
// connected player misses.
//
// The order is the contract's, and each step is a different kind of answer:
//
//  1. A session ticket of any length but 96 is BAD_REQUEST, absent and empty
//     included. Decided first, before a signature is checked and before anything is
//     looked up, because it is a question about the message rather than about a
//     credential — see [verify].
//  2. The ticket is verified: the signature, then the world it names, then its
//     expiry, in that order and all of it arithmetic. Every refusal is BAD_REQUEST
//     with one sentence for the client and the reason for the log.
//  3. The account the ticket names becomes a player id. Only now — nothing is looked
//     up for a ticket nobody has vouched for, which is what keeps an unauthenticated
//     connection from costing this server a disk read.
//  4. The account is claimed, and one already holding a live session is
//     ALREADY_CONNECTED. Before the list rather than after it, by the same rule the
//     step above keeps: a connection that is not going in has no business reading a
//     store.
//
// **Two things are gone from this list, and the second went with this issue.** There was
// a four-way rule over `player_token` — mint on an empty one, resume a known one, mint a
// new identity for an unknown one, refuse a wrong-length one — and a V7 server reads
// past that field entirely. And there was a *character* settled here, from the display
// name the hello carried, because choosing had no message to arrive in. It has one now,
// so this function settles an account and stops.
//
// **`ClientHello.player_name` therefore decides nothing at all any more.** It is still
// untrusted display text and it is simply not read: what a player is called here is the
// name their character was created under, which is the one that is unique on this world.
//
// The caller releases the claim in its teardown, last. See Serve.
func (i *Identities) Admit(hello *protocol.ClientHello) (Admitted, error) {
	if hello == nil {
		// Unreachable: Serve admits only a message that decoded as a hello.
		return Admitted{}, errors.New("session: no hello to admit an account from")
	}

	claims, err := i.verify(hello.SessionTicket)
	if err != nil {
		return Admitted{}, err
	}

	// The one line that converts between two packages' sixteen bytes, and it stops
	// compiling the day either of them is a different width — which is why the account
	// is carried in both places rather than shared through an import that would cost
	// internal/identity its leaf property.
	//
	// The account itself goes no further than this statement. Everything downstream —
	// the store, the simulation, every log line — is handed the player id, which is a
	// digest of it.
	id := identity.IDOf(identity.Account(claims.Account))

	if !i.claim(id) {
		return Admitted{}, &Refused{
			Reason: vnet.RejectReasonALREADY_CONNECTED,
			// Named as the account rather than as the identity, because that is what it
			// now is: the same person cannot hold two sessions on this world, whichever
			// two machines they are sitting at. No cause, because the detail is already
			// the whole reason and there is nothing here a client should not be told —
			// it is their own second connection.
			Detail: "that account already has a live session on this world",
		}
	}

	// A map hit and a copy, never a walk of the directory: the index was built when the
	// store was opened, precisely so that this — which happens on every connection — is
	// not a read of every character in the world.
	return Admitted{ID: id, Characters: i.store.Characters(id)}, nil
}

// Select settles this session on one character the account already owns.
//
// **The id is re-read from the store rather than matched against the list that was
// sent**, which is what schemas/handshake.fbs asks for: an id the server minted is the
// one kind of identifier a client may echo back, and whether it names a character *this
// account* owns is a decision made against the store rather than against a message this
// connection has in its hand.
//
// **"No such character" and "not yours" are one answer**, and that is the whole of the
// refusal design here. `game.Player.RemoveStructure` draws the same line for a camp: a
// client that could tell the two apart could map the world's characters by asking for
// ids it does not have. So both are BAD_REQUEST carrying one identical sentence, and the
// cause — which only an operator's log ever sees — is what says which it was.
func (i *Identities) Select(admitted Admitted, wanted persist.CharacterID) (Resolved, error) {
	if admitted.ID == (identity.PlayerID{}) {
		// Unreachable: a session reaches the character phase only through a successful
		// Admit, and that is the one thing that hands out an Admitted.
		return Resolved{}, errors.New("session: a character cannot be chosen before an account has been admitted")
	}

	character, known := i.store.Character(wanted)
	switch {
	case !known:
		return Resolved{}, refuseSelection(fmt.Errorf("no character on this world has id %s", wanted))
	case character.Owner != admitted.ID:
		return Resolved{}, refuseSelection(fmt.Errorf("character %s belongs to another account", wanted))
	}

	life, returning, err := i.recall(character)
	if err != nil {
		return Resolved{}, err
	}
	return i.playing(admitted, character, returning, life), nil
}

// Create makes a new character for this account and settles the session on it.
//
// **Creation and selection are one step**, which is the contract's shape rather than a
// shortcut: a created character is the character playing this session, so the answer to
// a `CreateCharacterRequest` is a `ServerWelcome`.
//
// **The appearance is checked here, before anything is written, and this is the gate
// schemas/common.fbs names in as many words.** A character persisted with a hair model
// no member names, or a colour carrying the reserved high byte, is one every client is
// required to refuse afterwards — and the person who cannot get in is not the person who
// sent it. It is BAD_REQUEST rather than a decode error for the reason that file gives:
// the frame is perfectly readable, and closing the connection over a value would answer a
// value question with a framing verdict. It is BAD_REQUEST rather than one of the three
// character refusals because those three are about the *name*, which is the half a player
// picked and can pick again.
func (i *Identities) Create(admitted Admitted, name string, appearance protocol.Appearance) (Resolved, error) {
	if admitted.ID == (identity.PlayerID{}) {
		// Unreachable, for the reason Select's guard is.
		return Resolved{}, errors.New("session: a character cannot be created before an account has been admitted")
	}

	if err := appearance.Validate(); err != nil {
		return Resolved{}, refuseAppearance(err)
	}

	character, err := i.store.Create(admitted.ID, name, appearance)
	if err != nil {
		return Resolved{}, refuseCharacter(err)
	}

	// No recall, deliberately. Store.Create has just written this character's first
	// record and a record no session has touched is not a life — recall would read the
	// file back only to be told what this line already knows.
	return i.playing(admitted, character, false, nil), nil
}

// playing records which character the claim is playing and answers with the resolution
// the rest of the session is built from.
//
// The character goes onto the live claim here rather than at the claim itself, because
// at the claim nobody had chosen one yet. What that costs is one window — an account is
// live and has no character while the person is looking at the list — and nothing reads
// it: the autosave asks the *simulation* which lives to write, and a session that has
// not chosen has not joined. [Identities.stillPlaying] fails closed over the same window.
func (i *Identities) playing(admitted Admitted, character persist.Character, returning bool, life *game.Life) Resolved {
	i.mu.Lock()
	if held, live := i.live[admitted.ID]; live {
		held.character = character.ID
	}
	// A claim that is not there is unreachable — this session holds it from Admit until
	// its own teardown — and the absence of an else is deliberate: there is nothing this
	// function could usefully do about it, and the teardown writes the record either way
	// because it is handed the Resolved rather than reading the claim back.
	i.mu.Unlock()

	return Resolved{
		ID:         admitted.ID,
		Character:  character.ID,
		Name:       character.Name,
		Appearance: character.Appearance,
		Returning:  returning,
		Life:       life,
	}
}

// refusedSelectionDetail is what a client is told about every character it may not play:
// one this world has never minted, and one another account owns.
//
// **The same sentence for both, deliberately**, and for the reason [refusedTicketDetail]
// is one sentence for five ticket refusals. A client that could tell "no such character"
// from "not yours" could ask this server questions about characters nobody presented and
// learn which ids exist — which is the enumeration `SelectCharacterRequest`'s own
// contract note refuses to allow.
const refusedSelectionDetail = "that character is not one this account can play on this world"

// refuseSelection is the one shape a refused selection takes: BAD_REQUEST, one sentence
// for the client, and the cause for the log.
func refuseSelection(cause error) *Refused {
	return &Refused{
		Reason: vnet.RejectReasonBAD_REQUEST,
		Detail: refusedSelectionDetail,
		Cause:  cause,
	}
}

// refuseAppearance is the answer to a creation whose appearance breaks the contract.
//
// The detail says which half of the request was wrong and nothing about the value:
// a client that sent it has the value already, and a client that did not is a build
// somebody has to fix rather than a player who has to choose again.
func refuseAppearance(cause error) *Refused {
	return &Refused{
		Reason: vnet.RejectReasonBAD_REQUEST,
		Detail: "that is not an appearance this world can store; the request named colours or a hair model this contract does not allow",
		Cause:  cause,
	}
}

// refuseCharacter turns a store refusal into the answer the contract has for it, and
// anything else into a server failure.
//
// **The three sentinels map one to one onto three reject reasons, and the mapping is a
// switch rather than a parse.** Deriving a wire code from an error's prose is how a log
// line becomes a contract — the same split [Refused] draws between Detail and Cause. A
// fourth kind of error is not a refusal at all: a store that cannot mint an id or cannot
// write is this server failing, and there is no reason code that says so.
//
// **The detail is the reason here, where a ticket's deliberately is not.** A refused
// ticket says one identical sentence for all five of its cases, because a client that
// could tell them apart could ask questions about credentials nobody presented. A
// refused *name* is the opposite situation: the player picked it, the client has to
// offer them another one, and the contract states in as many words that the client may
// tell CHARACTER_NAME_TAKEN from CHARACTER_NAME_REFUSED. Saying which is what makes the
// refusal actionable rather than a door that closes.
func refuseCharacter(err error) error {
	switch {
	case errors.Is(err, persist.ErrNameTaken):
		return &Refused{
			Reason: vnet.RejectReasonCHARACTER_NAME_TAKEN,
			Detail: "a character on this world already has that name; choose another",
			Cause:  err,
		}
	case errors.Is(err, persist.ErrNameRefused):
		return &Refused{
			Reason: vnet.RejectReasonCHARACTER_NAME_REFUSED,
			Detail: "that is not a name this world accepts; choose another",
			Cause:  err,
		}
	case errors.Is(err, persist.ErrCharacterLimit):
		return &Refused{
			Reason: vnet.RejectReasonCHARACTER_LIMIT_REACHED,
			Detail: "this account already holds as many characters as this world allows",
			Cause:  err,
		}
	}
	return fmt.Errorf("session: creating a character on this world: %w", err)
}

// verify turns the ticket in a hello into the claims it carries, or into the refusal it
// earns.
//
// **Framing before cryptography, and cryptography before any lookup.** The length is
// settled from a comparison, which is what schemas/handshake.fbs requires and is also
// the cheap half: a connection nobody has authenticated should not be able to spend an
// Ed25519 verification on bytes that cannot be a ticket. internal/ticket then does the
// rest in the order its own doc fixes — the signature before any field is read.
//
// Two of Verify's answers are not about the ticket at all and are not refusals here.
// [ticket.ErrPublicKeySize] and [ticket.ErrVerifierWorld] say this server is
// misconfigured, and telling a client BAD_REQUEST for that would blame it for
// something it did not do — the same split Refused's doc draws. [NewVerifier] makes
// both unreachable by refusing such a configuration at startup; they are handled
// anyway, because the cost is two lines and the alternative is a server that answers
// every player with a lie about their ticket.
func (i *Identities) verify(presented []byte) (ticket.Claims, error) {
	switch len(presented) {
	case protocol.SessionTicketLen:
	case 0:
		// Absent and empty arrive as one zero-length slice, and the contract says both
		// mean "this client claims no account". This server admits nobody it cannot
		// name, so that is a refusal — a policy answer rather than a framing one, which
		// is why it is its own sentinel and not a length complaint.
		return ticket.Claims{}, refuseTicket(ErrTicketAbsent)
	default:
		return ticket.Claims{}, refuseTicket(fmt.Errorf("%w, got %d", ErrTicketLength, len(presented)))
	}

	// [ticket.Verify] and never VerifyAnyWorld: the world comparison is what stops the
	// operator of one world collecting its players' tickets and presenting them at
	// another as those players, and what turns an account ticket away at a game
	// server's door. internal/ticket/callers_test.go holds that boundary by name.
	claims, err := ticket.Verify(i.verifier.pub, presented, i.verifier.world, i.verifier.now())
	if err != nil {
		if errors.Is(err, ticket.ErrPublicKeySize) || errors.Is(err, ticket.ErrVerifierWorld) {
			return ticket.Claims{}, fmt.Errorf("session: this server cannot verify tickets: %w", err)
		}
		return ticket.Claims{}, refuseTicket(err)
	}
	return claims, nil
}

// refuseTicket is the one shape every ticket refusal takes: BAD_REQUEST, one sentence
// for the client, and the cause for the log.
//
// One function rather than five literals, because the property being kept is that they
// are **identical on the wire** — and five copies of a detail string is five chances for
// one of them to say something the others do not.
func refuseTicket(cause error) *Refused {
	return &Refused{
		Reason: vnet.RejectReasonBAD_REQUEST,
		Detail: refusedTicketDetail,
		Cause:  cause,
	}
}

// recall loads the life stored for a character: the life itself, whether one was found,
// and any reason this session cannot proceed.
//
// Asked as a load rather than as a stat, because the two differ on the case that
// matters: a record that exists and cannot be read is not a first connection.
//
// **Four answers, and the middle two are where the rules are.** A record that is absent
// — or one the store wrote when the character was created and no session has touched
// since — is a first connection. A file that cannot be *reached* — a permission, a
// failing disk — is an error and refuses the connection, because a retry may well
// succeed and treating it as absent would throw away a readable life on a transient
// fault. A file that is *corrupt* — the wrong magic, a version this build does not
// speak, a broken checksum, or a life whose values game refuses — will never be
// readable, so refusing the connection for ever helps nobody: it is set aside under a
// name of its own and the character is played as new.
//
// **The move is load-bearing.** #146 could not set the file aside at all, and #147's
// answer rested on two things: the file is moved before this returns, *and* the identity
// minted next was a different one — 32 fresh random bytes, a different hash, a different
// file name — so a failed move cost nothing, because nothing that session went on to
// write could land on the record nobody could read.
//
// The second half of that went with the minting and has not come back with characters.
// A character id is stable for the life of the character, so the session admitted after
// a corrupt record writes to **exactly the same path**. If the move fails, its first
// teardown overwrites the one file nobody could read. So a failed move refuses the
// connection instead: it is a filesystem problem a retry may well survive, which is the
// same answer an unreachable record already gets one branch up, and the opposite answer
// — admit and lose the evidence — is the one nobody can undo. See [Identities.refuseRecord].
func (i *Identities) recall(character persist.Character) (*game.Life, bool, error) {
	rec, found, err := i.store.Load(character.ID)
	switch {
	case err != nil && !errors.Is(err, world.ErrCorruptStore):
		return nil, false, fmt.Errorf("session: reading the record for character %s: %w", character.ID, err)
	case err != nil:
		return nil, false, i.refuseRecord(character, err)
	case !found || rec.Unplayed():
		return nil, false, nil
	}

	// The store wrote these numbers down; it did not vouch for them. What an item id
	// means and how much health is a full bar are game's answers, so they are asked
	// here — once, before anything is built from them, and about the whole record
	// rather than slot by slot.
	life := game.Life{Pos: rec.Pos, Yaw: rec.Yaw, Health: rec.Health, Slots: rec.Slots}
	if vErr := life.Validate(); vErr != nil {
		return nil, false, i.refuseRecord(character, vErr)
	}
	return &life, true, nil
}

// refuseRecord sets a record this build cannot use aside and says so, loudly.
//
// Error level and not warn: a player is about to lose everything they had, and the
// only reason it is not a refused connection is that refusing would not give it back.
//
// **The character itself survives**: the store keeps it in the index, so the name is
// still theirs and the account still owns it. What is gone is the life, and the session
// that follows starts the character where one that has never played starts.
//
// **A failed move is returned rather than survived.** While an unreadable record was
// answered with a *freshly minted* identity, the move was belt to that identity's
// braces; a character id is stable now, so the session admitted after a failed move
// writes to exactly the path whose contents could not be read. A refusal costs that
// player one connection and an operator one look at a directory; the alternative costs
// the player the record and everybody the evidence.
//
// The account is named by the first eight characters of its digest and the character by
// its whole id: one is a prefix of a one-way hash and the other is a number this server
// minted, so neither line says who is playing here.
func (i *Identities) refuseRecord(character persist.Character, cause error) error {
	aside, err := i.store.Quarantine(character.ID)
	if err != nil {
		i.log.Error("a character record could not be read and could not be set aside; the connection is refused rather than writing over it",
			"player_id", character.Owner.Short(), "character", character.ID.String(), "reason", cause.Error(), "error", err)
		return fmt.Errorf("session: setting aside the unreadable record for character %s: %w", character.ID, err)
	}
	i.log.Error("a character record could not be read; it has been kept and the character starts as new",
		"player_id", character.Owner.Short(), "character", character.ID.String(), "reason", cause.Error(), "kept_at", aside)
	return nil
}

// Remember writes the record one session's character leaves behind. A no-op in an
// ephemeral world.
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
//
// **It takes the whole [Resolved] rather than an id, and that is what keeps #102's
// teardown ordering intact through the change of key.** The order is sim.Leave, then
// this, then Release — the claim goes last so a reconnect is never served a record that
// is still being written. Reading the character out of the live map instead would make
// this write *depend* on the claim it is supposed to precede: get the order wrong and
// the character would simply not be found, and the teardown would write nothing at all,
// silently. The session already holds the answer, so it hands it in.
func (i *Identities) Remember(self Resolved, life game.Life) error {
	i.writeMu.Lock()
	defer i.writeMu.Unlock()

	err := i.write(self.Character, life)
	i.finalise(self.ID)
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
		// The simulation keys a life by account, which is the one thing that cannot say
		// which character stood there. The live claim is what knows, and asking it is
		// also the skip: an account whose session has ended, or has written its last
		// word, has no character here to write under.
		character, playing := i.stillPlaying(id)
		if !playing {
			continue
		}
		if err := i.write(character, life); err != nil {
			errs = append(errs, err)
		}
	}
	return errors.Join(errs...)
}

// write puts one life on disk under the character it belongs to. The caller holds
// writeMu.
//
// **Nothing here names the character or its owner**, and that is the store's rule rather
// than an omission: persist.Store.Save fills both from its own index and ignores what a
// caller put in them, so there is no way for a save to rename a character or move it to
// another account. A session writes a life; who lived it was decided at creation.
func (i *Identities) write(character persist.CharacterID, life game.Life) error {
	return i.store.Save(character, persist.Record{
		// When this record was written, which is the end of the session on the teardown
		// path and the moment of the pass on the autosave's. Both are "the last time
		// this server knew anything about this character", which is what the field means.
		LastSeen: time.Now().UTC(),
		Pos:      life.Pos,
		Yaw:      life.Yaw,
		Health:   life.Health,
		Slots:    life.Slots,
	})
}

// stillPlaying reports the character id has a live session on, and whether that session
// has yet to write its last record.
//
// **A claim that has not chosen a character yet answers false**, which is the third of
// the three skips and the one the character phase added. Nothing should reach here in
// that state — the autosave asks the simulation which lives to write, and a session
// still looking at the list has not joined it — and answering `(0, true)` would send a
// life to [Store.Save] under the id that names no character, which is a write refused
// with an error rather than a write to the wrong file. Failing closed here costs
// nothing and keeps that from being the only thing standing in the way.
func (i *Identities) stillPlaying(id identity.PlayerID) (persist.CharacterID, bool) {
	i.mu.Lock()
	defer i.mu.Unlock()

	held, live := i.live[id]
	if !live || held.finalised || held.character.IsZero() {
		return 0, false
	}
	return held.character, true
}

// finalise records that id's session has written its last word.
func (i *Identities) finalise(id identity.PlayerID) {
	i.mu.Lock()
	defer i.mu.Unlock()

	if held, live := i.live[id]; live {
		held.finalised = true
	}
}

// claim adds id to the live set, answering false when it is already there.
//
// **It claims an account with no character named, and that is the phase this issue
// added.** The claim has to be taken at the door — before the list is sent, and long
// before a person has finished choosing — because the alternative is two connections for
// one account both browsing, both selecting, and one of them finding out afterwards. So
// the character arrives later, through [Identities.playing], and every reader of the
// claim fails closed over the window in between.
func (i *Identities) claim(id identity.PlayerID) bool {
	i.mu.Lock()
	defer i.mu.Unlock()

	if _, live := i.live[id]; live {
		return false
	}
	i.live[id] = &liveIdentity{}
	return true
}

// Release ends a session's exclusive hold on its account.
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

// Count reports how many accounts have a live session.
func (i *Identities) Count() int {
	i.mu.Lock()
	defer i.mu.Unlock()
	return len(i.live)
}
