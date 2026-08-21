package session_test

import (
	"bytes"
	"context"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
)

// The character phase, driven through a whole connection.
//
// The rules themselves are tested against Identities, where each one is a function of
// its inputs; what is tested here is the shape of the exchange — which frame answers
// which, in which order, and what a client is left holding when the answer is no.

// servingOne starts one session over a fake connection and answers with the pieces a
// test needs to drive it: the connection, and the channel Serve's return lands on.
func servingOne(t *testing.T, identities *session.Identities, entityID uint64) (*fakeConn, chan error) {
	t.Helper()

	chunks, sim, peers := serveDeps(t)
	conn := newFakeConn()
	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, serveConfig(), noTimeouts(),
			chunks, sim, peers, identities, entityID, discard())
	}()
	return conn, done
}

// endsCleanly waits for a session that was refused, which must return nil: a refusal is
// how a connection ends politely, not a failure of the server.
func endsCleanly(t *testing.T, done chan error) {
	t.Helper()

	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Serve returned %v, want nil for a refused handshake", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("Serve did not return after the refusal")
	}
}

// rejection reads a ServerReject frame, failing the test when the frame is anything
// else.
func rejection(t *testing.T, frame []byte) *vnet.ServerReject {
	t.Helper()

	env := vnet.GetRootAsEnvelope(frame, 0)
	if env.PayloadType() != vnet.PayloadServerReject {
		t.Fatalf("the answer is %s, want %s", env.PayloadType(), vnet.PayloadServerReject)
	}
	return rejectFrom(t, env)
}

// seedAt mints a character for account and puts it somewhere of its own, so that a
// welcome's spawn says which character was chosen.
func seedAt(t *testing.T, store *persist.Store, account ticket.AccountID, name string, at [3]float64) persist.Character {
	t.Helper()

	character := seedCharacter(t, store, account, name)
	rec := livingRecord()
	rec.Pos = at
	if err := store.Save(character.ID, rec); err != nil {
		t.Fatalf("seeding %q at %v: %v", name, at, err)
	}
	return character
}

// A hello is answered with this account's characters and the number it may hold — not
// with a welcome, because there is nothing yet to be welcomed.
func TestAHelloIsAnsweredWithTheAccountsCharacters(t *testing.T) {
	t.Parallel()

	account := testAccount(40)
	identities, store := knownIdentities(t)
	eivor := seedAt(t, store, account, "Eivor", [3]float64{10.5, 70, -20.5})
	sigrun := seedAt(t, store, account, "Sigrun", [3]float64{-30.5, 66, 44.5})
	// Somebody else's character, so "this account's characters" cannot pass by listing
	// the world.
	seedCharacter(t, store, testAccount(41), "Bjorn")

	conn, done := servingOne(t, identities, 1)
	conn.in <- hello(40)

	list := characterList(t, nextFrame(t, conn))
	if got := list.CharactersLength(); got != 2 {
		t.Fatalf("the list carries %d characters, want the 2 this account holds", got)
	}
	if got := list.MaxCharacters(); got != persist.MaxCharactersPerAccount {
		t.Errorf("the list allows %d characters, want %d", got, persist.MaxCharactersPerAccount)
	}

	// Both of them, with the name and the face each was created with — everything a
	// character-select screen draws a row from, and nothing about where they stood.
	wanted := map[uint64]persist.Character{
		uint64(eivor.ID):  eivor,
		uint64(sigrun.ID): sigrun,
	}
	for i := range list.CharactersLength() {
		var summary vnet.CharacterSummary
		if !list.Characters(&summary, i) {
			t.Fatalf("row %d of the list is absent", i)
		}
		want, mine := wanted[summary.CharacterId()]
		if !mine {
			t.Fatalf("the list carries character %d, which this account does not hold", summary.CharacterId())
		}
		delete(wanted, summary.CharacterId())

		if got := string(summary.Name()); got != want.Name {
			t.Errorf("character %d is named %q, want %q", summary.CharacterId(), got, want.Name)
		}
		worn := summary.Appearance(nil)
		if worn == nil {
			t.Fatalf("character %d carries no appearance; a client may not invent one", summary.CharacterId())
		}
		if got := worn.HairModel(); got != want.Appearance.HairModel {
			t.Errorf("character %d wears hair model %s, want %s", summary.CharacterId(), got, want.Appearance.HairModel)
		}
		if got := worn.SkinColor(); got != want.Appearance.SkinColor {
			t.Errorf("character %d has skin %#08x, want %#08x", summary.CharacterId(), got, want.Appearance.SkinColor)
		}
	}
	if len(wanted) != 0 {
		t.Errorf("the list left out %d of the account's characters", len(wanted))
	}

	// And the welcome that follows the choice is the chosen character's: the spawn is
	// where *Sigrun* stood, which is the whole reason the choice comes first.
	conn.in <- protocol.EncodeSelectCharacterRequest(protocol.SelectCharacterRequest{CharacterID: uint64(sigrun.ID)})
	welcome := welcomeFrom(t, vnet.GetRootAsEnvelope(nextFrame(t, conn), 0))
	spawn := welcome.Spawn(nil)
	if spawn == nil {
		t.Fatal("the welcome carries no spawn")
	}
	if got := ([3]float32{spawn.X(), spawn.Y(), spawn.Z()}); got != [3]float32{-30.5, 66, 44.5} {
		t.Errorf("the welcome's spawn is %v, want Sigrun's", got)
	}

	_ = conn.Close()
	<-done
}

