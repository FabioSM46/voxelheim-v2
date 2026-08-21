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

// Resolve settles which player a hello is claiming, and claims them for this session.
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
//  4. Which of that account's characters is playing is settled, and one is created if
//     the account has none here. The three ways a creation is refused are the three
//     the contract reserves: CHARACTER_NAME_TAKEN, CHARACTER_NAME_REFUSED and
//     CHARACTER_LIMIT_REACHED.
//  5. The store is read for the life that character left behind.
//  6. The account is claimed. One already holding a live session is ALREADY_CONNECTED.
//
// **What is gone from this list is the whole of the old model.** There was a four-way
// rule over `player_token`: mint on an empty one, resume a known one, mint a new
// identity for an unknown one, refuse a wrong-length one. A V7 server reads past that
// field entirely — schemas/handshake.fbs retires it in as many words — so there is no
// minting path here at all, and a client can no longer choose who it is by presenting
// bytes. It presents a ticket somebody signed, or it does not come in.
//
// The caller releases the claim in its teardown, last. See Serve.
func (i *Identities) Resolve(hello *protocol.ClientHello) (Resolved, error) {
	if hello == nil {
		// Unreachable: Serve resolves only a message that decoded as a hello.
		return Resolved{}, errors.New("session: no hello to resolve a player from")
	}

	claims, err := i.verify(hello.SessionTicket)
	if err != nil {
		return Resolved{}, err
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

	character, err := i.character(id, hello.PlayerName)
	if err != nil {
		return Resolved{}, err
	}

	life, returning, err := i.recall(character)
	if err != nil {
		return Resolved{}, err
	}

	if !i.claim(id, character.ID) {
		return Resolved{}, &Refused{
			Reason: vnet.RejectReasonALREADY_CONNECTED,
			// Named as the account rather than as the identity, because that is what it
			// now is: the same person cannot hold two sessions on this world, whichever
			// two machines they are sitting at. No cause, because the detail is already
			// the whole reason and there is nothing here a client should not be told —
			// it is their own second connection.
			Detail: "that account already has a live session on this world",
		}
	}
	return Resolved{ID: id, Character: character.ID, Name: character.Name, Returning: returning, Life: life}, nil
}

// character settles which of this account's characters is playing, creating one when
// the account holds none on this world.
//
// **Choosing is not on the wire yet and this is deliberately not a substitute for it.**
// schemas/handshake.fbs already reserves the exchange — ServerCharacterList, then
// SelectCharacterRequest or CreateCharacterRequest — and putting it there is the next
// issue. Until it lands there is exactly one thing a hello says about a character, the
// display name it has always carried, so that is what this resolves against:
//
//   - an account with characters here plays the one wearing that name, and the lowest
//     id it holds when none does. Deterministic, so two connections settle the same
//     way, and it never *creates* from a name — an account's second character is made
//     through the wire exchange or not at all;
//   - an account with none here has one created under that name, which is the only way
//     a first connection can become a character at all.
//
// The lookup goes through the store's own fold rather than a comparison written here:
// a name is taken under the store's rule, and a second spelling of that rule is one bug
// away from a name that is taken and cannot be found.
// **The decision and the write happen under the store's one lock**, which is why this
// asks for both at once instead of reading the roster and then creating. Reading first
// was a check-then-act race that #156's review found: two hellos for one fresh account
// both saw an empty roster, both created under different names — Create serialises per
// name, so both succeeded — and the one that then lost the single-session claim had
// already written a second character nobody asked for. Nothing deletes a character, so
// the roster slot and the name were gone for good.
func (i *Identities) character(id identity.PlayerID, requested string) (persist.Character, error) {
	character, _, err := i.store.ResolveOrCreate(id, requested)
	if err != nil {
		return persist.Character{}, refuseCharacter(err)
	}
	return character, nil
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
func (i *Identities) stillPlaying(id identity.PlayerID) (persist.CharacterID, bool) {
	i.mu.Lock()
	defer i.mu.Unlock()

	held, live := i.live[id]
	if !live || held.finalised {
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

// claim adds id to the live set under the character it is playing, answering false when
// it is already there.
//
// The character goes in with the claim rather than in a later call, so there is no
// window in which an account is live and the autosave cannot say what to write for it.
func (i *Identities) claim(id identity.PlayerID, character persist.CharacterID) bool {
	i.mu.Lock()
	defer i.mu.Unlock()

	if _, live := i.live[id]; live {
		return false
	}
	i.live[id] = &liveIdentity{character: character}
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