// An account that has never played here is answered with an empty list, which is a legal
// answer and not a refusal: it says the only way forward is a creation.
func TestAnAccountWithNoCharactersIsOfferedAnEmptyList(t *testing.T) {
	t.Parallel()

	identities, store := knownIdentities(t)
	conn, done := servingOne(t, identities, 1)
	conn.in <- hello(42)

	list := characterList(t, nextFrame(t, conn))
	if got := list.CharactersLength(); got != 0 {
		t.Errorf("an account that has never played here was offered %d characters", got)
	}
	if got := list.MaxCharacters(); got == 0 {
		t.Error("the list allows no characters at all, so the client can neither choose nor create")
	}

	conn.in <- creation("Halvar")
	if got := vnet.GetRootAsEnvelope(nextFrame(t, conn), 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the creation was answered with %s, want a welcome", got)
	}
	// Created and playing, in one step: the contract says a created character is the
	// character playing this session.
	if held := store.Characters(testPlayerID(testAccount(42))); len(held) != 1 || held[0].Name != "Halvar" {
		t.Errorf("the store holds %+v, want the character that was just created", held)
	}

	_ = conn.Close()
	<-done
}

// Nothing but the list arrives until a character has been chosen. **The welcome's spawn
// is why**: it belongs to a character, so a welcome sent before there is one would carry
// a position the client must not trust.
func TestNoWelcomeArrivesBeforeACharacterIsChosen(t *testing.T) {
	t.Parallel()

	identities, store := knownIdentities(t)
	seedCharacter(t, store, testAccount(43), "Eivor")

	conn, done := servingOne(t, identities, 1)
	conn.in <- hello(43)
	characterList(t, nextFrame(t, conn))

	// The session is now waiting, and a waiting session sends nothing. A window rather
	// than an instant, because the failure this catches is a welcome queued *behind* the
	// list rather than instead of it.
	select {
	case frame := <-conn.out:
		t.Fatalf("%s arrived before a character was chosen", vnet.GetRootAsEnvelope(frame, 0).PayloadType())
	case <-time.After(50 * time.Millisecond):
	}

	_ = conn.Close()
	<-done
}

// "No such character" and "not yours" leave the same frame, byte for byte.
//
// Through Serve rather than through Identities because the frame is what a client sees,
// and it is composed one layer out from the refusal: a detail that leaked which of the
// two it was would have to leak it here.
func TestACharacterThisAccountMayNotPlayLeavesTheSameFrame(t *testing.T) {
	t.Parallel()

	frames := make(map[string][]byte, 2)
	for name, wanted := range map[string]func(store *persist.Store) persist.CharacterID{
		"a character another account owns": func(store *persist.Store) persist.CharacterID {
			return seedCharacter(t, store, testAccount(45), "Eivor").ID
		},
		"a character nobody has": func(*persist.Store) persist.CharacterID {
			return persist.CharacterID(0xdead_beef)
		},
	} {
		identities, store := knownIdentities(t)
		conn, done := servingOne(t, identities, 1)

		id := wanted(store)
		conn.in <- hello(44)
		characterList(t, nextFrame(t, conn))
		conn.in <- protocol.EncodeSelectCharacterRequest(protocol.SelectCharacterRequest{CharacterID: uint64(id)})

		frame := nextFrame(t, conn)
		if got := rejection(t, frame).Reason(); got != vnet.RejectReasonBAD_REQUEST {
			t.Errorf("selecting %s was refused with %s, want BAD_REQUEST", name, got)
		}
		frames[name] = bytes.Clone(frame)

		endsCleanly(t, done)
	}

	var reference, referenceName string
	for name, frame := range frames {
		if referenceName == "" {
			reference, referenceName = string(frame), name
			continue
		}
		if string(frame) != reference {
			t.Errorf("the frame for %q differs from the one for %q; a client that can tell them apart "+
				"can enumerate this world's characters by asking", name, referenceName)
		}
	}
}

// The three ways a creation is refused, each with the code that says which — because
// unlike a ticket, a refused name is something the player chose and can choose again.
func TestARefusedCreationSaysWhichRefusalItWas(t *testing.T) {
	t.Parallel()

	for name, tc := range map[string]struct {
		seed  func(t *testing.T, store *persist.Store, account ticket.AccountID)
		asked string
		want  vnet.RejectReason
	}{
		"a name somebody else has": {
			seed: func(t *testing.T, store *persist.Store, _ ticket.AccountID) {
				t.Helper()
				seedCharacter(t, store, testAccount(47), "Eivor")
			},
			asked: "Eivor",
			want:  vnet.RejectReasonCHARACTER_NAME_TAKEN,
		},
		"a name this world will not accept": {
			seed:  func(*testing.T, *persist.Store, ticket.AccountID) {},
			asked: "   ",
			want:  vnet.RejectReasonCHARACTER_NAME_REFUSED,
		},
		"an account that is full": {
			seed: func(t *testing.T, store *persist.Store, account ticket.AccountID) {
				t.Helper()
				for i := range persist.MaxCharactersPerAccount {
					seedCharacter(t, store, account, seededName(i+1))
				}
			},
			asked: "OneTooMany",
			want:  vnet.RejectReasonCHARACTER_LIMIT_REACHED,
		},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			account := testAccount(46)
			identities, store := knownIdentities(t)
			tc.seed(t, store, account)

			conn, done := servingOne(t, identities, 1)
			conn.in <- hello(46)
			characterList(t, nextFrame(t, conn))
			conn.in <- creation(tc.asked)

			reject := rejection(t, nextFrame(t, conn))
			if got := reject.Reason(); got != tc.want {
				t.Errorf("the refusal is %s, want %s", got, tc.want)
			}
			if len(reject.Detail()) == 0 {
				t.Error("the refusal says nothing a player could act on")
			}
			endsCleanly(t, done)
		})
	}
}

// The character phase accepts two messages. Anything else is answered with BAD_REQUEST
// and a closed connection — the same shape a first message that is not a hello gets, and
// for the same reason: this is still the handshake, which is the one place a refusal has
// a reply payload to say so in.
func TestAMessageThatIsNotAChoiceEndsTheHandshake(t *testing.T) {
	t.Parallel()

	for name, frame := range map[string][]byte{
		"input from a client that thinks it is in the world": encodePlayerInput(1, 1),
		"a second hello":                hello(48),
		"a payload only a server sends": protocol.EncodeServerCharacterList(protocol.CharacterList{MaxCharacters: 1}),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			identities, _ := knownIdentities(t)
			conn, done := servingOne(t, identities, 1)

			conn.in <- hello(48)
			characterList(t, nextFrame(t, conn))
			conn.in <- frame

			if got := rejection(t, nextFrame(t, conn)).Reason(); got != vnet.RejectReasonBAD_REQUEST {
				t.Errorf("%s was refused with %s, want BAD_REQUEST", name, got)
			}
			endsCleanly(t, done)
		})
	}
}

// The whole exchange, three connections deep: a first creates a character, a second
// creates another, and a third is offered both and plays the one the second did not.
//
// **It is the acceptance criterion end to end**, and what it pins that the tests above
// do not is that the list a later connection is answered with is built from what the
// earlier ones wrote — the store, not anything either of them kept.
func TestASecondConnectionListsWhatTheFirstCreatedAndPlaysTheOther(t *testing.T) {
	t.Parallel()

	identities, _ := knownIdentities(t)

	// Two connections, one account, one after the other: the claim is released in
	// teardown, so the second is admitted only because the first has ended.
	made := make([]uint64, 0, 2)
	for entityID, name := range map[uint64]string{1: "Eivor", 2: "Sigrun"} {
		conn, done := servingOne(t, identities, entityID)
		conn.in <- hello(50)
		conn.in <- creation(name)

		list := characterList(t, nextFrame(t, conn))
		if got := list.CharactersLength(); got != len(made) {
			t.Fatalf("%q was offered %d characters, want the %d already made", name, got, len(made))
		}
		welcome := welcomeFrom(t, vnet.GetRootAsEnvelope(nextFrame(t, conn), 0))
		if welcome.EntityId() != entityID {
			t.Errorf("%q was welcomed as entity %d, want %d", name, welcome.EntityId(), entityID)
		}
		made = append(made, entityID)

		_ = conn.Close()
		<-done
	}

	// The third connection: both characters, and it plays the one it names rather than
	// the one the server happens to list first. The order above is a map's and therefore
	// varies between runs, which is the property this leans on — a client that took the
	// first row would pass on half of them.

	conn, done := servingOne(t, identities, 3)
	conn.in <- hello(50)

	list := characterList(t, nextFrame(t, conn))
	if got := list.CharactersLength(); got != 2 {
		t.Fatalf("the third connection was offered %d characters, want the 2 that were made", got)
	}

	named := map[string]uint64{}
	for i := range list.CharactersLength() {
		var summary vnet.CharacterSummary
		if !list.Characters(&summary, i) {
			t.Fatalf("row %d of the list is absent", i)
		}
		named[string(summary.Name())] = summary.CharacterId()
	}
	wanted, listed := named["Sigrun"]
	if !listed {
		t.Fatalf("the list carries %v, want both names that were created", named)
	}

	conn.in <- protocol.EncodeSelectCharacterRequest(protocol.SelectCharacterRequest{CharacterID: wanted})
	if got := vnet.GetRootAsEnvelope(nextFrame(t, conn), 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the selection was answered with %s, want a welcome", got)
	}

	_ = conn.Close()
	<-done
}
