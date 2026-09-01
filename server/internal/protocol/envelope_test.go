package protocol

import (
	"bytes"
	"errors"
	"math"
	"math/rand/v2"
	"reflect"
	"strings"
	"testing"
	"time"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// payloadTable unwraps an envelope's union payload for inspection. Tests read
// buffers the server itself produced, so unlike Decode they are free to use the
// generated accessors directly.
func payloadTable(t *testing.T, env *vnet.Envelope) flatbuffers.Table {
	t.Helper()

	var tbl flatbuffers.Table
	if !env.Payload(&tbl) {
		t.Fatal("envelope payload is absent")
	}
	return tbl
}

func TestClientHelloRoundTrip(t *testing.T) {
	t.Parallel()

	frame := EncodeClientHello(vnet.ProtocolVersionCurrent, "Eivor")

	msg, err := Decode(frame)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Kind != vnet.PayloadClientHello {
		t.Fatalf("Kind = %s, want %s", msg.Kind, vnet.PayloadClientHello)
	}
	if msg.ClientHello == nil {
		t.Fatal("ClientHello payload is nil")
	}
	if got := msg.ClientHello.ProtocolVersion; got != vnet.ProtocolVersionCurrent {
		t.Errorf("ProtocolVersion = %s, want %s", got, vnet.ProtocolVersionCurrent)
	}
	if got := msg.ClientHello.PlayerName; got != "Eivor" {
		t.Errorf("PlayerName = %q, want %q", got, "Eivor")
	}
}

// A hello with no version field at all must decode as Unknown rather than as the
// current version. That is what ProtocolVersion.Unknown = 0 exists for, and it is
// the difference between a version check that fails closed and one that does not.
// TestClientHelloRoundTripsATokenOfAnyLength is the decoder's half of the identity
// rule, and the half it deliberately does *not* enforce.
//
// The contract says a player_token is absent, empty or exactly 32 bytes, and that
// anything else is RejectReason.BAD_REQUEST. That is a refusal with a *reply*, so it
// belongs to the handshake: a decoder that shortened it to an error would close the
// connection with nothing said, and the client would never learn why. The house rule
// AttackRequest.slot documents — this package owns the envelope, the caller owns what
// a value means. session.Identities.Resolve is where the length is judged.
func TestClientHelloRoundTripsATokenOfAnyLength(t *testing.T) {
	t.Parallel()

	sizes := map[string]int{
		"no token at all": 0,
		"a whole token":   32,
		// The length the handshake refuses. It has to decode for that refusal to be
		// reachable at all: a frame that would not parse could only be answered with a
		// closed connection.
		"a wrong-length token": 7,
		"one byte":             1,
		"one byte too many":    33,
	}

	for name, size := range sizes {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			token := make([]byte, size)
			for i := range token {
				token[i] = byte(i + 1)
			}

			msg, err := Decode(EncodeClientHelloWithToken(vnet.ProtocolVersionCurrent, "Eivor", token))
			if err != nil {
				t.Fatalf("Decode: %v", err)
			}
			if msg.ClientHello == nil {
				t.Fatal("ClientHello payload is nil")
			}
			if got := msg.ClientHello.PlayerToken; !bytes.Equal(got, token) {
				t.Errorf("PlayerToken = %d bytes, want the %d given, byte for byte", len(got), size)
			}
			if got := msg.ClientHello.PlayerName; got != "Eivor" {
				t.Errorf("PlayerName = %q, want %q", got, "Eivor")
			}
		})
	}
}

// The two ways of saying "I have no token" are the same thing on the wire and must
// stay the same thing here: the contract calls a first connection "absent or empty".
func TestAnAbsentAndAnEmptyTokenAreTheSame(t *testing.T) {
	t.Parallel()

	absent, err := Decode(EncodeClientHello(vnet.ProtocolVersionCurrent, "Eivor"))
	if err != nil {
		t.Fatalf("Decode of a hello with no token: %v", err)
	}
	empty, err := Decode(EncodeClientHelloWithToken(vnet.ProtocolVersionCurrent, "Eivor", []byte{}))
	if err != nil {
		t.Fatalf("Decode of a hello with an empty token: %v", err)
	}

	if len(absent.ClientHello.PlayerToken) != 0 || len(empty.ClientHello.PlayerToken) != 0 {
		t.Errorf("absent is %d bytes and empty is %d; both must be zero-length",
			len(absent.ClientHello.PlayerToken), len(empty.ClientHello.PlayerToken))
	}
}

// The decoded token must not be a view over the frame. Every other field here is
// copied for the same reason: Decode is the one place untrusted bytes are read, and
// a live view handed to a caller moves the recover it depends on away from the code
// that needs it.
func TestTheDecodedTokenDoesNotAliasTheFrame(t *testing.T) {
	t.Parallel()

	token := make([]byte, 32)
	for i := range token {
		token[i] = byte(i + 1)
	}
	frame := EncodeClientHelloWithToken(vnet.ProtocolVersionCurrent, "Eivor", token)

	msg, err := Decode(frame)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	decoded := msg.ClientHello.PlayerToken

	// Scribble over the whole frame. Anything the decoder kept a view of changes too.
	for i := range frame {
		frame[i] = 0xFF
	}
	if !bytes.Equal(decoded, token) {
		t.Error("the decoded token changed when the frame it came from was overwritten")
	}
}

func TestClientHelloWithoutVersionDecodesAsUnknown(t *testing.T) {
	t.Parallel()

	msg, err := Decode(EncodeClientHello(vnet.ProtocolVersionUnknown, ""))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.ClientHello == nil {
		t.Fatal("ClientHello payload is nil")
	}
	if got := msg.ClientHello.ProtocolVersion; got != vnet.ProtocolVersionUnknown {
		t.Errorf("ProtocolVersion = %s, want %s", got, vnet.ProtocolVersionUnknown)
	}
}

// V6 arrived adding no payload at all — appended table fields and appended enum members
// — and then gained one without moving, which is a different claim and the one this test
// now makes. TestV6AppendsWithoutMovingWhatCameBefore sits beside it because the two
// failure modes are different: a moved union tag silently reinterprets every frame on
// the wire, a moved enum member silently reinterprets one field inside one.
//
// **Tag 20 is why the version did not move for it.** Union members are append-only
// precisely so that appending one is not a break: a peer built against V6 as it shipped
// reads tag 20 as a payload it has no name for and drops it, which costs it the refusal
// feedback and nothing else. Bumping ProtocolVersion.Current for that would refuse every
// peer already on the wire in exchange for a message they were never going to read — so
// the number is asserted here rather than left to whoever edits the list below.
//
// **V8 adds one tag and does move it, and the difference is direction rather than count.**
// "An older peer drops what it cannot name" is a claim about a *client*: this side does not
// drop, because direction is a protocol rule here and Session.handleMessage ends the
// connection on an unrecognised payload. So a V7 server and a V8 client would handshake
// cleanly and die on the first stack anybody put down, which is the mid-session decode
// failure ProtocolVersion exists to turn into a clean refusal. Read down the list below, the
// line is consistent rather than new: every client→server member arrived with a bump, and
// the one appended without one travels the other way.
//
// **V9 adds no tag at all, and moves the version anyway.** It appends MobAction.Dying — an
// enum member inside a table field, travelling server→client, which is the direction tag 20
// took for free. It does not get that exemption, because the exemption belongs to the union
// switch rather than to the direction: a tag with no arm is a whole frame the receiver never
// opens, while an enum member arrives inside a frame it has already committed to reading, in
// a field whose stated invariant is "a known non-zero member". The client refuses it, so a V8
// client against a V9 server dies on the first creature anybody kills. The list below is
// therefore unchanged and the number below it is not — which is exactly the shape of change
// this test exists to make visible.
//
// **V10 adds no tag either, and moves the version for a reason V9's argument does not
// reach.** It appends a *table field* — EntitySnapshot.dead_players — and an unknown table
// field is the one shape FlatBuffers really does let a receiver drop, because an older peer
// never looks the id up. So the old-peer direction, which decided every case above, decides
// nothing here. The new-peer direction decides it instead: the field's stated invariant is
// that the recipient's own entity id is in that vector exactly when self_vitals says Dead,
// and a V10 client enforces it. Against a V9 server the vector is absent on every frame, so
// that client connects perfectly, plays perfectly, and drops the session the first time it
// dies. Same conclusion as V9, arrived at from the other end of the wire — which is why the
// rule below is written about the receiver rather than about the sender.
//
// **V11 appends another table field, and moves for the silent direction.** A missing
// EntitySnapshot.drop_durabilities vector says every visible drop is wearless, so a V11
// client would accept a worn drop from a V10 server as pristine while the authoritative
// server still returned it worn on collection. The peers would silently disagree about
// one entity after a clean handshake.
//
// **V12 appends the leaving exchange and V13 appends PlayerAppearance.name.** The former
// adds a client request an older server would reject; the latter is a required string the
// newer client refuses when absent. Both would fail only after a clean handshake without
// their bumps.
//
// **V14 appends MobKind.Deer and MobAction.Flee.** Both travel inside MobState and the
// client refuses unknown enum members, so an older client would fail mid-session.
//
// **V15 appends ConsumeRequest and hunger to PlayerVitals.** The request is a new
// client intent, and the vitals append carries a new non-zero denominator the client
// validates; without the bump either mismatch would surface only after admission.
//
// **V16 appends RecipeID.CookedMeat.** It travels client to server in CraftRequest, so a
// V15 server would reject it only after a clean handshake.
//
// **V17 appends experience progress to PlayerVitals.** A V17 client requires the new
// experience-to-next denominator to be non-zero, while a V16 server omits it and sends
// the scalar default. Without the bump the peers would fail only after admission.
//
// **V18 appends equipment_slots to ServerWelcome and worn item ids to
// PlayerAppearance.** A V18 client requires the non-zero equipment count a V17 server
// omits, so the mismatch must be refused at the handshake.
//
// **V19 appends six RecipeID members that a client sends to the server.** A V18 server
// cannot name them and would reject the first armour craft after a clean handshake.

// **V20 appends chat and party requests plus party state in the snapshot.** The two
// client payloads are unknown to a V19 server and would otherwise fail only on first use.
//
// **V21 appends two corpse-loot requests and their two server answers.** A V20 server
// cannot name either request, and the snapshot gains stable roster and accessibility state.
//
// **V22 appends BlockRequest and raised-shield consistency state.** A V21 server cannot
// name the client request, and a V22 client cannot accept a snapshot that omits the
// matching blocking statements after a clean handshake.
//
// **V23 appends LootTakeAllRequest.** A V22 server cannot name that tag and closes the
// session rather than dropping it, so a V23 client must not handshake with one and then
// discover the mismatch on the first corpse it empties. Nothing server -> client is added:
// the answer is the LootState, LootClosed and TakeLoot/InventoryFull refusal V21 already
// defined.
//
// The rule that generalises, now that nine shapes have been argued: **ask what the receiver
// does with the value it does not recognise, not which way it travelled.** Dropping it is a
// bump avoided; refusing it is a bump owed. The same words are in schemas/common.fbs,
// schemas/AGENTS.md and the Rust half of this pin — this file is the copy that was missing
// them, and a rule stated in three places out of four is a rule somebody will read the wrong
// version of.
// **V24 appends the map: six members, three of them client -> server.** A V23 server
// cannot name MapTileRequest, MarkerPlaceRequest or MarkerRemoveRequest and closes the
// session rather than dropping any of them, so each owes the bump on its own and the three
// are taken together. The three that travel back would have owed nothing alone. The two
// refusal enums also gain members, and those do not independently owe a bump either: both
// decoders are total by design and read an unrecognised member as Unknown, which costs a
// player one sentence rather than the session.
//
// **V25 appends the settlement: five members, two of them client -> server.** A V24 server
// cannot name NpcInteractRequest or TradeRequest and closes the session on either, so each
// owes the bump alone. The bump is owed a third time from outside this union, and that one
// is the argument worth keeping: MobKind.Villager is an enum member travelling
// server -> client, and it moves the version where ActionRefused's new members do not,
// because MobState.kind is *refused* when the receiver cannot name it while a refusal
// reason is read as Unknown. Ask what the receiver does with the value it does not
// recognise, not which way it travelled.
//
// **V28 appends player-to-player trading.** PlayerTradeRequest travels client to server,
// so a V27 server would close the session on a tag it cannot name. The state and close
// messages travel back and would be safely droppable alone; the request is the bump owed.
// V29 then appends the persisted absolute world tick; absence would look like a fresh
// world rather than an unreadable one, so that append owes a clean handshake refusal.
func TestProtocolV29AddsTheAbsoluteWorldClock(t *testing.T) {
	t.Parallel()

	if got := uint16(vnet.ProtocolVersionCurrent); got != 29 {
		t.Fatalf("ProtocolVersion.Current = %d, want 29", got)
	}
	want := []vnet.Payload{
		vnet.PayloadClientHello,
		vnet.PayloadServerWelcome,
		vnet.PayloadServerReject,
		vnet.PayloadChunkData,
		vnet.PayloadChunkUnload,
		vnet.PayloadPlayerInput,
		vnet.PayloadEntitySnapshot,
		vnet.PayloadBlockEditRequest,
		vnet.PayloadBlockUpdate,
		vnet.PayloadInventoryState,
		vnet.PayloadChunkResendRequest,
		vnet.PayloadMineRequest,
		vnet.PayloadMineProgress,
		vnet.PayloadInventoryMoveRequest,
		vnet.PayloadAttackRequest,
		vnet.PayloadCraftRequest,
		vnet.PayloadRepairRequest,
		vnet.PayloadPlaceStructureRequest,
		vnet.PayloadRemoveStructureRequest,
		vnet.PayloadActionRefused,
		vnet.PayloadServerCharacterList,
		vnet.PayloadSelectCharacterRequest,
		vnet.PayloadCreateCharacterRequest,
		vnet.PayloadPlayerAppearance,
		vnet.PayloadDropItemRequest,
		vnet.PayloadLeaveRequest,
		vnet.PayloadLeaveStarted,
		vnet.PayloadConsumeRequest,
		vnet.PayloadChatRequest,
		vnet.PayloadChatMessage,
		vnet.PayloadPartyRequest,
		vnet.PayloadPartyInvite,
		vnet.PayloadLootOpenRequest,
		vnet.PayloadLootTakeRequest,
		vnet.PayloadLootState,
		vnet.PayloadLootClosed,
		vnet.PayloadMobHit,
		vnet.PayloadBlockRequest,
		vnet.PayloadLootTakeAllRequest,
		vnet.PayloadMapTileRequest,
		vnet.PayloadMapTile,
		vnet.PayloadMapExplored,
		vnet.PayloadMarkerPlaceRequest,
		vnet.PayloadMarkerRemoveRequest,
		vnet.PayloadMarkerList,
		vnet.PayloadResidentAppearance,
		vnet.PayloadNpcInteractRequest,
		vnet.PayloadVendorState,
		vnet.PayloadTradeRequest,
		vnet.PayloadVendorClosed,
		// V26's two, both server -> client. Neither owes the bump: an older client drops
		// a tag it cannot name and loses a warning or some shading. StructureKind.Runestone
		// is what moved the version, and it is not in this union at all.
		vnet.PayloadStormWarning,
		vnet.PayloadWardsNearby,
		// V27's request asks; its result is the only answer that resumes play.
		vnet.PayloadLeaveCancelRequest,
		vnet.PayloadLeaveCancelResult,
		// The stable contract's complete learned set, followed by its two intents.
		vnet.PayloadLearnedMounts,
		vnet.PayloadMountRequest,
		vnet.PayloadDismountRequest,
		// V28's intent, followed by the complete state and explicit end marker.
		vnet.PayloadPlayerTradeRequest,
		vnet.PayloadPlayerTradeState,
		vnet.PayloadPlayerTradeClosed,
	}
	for index, payload := range want {
		if got := byte(payload); got != byte(index+1) {
			t.Errorf("%s tag = %d, want %d", payload, got, index+1)
		}
	}

	// Membership, not just ordering. A swing is still answered by the next snapshot and
	// nothing else, and so is a craft and a repair; a *refused* placement is answered by
	// ActionRefused, and an accepted one is not. V12's LeaveStarted is the deliberately
	// exceptional acknowledgement: it reports the server's timer, never a client-owned
	// outcome. The size of the union is the only place that membership can be checked.
	// V7's four are the handshake's new phase and the appearance
	// that rides beside it, and none of them acknowledges anything either: a character is
	// chosen and the answer is ServerWelcome. V8's one does not break the run: a drop is
	// answered by the complete InventoryState that follows it and by the ItemDropState in
	// the next snapshot, both of which already existed. NONE is the implicit zero member
	// every FlatBuffers union carries.
	if got := len(vnet.EnumNamesPayload); got != len(want)+1 {
		t.Errorf("Payload has %d members, want %d plus NONE — a new member needs a decision, not a test edit", got, len(want))
	}
}

func TestLootRequestsCarryOnlyIntentAndRejectAbsentIdentities(t *testing.T) {
	t.Parallel()

	openWant := LootOpenRequest{CorpseID: 91, ClientTick: 44}
	open, err := Decode(EncodeLootOpenRequest(openWant))
	if err != nil || open.Kind != vnet.PayloadLootOpenRequest || open.LootOpen == nil || *open.LootOpen != openWant {
		t.Fatalf("loot open round trip = %+v, %v; want %+v", open, err, openWant)
	}
	takeWant := LootTakeRequest{CorpseID: 91, EntryID: 7, Revision: 3, ClientTick: 45}
	take, err := Decode(EncodeLootTakeRequest(takeWant))
	if err != nil || take.Kind != vnet.PayloadLootTakeRequest || take.LootTake == nil || *take.LootTake != takeWant {
		t.Fatalf("loot take round trip = %+v, %v; want %+v", take, err, takeWant)
	}

	// V23's take-everything intent is the single take with the entry id removed: the
	// server owns the order and the fit, so there is nothing left for the client to name.
	// Its revision is still mandatory, because a request written against a window the
	// server has since changed must be refused rather than applied to a different one.
	takeAllWant := LootTakeAllRequest{CorpseID: 91, Revision: 3, ClientTick: 46}
	takeAll, err := Decode(EncodeLootTakeAllRequest(takeAllWant))
	if err != nil || takeAll.Kind != vnet.PayloadLootTakeAllRequest ||
		takeAll.LootTakeAll == nil || *takeAll.LootTakeAll != takeAllWant {
		t.Fatalf("loot take-all round trip = %+v, %v; want %+v", takeAll, err, takeAllWant)
	}

	for name, frame := range map[string][]byte{
		"open without corpse":       EncodeLootOpenRequest(LootOpenRequest{}),
		"take without corpse":       EncodeLootTakeRequest(LootTakeRequest{EntryID: 1, Revision: 1}),
		"take without entry":        EncodeLootTakeRequest(LootTakeRequest{CorpseID: 1, Revision: 1}),
		"take without revision":     EncodeLootTakeRequest(LootTakeRequest{CorpseID: 1, EntryID: 1}),
		"take all without corpse":   EncodeLootTakeAllRequest(LootTakeAllRequest{Revision: 1}),
		"take all without revision": EncodeLootTakeAllRequest(LootTakeAllRequest{CorpseID: 1}),
	} {
		if _, decodeErr := Decode(frame); !errors.Is(decodeErr, ErrMalformed) {
			t.Errorf("%s decoded with %v, want ErrMalformed", name, decodeErr)
		}
	}

}

// A map tile is asked for on the grid, and the decode boundary is where the grid is held.
//
// Scale first, origin second, and the order is the argument: alignment is computed *from*
// the scale, so there is nothing to test a misaligned origin against until the scale is a
// member this contract names. The absent-field zero is one of the values that is not.
//
// schemas/world.fbs states both as refusals — RequestMapTile with TileMisaligned — and
// allows a decoder that can see the violation from the frame alone to close the session
// instead. This is that decoder, and this is the stricter of the two answers.
func TestMapTileRequestIsRefusedOffTheGridAndAtAnUnknownScale(t *testing.T) {
	t.Parallel()

	for _, want := range []MapTileRequest{
		{OriginX: 0, OriginZ: 0, Scale: 1, ClientTick: 7},
		{OriginX: 64, OriginZ: -128, Scale: 1, ClientTick: 8},
		{OriginX: 256, OriginZ: -256, Scale: 4, ClientTick: 9},
		{OriginX: -1024, OriginZ: 2048, Scale: 16, ClientTick: 10},
	} {
		msg, err := Decode(EncodeMapTileRequest(want))
		if err != nil || msg.Kind != vnet.PayloadMapTileRequest ||
			msg.MapTileRequest == nil || *msg.MapTileRequest != want {
			t.Fatalf("map tile round trip = %+v, %v; want %+v", msg, err, want)
		}
	}

	for name, request := range map[string]MapTileRequest{
		"absent scale":        {OriginX: 0, OriginZ: 0},
		"scale 2":             {OriginX: 0, OriginZ: 0, Scale: 2},
		"scale 255":           {OriginX: 0, OriginZ: 0, Scale: 255},
		"x off the grid":      {OriginX: 1, OriginZ: 0, Scale: 1},
		"z off the grid":      {OriginX: 0, OriginZ: 63, Scale: 1},
		"aligned for a finer": {OriginX: 64, OriginZ: 0, Scale: 4},
		"negative off grid":   {OriginX: -1, OriginZ: 0, Scale: 16},
	} {
		if _, err := Decode(EncodeMapTileRequest(request)); !errors.Is(err, ErrMalformed) {
			t.Errorf("%s decoded with %v, want ErrMalformed", name, err)
		}
	}

	// The three scales, their block spans and the exact size of the mask each one needs.
	// Scale 1 is the case that rounds: 4 chunk columns do not fill a byte, and the
	// contract says the four unused high bits are zero rather than that the vector is
	// half a byte long.
	for _, row := range []struct {
		scale         uint8
		span          int32
		exploredBytes int
	}{{1, 64, 1}, {4, 256, 8}, {16, 1024, 128}} {
		if got := MapTileSpan(row.scale); got != row.span {
			t.Errorf("MapTileSpan(%d) = %d, want %d", row.scale, got, row.span)
		}
		if got := MapTileExploredBytes(row.scale); got != row.exploredBytes {
			t.Errorf("MapTileExploredBytes(%d) = %d, want %d", row.scale, got, row.exploredBytes)
		}
	}
	if got := MapTileSpan(3); got != 0 {
		t.Errorf("MapTileSpan(3) = %d, want 0 for a scale this contract has no member for", got)
	}
	if got := MapTileExploredBytes(3); got != 0 {
		t.Errorf("MapTileExploredBytes(3) = %d, want 0", got)
	}
}

// A map height is a byte of shading that saturates at both ends.
//
// **The saturation is the half worth pinning.** Terrain here ranges over roughly
// [-11, 139] and nothing bounds it on either side, so an encoding that wrapped would draw
// the tallest peak in the world as a trench — a plausible picture of somewhere else. The
// bias is what makes the world's negative half survive one byte at all, and it is
// asserted at the exact block it puts at zero.
func TestAMapHeightIsBiasedAndSaturates(t *testing.T) {
	t.Parallel()

	for _, row := range []struct {
		y    int
		want byte
	}{
		{-1000, 0}, {-65, 0}, {-64, 0}, {-63, 1}, {-11, 53},
		{0, 64}, {47, 111}, {64, 128}, {139, 203},
		{191, 255}, {192, 255}, {1000, 255},
	} {
		if got := MapTileHeight(row.y); got != row.want {
			t.Errorf("MapTileHeight(%d) = %d, want %d", row.y, got, row.want)
		}
	}
}

// A mark is placed by kind, place and text, and never by id: identity is the server's.
//
// The note is bounded in bytes because that is what the wire carries, and it is checked
// for valid UTF-8 here because string() over invalid bytes succeeds silently in Go — so
// this is the only boundary that can tell.
func TestMarkerRequestsCarryOnlyIntentAndAreBoundedAtTheDecodeBoundary(t *testing.T) {
	t.Parallel()

	placeWant := MarkerPlaceRequest{X: -900, Z: 1200, Kind: vnet.MarkerKindCave, Note: "cold air", ClientTick: 11}
	place, err := Decode(EncodeMarkerPlaceRequest(placeWant))
	if err != nil || place.Kind != vnet.PayloadMarkerPlaceRequest ||
		place.MarkerPlace == nil || *place.MarkerPlace != placeWant {
		t.Fatalf("marker place round trip = %+v, %v; want %+v", place, err, placeWant)
	}

	// An empty note is ordinary, and absent and empty are the same empty note.
	emptyWant := MarkerPlaceRequest{X: 0, Z: 0, Kind: vnet.MarkerKindNote, ClientTick: 12}
	empty, err := Decode(EncodeMarkerPlaceRequest(emptyWant))
	if err != nil || empty.MarkerPlace == nil || *empty.MarkerPlace != emptyWant {
		t.Fatalf("empty note round trip = %+v, %v; want %+v", empty, err, emptyWant)
	}

	// Exactly at the bound is accepted; one byte past it is not.
	atBound := MarkerPlaceRequest{Kind: vnet.MarkerKindCamp, Note: strings.Repeat("a", MarkerNoteMaxBytes)}
	if _, err := Decode(EncodeMarkerPlaceRequest(atBound)); err != nil {
		t.Errorf("a %d-byte note was refused: %v", MarkerNoteMaxBytes, err)
	}

	removeWant := MarkerRemoveRequest{MarkerID: 12345, ClientTick: 13}
	remove, err := Decode(EncodeMarkerRemoveRequest(removeWant))
	if err != nil || remove.Kind != vnet.PayloadMarkerRemoveRequest ||
		remove.MarkerRemove == nil || *remove.MarkerRemove != removeWant {
		t.Fatalf("marker remove round trip = %+v, %v; want %+v", remove, err, removeWant)
	}

	for name, frame := range map[string][]byte{
		"absent kind":     EncodeMarkerPlaceRequest(MarkerPlaceRequest{Note: "somewhere"}),
		"unknown kind":    EncodeMarkerPlaceRequest(MarkerPlaceRequest{Kind: vnet.MarkerKind(200)}),
		"note past bound": EncodeMarkerPlaceRequest(MarkerPlaceRequest{Kind: vnet.MarkerKindNote, Note: strings.Repeat("a", MarkerNoteMaxBytes+1)}),
		// Bytes, not characters: 40 three-byte runes are 120 bytes and pass, 41 are 123
		// and do not. A bound counted in characters would have accepted the second.
		"multibyte past bound":   EncodeMarkerPlaceRequest(MarkerPlaceRequest{Kind: vnet.MarkerKindNote, Note: strings.Repeat("ᛗ", 41)}),
		"note that is not utf-8": EncodeMarkerPlaceRequest(MarkerPlaceRequest{Kind: vnet.MarkerKindNote, Note: string([]byte{0xff, 0xfe})}),
		"removal without an id":  EncodeMarkerRemoveRequest(MarkerRemoveRequest{ClientTick: 1}),
	} {
		if _, decodeErr := Decode(frame); !errors.Is(decodeErr, ErrMalformed) {
			t.Errorf("%s decoded with %v, want ErrMalformed", name, decodeErr)
		}
	}
	if _, err := Decode(EncodeMarkerPlaceRequest(MarkerPlaceRequest{Kind: vnet.MarkerKindNote, Note: strings.Repeat("ᛗ", 40)})); err != nil {
		t.Errorf("40 three-byte runes are exactly %d bytes and were refused: %v", MarkerNoteMaxBytes, err)
	}

	// The kind is a byte on the wire, and the refusals above pin membership rather than
	// number: they would pass unchanged with Cave and Monster swapped, which turns every
	// cave already saved to a marker file into a monster. Integers, in the shape V7 pins
	// HairModel's and V25 pins ResidentRole's.
	for name, pair := range map[string][2]byte{
		"MarkerKind.Unknown":  {byte(vnet.MarkerKindUnknown), 0},
		"MarkerKind.Resource": {byte(vnet.MarkerKindResource), 1},
		"MarkerKind.Cave":     {byte(vnet.MarkerKindCave), 2},
		"MarkerKind.Monster":  {byte(vnet.MarkerKindMonster), 3},
		"MarkerKind.Boss":     {byte(vnet.MarkerKindBoss), 4},
		"MarkerKind.Camp":     {byte(vnet.MarkerKindCamp), 5},
		"MarkerKind.Village":  {byte(vnet.MarkerKindVillage), 6},
		// Note is last because it is the fallback, not because it arrived last.
		"MarkerKind.Note": {byte(vnet.MarkerKindNote), 7},
	} {
		if pair[0] != pair[1] {
			t.Errorf("%s = %d, want %d", name, pair[0], pair[1])
		}
	}
	// An eighth kind is invisible to everything above — it is in neither list — while
	// MarkerKindOK answers false for it and internal/persist refuses the file that
	// carries it as corrupt. That decision is taken here or it is not taken.
	if got := len(vnet.EnumNamesMarkerKind); got != 8 {
		t.Errorf("MarkerKind has %d members, want 8 — a new one needs a decision, not a test edit", got)
	}
}

// The three server-to-client map messages encode the shapes their decoders are held to.
//
// Written as the encoders' own round trip through the generated accessors, because this
// side has no decode arm for a payload only a server sends — the client's decoder is what
// enforces the lengths, and it is tested there.
func TestMapServerMessagesCarryCompleteTilesLedgerPagesAndMarks(t *testing.T) {
	t.Parallel()

	tile := MapTile{
		OriginX: 256, OriginZ: -512, Scale: 4,
		Height:   make([]byte, MapTileCells),
		Surface:  make([]byte, MapTileCells),
		Explored: make([]byte, MapTileExploredBytes(4)),
	}
	tile.Height[0], tile.Height[MapTileCells-1] = 64, 255
	tile.Surface[0] = byte(vnet.MapSurfaceForest)
	tile.Surface[MapTileCells-1] = byte(vnet.MapSurfaceSettlement)
	tile.Explored[0] = 0b0000_0011

	env := vnet.GetRootAsEnvelope(EncodeMapTile(tile), 0)
	if env.PayloadType() != vnet.PayloadMapTile {
		t.Fatalf("payload = %s, want MapTile", env.PayloadType())
	}
	table := payloadTable(t, env)
	decoded := new(vnet.MapTile)
	decoded.Init(table.Bytes, table.Pos)
	if decoded.OriginX() != tile.OriginX || decoded.OriginZ() != tile.OriginZ || decoded.Scale() != tile.Scale {
		t.Fatalf("tile header = (%d, %d) scale %d", decoded.OriginX(), decoded.OriginZ(), decoded.Scale())
	}
	if !bytes.Equal(decoded.HeightBytes(), tile.Height) {
		t.Error("height did not survive the round trip")
	}
	if !bytes.Equal(decoded.SurfaceBytes(), tile.Surface) {
		t.Error("surface did not survive the round trip")
	}
	if !bytes.Equal(decoded.ExploredBytes(), tile.Explored) {
		t.Error("explored mask did not survive the round trip")
	}

	ledger := MapExplored{Columns: []MapColumn{{CX: 0, CZ: 0}, {CX: -3, CZ: 91}}}
	ledgerEnv := vnet.GetRootAsEnvelope(EncodeMapExplored(ledger), 0)
	if ledgerEnv.PayloadType() != vnet.PayloadMapExplored {
		t.Fatalf("payload = %s, want MapExplored", ledgerEnv.PayloadType())
	}
	ledgerTable := payloadTable(t, ledgerEnv)
	explored := new(vnet.MapExplored)
	explored.Init(ledgerTable.Bytes, ledgerTable.Pos)
	if explored.ColumnsLength() != len(ledger.Columns) {
		t.Fatalf("ledger page has %d columns, want %d", explored.ColumnsLength(), len(ledger.Columns))
	}
	for index, want := range ledger.Columns {
		column := new(vnet.MapColumn)
		if !explored.Columns(column, index) {
			t.Fatalf("column %d absent", index)
		}
		if got := (MapColumn{CX: column.Cx(), CZ: column.Cz()}); got != want {
			t.Errorf("column %d = %+v, want %+v", index, got, want)
		}
	}

	marks := MarkerList{Markers: []Marker{
		{MarkerID: 1, X: 10, Z: -10, Kind: vnet.MarkerKindBoss, Note: "the draugr"},
		{MarkerID: 2, X: 0, Z: 0, Kind: vnet.MarkerKindNote},
	}}
	marksEnv := vnet.GetRootAsEnvelope(EncodeMarkerList(marks), 0)
	if marksEnv.PayloadType() != vnet.PayloadMarkerList {
		t.Fatalf("payload = %s, want MarkerList", marksEnv.PayloadType())
	}
	marksTable := payloadTable(t, marksEnv)
	list := new(vnet.MarkerList)
	list.Init(marksTable.Bytes, marksTable.Pos)
	if list.MarkersLength() != len(marks.Markers) {
		t.Fatalf("list has %d marks, want %d", list.MarkersLength(), len(marks.Markers))
	}
	for index, want := range marks.Markers {
		marker := new(vnet.Marker)
		if !list.Markers(marker, index) {
			t.Fatalf("mark %d absent", index)
		}
		got := Marker{MarkerID: marker.MarkerId(), X: marker.X(), Z: marker.Z(),
			Kind: marker.Kind(), Note: string(marker.Note())}
		if got != want {
			t.Errorf("mark %d = %+v, want %+v", index, got, want)
		}
	}

	// An empty list is a legal message and the one a character who has marked nothing
	// receives. It is MapExplored, the additive one, whose empty page states nothing.
	emptyEnv := vnet.GetRootAsEnvelope(EncodeMarkerList(MarkerList{}), 0)
	emptyTable := payloadTable(t, emptyEnv)
	emptyList := new(vnet.MarkerList)
	emptyList.Init(emptyTable.Bytes, emptyTable.Pos)
	if emptyList.MarkersLength() != 0 {
		t.Errorf("empty list has %d marks, want 0", emptyList.MarkersLength())
	}
}

// A resident is addressed by id, and a trade is asked for by item, count and direction.
//
// **Neither request may carry a price or a total, and the strongest statement this test
// can make about that is a negative one**: the generated TradeRequest has no field to
// put one in, so the assertion lives in the schema and in the field list below rather
// than in an assertion here. What is tested is the four absent-field zeroes, each of
// which is a frame no correct client sends.
func TestSettlementRequestsCarryOnlyIntentAndRejectAbsentIdentities(t *testing.T) {
	t.Parallel()

	interactWant := NpcInteractRequest{EntityID: 4242, ClientTick: 21}
	interact, err := Decode(EncodeNpcInteractRequest(interactWant))
	if err != nil || interact.Kind != vnet.PayloadNpcInteractRequest ||
		interact.NpcInteract == nil || *interact.NpcInteract != interactWant {
		t.Fatalf("npc interact round trip = %+v, %v; want %+v", interact, err, interactWant)
	}

	// Both directions round trip, because `buying` is a bool and both of its values are
	// legal. **The false case is genuinely an absent field, and that is what makes it
	// safe**: TradeRequestAddBuying is `PrependBoolSlot(3, buying, false)`, so a sale
	// writes no `buying` slot at all and the reader answers false off the vtable's
	// default. Legal here precisely because the field is a bool — absent, default and
	// "selling" are one value, and there is no third meaning for the elision to be
	// confused with. The four ids below are the opposite case: their absent-field zero
	// is not a legal value, which is why each of them is refused by name.
	for name, want := range map[string]TradeRequest{
		"buying":  {EntityID: 4242, ItemID: 31, Count: 4, Buying: true, Revision: 2, ClientTick: 22},
		"selling": {EntityID: 4242, ItemID: 8, Count: 1, Buying: false, Revision: 2, ClientTick: 23},
	} {
		trade, tradeErr := Decode(EncodeTradeRequest(want))
		if tradeErr != nil || trade.Kind != vnet.PayloadTradeRequest ||
			trade.Trade == nil || *trade.Trade != want {
			t.Fatalf("%s round trip = %+v, %v; want %+v", name, trade, tradeErr, want)
		}
	}

	for name, frame := range map[string][]byte{
		"interact without an entity": EncodeNpcInteractRequest(NpcInteractRequest{ClientTick: 1}),
		"trade without an entity":    EncodeTradeRequest(TradeRequest{ItemID: 1, Count: 1, Revision: 1}),
		"trade without an item":      EncodeTradeRequest(TradeRequest{EntityID: 1, Count: 1, Revision: 1}),
		"trade for nothing":          EncodeTradeRequest(TradeRequest{EntityID: 1, ItemID: 1, Revision: 1}),
		"trade without a revision":   EncodeTradeRequest(TradeRequest{EntityID: 1, ItemID: 1, Count: 1}),
	} {
		if _, decodeErr := Decode(frame); !errors.Is(decodeErr, ErrMalformed) {
			t.Errorf("%s decoded with %v, want ErrMalformed", name, decodeErr)
		}
	}
}

func TestPlayerTradeRequestDecodesEveryActionAndCopiesIntent(t *testing.T) {
	t.Parallel()

	for _, action := range []vnet.PlayerTradeAction{
		vnet.PlayerTradeActionOpen,
		vnet.PlayerTradeActionSetItem,
		vnet.PlayerTradeActionClearItem,
		vnet.PlayerTradeActionSetSilver,
		vnet.PlayerTradeActionConfirm,
		vnet.PlayerTradeActionCancel,
	} {
		want := PlayerTradeRequest{
			Action: action, TargetEntityID: 71, TradeSlot: 4, PackSlot: 33,
			Silver: 120, Revision: 9, ClientTick: 44,
		}
		message, err := Decode(EncodePlayerTradeRequest(want))
		if err != nil {
			t.Fatalf("Decode(%s): %v", action, err)
		}
		if message.Kind != vnet.PayloadPlayerTradeRequest || message.PlayerTrade == nil || *message.PlayerTrade != want {
			t.Errorf("Decode(%s) = %+v, want %+v", action, message, want)
		}
	}

	for _, action := range []vnet.PlayerTradeAction{vnet.PlayerTradeActionUnknown, vnet.PlayerTradeAction(200)} {
		if _, err := Decode(EncodePlayerTradeRequest(PlayerTradeRequest{Action: action})); !errors.Is(err, ErrMalformed) {
			t.Errorf("Decode(action %d) = %v, want ErrMalformed", action, err)
		}
	}
}

func TestPlayerTradeStateRoundTripsCompleteAndEmptyOffers(t *testing.T) {
	t.Parallel()

	want := PlayerTradeState{
		PartnerEntityID: 72,
		PartnerName:     "Astrid",
		Revision:        8,
		MyOffer: []PlayerTradeSlot{
			{TradeSlot: 0, PackSlot: 12, ItemID: 31, Count: 4},
			{TradeSlot: 4, PackSlot: 7, ItemID: 8, Count: 1, Durability: 3, MaxDurability: 10},
		},
		TheirOffer: []PlayerTradeSlot{
			// A caller cannot leak this value: the encoder always writes zero for a
			// partner's pack slot, and the decoded expectation below pins that boundary.
			{TradeSlot: 2, PackSlot: 99, ItemID: 22, Count: 2},
		},
		MySilver: 40, TheirSilver: 90, MyConfirmed: true,
	}
	message, err := Decode(EncodePlayerTradeState(want))
	if err != nil {
		t.Fatalf("Decode complete state: %v", err)
	}
	want.TheirOffer[0].PackSlot = 0
	if message.Kind != vnet.PayloadPlayerTradeState || message.PlayerTradeState == nil ||
		!reflect.DeepEqual(*message.PlayerTradeState, want) {
		t.Fatalf("complete state = %+v, want %+v", message.PlayerTradeState, want)
	}

	emptyWant := PlayerTradeState{PartnerEntityID: 73, PartnerName: "", Revision: 1}
	empty, err := Decode(EncodePlayerTradeState(emptyWant))
	if err != nil {
		t.Fatalf("Decode empty state: %v", err)
	}
	if empty.PlayerTradeState == nil || len(empty.PlayerTradeState.MyOffer) != 0 || len(empty.PlayerTradeState.TheirOffer) != 0 || empty.PlayerTradeState.PartnerName != "" {
		t.Fatalf("empty state = %+v, want two empty offers and a present empty name", empty.PlayerTradeState)
	}
}

func TestPlayerTradeStateRefusesEveryWireInvariantViolation(t *testing.T) {
	t.Parallel()

	ordinary := PlayerTradeSlot{TradeSlot: 0, PackSlot: 3, ItemID: 31, Count: 2}
	durable := PlayerTradeSlot{TradeSlot: 1, PackSlot: 4, ItemID: 8, Count: 1, Durability: 3, MaxDurability: 10}
	valid := PlayerTradeState{
		PartnerEntityID: 72, PartnerName: "Astrid", Revision: 8,
		MyOffer:    []PlayerTradeSlot{ordinary, durable},
		TheirOffer: []PlayerTradeSlot{{TradeSlot: 0, ItemID: 22, Count: 1}},
	}

	tests := []struct {
		name  string
		state PlayerTradeState
		raw   playerTradeStateWireOptions
	}{
		{name: "partner id is zero", state: func() PlayerTradeState { got := valid; got.PartnerEntityID = 0; return got }()},
		{name: "partner name is absent", state: valid, raw: playerTradeStateWireOptions{omitName: true}},
		{name: "partner name is not UTF-8", state: func() PlayerTradeState { got := valid; got.PartnerName = string([]byte{0xff}); return got }()},
		{name: "revision is zero", state: func() PlayerTradeState { got := valid; got.Revision = 0; return got }()},
		{name: "my offer is absent", state: valid, raw: playerTradeStateWireOptions{omitMyOffer: true}},
		{name: "their offer is absent", state: valid, raw: playerTradeStateWireOptions{omitTheirOffer: true}},
		{name: "my offer has six entries", state: func() PlayerTradeState {
			got := valid
			got.MyOffer = []PlayerTradeSlot{{TradeSlot: 0}, {TradeSlot: 1}, {TradeSlot: 2}, {TradeSlot: 3}, {TradeSlot: 4}, {TradeSlot: 0}}
			for index := range got.MyOffer {
				got.MyOffer[index].Count = 1
			}
			return got
		}()},
		{name: "their offer has six entries", state: func() PlayerTradeState {
			got := valid
			got.TheirOffer = []PlayerTradeSlot{{TradeSlot: 0}, {TradeSlot: 1}, {TradeSlot: 2}, {TradeSlot: 3}, {TradeSlot: 4}, {TradeSlot: 0}}
			for index := range got.TheirOffer {
				got.TheirOffer[index].Count = 1
			}
			return got
		}()},
		{name: "my offer repeats a trade slot", state: func() PlayerTradeState {
			got := valid
			got.MyOffer = []PlayerTradeSlot{ordinary, ordinary}
			return got
		}()},
		{name: "their offer repeats a trade slot", state: func() PlayerTradeState {
			got := valid
			got.TheirOffer = []PlayerTradeSlot{{TradeSlot: 1, Count: 1}, {TradeSlot: 1, Count: 1}}
			return got
		}()},
		{name: "trade slot is five", state: func() PlayerTradeState {
			got := valid
			got.MyOffer = []PlayerTradeSlot{{TradeSlot: 5, Count: 1}}
			return got
		}()},
		{name: "count is zero", state: func() PlayerTradeState {
			got := valid
			got.MyOffer = []PlayerTradeSlot{{TradeSlot: 0}}
			return got
		}()},
		{name: "durability has no maximum", state: func() PlayerTradeState {
			got := valid
			got.MyOffer = []PlayerTradeSlot{{TradeSlot: 0, Count: 1, Durability: 1}}
			return got
		}()},
		{name: "durability exceeds maximum", state: func() PlayerTradeState {
			got := valid
			got.MyOffer = []PlayerTradeSlot{{TradeSlot: 0, Count: 1, Durability: 11, MaxDurability: 10}}
			return got
		}()},
		{name: "durable stack has count two", state: func() PlayerTradeState {
			got := valid
			got.MyOffer = []PlayerTradeSlot{{TradeSlot: 0, Count: 2, Durability: 3, MaxDurability: 10}}
			return got
		}()},
		{name: "partner offer exposes a pack slot", state: func() PlayerTradeState {
			got := valid
			got.TheirOffer = []PlayerTradeSlot{{TradeSlot: 0, PackSlot: 7, Count: 1}}
			return got
		}(), raw: playerTradeStateWireOptions{writeTheirPackSlot: true}},
	}

	for _, test := range tests {
		test := test
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			frame := encodePlayerTradeStateWire(test.state, test.raw)
			if _, err := Decode(frame); !errors.Is(err, ErrMalformed) {
				t.Fatalf("Decode = %v, want ErrMalformed", err)
			}
		})
	}
}

func TestPlayerTradeCloseAndRefusalEnumsDecodeTotally(t *testing.T) {
	t.Parallel()

	for _, reason := range []vnet.PlayerTradeCloseReason{
		vnet.PlayerTradeCloseReasonUnknown,
		vnet.PlayerTradeCloseReasonCompleted,
		vnet.PlayerTradeCloseReasonCancelled,
		vnet.PlayerTradeCloseReasonOutOfReach,
		vnet.PlayerTradeCloseReasonDied,
		vnet.PlayerTradeCloseReasonDisconnected,
		vnet.PlayerTradeCloseReasonFailed,
	} {
		message, err := Decode(EncodePlayerTradeClosed(PlayerTradeClosed{PartnerEntityID: 72, Reason: reason}))
		if err != nil || message.PlayerTradeClosed == nil || message.PlayerTradeClosed.Reason != reason {
			t.Errorf("close reason %s decoded as %+v, %v", reason, message.PlayerTradeClosed, err)
		}
	}

	unknownClose, err := Decode(EncodePlayerTradeClosed(PlayerTradeClosed{PartnerEntityID: 72, Reason: vnet.PlayerTradeCloseReason(200)}))
	if err != nil || unknownClose.PlayerTradeClosed == nil || unknownClose.PlayerTradeClosed.Reason != vnet.PlayerTradeCloseReasonUnknown {
		t.Errorf("unknown close reason decoded as %+v, %v; want Unknown", unknownClose.PlayerTradeClosed, err)
	}
	if _, err := Decode(EncodePlayerTradeClosed(PlayerTradeClosed{})); !errors.Is(err, ErrMalformed) {
		t.Errorf("zero partner close decoded with %v, want ErrMalformed", err)
	}

	unknownRefusal, err := Decode(EncodeActionRefused(ActionRefused{
		Action: vnet.RefusedAction(200), Reason: vnet.RefusalReason(200),
	}))
	if err != nil || unknownRefusal.ActionRefused == nil ||
		unknownRefusal.ActionRefused.Action != vnet.RefusedActionUnknown ||
		unknownRefusal.ActionRefused.Reason != vnet.RefusalReasonUnknown {
		t.Errorf("unknown refusal decoded as %+v, %v; want both enums Unknown", unknownRefusal.ActionRefused, err)
	}
}

type playerTradeStateWireOptions struct {
	omitName           bool
	omitMyOffer        bool
	omitTheirOffer     bool
	writeTheirPackSlot bool
}

// encodePlayerTradeStateWire can omit required fields and preserve a partner pack slot;
// the production encoder intentionally cannot produce either malformed shape.
func encodePlayerTradeStateWire(state PlayerTradeState, options playerTradeStateWireOptions) []byte {
	b := flatbuffers.NewBuilder(256)
	var name flatbuffers.UOffsetT
	if !options.omitName {
		name = b.CreateString(state.PartnerName)
	}
	var theirOffer flatbuffers.UOffsetT
	if !options.omitTheirOffer {
		vnet.PlayerTradeStateStartTheirOfferVector(b, len(state.TheirOffer))
		for index := len(state.TheirOffer) - 1; index >= 0; index-- {
			slot := state.TheirOffer[index]
			packSlot := uint8(0)
			if options.writeTheirPackSlot {
				packSlot = slot.PackSlot
			}
			vnet.CreatePlayerTradeSlot(b, slot.TradeSlot, packSlot, slot.ItemID, slot.Count, slot.Durability, slot.MaxDurability)
		}
		theirOffer = b.EndVector(len(state.TheirOffer))
	}
	var myOffer flatbuffers.UOffsetT
	if !options.omitMyOffer {
		vnet.PlayerTradeStateStartMyOfferVector(b, len(state.MyOffer))
		for index := len(state.MyOffer) - 1; index >= 0; index-- {
			slot := state.MyOffer[index]
			vnet.CreatePlayerTradeSlot(b, slot.TradeSlot, slot.PackSlot, slot.ItemID, slot.Count, slot.Durability, slot.MaxDurability)
		}
		myOffer = b.EndVector(len(state.MyOffer))
	}

	vnet.PlayerTradeStateStart(b)
	vnet.PlayerTradeStateAddPartnerEntityId(b, state.PartnerEntityID)
	if !options.omitName {
		vnet.PlayerTradeStateAddPartnerName(b, name)
	}
	vnet.PlayerTradeStateAddRevision(b, state.Revision)
	if !options.omitMyOffer {
		vnet.PlayerTradeStateAddMyOffer(b, myOffer)
	}
	if !options.omitTheirOffer {
		vnet.PlayerTradeStateAddTheirOffer(b, theirOffer)
	}
	vnet.PlayerTradeStateAddMySilver(b, state.MySilver)
	vnet.PlayerTradeStateAddTheirSilver(b, state.TheirSilver)
	vnet.PlayerTradeStateAddMyConfirmed(b, state.MyConfirmed)
	vnet.PlayerTradeStateAddTheirConfirmed(b, state.TheirConfirmed)
	payload := vnet.PlayerTradeStateEnd(b)
	return finishEnvelope(b, vnet.PayloadPlayerTradeState, payload)
}

// The three server-to-client settlement messages encode the shapes their decoders are
// held to.
//
// Written as the encoders' own round trip through the generated accessors, for the reason
// the map's three are: this side has no decode arm for a payload only a server sends, and
// the client's decoder is where the name bound, the role vocabulary and the price
// invariants are enforced.
func TestSettlementServerMessagesCarryNamesRolesAndPrices(t *testing.T) {
	t.Parallel()

	resident := ResidentAppearance{
		EntityID: 900, Name: "Ingrid", HasName: true,
		Role:          vnet.ResidentRoleSmith,
		Appearance:    Appearance{SkinColor: 0x00C8A0_80, HairColor: 0x00_3A2A_1A, HairModel: vnet.HairModelBraided},
		HasAppearance: true,
	}
	env := vnet.GetRootAsEnvelope(EncodeResidentAppearance(resident), 0)
	if env.PayloadType() != vnet.PayloadResidentAppearance {
		t.Fatalf("payload = %s, want ResidentAppearance", env.PayloadType())
	}
	table := payloadTable(t, env)
	decoded := new(vnet.ResidentAppearance)
	decoded.Init(table.Bytes, table.Pos)
	if decoded.EntityId() != resident.EntityID {
		t.Errorf("entity = %d, want %d", decoded.EntityId(), resident.EntityID)
	}
	if got := string(decoded.Name()); got != resident.Name {
		t.Errorf("name = %q, want %q", got, resident.Name)
	}
	if decoded.Role() != resident.Role {
		t.Errorf("role = %s, want %s", decoded.Role(), resident.Role)
	}
	// Read back rather than merely present. A non-nil table says the field was written,
	// not that any of the resident's face was: an encoder that nested a zero Appearance
	// would satisfy a presence check and put the same bald, black-haired stranger over
	// every name in the settlement.
	appearance := decoded.Appearance(nil)
	if appearance == nil {
		t.Fatal("appearance did not survive the round trip")
	}
	if got := decodeAppearance(appearance); got != resident.Appearance {
		t.Errorf("appearance = %+v, want %+v", got, resident.Appearance)
	}

	// Every member the contract names is a role, and the absent-field zero is not.
	for _, role := range []vnet.ResidentRole{
		vnet.ResidentRoleVillager, vnet.ResidentRoleSmith, vnet.ResidentRoleCarpenter,
		vnet.ResidentRoleCook, vnet.ResidentRoleTrader, vnet.ResidentRoleGuard,
		vnet.ResidentRoleStablemaster,
	} {
		if !ResidentRoleOK(role) {
			t.Errorf("ResidentRoleOK(%s) = false, want true", role)
		}
	}
	for _, role := range []vnet.ResidentRole{vnet.ResidentRoleUnknown, vnet.ResidentRole(200)} {
		if ResidentRoleOK(role) {
			t.Errorf("ResidentRoleOK(%d) = true, want false", byte(role))
		}
	}

	// The number is the contract, and the loop above does not pin one: it asks whether
	// each name is a member, so swapping Smith and Carpenter passes it while relabelling
	// every smith already on a screen. Integers, in the shape V7 pins HairModel's.
	for name, pair := range map[string][2]byte{
		// The absent-field zero, first because it is the member ResidentRoleOK exists
		// to exclude.
		"ResidentRole.Unknown": {byte(vnet.ResidentRoleUnknown), 0},
		// Villager is 1 rather than last because it is the ordinary case, which is a
		// decision schemas/player.fbs argues and this is where it is held.
		"ResidentRole.Villager":     {byte(vnet.ResidentRoleVillager), 1},
		"ResidentRole.Smith":        {byte(vnet.ResidentRoleSmith), 2},
		"ResidentRole.Carpenter":    {byte(vnet.ResidentRoleCarpenter), 3},
		"ResidentRole.Cook":         {byte(vnet.ResidentRoleCook), 4},
		"ResidentRole.Trader":       {byte(vnet.ResidentRoleTrader), 5},
		"ResidentRole.Guard":        {byte(vnet.ResidentRoleGuard), 6},
		"ResidentRole.Stablemaster": {byte(vnet.ResidentRoleStablemaster), 7},
	} {
		if pair[0] != pair[1] {
			t.Errorf("%s = %d, want %d", name, pair[0], pair[1])
		}
	}
	// And the count, because both loops above enumerate only what they already know
	// about: an eighth member appended to the schema is in neither of them, so nothing
	// here would notice while ResidentRoleOK went on answering false for a role the
	// contract now names. That is the one question this file can ask about a member
	// nobody has written down yet.
	if got := len(vnet.EnumNamesResidentRole); got != 8 {
		t.Errorf("ResidentRole has %d members, want 8 — a new one needs a decision, not a test edit", got)
	}

	// A name is written exactly as given, over-long ones included: truncating here would
	// hide a caller's defect from the decoder, which is where the bound belongs.
	//
	// **The bound is enforced, and not here.** ResidentAppearance travels server -> client
	// only, so this package has no decode arm for it; the reader is the client's, and it
	// refuses a 33-byte name as RESIDENT_NAME_MAX_BYTES in client/src/net/codec.rs. The two
	// constants are one number written twice, once on each side of the wire, and
	// schemas/player.fbs's "at most 32 bytes" is what both of them copy -- which is a claim
	// about the world, so TestTheSharedBoundsAreOneNumberOnBothSidesOfTheWire is what
	// re-checks it. It did not exist when this comment was first written and the sentence
	// was true of nothing: ResidentNameMaxBytes = 33 left this whole package green while the
	// client went on refusing at 32. What the four
	// cases below pin is this side's only rule, the encoder's: verbatim, whatever the
	// caller handed it — over-long, multibyte and not-even-UTF-8 alike, so that a caller's
	// defect arrives at the decoder intact instead of being quietly made to look legal.
	// They are the MarkerNoteMaxBytes cases in the same file, minus the refusal, which
	// this package owns for MarkerPlaceRequest only because that one travels the other way.
	for label, text := range map[string]string{
		"exactly at the bound": strings.Repeat("a", ResidentNameMaxBytes),
		"one byte past it":     strings.Repeat("a", ResidentNameMaxBytes+1),
		// Bytes, not characters: 11 three-byte runes are 33 bytes and past the bound,
		// 10 are 30 and are not. A bound counted in characters would accept the first.
		"multibyte past the bound": strings.Repeat("ᛗ", 11),
		// string() over invalid bytes succeeds silently in Go, so a name that is not
		// UTF-8 reaches the wire intact and only a decoder can ever tell.
		"not valid utf-8": string([]byte{0xff, 0xfe}),
	} {
		named := new(vnet.ResidentAppearance)
		namedTable := payloadTable(t, vnet.GetRootAsEnvelope(EncodeResidentAppearance(
			ResidentAppearance{EntityID: 901, Name: text, HasName: true, Role: vnet.ResidentRoleGuard}), 0))
		named.Init(namedTable.Bytes, namedTable.Pos)
		if got := string(named.Name()); got != text {
			t.Errorf("a name %s arrived as %q (%d bytes), want %q (%d bytes) written verbatim",
				label, got, len(got), text, len(text))
		}
		// None of these frames sets HasAppearance, and an omitted appearance must read as
		// a null table rather than an appearance of zeros — the call
		// TestAPlayerAppearanceWithNoAppearanceIsAbsentRatherThanBlack makes for the
		// player's own, and the reason HasAppearance is a field at all.
		if named.Appearance(nil) != nil {
			t.Errorf("a name %s carried an appearance nobody set", label)
		}
	}

	// **The nameless resident is the frame a client has to refuse**, and HasName exists so
	// that there is a way to build one. Absent, not present-and-empty: the encoder must
	// honour the flag rather than infer the field from a non-empty string, so the empty
	// name below is written and the missing one is not.
	nameless := new(vnet.ResidentAppearance)
	namelessTable := payloadTable(t, vnet.GetRootAsEnvelope(EncodeResidentAppearance(
		ResidentAppearance{EntityID: 902, Role: vnet.ResidentRoleCook}), 0))
	nameless.Init(namelessTable.Bytes, namelessTable.Pos)
	if got := nameless.Name(); got != nil {
		t.Errorf("omitted name = %q, want nil", got)
	}
	emptyNamed := new(vnet.ResidentAppearance)
	emptyNamedTable := payloadTable(t, vnet.GetRootAsEnvelope(EncodeResidentAppearance(
		ResidentAppearance{EntityID: 903, HasName: true, Role: vnet.ResidentRoleCook}), 0))
	emptyNamed.Init(emptyNamedTable.Bytes, emptyNamedTable.Pos)
	if got := emptyNamed.Name(); got == nil || string(got) != "" {
		t.Errorf("present empty name = %q, want a present empty string", got)
	}

	state := VendorState{
		EntityID: 900, Revision: 3,
		Sells: []VendorEntry{{ItemID: 31, Price: 12}, {ItemID: 8, Price: 40}},
		Buys:  []VendorEntry{{ItemID: 31, Price: 5}},
	}
	stateEnv := vnet.GetRootAsEnvelope(EncodeVendorState(state), 0)
	if stateEnv.PayloadType() != vnet.PayloadVendorState {
		t.Fatalf("payload = %s, want VendorState", stateEnv.PayloadType())
	}
	stateTable := payloadTable(t, stateEnv)
	prices := new(vnet.VendorState)
	prices.Init(stateTable.Bytes, stateTable.Pos)
	if prices.EntityId() != state.EntityID || prices.Revision() != state.Revision {
		t.Fatalf("vendor header = entity %d revision %d", prices.EntityId(), prices.Revision())
	}
	if prices.SellsLength() != len(state.Sells) || prices.BuysLength() != len(state.Buys) {
		t.Fatalf("vendor has %d sells and %d buys, want %d and %d",
			prices.SellsLength(), prices.BuysLength(), len(state.Sells), len(state.Buys))
	}
	// The same item at two prices in the two directions is the ordinary case: the spread
	// is what a stall is, and nothing here collapses the two vectors into one.
	for index, want := range state.Sells {
		entry := new(vnet.VendorEntry)
		if !prices.Sells(entry, index) {
			t.Fatalf("sells[%d] absent", index)
		}
		if got := (VendorEntry{ItemID: entry.ItemId(), Price: entry.Price()}); got != want {
			t.Errorf("sells[%d] = %+v, want %+v", index, got, want)
		}
	}
	for index, want := range state.Buys {
		entry := new(vnet.VendorEntry)
		if !prices.Buys(entry, index) {
			t.Fatalf("buys[%d] absent", index)
		}
		if got := (VendorEntry{ItemID: entry.ItemId(), Price: entry.Price()}); got != want {
			t.Errorf("buys[%d] = %+v, want %+v", index, got, want)
		}
	}

	// A vendor that only sells writes an empty `buys` rather than omitting it: the
	// contract requires both vectors present, and the client refuses an absent one.
	sellOnly := vnet.GetRootAsEnvelope(EncodeVendorState(VendorState{EntityID: 900, Revision: 4, Sells: []VendorEntry{{ItemID: 31, Price: 12}}}), 0)
	sellOnlyTable := payloadTable(t, sellOnly)
	sellOnlyState := new(vnet.VendorState)
	sellOnlyState.Init(sellOnlyTable.Bytes, sellOnlyTable.Pos)
	if sellOnlyState.BuysLength() != 0 {
		t.Errorf("a sell-only vendor carries %d buys, want 0", sellOnlyState.BuysLength())
	}
	// Present-and-empty rather than absent, read off the vtable because that is the only
	// place the two differ: BuysLength answers 0 for both, and the client refuses one of
	// them. `buys` is the fourth field, so its vtable slot is 4 + 2*3.
	const buysVTableSlot = flatbuffers.VOffsetT(10)
	sellOnlyVendorTable := sellOnlyState.Table()
	if sellOnlyVendorTable.Offset(buysVTableSlot) == 0 {
		t.Error("a sell-only vendor omitted buys; the contract requires an empty vector")
	}

	// The other direction, which is not the same assertion: the two vectors are built by
	// two separate loops over two separate slices, so one of them can be right while the
	// other is not. `sells` is the third field, so its vtable slot is 4 + 2*2.
	const sellsVTableSlot = flatbuffers.VOffsetT(8)
	buyOnly := vnet.GetRootAsEnvelope(EncodeVendorState(VendorState{EntityID: 900, Revision: 5, Buys: []VendorEntry{{ItemID: 8, Price: 3}}}), 0)
	buyOnlyTable := payloadTable(t, buyOnly)
	buyOnlyState := new(vnet.VendorState)
	buyOnlyState.Init(buyOnlyTable.Bytes, buyOnlyTable.Pos)
	if buyOnlyState.SellsLength() != 0 {
		t.Errorf("a buy-only vendor carries %d sells, want 0", buyOnlyState.SellsLength())
	}
	buyOnlyVendorTable := buyOnlyState.Table()
	if buyOnlyVendorTable.Offset(sellsVTableSlot) == 0 {
		t.Error("a buy-only vendor omitted sells; the contract requires an empty vector")
	}

	closedEnv := vnet.GetRootAsEnvelope(EncodeVendorClosed(VendorClosed{EntityID: 900}), 0)
	if closedEnv.PayloadType() != vnet.PayloadVendorClosed {
		t.Fatalf("payload = %s, want VendorClosed", closedEnv.PayloadType())
	}
	closedTable := payloadTable(t, closedEnv)
	closed := new(vnet.VendorClosed)
	closed.Init(closedTable.Bytes, closedTable.Pos)
	if closed.EntityId() != 900 {
		t.Errorf("closed entity = %d, want 900", closed.EntityId())
	}
}

func TestLootServerMessagesCarryCompleteEntriesAndExplicitClosure(t *testing.T) {
	t.Parallel()

	want := LootState{CorpseID: 400, Revision: 2, Silver: 37, Entries: []LootEntry{
		{EntryID: 9, ItemID: 31, Count: 4},
		{EntryID: 10, ItemID: 8, Count: 1, Durability: 3, MaxDurability: 10},
	}}
	env := vnet.GetRootAsEnvelope(EncodeLootState(want), 0)
	if env.PayloadType() != vnet.PayloadLootState {
		t.Fatalf("payload = %s, want LootState", env.PayloadType())
	}
	table := payloadTable(t, env)
	state := new(vnet.LootState)
	state.Init(table.Bytes, table.Pos)
	if state.CorpseId() != want.CorpseID || state.Revision() != want.Revision || state.EntriesLength() != len(want.Entries) {
		t.Fatalf("loot state header = corpse %d revision %d entries %d", state.CorpseId(), state.Revision(), state.EntriesLength())
	}
	if got := state.Silver(); got != want.Silver {
		t.Errorf("loot silver = %d, want %d", got, want.Silver)
	}
	for index, expected := range want.Entries {
		entry := new(vnet.LootEntry)
		if !state.Entries(entry, index) {
			t.Fatalf("entry %d absent", index)
		}
		got := LootEntry{EntryID: entry.EntryId(), ItemID: entry.ItemId(), Count: entry.Count(), Durability: entry.Durability(), MaxDurability: entry.MaxDurability()}
		if got != expected {
			t.Errorf("entry %d = %+v, want %+v", index, got, expected)
		}
	}

	// Currency-only loot is a live container, not a closure. Its required entries
	// vector is present and empty; EntriesLength alone cannot distinguish that from
	// an omitted vector. `entries` is the third field, so its vtable slot is 4 + 2*2.
	const entriesVTableSlot = flatbuffers.VOffsetT(8)
	silverOnlyEnv := vnet.GetRootAsEnvelope(EncodeLootState(LootState{CorpseID: 401, Revision: 1, Silver: 12}), 0)
	silverOnlyTable := payloadTable(t, silverOnlyEnv)
	silverOnly := new(vnet.LootState)
	silverOnly.Init(silverOnlyTable.Bytes, silverOnlyTable.Pos)
	if silverOnly.EntriesLength() != 0 || silverOnly.Silver() != 12 {
		t.Errorf("silver-only loot carries %d entries and %d silver, want 0 and 12", silverOnly.EntriesLength(), silverOnly.Silver())
	}
	silverOnlyLootTable := silverOnly.Table()
	if silverOnlyLootTable.Offset(entriesVTableSlot) == 0 {
		t.Error("silver-only loot omitted entries; the contract requires an empty vector")
	}

	closedEnv := vnet.GetRootAsEnvelope(EncodeLootClosed(LootClosed{CorpseID: 400}), 0)
	if closedEnv.PayloadType() != vnet.PayloadLootClosed {
		t.Fatalf("payload = %s, want LootClosed", closedEnv.PayloadType())
	}
	closedTable := payloadTable(t, closedEnv)
	closed := new(vnet.LootClosed)
	closed.Init(closedTable.Bytes, closedTable.Pos)
	if closed.CorpseId() != 400 {
		t.Errorf("closed corpse = %d, want 400", closed.CorpseId())
	}
}

func TestMobHitCarriesAuthoritativeAttackerIdentityAndPosition(t *testing.T) {
	t.Parallel()

	want := MobHit{AttackerEntityID: 91, AttackerPos: [3]float32{1.5, 64, -2.5}}
	env := vnet.GetRootAsEnvelope(EncodeMobHit(want), 0)
	if env.PayloadType() != vnet.PayloadMobHit {
		t.Fatalf("payload = %s, want MobHit", env.PayloadType())
	}
	table := payloadTable(t, env)
	hit := new(vnet.MobHit)
	hit.Init(table.Bytes, table.Pos)
	pos := hit.AttackerPos(nil)
	if pos == nil {
		t.Fatal("attacker position is absent")
	}
	got := MobHit{
		AttackerEntityID: hit.AttackerEntityId(),
		AttackerPos:      [3]float32{pos.X(), pos.Y(), pos.Z()},
	}
	if got != want {
		t.Errorf("MobHit = %+v, want %+v", got, want)
	}
}

func TestChatAndPartyRequestsRoundTripVerbatim(t *testing.T) {
	t.Parallel()

	chat := ChatRequest{Text: "  skål, wanderer\n"}
	message, err := Decode(EncodeChatRequest(chat))
	if err != nil {
		t.Fatalf("Decode chat: %v", err)
	}
	if message.Kind != vnet.PayloadChatRequest || message.Chat == nil || *message.Chat != chat {
		t.Fatalf("chat round trip = %+v, want %+v", message, chat)
	}
	if empty, err := Decode(EncodeChatRequest(ChatRequest{})); err != nil || empty.Chat == nil || empty.Chat.Text != "" {
		t.Fatalf("empty chat round trip = %+v, %v", empty, err)
	}

	for _, want := range []PartyRequest{
		{Action: vnet.PartyActionInvite, TargetName: "  Freya  "},
		{Action: vnet.PartyActionAccept},
		{Action: vnet.PartyActionDecline, TargetName: "ignored verbatim"},
		{Action: vnet.PartyActionLeave},
		{Action: vnet.PartyActionKick, TargetName: "Skadi"},
	} {
		got, decodeErr := Decode(EncodePartyRequest(want))
		if decodeErr != nil {
			t.Fatalf("Decode party %s: %v", want.Action, decodeErr)
		}
		if got.Kind != vnet.PayloadPartyRequest || got.Party == nil || *got.Party != want {
			t.Errorf("party round trip = %+v, want %+v", got, want)
		}
	}
}

func TestPartyRequestUnknownActionFailsClosed(t *testing.T) {
	t.Parallel()

	for _, action := range []vnet.PartyAction{vnet.PartyActionUnknown, vnet.PartyAction(99)} {
		if _, err := Decode(EncodePartyRequest(PartyRequest{Action: action})); !errors.Is(err, ErrMalformed) {
			t.Errorf("Decode action %d = %v, want ErrMalformed", action, err)
		}
	}
}

func TestChatMessageAndPartyInviteEncodeEveryField(t *testing.T) {
	t.Parallel()

	chatWant := ChatMessage{SenderEntityID: 41, SenderName: "Eir", Text: "  hold fast  "}
	chatEnvelope := vnet.GetRootAsEnvelope(EncodeChatMessage(chatWant), 0)
	if chatEnvelope.PayloadType() != vnet.PayloadChatMessage {
		t.Fatalf("chat payload = %s", chatEnvelope.PayloadType())
	}
	chatTable := payloadTable(t, chatEnvelope)
	chat := new(vnet.ChatMessage)
	chat.Init(chatTable.Bytes, chatTable.Pos)
	if got := (ChatMessage{SenderEntityID: chat.SenderEntityId(), SenderName: string(chat.SenderName()), Text: string(chat.Text())}); got != chatWant {
		t.Errorf("chat message = %+v, want %+v", got, chatWant)
	}

	inviteWant := PartyInvite{FromEntityID: 72, FromName: "Sif", ExpiresMS: 15_000}
	inviteEnvelope := vnet.GetRootAsEnvelope(EncodePartyInvite(inviteWant), 0)
	if inviteEnvelope.PayloadType() != vnet.PayloadPartyInvite {
		t.Fatalf("invite payload = %s", inviteEnvelope.PayloadType())
	}
	inviteTable := payloadTable(t, inviteEnvelope)
	invite := new(vnet.PartyInvite)
	invite.Init(inviteTable.Bytes, inviteTable.Pos)
	if got := (PartyInvite{FromEntityID: invite.FromEntityId(), FromName: string(invite.FromName()), ExpiresMS: invite.ExpiresMs()}); got != inviteWant {
		t.Errorf("party invite = %+v, want %+v", got, inviteWant)
	}
}

func TestTheLeavingExchangeCarriesOnlyTheServersDecision(t *testing.T) {
	t.Parallel()

	msg, err := Decode(EncodeLeaveRequest())
	if err != nil {
		t.Fatalf("Decode LeaveRequest: %v", err)
	}
	if msg.Kind != vnet.PayloadLeaveRequest || msg.LeaveRequest == nil {
		t.Fatalf("decoded leave = %+v, want an empty LeaveRequest", msg)
	}

	frame := EncodeLeaveStarted(10 * time.Second)
	env := vnet.GetRootAsEnvelope(frame, 0)
	if env.PayloadType() != vnet.PayloadLeaveStarted {
		t.Fatalf("leave acknowledgement is %s, want %s", env.PayloadType(), vnet.PayloadLeaveStarted)
	}
	table := payloadTable(t, env)
	started := new(vnet.LeaveStarted)
	started.Init(table.Bytes, table.Pos)
	if got := started.RemainingMs(); got != 10_000 {
		t.Errorf("RemainingMs = %d, want 10000", got)
	}

	cancel, err := Decode(EncodeLeaveCancelRequest())
	if err != nil {
		t.Fatalf("Decode LeaveCancelRequest: %v", err)
	}
	if cancel.Kind != vnet.PayloadLeaveCancelRequest || cancel.LeaveCancelRequest == nil {
		t.Fatalf("decoded cancellation = %+v, want an empty LeaveCancelRequest", cancel)
	}

	for _, test := range []struct {
		name      string
		accepted  bool
		remaining time.Duration
	}{
		{name: "accepted", accepted: true},
		{name: "refused", remaining: 4 * time.Second},
	} {
		t.Run(test.name, func(t *testing.T) {
			frame := EncodeLeaveCancelResult(test.accepted, test.remaining)
			env := vnet.GetRootAsEnvelope(frame, 0)
			if env.PayloadType() != vnet.PayloadLeaveCancelResult {
				t.Fatalf("cancellation result is %s, want %s", env.PayloadType(), vnet.PayloadLeaveCancelResult)
			}
			table := payloadTable(t, env)
			result := new(vnet.LeaveCancelResult)
			result.Init(table.Bytes, table.Pos)
			if got := result.Accepted(); got != test.accepted {
				t.Errorf("Accepted = %t, want %t", got, test.accepted)
			}
			if got := result.RemainingMs(); got != uint32(test.remaining/time.Millisecond) {
				t.Errorf("RemainingMs = %d, want %d", got, test.remaining/time.Millisecond)
			}
		})
	}
}

func TestServerWelcomeEncodesEveryField(t *testing.T) {
	t.Parallel()

	token := make([]byte, 32)
	for i := range token {
		token[i] = byte(255 - i)
	}
	want := Welcome{
		EntityID:       42,
		Spawn:          [3]float32{1.5, 64, -2.25},
		WorldSeed:      -7,
		TickRate:       20,
		ChunkSize:      32,
		ViewDistance:   3,
		InventorySlots: InventorySlots,
		HotbarSlots:    HotbarSlots,
		EquipmentSlots: EquipmentSlots,
		PlayerToken:    token,
		// A clock this encoder has no opinion about: the numbers are the simulation's,
		// and what is being checked here is that all three survive the wire in the order
		// they were given. The ordering rule between them is the client's to enforce,
		// because there they are untrusted input.
		DayLengthTicks:  24000,
		NightStartTicks: 14400,
		NightEndTicks:   21600,
	}

	frame := EncodeServerWelcome(want)

	// Read back through the generated accessors: the server produced this buffer,
	// so the test is checking the encoder, not guarding against hostile input.
	if !vnet.EnvelopeBufferHasIdentifier(frame) {
		t.Fatal("welcome frame is missing the file identifier")
	}
	env := vnet.GetRootAsEnvelope(frame, 0)
	if env.PayloadType() != vnet.PayloadServerWelcome {
		t.Fatalf("PayloadType = %s, want %s", env.PayloadType(), vnet.PayloadServerWelcome)
	}
	table := new(vnet.ServerWelcome)
	tbl := payloadTable(t, env)
	table.Init(tbl.Bytes, tbl.Pos)

	if got := table.EntityId(); got != want.EntityID {
		t.Errorf("EntityId = %d, want %d", got, want.EntityID)
	}
	spawn := table.Spawn(nil)
	if spawn == nil {
		t.Fatal("Spawn is absent")
	}
	if spawn.X() != want.Spawn[0] || spawn.Y() != want.Spawn[1] || spawn.Z() != want.Spawn[2] {
		t.Errorf("Spawn = (%v, %v, %v), want %v", spawn.X(), spawn.Y(), spawn.Z(), want.Spawn)
	}
	if got := table.WorldSeed(); got != want.WorldSeed {
		t.Errorf("WorldSeed = %d, want %d", got, want.WorldSeed)
	}
	if got := table.TickRate(); got != want.TickRate {
		t.Errorf("TickRate = %d, want %d", got, want.TickRate)
	}
	if got := table.ChunkSize(); got != want.ChunkSize {
		t.Errorf("ChunkSize = %d, want %d", got, want.ChunkSize)
	}
	if got := table.ViewDistance(); got != want.ViewDistance {
		t.Errorf("ViewDistance = %d, want %d", got, want.ViewDistance)
	}
	if got := table.InventorySlots(); got != want.InventorySlots {
		t.Errorf("InventorySlots = %d, want %d", got, want.InventorySlots)
	}
	if got := table.HotbarSlots(); got != want.HotbarSlots {
		t.Errorf("HotbarSlots = %d, want %d", got, want.HotbarSlots)
	}
	if got := table.EquipmentSlots(); got != want.EquipmentSlots {
		t.Errorf("EquipmentSlots = %d, want %d", got, want.EquipmentSlots)
	}
	// Present and exactly 32 bytes on every accepted handshake — a decoder invariant
	// the client enforces the way it enforces a non-zero tick rate.
	if got := table.PlayerTokenBytes(); !bytes.Equal(got, want.PlayerToken) {
		t.Errorf("PlayerToken = %d bytes, want the %d given", len(got), len(want.PlayerToken))
	}
	if got := table.DayLengthTicks(); got != want.DayLengthTicks {
		t.Errorf("DayLengthTicks = %d, want %d", got, want.DayLengthTicks)
	}
	if got := table.NightStartTicks(); got != want.NightStartTicks {
		t.Errorf("NightStartTicks = %d, want %d", got, want.NightStartTicks)
	}
	if got := table.NightEndTicks(); got != want.NightEndTicks {
		t.Errorf("NightEndTicks = %d, want %d", got, want.NightEndTicks)
	}
}

// A server that keeps no clock says so with three zeros, and that is an announcement
// rather than an omission.
//
// It is also the shape every caller in this repository produces today: nothing builds a
// Welcome with a clock yet, so this is what the wire actually carries until the world
// clock lands. The test exists to make that state deliberate — a decoder is entitled to
// read a zero day length as "no time passes here" precisely because an encoder cannot
// reach it by accident and mean something else.
func TestAWelcomeWithNoClockAnnouncesThreeZeros(t *testing.T) {
	t.Parallel()

	frame := EncodeServerWelcome(Welcome{
		EntityID:       1,
		TickRate:       20,
		ChunkSize:      32,
		InventorySlots: InventorySlots,
		HotbarSlots:    HotbarSlots,
		PlayerToken:    make([]byte, 32),
	})

	env := vnet.GetRootAsEnvelope(frame, 0)
	table := new(vnet.ServerWelcome)
	tbl := payloadTable(t, env)
	table.Init(tbl.Bytes, tbl.Pos)

	for name, got := range map[string]uint32{
		"DayLengthTicks":  table.DayLengthTicks(),
		"NightStartTicks": table.NightStartTicks(),
		"NightEndTicks":   table.NightEndTicks(),
	} {
		if got != 0 {
			t.Errorf("%s = %d on a clock-less welcome, want 0", name, got)
		}
	}
}

func TestServerRejectCarriesReasonAndDetail(t *testing.T) {
	t.Parallel()

	frame := EncodeServerReject(vnet.RejectReasonPROTOCOL_MISMATCH, "server speaks 1")

	env := vnet.GetRootAsEnvelope(frame, 0)
	if env.PayloadType() != vnet.PayloadServerReject {
		t.Fatalf("PayloadType = %s, want %s", env.PayloadType(), vnet.PayloadServerReject)
	}
	reject := new(vnet.ServerReject)
	tbl := payloadTable(t, env)
	reject.Init(tbl.Bytes, tbl.Pos)

	if got := reject.Reason(); got != vnet.RejectReasonPROTOCOL_MISMATCH {
		t.Errorf("Reason = %s, want %s", got, vnet.RejectReasonPROTOCOL_MISMATCH)
	}
	if got := string(reject.Detail()); got != "server speaks 1" {
		t.Errorf("Detail = %q, want %q", got, "server speaks 1")
	}
}

// Decode reports the kind of payloads it does not unpack, which is what lets the
// session refuse a client that sends a server-only message.
func TestDecodeReportsKindWithoutUnpacking(t *testing.T) {
	t.Parallel()

	msg, err := Decode(EncodeServerWelcome(Welcome{TickRate: 20, ChunkSize: 32}))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Kind != vnet.PayloadServerWelcome {
		t.Errorf("Kind = %s, want %s", msg.Kind, vnet.PayloadServerWelcome)
	}
	if msg.ClientHello != nil {
		t.Error("ClientHello was populated from a ServerWelcome")
	}
}

func TestDecodeRejectsUndecodableInput(t *testing.T) {
	t.Parallel()

	valid := EncodeClientHello(vnet.ProtocolVersionCurrent, "Eivor")
	wrongIdentifier := bytes.Clone(valid)
	copy(wrongIdentifier[4:8], "NOPE")

	cases := map[string][]byte{
		"empty":            {},
		"too short":        {0x04, 0x00, 0x00},
		"wrong identifier": wrongIdentifier,
		"garbage":          bytes.Repeat([]byte{0xFF}, 64),
	}

	for name, frame := range cases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			if _, err := Decode(frame); !errors.Is(err, ErrMalformed) {
				t.Fatalf("err = %v, want ErrMalformed", err)
			}
		})
	}
}

// The point of this test is the absence of a panic. The Go FlatBuffers runtime has
// no verifier, so a truncated or corrupted buffer surfaces as an out-of-range
// index deep inside generated code; Decode's recover is what turns that into an
// error, and the only way to know it holds is to feed it damage.
func TestDecodeNeverPanicsOnDamagedInput(t *testing.T) {
	t.Parallel()

	valid := EncodeClientHello(vnet.ProtocolVersionCurrent, "Eivor")

	t.Run("every truncation", func(t *testing.T) {
		t.Parallel()
		for i := range len(valid) {
			msg, err := Decode(valid[:i])
			if err == nil && msg.ClientHello != nil && msg.ClientHello.PlayerName != "Eivor" {
				t.Errorf("truncation at %d decoded to a corrupted player name %q", i, msg.ClientHello.PlayerName)
			}
		}
	})

	t.Run("every single-byte corruption", func(t *testing.T) {
		t.Parallel()
		for i := range len(valid) {
			damaged := bytes.Clone(valid)
			damaged[i] ^= 0xFF
			_, _ = Decode(damaged)
		}
	})

	t.Run("random noise", func(t *testing.T) {
		t.Parallel()
		// Deterministic seed: a fixed sequence keeps a failure reproducible, and
		// this test is about robustness rather than coverage of the input space.
		rng := rand.New(rand.NewPCG(1, 2))
		buf := make([]byte, len(valid))
		for range 512 {
			for i := range buf {
				buf[i] = byte(rng.UintN(256))
			}
			copy(buf[4:8], vnet.EnvelopeIdentifier) // get past the identifier check
			_, _ = Decode(buf)
		}
	})
}

// PlayerInput is the one payload a *client* chooses the bytes of, so this is the round
// trip that matters most for input: every field has to survive, including the ones the
// contract forbids. Judging them is the simulation's job, and it cannot judge what the
// decoder quietly repaired.
func TestPlayerInputRoundTripsEveryField(t *testing.T) {
	t.Parallel()

	want := PlayerInput{
		ClientTick: 987654,
		MoveX:      -0.25,
		MoveZ:      0.75,
		Yaw:        1.5,
		Pitch:      -0.5,
		Jump:       true,
	}

	msg, err := Decode(EncodePlayerInput(want))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Kind != vnet.PayloadPlayerInput {
		t.Fatalf("Kind = %s, want %s", msg.Kind, vnet.PayloadPlayerInput)
	}
	if msg.PlayerInput == nil {
		t.Fatal("PlayerInput payload is nil")
	}
	if *msg.PlayerInput != want {
		t.Errorf("decoded %+v, want %+v", *msg.PlayerInput, want)
	}
}

// A PlayerInput with no fields set decodes to the schema's defaults rather than to an
// error: FlatBuffers omits a field equal to its default, so an honest client that is
// standing still sends exactly this.
func TestAnEmptyPlayerInputDecodesToStandingStill(t *testing.T) {
	t.Parallel()

	b := flatbuffers.NewBuilder(64)
	vnet.PlayerInputStart(b)
	input := vnet.PlayerInputEnd(b)
	frame := finishEnvelope(b, vnet.PayloadPlayerInput, input)

	msg, err := Decode(frame)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.PlayerInput == nil {
		t.Fatal("PlayerInput payload is nil")
	}
	if *msg.PlayerInput != (PlayerInput{}) {
		t.Errorf("decoded %+v, want every field at its default", *msg.PlayerInput)
	}
}

// Values the contract forbids reach the caller untouched. This is the decode half of
// the finite-float invariant: protocol carries, game refuses. A decoder that clamped
// here would do nothing to a NaN anyway, and a decoder that errored would end a
// connection whose framing is still perfectly readable.
func TestPlayerInputCarriesNonFiniteValuesRatherThanRepairingThem(t *testing.T) {
	t.Parallel()

	frame := EncodePlayerInput(PlayerInput{
		MoveX: float32(math.NaN()),
		MoveZ: float32(math.Inf(1)),
		Yaw:   float32(math.Inf(-1)),
		Pitch: 1e30,
	})

	msg, err := Decode(frame)
	if err != nil {
		t.Fatalf("Decode refused a well-framed message over its values: %v", err)
	}
	if !math.IsNaN(float64(msg.PlayerInput.MoveX)) {
		t.Errorf("MoveX = %v, want the NaN the client sent", msg.PlayerInput.MoveX)
	}
	if !math.IsInf(float64(msg.PlayerInput.MoveZ), 1) {
		t.Errorf("MoveZ = %v, want +Inf", msg.PlayerInput.MoveZ)
	}
	if !math.IsInf(float64(msg.PlayerInput.Yaw), -1) {
		t.Errorf("Yaw = %v, want -Inf", msg.PlayerInput.Yaw)
	}
}

// A union tag that names a payload the envelope does not carry is malformed, not
// empty: the tag is what every consumer branches on.
func TestAPlayerInputTagWithNoPayloadIsMalformed(t *testing.T) {
	t.Parallel()

	b := flatbuffers.NewBuilder(64)
	vnet.EnvelopeStart(b)
	vnet.EnvelopeAddPayloadType(b, vnet.PayloadPlayerInput)
	env := vnet.EnvelopeEnd(b)
	vnet.FinishEnvelopeBuffer(b, env)

	if _, err := Decode(b.FinishedBytes()); !errors.Is(err, ErrMalformed) {
		t.Errorf("Decode returned %v, want an error wrapping ErrMalformed", err)
	}
}

// The truncation and corruption sweep, for the second payload a client can choose the
// bytes of. Decode must stay total over it: an error or a message, never a panic.
func TestDecodeIsTotalOverADamagedPlayerInput(t *testing.T) {
	t.Parallel()

	valid := EncodePlayerInput(PlayerInput{ClientTick: 7, MoveZ: 1, Yaw: 0.5, Jump: true})

	for i := range len(valid) {
		if _, err := Decode(valid[:i]); err == nil {
			t.Errorf("a %d-byte prefix of a %d-byte input frame decoded successfully", i, len(valid))
		}
	}
	for i := range len(valid) {
		damaged := bytes.Clone(valid)
		damaged[i] ^= 0xFF
		_, _ = Decode(damaged)
	}
}

func TestEntitySnapshotCarriesEveryEntityInOrder(t *testing.T) {
	t.Parallel()

	want := []EntityState{
		{EntityID: 1, Pos: [3]float32{0.5, 64, -0.5}, Vel: [3]float32{1, 0, -2}, Yaw: 0.25},
		{EntityID: 9, Pos: [3]float32{100.5, 44.25, 7}, Vel: [3]float32{}, Yaw: -3},
		{EntityID: 4096, Pos: [3]float32{-1, -2, -3}, Vel: [3]float32{0, -60, 0}, Yaw: 3.14},
	}

	wantDrops := []ItemDropState{
		{EntityID: 50, Pos: [3]float32{2.5, 65, -9.5}, ItemID: 3, Count: 1},
		{EntityID: 51, Pos: [3]float32{-4, 12.25, 8}, ItemID: 7, Count: 1, Durability: 12, MaxDurability: 200},
	}

	wantMobs := []MobState{
		{
			EntityID: 900, Kind: vnet.MobKindDraugr,
			Pos: [3]float32{8.5, 64, -12.25}, Vel: [3]float32{0.5, 0, -0.5},
			Yaw: 1.5, Health: 60, MaxHealth: 60, Action: vnet.MobActionChase, TargetEntityID: 41,
		},
		{
			EntityID: 901, Kind: vnet.MobKindDraugr,
			Pos: [3]float32{-30, 44, 3}, Vel: [3]float32{},
			Yaw: -3, Health: 1, MaxHealth: 60, Action: vnet.MobActionWindup,
		},
	}
	wantVitals := PlayerVitals{
		Health: 35, MaxHealth: 100, LifeState: vnet.LifeStateAlive,
		RespawnTicks: 0, Invulnerable: true, Hunger: 47, MaxHunger: 100,
		Level: 4, Experience: 23, ExperienceToNext: 200,
	}

	frame := EncodeEntitySnapshot(EntitySnapshot{
		Tick:     1234,
		Entities: want,
		Drops:    wantDrops,
		Mobs:     wantMobs,
		Vitals:   wantVitals,
	})

	if !vnet.EnvelopeBufferHasIdentifier(frame) {
		t.Fatal("snapshot frame is missing the file identifier")
	}
	env := vnet.GetRootAsEnvelope(frame, 0)
	if env.PayloadType() != vnet.PayloadEntitySnapshot {
		t.Fatalf("PayloadType = %s, want %s", env.PayloadType(), vnet.PayloadEntitySnapshot)
	}

	snapshot := new(vnet.EntitySnapshot)
	tbl := payloadTable(t, env)
	snapshot.Init(tbl.Bytes, tbl.Pos)

	if got := snapshot.ServerTick(); got != 1234 {
		t.Errorf("ServerTick = %d, want 1234", got)
	}
	if got := snapshot.EntitiesLength(); got != len(want) {
		t.Fatalf("the snapshot holds %d entities, want %d", got, len(want))
	}

	// Order is asserted, not just membership: a struct vector is built back to front,
	// so an encoder that forgot to reverse would still produce a valid buffer with
	// every entity in it and the list mirrored.
	for i, expected := range want {
		var entity vnet.EntityState
		if !snapshot.Entities(&entity, i) {
			t.Fatalf("entity %d is missing", i)
		}
		pos, vel := new(vnet.Vec3), new(vnet.Vec3)
		entity.Pos(pos)
		entity.Vel(vel)

		got := EntityState{
			EntityID: entity.EntityId(),
			Pos:      [3]float32{pos.X(), pos.Y(), pos.Z()},
			Vel:      [3]float32{vel.X(), vel.Y(), vel.Z()},
			Yaw:      entity.Yaw(),
		}
		if got != expected {
			t.Errorf("entity %d decoded as %+v, want %+v", i, got, expected)
		}
	}

	if got := snapshot.DropsLength(); got != len(wantDrops) {
		t.Fatalf("the snapshot holds %d drops, want %d", got, len(wantDrops))
	}
	gotDrops := make([]ItemDropState, len(wantDrops))
	for i := range wantDrops {
		var drop vnet.ItemDropState
		if !snapshot.Drops(&drop, i) {
			t.Fatalf("drop %d is missing", i)
		}
		pos := drop.Pos(nil)
		if pos == nil {
			t.Fatalf("drop %d has no position", i)
		}
		gotDrops[i] = ItemDropState{
			EntityID: drop.EntityId(),
			Pos:      [3]float32{pos.X(), pos.Y(), pos.Z()},
			ItemID:   drop.ItemId(),
			Count:    drop.Count(),
		}
	}
	if got := snapshot.DropDurabilitiesLength(); got != 1 {
		t.Fatalf("the snapshot holds %d durability entries, want 1", got)
	}
	for i := range snapshot.DropDurabilitiesLength() {
		var wear vnet.ItemDropDurability
		if !snapshot.DropDurabilities(&wear, i) {
			t.Fatalf("drop durability %d is missing", i)
		}
		matched := false
		for j := range gotDrops {
			if gotDrops[j].EntityID != wear.EntityId() {
				continue
			}
			gotDrops[j].Durability = wear.Durability()
			gotDrops[j].MaxDurability = wear.MaxDurability()
			matched = true
			break
		}
		if !matched {
			t.Errorf("durability entry %d names unknown drop %d", i, wear.EntityId())
		}
	}
	for i, expected := range wantDrops {
		if gotDrops[i] != expected {
			t.Errorf("drop %d decoded as %+v, want %+v", i, gotDrops[i], expected)
		}
	}

	// Mobs are a vector of *tables*, so order is carried by a vector of offsets rather
	// than by inlined bytes. Asserting it is the same requirement for the same reason:
	// a vector built without reversing is still a valid buffer with the list mirrored.
	if got := snapshot.MobsLength(); got != len(wantMobs) {
		t.Fatalf("the snapshot holds %d mobs, want %d", got, len(wantMobs))
	}
	for i, expected := range wantMobs {
		var mob vnet.MobState
		if !snapshot.Mobs(&mob, i) {
			t.Fatalf("mob %d is missing", i)
		}
		pos, vel := mob.Pos(nil), mob.Vel(nil)
		if pos == nil || vel == nil {
			t.Fatalf("mob %d has no position or no velocity", i)
		}
		got := MobState{
			EntityID:       mob.EntityId(),
			Kind:           mob.Kind(),
			Pos:            [3]float32{pos.X(), pos.Y(), pos.Z()},
			Vel:            [3]float32{vel.X(), vel.Y(), vel.Z()},
			Yaw:            mob.Yaw(),
			Health:         mob.Health(),
			MaxHealth:      mob.MaxHealth(),
			Action:         mob.Action(),
			TargetEntityID: mob.TargetEntityId(),
		}
		if got != expected {
			t.Errorf("mob %d decoded as %+v, want %+v", i, got, expected)
		}
	}

	vitals := snapshot.SelfVitals(nil)
	if vitals == nil {
		t.Fatal("the snapshot carries no self_vitals")
	}
	gotVitals := PlayerVitals{
		Health:           vitals.Health(),
		MaxHealth:        vitals.MaxHealth(),
		LifeState:        vitals.LifeState(),
		RespawnTicks:     vitals.RespawnTicks(),
		Invulnerable:     vitals.Invulnerable(),
		Hunger:           vitals.Hunger(),
		MaxHunger:        vitals.MaxHunger(),
		Level:            vitals.Level(),
		Experience:       vitals.Experience(),
		ExperienceToNext: vitals.ExperienceToNext(),
	}
	if gotVitals != wantVitals {
		t.Errorf("self_vitals decoded as %+v, want %+v", gotVitals, wantVitals)
	}
}

func TestV27MountProjectionCastAndLearnedSetKeepAuthoritativeOrder(t *testing.T) {
	t.Parallel()

	frame := EncodeEntitySnapshot(EntitySnapshot{
		Tick:     8,
		Entities: []EntityState{{EntityID: 7}, {EntityID: 9}},
		Mounts: []MountState{
			{EntityID: 9, Mount: vnet.MountKindGreyHorse},
			{EntityID: 7, Mount: vnet.MountKindBlackHorse},
		},
		Vitals:  PlayerVitals{Health: 100, MaxHealth: 100, Hunger: 100, MaxHunger: 100, Level: 1, ExperienceToNext: 50, LifeState: vnet.LifeStateAlive},
		HasCast: true,
		Cast:    CastState{Kind: vnet.CastKindMount, Progress: 128},
	})
	env := vnet.GetRootAsEnvelope(frame, 0)
	table := payloadTable(t, env)
	snapshot := new(vnet.EntitySnapshot)
	snapshot.Init(table.Bytes, table.Pos)
	if got := snapshot.MountsLength(); got != 2 {
		t.Fatalf("MountsLength = %d, want 2", got)
	}
	for index, want := range []MountState{
		{EntityID: 9, Mount: vnet.MountKindGreyHorse},
		{EntityID: 7, Mount: vnet.MountKindBlackHorse},
	} {
		state := new(vnet.MountState)
		if !snapshot.Mounts(state, index) {
			t.Fatalf("mount %d is absent", index)
		}
		if got := (MountState{EntityID: state.EntityId(), Mount: state.Mount()}); got != want {
			t.Errorf("mount %d = %+v, want %+v", index, got, want)
		}
	}
	cast := snapshot.SelfCast(nil)
	if cast == nil || cast.Kind() != vnet.CastKindMount || cast.Progress() != 128 {
		t.Fatalf("self cast = %#v, want Mount at 128", cast)
	}

	learnedEnv := vnet.GetRootAsEnvelope(EncodeLearnedMounts(LearnedMounts{Mounts: []vnet.MountKind{
		vnet.MountKindBrownHorse, vnet.MountKindGreyHorse,
	}}), 0)
	learnedTable := payloadTable(t, learnedEnv)
	learned := new(vnet.LearnedMounts)
	learned.Init(learnedTable.Bytes, learnedTable.Pos)
	if learned.MountsLength() != 2 || learned.Mounts(0) != vnet.MountKindBrownHorse || learned.Mounts(1) != vnet.MountKindGreyHorse {
		t.Errorf("learned mount order = [%s, %s], want [BrownHorse, GreyHorse]", learned.Mounts(0), learned.Mounts(1))
	}
}

func TestV27MountAndDismountRequestsDecodeAsIntent(t *testing.T) {
	t.Parallel()

	for name, frame := range map[string][]byte{
		"mount":    EncodeMountRequest(MountRequest{Mount: vnet.MountKindBrownHorse}),
		"dismount": EncodeDismountRequest(),
	} {
		message, err := Decode(frame)
		if err != nil {
			t.Fatalf("%s Decode: %v", name, err)
		}
		switch name {
		case "mount":
			if message.Kind != vnet.PayloadMountRequest || message.MountRequest == nil || message.MountRequest.Mount != vnet.MountKindBrownHorse {
				t.Errorf("mount decoded as %+v", message)
			}
		case "dismount":
			if message.Kind != vnet.PayloadDismountRequest || message.DismountRequest == nil {
				t.Errorf("dismount decoded as %+v", message)
			}
		}
	}

	message, err := Decode(EncodeMountRequest(MountRequest{Mount: vnet.MountKindUnknown}))
	if err != nil || message.MountRequest == nil || message.MountRequest.Mount != vnet.MountKindUnknown {
		t.Errorf("Unknown mount intent must be copied for simulation refusal, got %+v / %v", message, err)
	}
}

func TestV27SnapshotValidatorRefusesBrokenMountAssociations(t *testing.T) {
	t.Parallel()

	vitals := PlayerVitals{Health: 100, MaxHealth: 100, Hunger: 100, MaxHunger: 100, Level: 1, ExperienceToNext: 50, LifeState: vnet.LifeStateAlive}
	valid := EntitySnapshot{
		Entities: []EntityState{{EntityID: 7}},
		Mounts:   []MountState{{EntityID: 7, Mount: vnet.MountKindBlackHorse}},
		Vitals:   vitals,
	}
	if err := ValidateEntitySnapshot(EncodeEntitySnapshot(valid)); err != nil {
		t.Fatalf("valid snapshot: %v", err)
	}

	for name, snapshot := range map[string]EntitySnapshot{
		"missing player": {
			Entities: []EntityState{{EntityID: 7}},
			Mounts:   []MountState{{EntityID: 9, Mount: vnet.MountKindBlackHorse}},
			Vitals:   vitals,
		},
		"zero player id": {
			Entities: []EntityState{{EntityID: 0}},
			Vitals:   vitals,
		},
		"duplicate player": {
			Entities: []EntityState{{EntityID: 7}, {EntityID: 7}},
			Vitals:   vitals,
		},
		"zero mount id": {
			Entities: []EntityState{{EntityID: 7}},
			Mounts:   []MountState{{EntityID: 0, Mount: vnet.MountKindBlackHorse}},
			Vitals:   vitals,
		},
		"duplicate mount": {
			Entities: []EntityState{{EntityID: 7}},
			Mounts: []MountState{
				{EntityID: 7, Mount: vnet.MountKindBlackHorse},
				{EntityID: 7, Mount: vnet.MountKindBrownHorse},
			},
			Vitals: vitals,
		},
		"unknown mount": {
			Entities: []EntityState{{EntityID: 7}},
			Mounts:   []MountState{{EntityID: 7, Mount: vnet.MountKindUnknown}},
			Vitals:   vitals,
		},
		"unknown cast": {
			Entities: []EntityState{{EntityID: 7}},
			Vitals:   vitals,
			HasCast:  true,
			Cast:     CastState{Kind: vnet.CastKindUnknown},
		},
		"completed cast": {
			Entities: []EntityState{{EntityID: 7}},
			Vitals:   vitals,
			HasCast:  true,
			Cast:     CastState{Kind: vnet.CastKindMount, Progress: ^uint8(0)},
		},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			if err := ValidateEntitySnapshot(EncodeEntitySnapshot(snapshot)); !errors.Is(err, ErrMalformed) {
				t.Errorf("ValidateEntitySnapshot = %v, want ErrMalformed", err)
			}
		})
	}
}

// A session that can see nobody is possible in principle, and an empty snapshot has to
// be a snapshot rather than a special case: the tick is still information.
func TestAnEmptyEntitySnapshotIsStillASnapshot(t *testing.T) {
	t.Parallel()

	frame := EncodeEntitySnapshot(EntitySnapshot{
		Tick: 5,
		Vitals: PlayerVitals{
			Health: 100, MaxHealth: 100, LifeState: vnet.LifeStateAlive,
		},
	})

	env := vnet.GetRootAsEnvelope(frame, 0)
	if env.PayloadType() != vnet.PayloadEntitySnapshot {
		t.Fatalf("PayloadType = %s, want %s", env.PayloadType(), vnet.PayloadEntitySnapshot)
	}
	snapshot := new(vnet.EntitySnapshot)
	tbl := payloadTable(t, env)
	snapshot.Init(tbl.Bytes, tbl.Pos)

	if got := snapshot.ServerTick(); got != 5 {
		t.Errorf("ServerTick = %d, want 5", got)
	}
	if got := snapshot.EntitiesLength(); got != 0 {
		t.Errorf("an empty snapshot holds %d entities", got)
	}
	if got := snapshot.DropsLength(); got != 0 {
		t.Errorf("an empty snapshot holds %d drops", got)
	}
}

func TestEntitySnapshotCarriesPartyInAuthoritativeOrder(t *testing.T) {
	t.Parallel()

	// Recipient 91 is the leader. Its own id is deliberately absent from members:
	// EncodeEntitySnapshot does not receive that id separately, so this producer fixture
	// pins the recipient-aware invariant the frame-only Rust decoder cannot prove.
	want := []PartyMemberState{
		{EntityID: 17, Pos: [3]float32{1.5, 62, -8}, Health: 44, MaxHealth: 50, Alive: true},
		{EntityID: 23, Pos: [3]float32{-2, 70.25, 11}, Health: 0, MaxHealth: 80, Alive: false},
	}
	env := vnet.GetRootAsEnvelope(EncodeEntitySnapshot(EntitySnapshot{
		Tick:                8,
		Vitals:              PlayerVitals{Health: 100, MaxHealth: 100, LifeState: vnet.LifeStateAlive},
		PartyLeaderEntityID: 91,
		PartyMembers:        want,
	}), 0)
	table := payloadTable(t, env)
	snapshot := new(vnet.EntitySnapshot)
	snapshot.Init(table.Bytes, table.Pos)

	if got := snapshot.PartyLeaderEntityId(); got != 91 {
		t.Fatalf("party leader = %d, want recipient 91", got)
	}
	if got := snapshot.PartyMembersLength(); got != len(want) {
		t.Fatalf("party members = %d, want %d", got, len(want))
	}
	for index, expected := range want {
		var member vnet.PartyMemberState
		if !snapshot.PartyMembers(&member, index) {
			t.Fatalf("party member %d is absent", index)
		}
		pos := member.Pos(nil)
		got := PartyMemberState{
			EntityID: member.EntityId(), Pos: [3]float32{pos.X(), pos.Y(), pos.Z()},
			Health: member.Health(), MaxHealth: member.MaxHealth(), Alive: member.Alive(),
		}
		if got != expected {
			t.Errorf("party member %d = %+v, want %+v", index, got, expected)
		}
		if got.EntityID == 91 {
			t.Errorf("party member %d repeats recipient id 91", index)
		}
	}
}

func TestEntitySnapshotCarriesStableRosterAndAccessibleCorpses(t *testing.T) {
	t.Parallel()

	roster := []PartyRosterMember{
		{CharacterID: 101, Name: "Offline leader"},
		{CharacterID: 202, EntityID: 72, Name: "Online member", Online: true},
	}
	accessible := []uint64{800, 801}
	env := vnet.GetRootAsEnvelope(EncodeEntitySnapshot(EntitySnapshot{
		Tick:                  9,
		Vitals:                PlayerVitals{Health: 10, MaxHealth: 10, LifeState: vnet.LifeStateAlive},
		PartyRoster:           roster,
		AccessibleLootCorpses: accessible,
	}), 0)
	table := payloadTable(t, env)
	snapshot := new(vnet.EntitySnapshot)
	snapshot.Init(table.Bytes, table.Pos)
	if snapshot.PartyLeaderEntityId() != 0 {
		t.Errorf("offline leader entity id = %d, want 0", snapshot.PartyLeaderEntityId())
	}
	if snapshot.PartyRosterLength() != len(roster) || snapshot.AccessibleLootCorpsesLength() != len(accessible) {
		t.Fatalf("roster/corpse lengths = %d/%d", snapshot.PartyRosterLength(), snapshot.AccessibleLootCorpsesLength())
	}
	for index, expected := range roster {
		member := new(vnet.PartyRosterMember)
		if !snapshot.PartyRoster(member, index) {
			t.Fatalf("roster member %d absent", index)
		}
		got := PartyRosterMember{CharacterID: member.CharacterId(), EntityID: member.EntityId(), Name: string(member.Name()), Online: member.Online()}
		if got != expected {
			t.Errorf("roster member %d = %+v, want %+v", index, got, expected)
		}
	}
	for index, expected := range accessible {
		if got := snapshot.AccessibleLootCorpses(index); got != expected {
			t.Errorf("accessible corpse %d = %d, want %d", index, got, expected)
		}
	}
}

func TestEntitySnapshotOmitsThePartyVectorWhenEmpty(t *testing.T) {
	t.Parallel()

	env := vnet.GetRootAsEnvelope(EncodeEntitySnapshot(EntitySnapshot{
		Tick:                3,
		Vitals:              PlayerVitals{Health: 10, MaxHealth: 10, LifeState: vnet.LifeStateAlive},
		PartyLeaderEntityID: 91,
	}), 0)
	table := payloadTable(t, env)
	snapshot := new(vnet.EntitySnapshot)
	snapshot.Init(table.Bytes, table.Pos)
	if got := snapshot.PartyLeaderEntityId(); got != 91 {
		t.Errorf("party leader = %d, want 91", got)
	}
	if got := snapshot.PartyMembersLength(); got != 0 {
		t.Errorf("empty party member vector has length %d", got)
	}
}

// ---------------------------------------------------------------------------
// Editing the world
// ---------------------------------------------------------------------------

func TestBlockEditRequestRoundTripsEveryField(t *testing.T) {
	t.Parallel()

	want := BlockEditRequest{
		Pos:        [3]int32{-7, 64, 129},
		HasPos:     true,
		Action:     vnet.EditActionPlace,
		Slot:       3,
		ClientTick: 918,
	}

	msg, err := Decode(EncodeBlockEditRequest(want))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Kind != vnet.PayloadBlockEditRequest {
		t.Fatalf("Kind = %s, want %s", msg.Kind, vnet.PayloadBlockEditRequest)
	}
	if msg.BlockEditRequest == nil {
		t.Fatal("BlockEditRequest payload is nil")
	}
	if got := *msg.BlockEditRequest; got != want {
		t.Errorf("decoded %+v, want %+v", got, want)
	}
}

// A struct field is absent or complete, never partial — so an edit request with no
// position must decode as *no* position. Reading it as the zero value would put the
// request at the world origin, which is a real place somebody would then have edited
// without naming it. The decoder copies the presence out; refusing it is the
// simulation's job.
func TestABlockEditRequestWithoutAPositionDecodesAsAbsent(t *testing.T) {
	t.Parallel()

	msg, err := Decode(EncodeBlockEditRequest(BlockEditRequest{Action: vnet.EditActionBreak}))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.BlockEditRequest == nil {
		t.Fatal("BlockEditRequest payload is nil")
	}
	if msg.BlockEditRequest.HasPos {
		t.Error("HasPos is true for a request that carried no position")
	}
	if got := msg.BlockEditRequest.Pos; got != [3]int32{} {
		t.Errorf("Pos = %v for an absent position; it must not be invented", got)
	}
}

// An absent action decodes as Unknown rather than as one of the two real ones. That is
// what EditAction.Unknown = 0 exists for, and it is the difference between an action
// check that fails closed and one that guesses.
func TestABlockEditRequestWithoutAnActionDecodesAsUnknown(t *testing.T) {
	t.Parallel()

	msg, err := Decode(EncodeBlockEditRequest(BlockEditRequest{Pos: [3]int32{1, 2, 3}, HasPos: true}))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.BlockEditRequest == nil {
		t.Fatal("BlockEditRequest payload is nil")
	}
	if got := msg.BlockEditRequest.Action; got != vnet.EditActionUnknown {
		t.Errorf("Action = %s, want %s", got, vnet.EditActionUnknown)
	}
}

// Values the contract forbids are carried through rather than repaired, exactly as a
// non-finite PlayerInput axis is: this package owns the envelope, and what an illegal
// value *means* is a decision for the simulation.
func TestBlockEditRequestCarriesActionsOutsideTheEnum(t *testing.T) {
	t.Parallel()

	msg, err := Decode(EncodeBlockEditRequest(BlockEditRequest{
		Pos: [3]int32{1, 2, 3}, HasPos: true, Action: vnet.EditAction(200), Slot: 35,
	}))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if got := msg.BlockEditRequest.Action; got != vnet.EditAction(200) {
		t.Errorf("Action = %s, want the 200 the frame carried", got)
	}
	if got := msg.BlockEditRequest.Slot; got != 35 {
		t.Errorf("Slot = %d, want the 35 the frame carried", got)
	}
}

func TestABlockEditRequestTagWithNoPayloadIsMalformed(t *testing.T) {
	t.Parallel()

	b := flatbuffers.NewBuilder(64)
	vnet.EnvelopeStart(b)
	vnet.EnvelopeAddPayloadType(b, vnet.PayloadBlockEditRequest)
	env := vnet.EnvelopeEnd(b)
	vnet.FinishEnvelopeBuffer(b, env)

	if _, err := Decode(b.FinishedBytes()); !errors.Is(err, ErrMalformed) {
		t.Errorf("Decode returned %v, want an error wrapping ErrMalformed", err)
	}
}

// The truncation and corruption sweep, for the third payload a client chooses the bytes
// of. Decode must stay total over it: an error or a message, never a panic.
func TestDecodeIsTotalOverADamagedBlockEditRequest(t *testing.T) {
	t.Parallel()

	valid := EncodeBlockEditRequest(BlockEditRequest{
		Pos: [3]int32{-7, 64, 129}, HasPos: true, Action: vnet.EditActionPlace, Slot: 3, ClientTick: 918,
	})

	for i := range len(valid) {
		if _, err := Decode(valid[:i]); err == nil {
			t.Errorf("a %d-byte prefix of a %d-byte edit request decoded successfully", i, len(valid))
		}
	}
	for i := range len(valid) {
		damaged := bytes.Clone(valid)
		damaged[i] ^= 0xFF
		_, _ = Decode(damaged)
	}
}

func TestBlockUpdateEncodesThePositionAndTheBlock(t *testing.T) {
	t.Parallel()

	want := BlockUpdate{Pos: [3]int32{-1, 63, 4096}, BlockID: 2}

	env := vnet.GetRootAsEnvelope(EncodeBlockUpdate(want), 0)
	if env.PayloadType() != vnet.PayloadBlockUpdate {
		t.Fatalf("payload is %s, want %s", env.PayloadType(), vnet.PayloadBlockUpdate)
	}

	tbl := payloadTable(t, env)
	update := new(vnet.BlockUpdate)
	update.Init(tbl.Bytes, tbl.Pos)

	pos := update.Pos(nil)
	if pos == nil {
		t.Fatal("the encoded update carries no position")
	}
	if got := [3]int32{pos.X(), pos.Y(), pos.Z()}; got != want.Pos {
		t.Errorf("Pos = %v, want %v", got, want.Pos)
	}
	if got := update.BlockId(); got != want.BlockID {
		t.Errorf("BlockId = %d, want %d", got, want.BlockID)
	}
}

// A break is a placement of Air, so block id 0 has to survive the encoder. FlatBuffers
// omits a scalar equal to its default and reports the default on read, which is exactly
// what makes this the case worth pinning rather than the case that obviously works.
func TestABreakEncodesAsAPlacementOfAir(t *testing.T) {
	t.Parallel()

	env := vnet.GetRootAsEnvelope(EncodeBlockUpdate(BlockUpdate{Pos: [3]int32{5, 6, 7}}), 0)
	tbl := payloadTable(t, env)
	update := new(vnet.BlockUpdate)
	update.Init(tbl.Bytes, tbl.Pos)

	if got := update.BlockId(); got != 0 {
		t.Errorf("BlockId = %d, want 0 for a break", got)
	}
	pos := update.Pos(nil)
	if pos == nil {
		t.Fatal("a break carries no position")
	}
	if got := [3]int32{pos.X(), pos.Y(), pos.Z()}; got != [3]int32{5, 6, 7} {
		t.Errorf("Pos = %v, want {5 6 7}", got)
	}
}

// ---------------------------------------------------------------------------
// InventoryState
// ---------------------------------------------------------------------------

func TestInventoryStateEncodesEveryStackAsOnePair(t *testing.T) {
	t.Parallel()

	stacks := make([]InventoryStack, int(InventorySlots))
	stacks[0] = InventoryStack{ItemID: 1, Count: 7}
	stacks[5] = InventoryStack{ItemID: 4, Count: 65_535}
	want := InventoryState{Stacks: stacks}

	env := vnet.GetRootAsEnvelope(EncodeInventoryState(want), 0)
	if env.PayloadType() != vnet.PayloadInventoryState {
		t.Fatalf("payload is %s, want %s", env.PayloadType(), vnet.PayloadInventoryState)
	}

	tbl := payloadTable(t, env)
	state := new(vnet.InventoryState)
	state.Init(tbl.Bytes, tbl.Pos)

	wantPairs := make([]uint16, int(InventorySlots)*2)
	wantPairs[0], wantPairs[1] = 1, 7
	wantPairs[10], wantPairs[11] = 4, 65_535
	if got := state.StacksLength(); got != len(wantPairs) {
		t.Fatalf("StacksLength = %d, want %d", got, len(wantPairs))
	}
	for index, want := range wantPairs {
		if got := state.Stacks(index); got != want {
			t.Errorf("Stacks(%d) = %d, want %d", index, got, want)
		}
	}
}

func TestAnEmptyInventoryIsStillAnInventoryState(t *testing.T) {
	t.Parallel()

	env := vnet.GetRootAsEnvelope(EncodeInventoryState(InventoryState{}), 0)
	if env.PayloadType() != vnet.PayloadInventoryState {
		t.Fatalf("payload is %s, want %s", env.PayloadType(), vnet.PayloadInventoryState)
	}

	tbl := payloadTable(t, env)
	state := new(vnet.InventoryState)
	state.Init(tbl.Bytes, tbl.Pos)
	if got := state.StacksLength(); got != int(InventorySlots)*2 {
		t.Errorf("StacksLength = %d, want %d scalars for an empty inventory", got, int(InventorySlots)*2)
	}
	for index := range state.StacksLength() {
		if got := state.Stacks(index); got != 0 {
			t.Errorf("Stacks(%d) = %d, want 0 for an empty slot", index, got)
		}
	}
}

// ---------------------------------------------------------------------------
// Iteration 3 intent and progress
// ---------------------------------------------------------------------------

func TestMineRequestRoundTripsEveryField(t *testing.T) {
	t.Parallel()

	want := MineRequest{
		Pos:        [3]int32{-9, 70, 14},
		HasPos:     true,
		Active:     true,
		ClientTick: 44,
	}
	msg, err := Decode(EncodeMineRequest(want))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Kind != vnet.PayloadMineRequest {
		t.Fatalf("Kind = %s, want %s", msg.Kind, vnet.PayloadMineRequest)
	}
	if msg.MineRequest == nil {
		t.Fatal("MineRequest payload is nil")
	}
	if got := *msg.MineRequest; got != want {
		t.Errorf("round trip produced %+v, want %+v", got, want)
	}
}

func TestMineRequestWithoutAPositionIsMalformed(t *testing.T) {
	t.Parallel()

	_, err := Decode(EncodeMineRequest(MineRequest{Active: true, ClientTick: 1}))
	if !errors.Is(err, ErrMalformed) {
		t.Fatalf("Decode returned %v, want an error wrapping ErrMalformed", err)
	}
}

func TestMineProgressEncodesPositionAndFraction(t *testing.T) {
	t.Parallel()

	want := MineProgress{Pos: [3]int32{12, -3, 90}, Progress: 173}
	env := vnet.GetRootAsEnvelope(EncodeMineProgress(want), 0)
	if env.PayloadType() != vnet.PayloadMineProgress {
		t.Fatalf("payload is %s, want %s", env.PayloadType(), vnet.PayloadMineProgress)
	}
	tbl := payloadTable(t, env)
	progress := new(vnet.MineProgress)
	progress.Init(tbl.Bytes, tbl.Pos)
	pos := progress.Pos(nil)
	if pos == nil {
		t.Fatal("MineProgress carries no position")
	}
	if got := [3]int32{pos.X(), pos.Y(), pos.Z()}; got != want.Pos {
		t.Errorf("Pos = %v, want %v", got, want.Pos)
	}
	if got := progress.Progress(); got != want.Progress {
		t.Errorf("Progress = %d, want %d", got, want.Progress)
	}
}

// A refusal carries its action, its reason and the cell the request named — and carries
// no anchor at all when the request named none.
//
// **Absent is not zero here, and that is the whole of the second case.** The origin is a
// real place: a refusal that wrote (0, 0, 0) in place of "no anchor" would tell a client
// the server refused something at the world origin, which is exactly the mistake
// PlaceStructureRequest.HasAnchor exists to prevent in the other direction.
func TestActionRefusedEncodesTheAnswerAndOmitsAnAnchorItNeverHad(t *testing.T) {
	t.Parallel()

	want := ActionRefused{
		Action:    vnet.RefusedActionPlaceStructure,
		Reason:    vnet.RefusalReasonGroundIsAir,
		Anchor:    [3]int32{-4, 63, 17},
		HasAnchor: true,
	}
	env := vnet.GetRootAsEnvelope(EncodeActionRefused(want), 0)
	if env.PayloadType() != vnet.PayloadActionRefused {
		t.Fatalf("payload is %s, want %s", env.PayloadType(), vnet.PayloadActionRefused)
	}
	tbl := payloadTable(t, env)
	refused := new(vnet.ActionRefused)
	refused.Init(tbl.Bytes, tbl.Pos)
	if got := refused.Action(); got != want.Action {
		t.Errorf("Action = %s, want %s", got, want.Action)
	}
	if got := refused.Reason(); got != want.Reason {
		t.Errorf("Reason = %s, want %s", got, want.Reason)
	}
	anchor := refused.Anchor(nil)
	if anchor == nil {
		t.Fatal("the refusal carries no anchor, and the request it answers named one")
	}
	if got := [3]int32{anchor.X(), anchor.Y(), anchor.Z()}; got != want.Anchor {
		t.Errorf("Anchor = %v, want %v", got, want.Anchor)
	}

	anchorless := ActionRefused{
		Action: vnet.RefusedActionPlaceStructure,
		Reason: vnet.RefusalReasonMalformedNoAnchor,
		// Deliberately non-zero and deliberately not sent: HasAnchor is what decides,
		// so a stale value in the field must not reach the wire.
		Anchor: [3]int32{9, 9, 9},
	}
	env = vnet.GetRootAsEnvelope(EncodeActionRefused(anchorless), 0)
	tbl = payloadTable(t, env)
	refused = new(vnet.ActionRefused)
	refused.Init(tbl.Bytes, tbl.Pos)
	if got := refused.Anchor(nil); got != nil {
		t.Errorf("Anchor = (%d, %d, %d), want absent", got.X(), got.Y(), got.Z())
	}
	if got := refused.Reason(); got != vnet.RefusalReasonMalformedNoAnchor {
		t.Errorf("Reason = %s, want MalformedNoAnchor", got)
	}
}

// The two enums the refusal rides on fail closed on zero, and every member's value is
// pinned.
//
// The zero is the load-bearing one for the reason it always is: FlatBuffers decodes an
// absent scalar as its type's zero, so a refusal that lost its reason in transit must read
// as a code nobody can explain rather than as the ground being air.
//
// The rest is the split schemas/player.fbs draws between a world that said no and a
// request no correct client sends. It is a property of the *numbers*, so it is pinned as
// numbers on both sides — the Rust half is `the_refusal_reasons_keep_their_two_groups`.
func TestRefusalEnumsFailClosedAndKeepTheirTwoGroups(t *testing.T) {
	t.Parallel()

	if got := byte(vnet.RefusedActionUnknown); got != 0 {
		t.Errorf("RefusedAction.Unknown = %d, want 0", got)
	}
	if got := byte(vnet.RefusedActionPlaceStructure); got != 1 {
		t.Errorf("RefusedAction.PlaceStructure = %d, want 1", got)
	}
	// Reserved rather than sent: mining, block edits, crafting, repair and dropping refuse
	// in the same silence today and will reuse this message, and a member is an integer on
	// the wire — so the cheap moment to agree on the number is before anything depends on
	// it.
	for name, pair := range map[string][2]byte{
		"RefusedAction.MineBlock": {byte(vnet.RefusedActionMineBlock), 2},
		"RefusedAction.EditBlock": {byte(vnet.RefusedActionEditBlock), 3},
		"RefusedAction.Craft":     {byte(vnet.RefusedActionCraft), 4},
		"RefusedAction.Repair":    {byte(vnet.RefusedActionRepair), 5},
		"RefusedAction.DropItem":  {byte(vnet.RefusedActionDropItem), 6},
		"RefusedAction.Chat":      {byte(vnet.RefusedActionChat), 7},
		"RefusedAction.Party":     {byte(vnet.RefusedActionParty), 8},
		"RefusedAction.OpenLoot":  {byte(vnet.RefusedActionOpenLoot), 9},
		"RefusedAction.TakeLoot":  {byte(vnet.RefusedActionTakeLoot), 10},
		"RefusedAction.Attack":    {byte(vnet.RefusedActionAttack), 11},
		// V24. Reserved on the same terms as the eight above: the map's three client
		// requests all refuse, and none of them is answered by anything today.
		"RefusedAction.RequestMapTile": {byte(vnet.RefusedActionRequestMapTile), 12},
		"RefusedAction.PlaceMarker":    {byte(vnet.RefusedActionPlaceMarker), 13},
		"RefusedAction.RemoveMarker":   {byte(vnet.RefusedActionRemoveMarker), 14},
		// V25. The settlement's two client requests, reserved on the same terms: neither
		// is answered by anything today, and the number is free only until it is not.
		"RefusedAction.Interact": {byte(vnet.RefusedActionInteract), 15},
		"RefusedAction.Trade":    {byte(vnet.RefusedActionTrade), 16},
		// V26's two, the actions warded ground refuses. Edit and Mine are separate
		// members rather than one, because they are separate requests with separate
		// answers. Mine sits beside the reserved MineBlock = 2 rather than replacing it:
		// removing or renumbering that one would relabel every refusal already sent.
		"RefusedAction.Edit":        {byte(vnet.RefusedActionEdit), 17},
		"RefusedAction.Mine":        {byte(vnet.RefusedActionMine), 18},
		"RefusedAction.Mount":       {byte(vnet.RefusedActionMount), 19},
		"RefusedAction.PlayerTrade": {byte(vnet.RefusedActionPlayerTrade), 20},
	} {
		if pair[0] != pair[1] {
			t.Errorf("%s = %d, want %d", name, pair[0], pair[1])
		}
	}
	// **No member for a removal, and its absence is the decision.** A refused removal is
	// silence on purpose: a client that could tell "no such structure" from "not yours"
	// from "too far away" could map somebody else's camp by asking for ids it does not
	// have.
	//
	// **A drop has one for exactly that reason read the other way.** Every question a refused
	// drop could answer — that slot is empty, that item wears out, you are dead — is about
	// the asking player's own pack, which they already hold a complete InventoryState of. So
	// seventeen is the count, and it is what says nobody added another for a removal.
	if got := len(vnet.EnumNamesRefusedAction); got != 21 {
		t.Errorf("RefusedAction has %d members, want 21 — a removal is refused in silence by design", got)
	}

	if got := byte(vnet.RefusalReasonUnknown); got != 0 {
		t.Errorf("RefusalReason.Unknown = %d, want 0", got)
	}
	for name, pair := range map[string][2]byte{
		"GroundNotGenerated": {byte(vnet.RefusalReasonGroundNotGenerated), 1},
		"GroundIsAir":        {byte(vnet.RefusalReasonGroundIsAir), 2},
		"SpaceNotGenerated":  {byte(vnet.RefusalReasonSpaceNotGenerated), 3},
		"SpaceBlocked":       {byte(vnet.RefusalReasonSpaceBlocked), 4},
		"OutOfReach":         {byte(vnet.RefusalReasonOutOfReach), 5},
		"PlayerIsDead":       {byte(vnet.RefusalReasonPlayerIsDead), 6},
		"SlotEmpty":          {byte(vnet.RefusalReasonSlotEmpty), 7},
		"SlotUnusable":       {byte(vnet.RefusalReasonSlotUnusable), 8},
		"SlotChanged":        {byte(vnet.RefusalReasonSlotChanged), 9},
		"InventoryBusy":      {byte(vnet.RefusalReasonInventoryBusy), 10},
		"TentAlreadyPlaced":  {byte(vnet.RefusalReasonTentAlreadyPlaced), 11},
		"TooFast":            {byte(vnet.RefusalReasonTooFast), 12},
		"PartyFull":          {byte(vnet.RefusalReasonPartyFull), 13},
		"NoSuchPlayer":       {byte(vnet.RefusalReasonNoSuchPlayer), 14},
		"AlreadyInParty":     {byte(vnet.RefusalReasonAlreadyInParty), 15},
		"NoInvite":           {byte(vnet.RefusalReasonNoInvite), 16},
		"NotLeader":          {byte(vnet.RefusalReasonNotLeader), 17},
		"CorpseUnavailable":  {byte(vnet.RefusalReasonCorpseUnavailable), 18},
		"LootNotOwned":       {byte(vnet.RefusalReasonLootNotOwned), 19},
		"StaleRevision":      {byte(vnet.RefusalReasonStaleRevision), 20},
		"InventoryFull":      {byte(vnet.RefusalReasonInventoryFull), 21},
		"NoAmmunition":       {byte(vnet.RefusalReasonNoAmmunition), 22},
		"TileMisaligned":     {byte(vnet.RefusalReasonTileMisaligned), 23},
		"TooManyMarkers":     {byte(vnet.RefusalReasonTooManyMarkers), 24},
		"NoteTooLong":        {byte(vnet.RefusalReasonNoteTooLong), 25},
		"MarkerUnknown":      {byte(vnet.RefusalReasonMarkerUnknown), 26},
		// V25's three, appended inside the low group: each is the world answering a legal
		// question with no, which is the half a player is told about.
		"NotAVendor":        {byte(vnet.RefusalReasonNotAVendor), 27},
		"NotEnoughSilver":   {byte(vnet.RefusalReasonNotEnoughSilver), 28},
		"VendorDoesNotWant": {byte(vnet.RefusalReasonVendorDoesNotWant), 29},
		// V26's one, appended inside the low group for the same reason: the ground is
		// warded, the request was legal, and the player can walk somewhere else.
		"Warded":                      {byte(vnet.RefusalReasonWarded), 30},
		"MountNotLearned":             {byte(vnet.RefusalReasonMountNotLearned), 31},
		"AlreadyMounted":              {byte(vnet.RefusalReasonAlreadyMounted), 32},
		"MountNotGrounded":            {byte(vnet.RefusalReasonMountNotGrounded), 33},
		"MountIndoors":                {byte(vnet.RefusalReasonMountIndoors), 34},
		"MountLowCeiling":             {byte(vnet.RefusalReasonMountLowCeiling), 35},
		"CastAlreadyInProgress":       {byte(vnet.RefusalReasonCastAlreadyInProgress), 36},
		"CastInterruptedByDamage":     {byte(vnet.RefusalReasonCastInterruptedByDamage), 37},
		"CastInterruptedByMovement":   {byte(vnet.RefusalReasonCastInterruptedByMovement), 38},
		"CastInterruptedByJump":       {byte(vnet.RefusalReasonCastInterruptedByJump), 39},
		"CastInterruptedByDeath":      {byte(vnet.RefusalReasonCastInterruptedByDeath), 40},
		"ActionForbiddenWhileMounted": {byte(vnet.RefusalReasonActionForbiddenWhileMounted), 41},
		"MountAlreadyLearned":         {byte(vnet.RefusalReasonMountAlreadyLearned), 42},
		"AlreadyTrading":              {byte(vnet.RefusalReasonAlreadyTrading), 43},
		"TradeNotOpen":                {byte(vnet.RefusalReasonTradeNotOpen), 44},
		"TradeSlotTaken":              {byte(vnet.RefusalReasonTradeSlotTaken), 45},
		"NothingToOffer":              {byte(vnet.RefusalReasonNothingToOffer), 46},
		"TradeCooldown":               {byte(vnet.RefusalReasonTradeCooldown), 47},
		"MalformedNoAnchor":           {byte(vnet.RefusalReasonMalformedNoAnchor), 64},
		"MalformedFacing":             {byte(vnet.RefusalReasonMalformedFacing), 65},
		"MalformedSlot":               {byte(vnet.RefusalReasonMalformedSlot), 66},
		"MalformedKind":               {byte(vnet.RefusalReasonMalformedKind), 67},
	} {
		if pair[0] != pair[1] {
			t.Errorf("RefusalReason.%s = %d, want %d", name, pair[0], pair[1])
		}
	}
	if got := len(vnet.EnumNamesRefusalReason); got != 52 {
		t.Errorf("RefusalReason has %d members, want 52 — a new one needs a decision, not a test edit", got)
	}
}

func TestInventoryMoveRequestRoundTripsEveryField(t *testing.T) {
	t.Parallel()

	want := InventoryMoveRequest{From: 2, To: 35, Count: 17}
	msg, err := Decode(EncodeInventoryMoveRequest(want))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Kind != vnet.PayloadInventoryMoveRequest {
		t.Fatalf("Kind = %s, want %s", msg.Kind, vnet.PayloadInventoryMoveRequest)
	}
	if msg.InventoryMove == nil {
		t.Fatal("InventoryMoveRequest payload is nil")
	}
	if got := *msg.InventoryMove; got != want {
		t.Errorf("round trip produced %+v, want %+v", got, want)
	}
}

func TestInventoryMoveRequestRejectsMalformedValues(t *testing.T) {
	t.Parallel()

	tests := map[string]InventoryMoveRequest{
		"from outside inventory": {From: InventorySlots, To: 0, Count: 1},
		"to outside inventory":   {From: 0, To: InventorySlots, Count: 1},
		"zero count":             {From: 0, To: 1, Count: 0},
	}
	for name, request := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			if _, err := Decode(EncodeInventoryMoveRequest(request)); !errors.Is(err, ErrMalformed) {
				t.Errorf("Decode returned %v, want an error wrapping ErrMalformed", err)
			}
		})
	}
}

func TestNewClientPayloadTagsWithoutPayloadsAreMalformed(t *testing.T) {
	t.Parallel()

	for _, kind := range []vnet.Payload{vnet.PayloadMineRequest, vnet.PayloadInventoryMoveRequest, vnet.PayloadAttackRequest} {
		kind := kind
		t.Run(kind.String(), func(t *testing.T) {
			t.Parallel()
			b := flatbuffers.NewBuilder(64)
			vnet.EnvelopeStart(b)
			vnet.EnvelopeAddPayloadType(b, kind)
			env := vnet.EnvelopeEnd(b)
			vnet.FinishEnvelopeBuffer(b, env)
			if _, err := Decode(b.FinishedBytes()); !errors.Is(err, ErrMalformed) {
				t.Errorf("Decode returned %v, want an error wrapping ErrMalformed", err)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// ChunkResendRequest
// ---------------------------------------------------------------------------

func TestChunkResendRequestRoundTripsItsCoordinate(t *testing.T) {
	t.Parallel()

	want := ChunkResendRequest{Coord: ChunkCoord{X: -4, Y: 12, Z: 900}, HasCoord: true}

	msg, err := Decode(EncodeChunkResendRequest(want))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Kind != vnet.PayloadChunkResendRequest {
		t.Fatalf("Kind = %s, want %s", msg.Kind, vnet.PayloadChunkResendRequest)
	}
	if msg.ChunkResendRequest == nil {
		t.Fatal("ChunkResendRequest payload is nil")
	}
	if got := *msg.ChunkResendRequest; got != want {
		t.Errorf("round trip produced %+v, want %+v", got, want)
	}
}

// The absent-coordinate case, and the reason HasCoord exists at all: chunk (0, 0, 0) is a
// real chunk, so a request that named nothing must not decode as a request for the origin.
func TestAChunkResendRequestWithoutACoordinateDecodesAsAbsent(t *testing.T) {
	t.Parallel()

	msg, err := Decode(EncodeChunkResendRequest(ChunkResendRequest{}))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.ChunkResendRequest == nil {
		t.Fatal("ChunkResendRequest payload is nil")
	}
	if msg.ChunkResendRequest.HasCoord {
		t.Error("HasCoord is true for a request that carried no coordinate")
	}
	if got := msg.ChunkResendRequest.Coord; got != (ChunkCoord{}) {
		t.Errorf("the absent coordinate decoded as %+v", got)
	}
}

func TestAChunkResendRequestTagWithNoPayloadIsMalformed(t *testing.T) {
	t.Parallel()

	b := flatbuffers.NewBuilder(64)
	vnet.EnvelopeStart(b)
	vnet.EnvelopeAddPayloadType(b, vnet.PayloadChunkResendRequest)
	env := vnet.EnvelopeEnd(b)
	vnet.FinishEnvelopeBuffer(b, env)

	if _, err := Decode(b.FinishedBytes()); !errors.Is(err, ErrMalformed) {
		t.Errorf("Decode returned %v, want an error wrapping ErrMalformed", err)
	}
}

// The truncation and corruption sweep, for the fourth payload a client chooses the bytes
// of. Decode must stay total over it: an error or a message, never a panic.
func TestDecodeIsTotalOverADamagedChunkResendRequest(t *testing.T) {
	t.Parallel()

	valid := EncodeChunkResendRequest(ChunkResendRequest{Coord: ChunkCoord{X: 3, Y: -1, Z: 77}, HasCoord: true})

	for i := range len(valid) {
		if _, err := Decode(valid[:i]); err == nil {
			t.Errorf("a %d-byte prefix of a %d-byte resend request decoded successfully", i, len(valid))
		}
	}
	for i := range len(valid) {
		damaged := bytes.Clone(valid)
		damaged[i] ^= 0xFF
		_, _ = Decode(damaged)
	}
}

// ---------------------------------------------------------------------------
// Protocol V3 — attack intent, vitals, mobs and durability
// ---------------------------------------------------------------------------

func TestAttackRequestRoundTripsAsClientIntent(t *testing.T) {
	t.Parallel()

	want := AttackRequest{Slot: 0, ClientTick: 4_294_967_295}

	msg, err := Decode(EncodeAttackRequest(want))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Kind != vnet.PayloadAttackRequest {
		t.Fatalf("Kind = %s, want %s", msg.Kind, vnet.PayloadAttackRequest)
	}
	if msg.Attack == nil {
		t.Fatal("AttackRequest payload is nil")
	}
	if got := *msg.Attack; got != want {
		t.Errorf("round trip produced %+v, want %+v", got, want)
	}
}

func TestBlockRequestRoundTripsAsHeldIntent(t *testing.T) {
	t.Parallel()
	want := BlockRequest{Active: true, ClientTick: ^uint32(0)}
	msg, err := Decode(EncodeBlockRequest(want))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Kind != vnet.PayloadBlockRequest || msg.Block == nil {
		t.Fatalf("message = %+v, want BlockRequest", msg)
	}
	if *msg.Block != want {
		t.Errorf("BlockRequest = %+v, want %+v", *msg.Block, want)
	}
}

// The whole of what the wire carries, asserted by absence: a swing names a slot and a
// tick, and there is no field for a victim, a position, an aim, a damage or a result.
// The vtable is the only place that claim can be checked, because a field nobody added
// is indistinguishable from a field nobody set through the accessors.
func TestAttackRequestCarriesNothingButASlotAndATick(t *testing.T) {
	t.Parallel()

	frame := EncodeAttackRequest(AttackRequest{Slot: 3, ClientTick: 9})
	env := vnet.GetRootAsEnvelope(frame, 0)
	tbl := payloadTable(t, env)

	// A FlatBuffers vtable is [vtable size, table size, one voffset per field], each a
	// uint16, so the field count is (size - 4) / 2. Two is the largest this table may
	// ever have while it means intent. The measure is verified by the numbers it gives
	// its neighbours — PlayerInput 6, MineRequest 3, InventoryMoveRequest 3 — and its
	// one blind spot is stated rather than papered over: trailing default-valued fields
	// are truncated out of a vtable, so a field added and never set would not be seen
	// here. A field added and *used* is exactly what this is for.
	vtableOffset := flatbuffers.UOffsetT(flatbuffers.SOffsetT(tbl.Pos) - flatbuffers.GetSOffsetT(tbl.Bytes[tbl.Pos:]))
	vtableSize := flatbuffers.GetVOffsetT(tbl.Bytes[vtableOffset:])
	if fields := (int(vtableSize) - 4) / 2; fields != 2 {
		t.Errorf("AttackRequest has %d fields on the wire, want exactly slot and client_tick", fields)
	}
}

// A slot outside the inventory reaches the simulation as a value to refuse, not as a
// malformed frame. This is the deliberate difference from InventoryMoveRequest, whose
// slots the decoder does bound: a move's slots are indexed with, a swing's is looked up.
func TestAnOutOfRangeAttackSlotIsNotAProtocolError(t *testing.T) {
	t.Parallel()

	msg, err := Decode(EncodeAttackRequest(AttackRequest{Slot: 255, ClientTick: 1}))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Attack == nil {
		t.Fatal("AttackRequest payload is nil")
	}
	if got := msg.Attack.Slot; got != 255 {
		t.Errorf("Slot = %d, want 255 carried through verbatim", got)
	}
}

// The truncation and corruption sweep for the payload tag 15 adds. Decode must stay
// total over bytes a client chose: an error or a message, never a panic.
func TestDecodeIsTotalOverADamagedAttackRequest(t *testing.T) {
	t.Parallel()

	valid := EncodeAttackRequest(AttackRequest{Slot: 2, ClientTick: 77})

	for i := range len(valid) {
		if _, err := Decode(valid[:i]); err == nil {
			t.Errorf("a %d-byte prefix of a %d-byte attack frame decoded successfully", i, len(valid))
		}
	}
	for i := range len(valid) {
		damaged := bytes.Clone(valid)
		damaged[i] ^= 0xFF
		_, _ = Decode(damaged)
	}
}

// The one required field on the wire, and the test schemas/player.fbs names.
//
// flatc's Go output carries no assertion for `(required)` — EntitySnapshotEnd is a bare
// EndObject — so nothing but this test stands between a refactor and a snapshot the
// Rust verifier refuses. The empty snapshot is the case worth pinning: it is the one
// where a caller supplies nothing at all.
func TestEntitySnapshotAlwaysCarriesTheRecipientsVitals(t *testing.T) {
	t.Parallel()

	for name, snapshot := range map[string]EntitySnapshot{
		"empty":         {Tick: 1, Vitals: PlayerVitals{Health: 100, MaxHealth: 100, LifeState: vnet.LifeStateAlive}},
		"zero vitals":   {Tick: 2},
		"with entities": {Tick: 3, Entities: []EntityState{{EntityID: 1}}},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			env := vnet.GetRootAsEnvelope(EncodeEntitySnapshot(snapshot), 0)
			tbl := payloadTable(t, env)
			decoded := new(vnet.EntitySnapshot)
			decoded.Init(tbl.Bytes, tbl.Pos)

			if decoded.SelfVitals(nil) == nil {
				t.Error("the snapshot carries no self_vitals")
			}
		})
	}
}

// A dead player's countdown, which is the shape legacy PR 93 will actually emit and the one the
// client's decoder has the most invariants about.
func TestEntitySnapshotCarriesADeadPlayersCountdown(t *testing.T) {
	t.Parallel()

	want := PlayerVitals{
		Health: 0, MaxHealth: 100, LifeState: vnet.LifeStateDead, RespawnTicks: 60,
		Hunger: 17, MaxHunger: 100,
		Level: 6, Experience: 17, ExperienceToNext: 300,
	}

	env := vnet.GetRootAsEnvelope(EncodeEntitySnapshot(EntitySnapshot{Tick: 9, Vitals: want}), 0)
	tbl := payloadTable(t, env)
	decoded := new(vnet.EntitySnapshot)
	decoded.Init(tbl.Bytes, tbl.Pos)

	vitals := decoded.SelfVitals(nil)
	if vitals == nil {
		t.Fatal("the snapshot carries no self_vitals")
	}
	got := PlayerVitals{
		Health:           vitals.Health(),
		MaxHealth:        vitals.MaxHealth(),
		LifeState:        vitals.LifeState(),
		RespawnTicks:     vitals.RespawnTicks(),
		Invulnerable:     vitals.Invulnerable(),
		Hunger:           vitals.Hunger(),
		MaxHunger:        vitals.MaxHunger(),
		Level:            vitals.Level(),
		Experience:       vitals.Experience(),
		ExperienceToNext: vitals.ExperienceToNext(),
	}
	if got != want {
		t.Errorf("self_vitals decoded as %+v, want %+v", got, want)
	}
}

// Every new enum's zero member is Unknown, and every member's value is pinned.
//
// The zero is the load-bearing one: FlatBuffers decodes an absent scalar as zero, so a
// vitals table with no life_state, or a mob with no kind, must read as something a
// decoder refuses rather than as a real state. Renumbering any of the rest is a wire
// break that compiles perfectly.
func TestV3EnumsFailClosedOnZero(t *testing.T) {
	t.Parallel()

	if got := byte(vnet.LifeStateUnknown); got != 0 {
		t.Errorf("LifeState.Unknown = %d, want 0", got)
	}
	if got := byte(vnet.MobKindUnknown); got != 0 {
		t.Errorf("MobKind.Unknown = %d, want 0", got)
	}
	if got := byte(vnet.MobActionUnknown); got != 0 {
		t.Errorf("MobAction.Unknown = %d, want 0", got)
	}

	for value, want := range map[byte]byte{
		byte(vnet.LifeStateAlive): 1, byte(vnet.LifeStateDead): 2,
	} {
		if value != want {
			t.Errorf("a LifeState member is %d, want %d", value, want)
		}
	}
	if got := byte(vnet.MobKindDraugr); got != 1 {
		t.Errorf("MobKind.Draugr = %d, want 1", got)
	}
	for name, pair := range map[string][2]byte{
		"Idle":     {byte(vnet.MobActionIdle), 1},
		"Chase":    {byte(vnet.MobActionChase), 2},
		"Windup":   {byte(vnet.MobActionWindup), 3},
		"Recovery": {byte(vnet.MobActionRecovery), 4},
		// Appended by V9, and pinned here with the four that came before it for the reason
		// they are: the value is an integer on the wire, so a renumbering would draw one
		// action where the server said another and no compiler would object.
		"Dying":  {byte(vnet.MobActionDying), 5},
		"Corpse": {byte(vnet.MobActionCorpse), 7},
	} {
		if pair[0] != pair[1] {
			t.Errorf("MobAction.%s = %d, want %d", name, pair[0], pair[1])
		}
	}
}

// The same guarantee for V4's vocabulary, and the zero is load-bearing in a stronger
// sense here: all three of these enums ride on client -> server messages, where the
// absent-field zero is the value a hostile or merely old peer produces for free. A
// RecipeID that read as a real recipe when omitted would be a craft nobody asked for.
func TestV4EnumsFailClosedOnZero(t *testing.T) {
	t.Parallel()

	if got := byte(vnet.RecipeIDUnknown); got != 0 {
		t.Errorf("RecipeID.Unknown = %d, want 0", got)
	}
	if got := byte(vnet.StructureKindUnknown); got != 0 {
		t.Errorf("StructureKind.Unknown = %d, want 0", got)
	}
	if got := byte(vnet.FacingUnknown); got != 0 {
		t.Errorf("Facing.Unknown = %d, want 0", got)
	}

	for name, pair := range map[string][2]byte{
		"Forge":           {byte(vnet.RecipeIDForge), 1},
		"IronSword":       {byte(vnet.RecipeIDIronSword), 2},
		"SharpeningStone": {byte(vnet.RecipeIDSharpeningStone), 3},
		"Tent":            {byte(vnet.RecipeIDTent), 4},
	} {
		if pair[0] != pair[1] {
			t.Errorf("RecipeID.%s = %d, want %d", name, pair[0], pair[1])
		}
	}
	for name, pair := range map[string][2]byte{
		"Tent":  {byte(vnet.StructureKindTent), 1},
		"Forge": {byte(vnet.StructureKindForge), 2},
	} {
		if pair[0] != pair[1] {
			t.Errorf("StructureKind.%s = %d, want %d", name, pair[0], pair[1])
		}
	}
	for name, pair := range map[string][2]byte{
		"North": {byte(vnet.FacingNorth), 1},
		"East":  {byte(vnet.FacingEast), 2},
		"South": {byte(vnet.FacingSouth), 3},
		"West":  {byte(vnet.FacingWest), 4},
	} {
		if pair[0] != pair[1] {
			t.Errorf("Facing.%s = %d, want %d", name, pair[0], pair[1])
		}
	}
}

// V6's three new enum members sit where they were appended, and the members they were
// appended after have not moved.
//
// **The number is the contract, and nothing else about an enum member is.** A rename is
// a compile error on both sides and gets fixed in an afternoon; a renumbering compiles
// perfectly everywhere and turns a draugr into a vargr in every build already shipped,
// which is why this file pins integers rather than trusting declaration order. The
// members below are exactly the ones V6 adds, so a fourth arriving without a decision
// fails here.
func TestV6AppendsWithoutMovingWhatCameBefore(t *testing.T) {
	t.Parallel()

	for name, pair := range map[string][2]byte{
		// Appended after Draugr = 1.
		"MobKind.Vargr": {byte(vnet.MobKindVargr), 2},
		// Appended in V14 after Vargr = 2.
		"MobKind.Deer": {byte(vnet.MobKindDeer), 3},
		// Appended in V25 after Deer = 3, and the one member of this enum that moved
		// ProtocolVersion.Current by itself: MobState.kind is refused rather than
		// dropped when the receiver cannot name it.
		"MobKind.Villager": {byte(vnet.MobKindVillager), 4},
		// V27 reserves the mountable stable resident; authoritative behavior follows.
		"MobKind.Horse": {byte(vnet.MobKindHorse), 5},
		// Appended after Forge = 2.
		"StructureKind.Campfire": {byte(vnet.StructureKindCampfire), 3},
		// Appended after Tent = 4.
		"RecipeID.Campfire":     {byte(vnet.RecipeIDCampfire), 5},
		"RecipeID.LeatherPatch": {byte(vnet.RecipeIDLeatherPatch), 6},

		// V8's three, appended after LeatherPatch = 6. The value is a byte on the wire,
		// so a renumbering turns every craft a client asks for into a different one.
		"RecipeID.Shovel":          {byte(vnet.RecipeIDShovel), 7},
		"RecipeID.Pickaxe":         {byte(vnet.RecipeIDPickaxe), 8},
		"RecipeID.Axe":             {byte(vnet.RecipeIDAxe), 9},
		"RecipeID.CookedMeat":      {byte(vnet.RecipeIDCookedMeat), 10},
		"RecipeID.LeatherCap":      {byte(vnet.RecipeIDLeatherCap), 11},
		"RecipeID.LeatherJerkin":   {byte(vnet.RecipeIDLeatherJerkin), 12},
		"RecipeID.LeatherLeggings": {byte(vnet.RecipeIDLeatherLeggings), 13},
		"RecipeID.IronHelm":        {byte(vnet.RecipeIDIronHelm), 14},
		"RecipeID.IronCuirass":     {byte(vnet.RecipeIDIronCuirass), 15},
		"RecipeID.IronGreaves":     {byte(vnet.RecipeIDIronGreaves), 16},
		"RecipeID.WoodenShield":    {byte(vnet.RecipeIDWoodenShield), 17},
		"RecipeID.Bow":             {byte(vnet.RecipeIDBow), 18},
		"RecipeID.Arrows":          {byte(vnet.RecipeIDArrows), 19},
		"RecipeID.WoodenSceptre":   {byte(vnet.RecipeIDWoodenSceptre), 20},

		// V26's two. StructureKind.Runestone is the member that moved
		// ProtocolVersion.Current on its own — StructureState.kind is refused rather than
		// dropped when a receiver cannot name it, which ends the session. RecipeID.Runestone
		// travels the other way and owes nothing: an unknown recipe is refused by the
		// authoritative server as an ordinary answer.
		"StructureKind.Runestone": {byte(vnet.StructureKindRunestone), 4},
		"RecipeID.Runestone":      {byte(vnet.RecipeIDRunestone), 21},
	} {
		if pair[0] != pair[1] {
			t.Errorf("%s = %d, want %d", name, pair[0], pair[1])
		}
	}

	// Membership, in the shape the Payload union is checked in: a member added without
	// a decision fails here rather than reaching the wire. Each count includes the
	// zero member every one of these enums carries to fail closed.
	for name, pair := range map[string][2]int{
		// Five since V25's Villager, which is the one member of this enum whose arrival
		// moved ProtocolVersion.Current on its own.
		"MobKind":       {len(vnet.EnumNamesMobKind), 6},
		"StructureKind": {len(vnet.EnumNamesStructureKind), 5},
		"RecipeID":      {len(vnet.EnumNamesRecipeID), 22},
	} {
		if pair[0] != pair[1] {
			t.Errorf("%s has %d members, want %d — a new one needs a decision, not a test edit", name, pair[0], pair[1])
		}
	}
}

// The alignment that makes three vectors safe to read as one row per slot: every vector
// is exactly InventorySlots long, and index i of each describes the same slot.
func TestInventoryStateEmitsAlignedDurabilityVectors(t *testing.T) {
	t.Parallel()

	stacks := make([]InventoryStack, int(InventorySlots))
	stacks[0] = InventoryStack{ItemID: 7, Count: 1, Durability: 100, MaxDurability: 100}
	stacks[1] = InventoryStack{ItemID: 2, Count: 64}
	// Worn out, and still carried: zero durability under a non-zero maximum is an
	// unusable item, never an empty slot.
	stacks[35] = InventoryStack{ItemID: 7, Count: 1, Durability: 0, MaxDurability: 100}

	env := vnet.GetRootAsEnvelope(EncodeInventoryState(InventoryState{Stacks: stacks, Silver: 1234}), 0)
	tbl := payloadTable(t, env)
	state := new(vnet.InventoryState)
	state.Init(tbl.Bytes, tbl.Pos)
	if got := state.Silver(); got != 1234 {
		t.Errorf("Silver = %d, want 1234", got)
	}

	slots := int(InventorySlots)
	if got := state.StacksLength(); got != slots*2 {
		t.Fatalf("StacksLength = %d, want %d", got, slots*2)
	}
	if got := state.DurabilityLength(); got != slots {
		t.Fatalf("DurabilityLength = %d, want %d", got, slots)
	}
	if got := state.MaxDurabilityLength(); got != slots {
		t.Fatalf("MaxDurabilityLength = %d, want %d", got, slots)
	}

	for slot := range slots {
		want := stacks[slot]
		got := InventoryStack{
			ItemID:        state.Stacks(slot * 2),
			Count:         state.Stacks(slot*2 + 1),
			Durability:    state.Durability(slot),
			MaxDurability: state.MaxDurability(slot),
		}
		if got != want {
			t.Errorf("slot %d decoded as %+v, want %+v", slot, got, want)
		}
	}
}

// An inventory of resources carries the vectors anyway, all zero. The client's decoder
// requires all three to be the same length, so a server that emitted them only when
// something was durable would produce a frame nobody could read.
func TestAnEmptyInventoryStillCarriesZeroDurabilityVectors(t *testing.T) {
	t.Parallel()

	env := vnet.GetRootAsEnvelope(EncodeInventoryState(InventoryState{}), 0)
	tbl := payloadTable(t, env)
	state := new(vnet.InventoryState)
	state.Init(tbl.Bytes, tbl.Pos)

	slots := int(InventorySlots)
	if got := state.DurabilityLength(); got != slots {
		t.Fatalf("DurabilityLength = %d, want %d", got, slots)
	}
	if got := state.MaxDurabilityLength(); got != slots {
		t.Fatalf("MaxDurabilityLength = %d, want %d", got, slots)
	}
	for slot := range slots {
		if got := state.Durability(slot); got != 0 {
			t.Errorf("Durability(%d) = %d, want 0", slot, got)
		}
		if got := state.MaxDurability(slot); got != 0 {
			t.Errorf("MaxDurability(%d) = %d, want 0", slot, got)
		}
	}
}

// ---------------------------------------------------------------------------
// Protocol V4 — placed structures
// ---------------------------------------------------------------------------

// Both new client->server requests round trip through the encoder this package owns.
// The values are the ones the simulation will be handed, and every one of them is
// carried verbatim: judging them here would put a decision in the package that owns the
// envelope.
func TestTheStructureRequestsRoundTrip(t *testing.T) {
	t.Parallel()

	place := PlaceStructureRequest{
		Slot:       4,
		Anchor:     [3]int32{-7, 63, 12},
		HasAnchor:  true,
		Facing:     vnet.FacingWest,
		ClientTick: 4242,
	}
	msg, err := Decode(EncodePlaceStructureRequest(place))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Kind != vnet.PayloadPlaceStructureRequest {
		t.Fatalf("Kind = %s, want PlaceStructureRequest", msg.Kind)
	}
	if msg.PlaceStructure == nil {
		t.Fatal("PlaceStructureRequest payload is nil")
	}
	if *msg.PlaceStructure != place {
		t.Errorf("decoded %+v, want %+v", *msg.PlaceStructure, place)
	}

	remove := RemoveStructureRequest{StructureID: 1 << 40, ClientTick: 9}
	msg, err = Decode(EncodeRemoveStructureRequest(remove))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Kind != vnet.PayloadRemoveStructureRequest {
		t.Fatalf("Kind = %s, want RemoveStructureRequest", msg.Kind)
	}
	if msg.RemoveStructure == nil {
		t.Fatal("RemoveStructureRequest payload is nil")
	}
	if *msg.RemoveStructure != remove {
		t.Errorf("decoded %+v, want %+v", *msg.RemoveStructure, remove)
	}
}

// An absent anchor decodes as absent rather than as the origin, which is the invariant
// schemas/player.fbs states and the reason HasAnchor exists at all. The frame is still a
// message: whether a placement with no anchor is legal is a decision, and decisions
// belong to the simulation.
func TestAPlacementWithNoAnchorDecodesAsAbsentRatherThanTheOrigin(t *testing.T) {
	t.Parallel()

	msg, err := Decode(EncodePlaceStructureRequest(PlaceStructureRequest{Slot: 1, Facing: vnet.FacingNorth}))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.PlaceStructure == nil {
		t.Fatal("PlaceStructureRequest payload is nil")
	}
	if msg.PlaceStructure.HasAnchor {
		t.Error("a request with no anchor decoded as one that has an anchor")
	}
	if got := msg.PlaceStructure.Anchor; got != [3]int32{} {
		t.Errorf("Anchor = %v on a request that carried none", got)
	}
}

// Values the contract forbids reach the simulation as values to refuse, exactly as an
// out-of-range attack slot does. An Unknown facing is the absent-field case, a slot past
// the inventory is a wrong number in a well-formed frame, and a structure id nobody has
// is a question rather than a claim.
func TestForbiddenStructureValuesAreCarriedRatherThanRejected(t *testing.T) {
	t.Parallel()

	msg, err := Decode(EncodePlaceStructureRequest(PlaceStructureRequest{
		Slot: 255, Anchor: [3]int32{1, 2, 3}, HasAnchor: true, Facing: vnet.FacingUnknown,
	}))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.PlaceStructure == nil {
		t.Fatal("PlaceStructureRequest payload is nil")
	}
	if got := msg.PlaceStructure.Slot; got != 255 {
		t.Errorf("Slot = %d, want 255 carried through verbatim", got)
	}
	if got := msg.PlaceStructure.Facing; got != vnet.FacingUnknown {
		t.Errorf("Facing = %s, want Unknown carried through verbatim", got)
	}

	msg, err = Decode(EncodeRemoveStructureRequest(RemoveStructureRequest{StructureID: 0}))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.RemoveStructure == nil {
		t.Fatal("RemoveStructureRequest payload is nil")
	}
	if got := msg.RemoveStructure.StructureID; got != 0 {
		t.Errorf("StructureID = %d, want the reserved zero carried through verbatim", got)
	}
}

// The truncation and corruption sweep for the two payloads tags 18 and 19 add. Decode
// must stay total over bytes a client chose: an error or a message, never a panic.
func TestDecodeIsTotalOverDamagedStructureRequests(t *testing.T) {
	t.Parallel()

	for name, valid := range map[string][]byte{
		"place": EncodePlaceStructureRequest(PlaceStructureRequest{
			Slot: 2, Anchor: [3]int32{4, 5, 6}, HasAnchor: true, Facing: vnet.FacingEast, ClientTick: 77,
		}),
		"remove": EncodeRemoveStructureRequest(RemoveStructureRequest{StructureID: 12345, ClientTick: 78}),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			for i := range len(valid) {
				if _, err := Decode(valid[:i]); err == nil {
					t.Errorf("a %d-byte prefix of a %d-byte %s frame decoded successfully", i, len(valid), name)
				}
			}
			for i := range len(valid) {
				damaged := bytes.Clone(valid)
				damaged[i] ^= 0xFF
				_, _ = Decode(damaged)
			}
		})
	}
}

// The snapshot's new vector, laid out beside the three it already had. The order is the
// caller's and the encoder keeps it, which is what lets the simulation sort by identity
// once rather than every consumer sorting again.
func TestEntitySnapshotLaysOutItsStructures(t *testing.T) {
	t.Parallel()

	want := []StructureState{
		{StructureID: 101, Kind: vnet.StructureKindTent, Anchor: [3]int32{0, 63, 0}, Facing: vnet.FacingNorth, OwnerEntityID: 7},
		{StructureID: 102, Kind: vnet.StructureKindForge, Anchor: [3]int32{-4, 62, 9}, Facing: vnet.FacingSouth, OwnerEntityID: 8},
	}

	env := vnet.GetRootAsEnvelope(EncodeEntitySnapshot(EntitySnapshot{Tick: 5, Structures: want}), 0)
	tbl := payloadTable(t, env)
	snapshot := new(vnet.EntitySnapshot)
	snapshot.Init(tbl.Bytes, tbl.Pos)

	if got := snapshot.StructuresLength(); got != len(want) {
		t.Fatalf("StructuresLength = %d, want %d", got, len(want))
	}
	for i, expected := range want {
		var held vnet.StructureState
		if !snapshot.Structures(&held, i) {
			t.Fatalf("structure %d is missing from a snapshot that claims to hold it", i)
		}
		anchor := held.Anchor(nil)
		if anchor == nil {
			t.Fatalf("structure %d carries no anchor", i)
		}
		got := StructureState{
			StructureID:   held.StructureId(),
			Kind:          held.Kind(),
			Anchor:        [3]int32{anchor.X(), anchor.Y(), anchor.Z()},
			Facing:        held.Facing(),
			OwnerEntityID: held.OwnerEntityId(),
		}
		if got != expected {
			t.Errorf("structure %d decoded as %+v, want %+v", i, got, expected)
		}
	}
}

// A snapshot with no structures is a snapshot that says so: the length is zero, and the
// encoder needed no branch to produce it.
//
// **The length is the only thing worth asserting here, and that is a fact about the
// consumers rather than a gap in the test.** An absent vector and an empty one both read
// as "no structures" on both sides, so there is nothing on the wire to tell them apart —
// which is also why the encoder writes the vector unconditionally instead of only when it
// has something to put in it.
//
// It deliberately does not read element 0 to prove the vector is empty. flatc's Go vector
// accessor is **not bounds-checked**: it computes an offset and reports success, so
// `Structures(&held, 0)` against an empty vector returns true with whatever bytes happen
// to follow. Every loop over one of these ranges over its length for that reason, here and
// in the simulation's own contract tests.
func TestASnapshotWithNoStructuresSaysSo(t *testing.T) {
	t.Parallel()

	env := vnet.GetRootAsEnvelope(EncodeEntitySnapshot(EntitySnapshot{Tick: 1}), 0)
	tbl := payloadTable(t, env)
	snapshot := new(vnet.EntitySnapshot)
	snapshot.Init(tbl.Bytes, tbl.Pos)

	if got := snapshot.StructuresLength(); got != 0 {
		t.Fatalf("StructuresLength = %d, want 0", got)
	}
}

// ---------------------------------------------------------------------------
// Protocol V10 — a death every viewer is told about
// ---------------------------------------------------------------------------

// The dead ride in the snapshot, in the order they were given and with nothing else
// displaced. The second half matters as much as the first: an appended field that displaced
// an existing one would satisfy every assertion about itself while breaking every frame
// already on the wire.
func TestASnapshotCarriesTheDeadPlayers(t *testing.T) {
	t.Parallel()

	env := vnet.GetRootAsEnvelope(EncodeEntitySnapshot(EntitySnapshot{
		Tick: 7,
		Entities: []EntityState{
			{EntityID: 11, Pos: [3]float32{1, 2, 3}},
			{EntityID: 22, Pos: [3]float32{4, 5, 6}},
		},
		Vitals:      PlayerVitals{Health: 0, MaxHealth: 10, LifeState: vnet.LifeStateDead, RespawnTicks: 40},
		TickOfDay:   1234,
		DeadPlayers: []uint64{22, 11},
	}), 0)
	tbl := payloadTable(t, env)
	snapshot := new(vnet.EntitySnapshot)
	snapshot.Init(tbl.Bytes, tbl.Pos)

	if got := snapshot.DeadPlayersLength(); got != 2 {
		t.Fatalf("DeadPlayersLength = %d, want 2", got)
	}
	for i, want := range []uint64{22, 11} {
		if got := snapshot.DeadPlayers(i); got != want {
			t.Errorf("DeadPlayers(%d) = %d, want %d — the vector is not in the order it was given", i, got, want)
		}
	}

	if got := snapshot.ServerTick(); got != 7 {
		t.Errorf("ServerTick = %d, want 7 — dead_players was appended, not inserted", got)
	}
	if got := snapshot.TickOfDay(); got != 1234 {
		t.Errorf("TickOfDay = %d, want 1234 — dead_players was appended, not inserted", got)
	}
	if vitals := snapshot.SelfVitals(nil); vitals == nil || vitals.RespawnTicks() != 40 {
		t.Error("self_vitals did not survive the field appended after it")
	}
}

// **Nobody dead costs nothing at all, and that is the whole argument for this shape** — plus
// the measurement it was chosen on, kept as a test so the numbers in the pull request can be
// re-derived rather than believed.
//
// Nil and empty are the same wire value and both produce the bytes a pre-V10 encoder did: the
// field is not written, and a FlatBuffers vtable is trimmed of its trailing empty slots, so
// the last field in the table leaves no trace when absent. Byte equality rather than a
// length, which would pass on two differently laid-out frames of the same size.
//
// Three shapes could have put a life state beside every player: a fifth field in the
// EntityState struct, a table per player in the shape MobState took, and this sparse vector.
// The first is unavailable — a struct's field list can never be taken back. The gap between
// the other two is pinned at a realistic player count, deliberately as a *bound* rather than
// an equality.
func TestWhatADeathCostsOnTheWire(t *testing.T) {
	t.Parallel()

	const players = 8

	snapshot := func(dead int) EntitySnapshot {
		s := EntitySnapshot{
			Tick:   1,
			Vitals: PlayerVitals{Health: 10, MaxHealth: 10, LifeState: vnet.LifeStateAlive},
		}
		for i := range players {
			s.Entities = append(s.Entities, EntityState{
				EntityID: uint64(i + 1),
				Pos:      [3]float32{float32(i), 64, float32(i)},
			})
			if i < dead {
				s.DeadPlayers = append(s.DeadPlayers, uint64(i+1))
			}
		}
		return s
	}

	absent := EncodeEntitySnapshot(snapshot(0))
	nobody := snapshot(0)
	nobody.DeadPlayers = []uint64{}
	if !bytes.Equal(absent, EncodeEntitySnapshot(nobody)) {
		t.Error("an empty dead_players encoded differently from an absent one")
	}

	none := len(absent)
	one := len(EncodeEntitySnapshot(snapshot(1)))
	all := len(EncodeEntitySnapshot(snapshot(players)))

	// The rejected shape, measured rather than estimated: MobState is the table-per-entity
	// payload this contract already has, so eight of them beside the same entity vector is
	// what a per-player table would have cost every tick, alive or dead.
	tables := snapshot(0)
	for i := range players {
		tables.Mobs = append(tables.Mobs, MobState{
			EntityID: uint64(1000 + i), Kind: vnet.MobKindDraugr, Action: vnet.MobActionIdle,
			Pos: [3]float32{float32(i), 64, float32(i)}, MaxHealth: 10, Health: 10,
		})
	}
	perTable := (len(EncodeEntitySnapshot(tables)) - none) / players

	t.Logf("%d players: nobody dead %d bytes, one dead %d bytes (+%d), all dead %d bytes (+%d); "+
		"a table per player would cost about %d bytes each, %d per tick, dead or alive",
		players, none, one, one-none, all, all-none, perTable, perTable*players)

	// A handful of bytes — its own length plus an offset and a vtable slot — rather than a
	// per-entity charge, which is the property being pinned.
	if cost := one - none; cost > 32 {
		t.Errorf("one dead player costs %d bytes, want no more than 32", cost)
	}
	if all-none > perTable*players {
		t.Errorf("naming every player dead costs %d bytes, more than a table per player would have (%d)",
			all-none, perTable*players)
	}
	if perTable*players <= 0 {
		t.Fatal("the table-per-player shape measured as free, so the comparison above proves nothing")
	}
}

// ---------------------------------------------------------------------------
// Protocol V6 — the world's clock
// ---------------------------------------------------------------------------

// Both halves of the clock ride in the snapshot and survive the encoder unchanged,
// including the last tick of a day. The protocol layer lays the pair out; the
// session-aware client, which also has the announced day length, validates its modulo.
func TestASnapshotCarriesTheAbsoluteAndWrappedClock(t *testing.T) {
	t.Parallel()

	for _, tickOfDay := range []uint32{0, 1, 14400, 23999} {
		worldTick := 17*uint64(24_000) + uint64(tickOfDay)
		env := vnet.GetRootAsEnvelope(EncodeEntitySnapshot(EntitySnapshot{
			Tick: 7, TickOfDay: tickOfDay, WorldTick: worldTick,
		}), 0)
		tbl := payloadTable(t, env)
		snapshot := new(vnet.EntitySnapshot)
		snapshot.Init(tbl.Bytes, tbl.Pos)

		if got := snapshot.TickOfDay(); got != tickOfDay {
			t.Errorf("TickOfDay = %d, want %d", got, tickOfDay)
		}
		if got := snapshot.WorldTick(); got != worldTick {
			t.Errorf("WorldTick = %d, want %d", got, worldTick)
		}
		// The field it was appended after, read in the same breath: an appended scalar
		// that displaced an existing one would pass the assertion above and still have
		// broken the contract.
		if got := snapshot.ServerTick(); got != 7 {
			t.Errorf("ServerTick = %d, want 7 — TickOfDay was appended, not inserted", got)
		}
	}
}

// A snapshot from a server with no clock carries zero, which is the same value a
// pre-V6 encoder produced by having no such field at all.
//
// That equivalence is the whole reason the zero means what it means: FlatBuffers writes
// no bytes for a scalar equal to its default, so "the server keeps no clock" and "this
// build predates the clock" are byte-identical on the wire, and a receiver that treats
// them the same is not making an assumption — it is reading the only thing there is.
func TestASnapshotWithNoClockCarriesZero(t *testing.T) {
	t.Parallel()

	env := vnet.GetRootAsEnvelope(EncodeEntitySnapshot(EntitySnapshot{Tick: 1}), 0)
	tbl := payloadTable(t, env)
	snapshot := new(vnet.EntitySnapshot)
	snapshot.Init(tbl.Bytes, tbl.Pos)

	if got := snapshot.TickOfDay(); got != 0 {
		t.Errorf("TickOfDay = %d on a clock-less snapshot, want 0", got)
	}
	if got := snapshot.WorldTick(); got != 0 {
		t.Errorf("WorldTick = %d on a clock-less snapshot, want 0", got)
	}
}

// ---------------------------------------------------------------------------
// Protocol V4 — crafting
// ---------------------------------------------------------------------------

// A craft intent round trips through the encoder this package owns, and it carries a
// recipe and an ordering tick and nothing else. The absence is the design: a message with
// an ingredient list on it would be a client stating what it is spending.
func TestACraftRequestRoundTripsAndNamesOnlyARecipe(t *testing.T) {
	t.Parallel()

	want := CraftRequest{Recipe: vnet.RecipeIDIronSword, ClientTick: 909}
	msg, err := Decode(EncodeCraftRequest(want))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Kind != vnet.PayloadCraftRequest {
		t.Fatalf("Kind = %s, want CraftRequest", msg.Kind)
	}
	if msg.Craft == nil {
		t.Fatal("CraftRequest payload is nil")
	}
	if *msg.Craft != want {
		t.Errorf("decoded %+v, want %+v", *msg.Craft, want)
	}

	// Two fields on the wire, measured the way the attack request's are: a vtable is a
	// uint16 count plus a uint16 per field, so the field count is (size - 4) / 2. Its one
	// blind spot is the same — a trailing default-valued field is truncated out of a
	// vtable — and a field added and *used* is exactly what this is for.
	env := vnet.GetRootAsEnvelope(EncodeCraftRequest(want), 0)
	tbl := payloadTable(t, env)
	vtableOffset := flatbuffers.UOffsetT(flatbuffers.SOffsetT(tbl.Pos) - flatbuffers.GetSOffsetT(tbl.Bytes[tbl.Pos:]))
	vtableSize := flatbuffers.GetVOffsetT(tbl.Bytes[vtableOffset:])
	if fields := (int(vtableSize) - 4) / 2; fields != 2 {
		t.Errorf("CraftRequest has %d fields on the wire, want exactly recipe and client_tick", fields)
	}
}

// `Unknown` and a value no member has both reach the simulation as values to refuse. The
// zero is the load-bearing one: FlatBuffers decodes an absent scalar as it, so a request
// that names no recipe must not arrive looking like one that names the first.
func TestAnUnknownRecipeIsCarriedRatherThanRejected(t *testing.T) {
	t.Parallel()

	for _, recipe := range []vnet.RecipeID{vnet.RecipeIDUnknown, vnet.RecipeID(200)} {
		msg, err := Decode(EncodeCraftRequest(CraftRequest{Recipe: recipe, ClientTick: 1}))
		if err != nil {
			t.Fatalf("Decode: %v", err)
		}
		if msg.Craft == nil {
			t.Fatal("CraftRequest payload is nil")
		}
		if got := msg.Craft.Recipe; got != recipe {
			t.Errorf("Recipe = %s, want %s carried through verbatim", got, recipe)
		}
	}
}

// The truncation and corruption sweep for the payload tag 16 adds. Decode must stay total
// over bytes a client chose: an error or a message, never a panic.
func TestDecodeIsTotalOverADamagedCraftRequest(t *testing.T) {
	t.Parallel()

	valid := EncodeCraftRequest(CraftRequest{Recipe: vnet.RecipeIDForge, ClientTick: 12})

	for i := range len(valid) {
		if _, err := Decode(valid[:i]); err == nil {
			t.Errorf("a %d-byte prefix of a %d-byte craft frame decoded successfully", i, len(valid))
		}
	}
	for i := range len(valid) {
		damaged := bytes.Clone(valid)
		damaged[i] ^= 0xFF
		_, _ = Decode(damaged)
	}
}

// ---------------------------------------------------------------------------
// Protocol V4 — field repair
// ---------------------------------------------------------------------------

// A repair intent round trips through the encoder this package owns, and it carries two
// slot indexes and an ordering tick and nothing else. The absence is the design: a message
// with a durability on it would be a repair granted by asking for one.
func TestARepairRequestRoundTripsAndNamesOnlyTwoSlots(t *testing.T) {
	t.Parallel()

	want := RepairRequest{KitSlot: 3, TargetSlot: 0, ClientTick: 909}
	msg, err := Decode(EncodeRepairRequest(want))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Kind != vnet.PayloadRepairRequest {
		t.Fatalf("Kind = %s, want RepairRequest", msg.Kind)
	}
	if msg.Repair == nil {
		t.Fatal("RepairRequest payload is nil")
	}
	if *msg.Repair != want {
		t.Errorf("decoded %+v, want %+v", *msg.Repair, want)
	}

	// Three fields on the wire, measured the way the craft request's two are: a vtable is
	// a uint16 count plus a uint16 per field, so the field count is (size - 4) / 2.
	env := vnet.GetRootAsEnvelope(EncodeRepairRequest(want), 0)
	tbl := payloadTable(t, env)
	vtableOffset := flatbuffers.UOffsetT(flatbuffers.SOffsetT(tbl.Pos) - flatbuffers.GetSOffsetT(tbl.Bytes[tbl.Pos:]))
	vtableSize := flatbuffers.GetVOffsetT(tbl.Bytes[vtableOffset:])
	if fields := (int(vtableSize) - 4) / 2; fields != 3 {
		t.Errorf("RepairRequest has %d fields on the wire, want kit_slot, target_slot and client_tick", fields)
	}
}

// The two shapes an InventoryMoveRequest is refused for are carried here instead, and the
// asymmetry is deliberate: a move names slots this package indexes with, so it bounds them
// before anything reads an array. A repair names slots the *simulation* looks up against
// the player's own pack, where an index past the end and one slot named twice are ordinary
// refusals rather than a frame that lies about itself. Rejecting them here would close a
// connection whose framing is perfectly readable.
func TestARepairRequestCarriesSlotsTheSimulationHasToRefuse(t *testing.T) {
	t.Parallel()

	for _, want := range []RepairRequest{
		{KitSlot: InventorySlots, TargetSlot: 0, ClientTick: 1},
		{KitSlot: 0, TargetSlot: InventorySlots, ClientTick: 2},
		{KitSlot: 255, TargetSlot: 255, ClientTick: 3},
		{KitSlot: 4, TargetSlot: 4, ClientTick: 4},
	} {
		msg, err := Decode(EncodeRepairRequest(want))
		if err != nil {
			t.Fatalf("Decode of %+v: %v", want, err)
		}
		if msg.Repair == nil {
			t.Fatalf("RepairRequest payload is nil for %+v", want)
		}
		if *msg.Repair != want {
			t.Errorf("decoded %+v, want %+v carried through verbatim", *msg.Repair, want)
		}
	}
}

// The truncation and corruption sweep for the payload tag 17 adds. Decode must stay total
// over bytes a client chose: an error or a message, never a panic.
func TestDecodeIsTotalOverADamagedRepairRequest(t *testing.T) {
	t.Parallel()

	valid := EncodeRepairRequest(RepairRequest{KitSlot: 1, TargetSlot: 2, ClientTick: 12})

	for i := range len(valid) {
		if _, err := Decode(valid[:i]); err == nil {
			t.Errorf("a %d-byte prefix of a %d-byte repair frame decoded successfully", i, len(valid))
		}
	}
	for i := range len(valid) {
		damaged := bytes.Clone(valid)
		damaged[i] ^= 0xFF
		_, _ = Decode(damaged)
	}
}

// ---------------------------------------------------------------------------
// Protocol V15 — consumption and hunger
// ---------------------------------------------------------------------------

// A consume intent round trips through the encoder this package owns and carries only
// the authoritative slot plus the client's ordering tick. Hunger restoration and item
// identity are deliberately absent: both are server-owned registry facts.
func TestAConsumeRequestRoundTripsAndNamesOnlyOneSlot(t *testing.T) {
	t.Parallel()

	want := ConsumeRequest{Slot: 35, ClientTick: 909}
	msg, err := Decode(EncodeConsumeRequest(want))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Kind != vnet.PayloadConsumeRequest {
		t.Fatalf("Kind = %s, want ConsumeRequest", msg.Kind)
	}
	if msg.Consume == nil {
		t.Fatal("ConsumeRequest payload is nil")
	}
	if *msg.Consume != want {
		t.Errorf("decoded %+v, want %+v", *msg.Consume, want)
	}

	env := vnet.GetRootAsEnvelope(EncodeConsumeRequest(want), 0)
	tbl := payloadTable(t, env)
	vtableOffset := flatbuffers.UOffsetT(flatbuffers.SOffsetT(tbl.Pos) - flatbuffers.GetSOffsetT(tbl.Bytes[tbl.Pos:]))
	vtableSize := flatbuffers.GetVOffsetT(tbl.Bytes[vtableOffset:])
	if fields := (int(vtableSize) - 4) / 2; fields != 2 {
		t.Errorf("ConsumeRequest has %d fields on the wire, want slot and client_tick", fields)
	}
}

// A uint16 slot outside the announced inventory is valid framing. It reaches the
// simulation whole so gameplay can refuse it without ending the session.
func TestAConsumeRequestCarriesSlotsTheSimulationHasToRefuse(t *testing.T) {
	t.Parallel()

	for _, slot := range []uint16{uint16(InventorySlots), 255, 256, 65_535} {
		want := ConsumeRequest{Slot: slot, ClientTick: uint32(slot) + 1}
		msg, err := Decode(EncodeConsumeRequest(want))
		if err != nil {
			t.Fatalf("Decode of %+v: %v", want, err)
		}
		if msg.Consume == nil || *msg.Consume != want {
			t.Errorf("decoded %+v, want %+v carried through verbatim", msg.Consume, want)
		}
	}
}

// The truncation and corruption sweep for payload tag 28. Decode must stay total over
// bytes a client chose: an error or a message, never a panic.
func TestDecodeIsTotalOverADamagedConsumeRequest(t *testing.T) {
	t.Parallel()

	valid := EncodeConsumeRequest(ConsumeRequest{Slot: 2, ClientTick: 77})
	for i := range len(valid) {
		if _, err := Decode(valid[:i]); err == nil {
			t.Errorf("a %d-byte prefix of a %d-byte consume frame decoded successfully", i, len(valid))
		}
	}
	for i := range len(valid) {
		damaged := bytes.Clone(valid)
		damaged[i] ^= 0xFF
		_, _ = Decode(damaged)
	}
}

// ---------------------------------------------------------------------------
// Protocol V7 — the session ticket, the character phase, and the face
// ---------------------------------------------------------------------------

// anAppearance is a complete, legal-looking appearance for the round-trip tests.
// Every field is distinct so a transposition shows up as a wrong value rather than
// as an equal one.
func anAppearance() Appearance {
	return Appearance{
		SkinColor:     0x00E3C4A0,
		ShirtColor:    0x004A5D3B,
		TrousersColor: 0x002B2118,
		ShoesColor:    0x00553311,
		HairModel:     vnet.HairModelBraided,
		HairColor:     0x00B07A32,
	}
}

// TestClientHelloRoundTripsATicketOfAnyLength is the ticket's half of
// TestClientHelloRoundTripsATokenOfAnyLength, and it is deliberately the same shape.
//
// The contract says a session_ticket is absent, empty or exactly SessionTicketLen
// bytes and that anything else is RejectReason.BAD_REQUEST. That is a refusal with a
// *reply*, so it belongs to the handshake and not to this package: a decoder that
// shortened it to an error would close the connection with nothing said.
// session.Identities.Resolve is where the length is judged, and
// TestAWrongLengthTicketIsRefusedBeforeAnythingIsLookedUp is where that is pinned.
func TestClientHelloRoundTripsATicketOfAnyLength(t *testing.T) {
	t.Parallel()

	sizes := map[string]int{
		"no ticket at all": 0,
		"a whole ticket":   SessionTicketLen,
		// The lengths the handshake refuses. They have to decode for that refusal to be
		// reachable at all: a frame that would not parse could only be answered with a
		// closed connection.
		"one byte":              1,
		"one byte short":        SessionTicketLen - 1,
		"one byte too many":     SessionTicketLen + 1,
		"a token-length ticket": 32,
	}

	for name, size := range sizes {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			ticket := make([]byte, size)
			for i := range ticket {
				ticket[i] = byte(i + 1)
			}

			msg, err := Decode(EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", ticket))
			if err != nil {
				t.Fatalf("Decode: %v", err)
			}
			if msg.ClientHello == nil {
				t.Fatal("ClientHello payload is nil")
			}
			if got := msg.ClientHello.SessionTicket; !bytes.Equal(got, ticket) {
				t.Errorf("SessionTicket = %d bytes, want the %d given, byte for byte", len(got), size)
			}
		})
	}
}

// The two ways of saying "I present no account" are the same thing on the wire and
// must stay the same thing here — the rule player_token already follows one field up.
func TestAnAbsentAndAnEmptyTicketAreTheSame(t *testing.T) {
	t.Parallel()

	absent, err := Decode(EncodeClientHello(vnet.ProtocolVersionCurrent, "Eivor"))
	if err != nil {
		t.Fatalf("Decode of a hello with no ticket: %v", err)
	}
	empty, err := Decode(EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", []byte{}))
	if err != nil {
		t.Fatalf("Decode of a hello with an empty ticket: %v", err)
	}

	if len(absent.ClientHello.SessionTicket) != 0 || len(empty.ClientHello.SessionTicket) != 0 {
		t.Errorf("absent is %d bytes and empty is %d; both must be zero-length",
			len(absent.ClientHello.SessionTicket), len(empty.ClientHello.SessionTicket))
	}
}

// A hello may carry both fields, and the two must not be confused for one another: a
// V6 peer writes only the token, a V7 peer writes only the ticket, and a peer in
// between writes both.
func TestAHelloCarriesTheTokenAndTheTicketApart(t *testing.T) {
	t.Parallel()

	token := bytes.Repeat([]byte{0xA1}, 32)
	ticket := bytes.Repeat([]byte{0xB2}, SessionTicketLen)

	msg, err := Decode(EncodeClientHelloFull(vnet.ProtocolVersionCurrent, "Eivor", token, ticket))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if got := msg.ClientHello.PlayerToken; !bytes.Equal(got, token) {
		t.Errorf("PlayerToken = %d bytes, want the 32 given", len(got))
	}
	if got := msg.ClientHello.SessionTicket; !bytes.Equal(got, ticket) {
		t.Errorf("SessionTicket = %d bytes, want the %d given", len(got), SessionTicketLen)
	}
}

// The decoded ticket must not be a view over the frame, for the reason the token must
// not be: Decode is the one place untrusted bytes are read, and a live view handed to
// a caller moves the recover it depends on away from the code that needs it.
func TestTheDecodedTicketDoesNotAliasTheFrame(t *testing.T) {
	t.Parallel()

	ticket := make([]byte, SessionTicketLen)
	for i := range ticket {
		ticket[i] = byte(i)
	}
	frame := EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", ticket)

	msg, err := Decode(frame)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	decoded := msg.ClientHello.SessionTicket

	for i := range frame {
		frame[i] = 0
	}
	if !bytes.Equal(decoded, ticket) {
		t.Error("the decoded ticket changed when the frame was overwritten; it aliases the buffer")
	}
}

// V7's members sit where they were appended and the enums they were appended to did
// not move. The value is an integer on the wire: a renumbered RejectReason relabels
// every refusal already written to a log, and a renumbered HairModel puts a different
// head on every character already stored.
func TestV7AppendsWithoutMovingWhatCameBefore(t *testing.T) {
	t.Parallel()

	for name, pair := range map[string][2]byte{
		// The three the contract had before V7, restated so a renumbering fails here.
		"RejectReason.PROTOCOL_MISMATCH": {byte(vnet.RejectReasonPROTOCOL_MISMATCH), 0},
		"RejectReason.SERVER_FULL":       {byte(vnet.RejectReasonSERVER_FULL), 1},
		"RejectReason.BAD_REQUEST":       {byte(vnet.RejectReasonBAD_REQUEST), 2},
		"RejectReason.ALREADY_CONNECTED": {byte(vnet.RejectReasonALREADY_CONNECTED), 3},
		// Appended after ALREADY_CONNECTED = 3.
		"RejectReason.CHARACTER_NAME_TAKEN":    {byte(vnet.RejectReasonCHARACTER_NAME_TAKEN), 4},
		"RejectReason.CHARACTER_NAME_REFUSED":  {byte(vnet.RejectReasonCHARACTER_NAME_REFUSED), 5},
		"RejectReason.CHARACTER_LIMIT_REACHED": {byte(vnet.RejectReasonCHARACTER_LIMIT_REACHED), 6},
		// New in V7, and the zero member is the one that matters: an Appearance with no
		// hair model must fail closed rather than read as a head somebody chose.
		"HairModel.Unknown": {byte(vnet.HairModelUnknown), 0},
		"HairModel.Shaved":  {byte(vnet.HairModelShaved), 1},
		"HairModel.Cropped": {byte(vnet.HairModelCropped), 2},
		"HairModel.Braided": {byte(vnet.HairModelBraided), 3},
		"HairModel.Loose":   {byte(vnet.HairModelLoose), 4},
		"HairModel.Topknot": {byte(vnet.HairModelTopknot), 5},
	} {
		if pair[0] != pair[1] {
			t.Errorf("%s = %d, want %d", name, pair[0], pair[1])
		}
	}

	// Membership, in the shape the Payload union is checked in: a member added without
	// a decision fails here rather than reaching the wire.
	for name, pair := range map[string][2]int{
		"RejectReason": {len(vnet.EnumNamesRejectReason), 7},
		"HairModel":    {len(vnet.EnumNamesHairModel), 6},
	} {
		if pair[0] != pair[1] {
			t.Errorf("%s has %d members, want %d — a new one needs a decision, not a test edit", name, pair[0], pair[1])
		}
	}
}

// EntityState is a struct, so its size is the stride of the entity array in every
// snapshot — the most frequently sent payload in the game. V7 gave every player an
// appearance and put none of it here; this is what catches somebody quietly adding a
// field later, which a FlatBuffers struct can never take back.
//
// Measured from the encoded frame rather than read from a constant: the stride is
// baked into the generated accessor, so two adjacent elements are the only place the
// number can be observed rather than restated.
func TestEntityStateIsStillFortyBytesOnTheWire(t *testing.T) {
	t.Parallel()

	frame := EncodeEntitySnapshot(EntitySnapshot{
		Tick: 1,
		Entities: []EntityState{
			{EntityID: 1, Pos: [3]float32{1, 2, 3}},
			{EntityID: 2, Pos: [3]float32{4, 5, 6}},
		},
		Vitals: PlayerVitals{Health: 10, MaxHealth: 10, LifeState: vnet.LifeStateAlive},
	})

	env := vnet.GetRootAsEnvelope(frame, 0)
	table := payloadTable(t, env)
	var snapshot vnet.EntitySnapshot
	snapshot.Init(table.Bytes, table.Pos)

	var first, second vnet.EntityState
	if !snapshot.Entities(&first, 0) || !snapshot.Entities(&second, 1) {
		t.Fatal("the snapshot does not carry the two entities it was given")
	}
	if got := int(second.Table().Pos - first.Table().Pos); got != 40 {
		t.Errorf("EntityState is %d bytes inlined, want 40 — appearance belongs in PlayerAppearance, not here", got)
	}
}

// ---------------------------------------------------------------------------
// V7 — the character phase
// ---------------------------------------------------------------------------

func TestServerCharacterListRoundTripsEveryCharacter(t *testing.T) {
	t.Parallel()

	want := CharacterList{
		Characters: []CharacterSummary{
			{CharacterID: 900, Name: "Eivor", Appearance: anAppearance()},
			{CharacterID: 7, Name: "Sigrún", Appearance: Appearance{
				SkinColor: 1, ShirtColor: 2, TrousersColor: 3, ShoesColor: 4,
				HairModel: vnet.HairModelShaved, HairColor: 5,
			}},
		},
		MaxCharacters: 5,
	}

	frame := EncodeServerCharacterList(want)
	env := vnet.GetRootAsEnvelope(frame, 0)
	if got := env.PayloadType(); got != vnet.PayloadServerCharacterList {
		t.Fatalf("PayloadType = %s, want %s", got, vnet.PayloadServerCharacterList)
	}
	table := payloadTable(t, env)
	var list vnet.ServerCharacterList
	list.Init(table.Bytes, table.Pos)

	if got := int(list.MaxCharacters()); got != int(want.MaxCharacters) {
		t.Errorf("MaxCharacters = %d, want %d", got, want.MaxCharacters)
	}
	if got := list.CharactersLength(); got != len(want.Characters) {
		t.Fatalf("CharactersLength = %d, want %d", got, len(want.Characters))
	}

	// In order: the server chose the order and the wire must not reshuffle it.
	for i, character := range want.Characters {
		var summary vnet.CharacterSummary
		if !list.Characters(&summary, i) {
			t.Fatalf("character %d is absent", i)
		}
		if got := summary.CharacterId(); got != character.CharacterID {
			t.Errorf("character %d id = %d, want %d", i, got, character.CharacterID)
		}
		if got := string(summary.Name()); got != character.Name {
			t.Errorf("character %d name = %q, want %q", i, got, character.Name)
		}
		appearance := summary.Appearance(nil)
		if appearance == nil {
			t.Fatalf("character %d carries no appearance", i)
		}
		if got := decodeAppearance(appearance); got != character.Appearance {
			t.Errorf("character %d appearance = %+v, want %+v", i, got, character.Appearance)
		}
	}
}

// An account with no characters here is a legal and expected answer, not a refusal:
// it says the only way forward is a CreateCharacterRequest. The vector is present and
// empty rather than absent, so a server's frames have the same shape either way.
func TestAnEmptyCharacterListIsStillACharacterList(t *testing.T) {
	t.Parallel()

	frame := EncodeServerCharacterList(CharacterList{MaxCharacters: 3})
	env := vnet.GetRootAsEnvelope(frame, 0)
	table := payloadTable(t, env)
	var list vnet.ServerCharacterList
	list.Init(table.Bytes, table.Pos)

	if got := list.CharactersLength(); got != 0 {
		t.Errorf("CharactersLength = %d, want 0", got)
	}
	// Present and empty rather than absent: the field offset is there, so a reader
	// that asks for the vector gets one of length zero instead of a missing field.
	listTable := list.Table()
	if o := listTable.Offset(4); o == 0 {
		t.Error("the characters vector was omitted; an empty list still carries one")
	}
	if got := int(list.MaxCharacters()); got != 3 {
		t.Errorf("MaxCharacters = %d, want 3", got)
	}
}

func TestPlayerAppearanceCarriesTheEntityFaceNameLevelAndWornItems(t *testing.T) {
	t.Parallel()

	want := PlayerAppearance{
		EntityID:      4242,
		Appearance:    anAppearance(),
		Name:          "Brynhildr",
		Level:         7,
		WornHead:      101,
		WornChest:     102,
		WornLegs:      103,
		WornOffHand:   104,
		HasAppearance: true,
		HasName:       true,
	}

	frame := EncodePlayerAppearance(want)
	env := vnet.GetRootAsEnvelope(frame, 0)
	if got := env.PayloadType(); got != vnet.PayloadPlayerAppearance {
		t.Fatalf("PayloadType = %s, want %s", got, vnet.PayloadPlayerAppearance)
	}
	table := payloadTable(t, env)
	var payload vnet.PlayerAppearance
	payload.Init(table.Bytes, table.Pos)

	if got := payload.EntityId(); got != want.EntityID {
		t.Errorf("EntityId = %d, want %d", got, want.EntityID)
	}
	appearance := payload.Appearance(nil)
	if appearance == nil {
		t.Fatal("PlayerAppearance carries no appearance")
	}
	if got := decodeAppearance(appearance); got != want.Appearance {
		t.Errorf("Appearance = %+v, want %+v", got, want.Appearance)
	}
	if got := string(payload.Name()); got != want.Name {
		t.Errorf("Name = %q, want %q", got, want.Name)
	}
	if got := payload.Level(); got != want.Level {
		t.Errorf("Level = %d, want %d", got, want.Level)
	}
	if got := payload.WornHead(); got != want.WornHead {
		t.Errorf("WornHead = %d, want %d", got, want.WornHead)
	}
	if got := payload.WornChest(); got != want.WornChest {
		t.Errorf("WornChest = %d, want %d", got, want.WornChest)
	}
	if got := payload.WornLegs(); got != want.WornLegs {
		t.Errorf("WornLegs = %d, want %d", got, want.WornLegs)
	}
	if got := payload.WornOffhand(); got != want.WornOffHand {
		t.Errorf("WornOffhand = %d, want %d", got, want.WornOffHand)
	}
}

// The encoder honours HasAppearance so a test can build the frame a client has to
// refuse. An absent appearance reads as a null table, never as an appearance of zeros
// — the same reasoning that keeps an absent BlockCoord from reading as the origin.
func TestAPlayerAppearanceWithNoAppearanceIsAbsentRatherThanBlack(t *testing.T) {
	t.Parallel()

	frame := EncodePlayerAppearance(PlayerAppearance{EntityID: 1})
	env := vnet.GetRootAsEnvelope(frame, 0)
	table := payloadTable(t, env)
	var payload vnet.PlayerAppearance
	payload.Init(table.Bytes, table.Pos)

	if got := payload.Appearance(nil); got != nil {
		t.Error("an omitted appearance came back as a table")
	}
}

// Present-empty and absent are different contract values: an empty name remains display
// text, while an absent name is a pre-V13 description the client refuses.
func TestAPlayerAppearanceCanCarryAnEmptyNameWithoutOmittingIt(t *testing.T) {
	t.Parallel()

	present := EncodePlayerAppearance(PlayerAppearance{EntityID: 1, HasName: true})
	presentTable := payloadTable(t, vnet.GetRootAsEnvelope(present, 0))
	var presentPayload vnet.PlayerAppearance
	presentPayload.Init(presentTable.Bytes, presentTable.Pos)
	if got := presentPayload.Name(); got == nil || string(got) != "" {
		t.Fatalf("present empty name = %q, want a present empty string", got)
	}

	absent := EncodePlayerAppearance(PlayerAppearance{EntityID: 1})
	absentTable := payloadTable(t, vnet.GetRootAsEnvelope(absent, 0))
	var absentPayload vnet.PlayerAppearance
	absentPayload.Init(absentTable.Bytes, absentTable.Pos)
	if got := absentPayload.Name(); got != nil {
		t.Fatalf("omitted name = %q, want nil", got)
	}
}

func TestSelectCharacterRequestRoundTripsItsID(t *testing.T) {
	t.Parallel()

	// Ids the handshake has to refuse are round-tripped too: zero names no character
	// anywhere in this contract, and a refusal that cannot be built cannot be tested.
	for name, id := range map[string]uint64{
		"a character the account owns": 900,
		"the reserved zero":            0,
		"an id nobody has":             1 << 62,
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			msg, err := Decode(EncodeSelectCharacterRequest(SelectCharacterRequest{CharacterID: id}))
			if err != nil {
				t.Fatalf("Decode: %v", err)
			}
			if msg.Kind != vnet.PayloadSelectCharacterRequest {
				t.Fatalf("Kind = %s, want %s", msg.Kind, vnet.PayloadSelectCharacterRequest)
			}
			if msg.SelectCharacter == nil {
				t.Fatal("SelectCharacterRequest payload is nil")
			}
			if got := msg.SelectCharacter.CharacterID; got != id {
				t.Errorf("CharacterID = %d, want %d", got, id)
			}
		})
	}
}

func TestCreateCharacterRequestRoundTripsANameAndAFace(t *testing.T) {
	t.Parallel()

	want := CreateCharacterRequest{Name: "Sigrún", Appearance: anAppearance(), HasAppearance: true}

	msg, err := Decode(EncodeCreateCharacterRequest(want))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Kind != vnet.PayloadCreateCharacterRequest {
		t.Fatalf("Kind = %s, want %s", msg.Kind, vnet.PayloadCreateCharacterRequest)
	}
	if msg.CreateCharacter == nil {
		t.Fatal("CreateCharacterRequest payload is nil")
	}
	if got := *msg.CreateCharacter; got != want {
		t.Errorf("CreateCharacterRequest = %+v, want %+v", got, want)
	}
}

// A name is a decision and an appearance is a framing question, and the difference is
// the whole of the split schemas/handshake.fbs documents.
//
// A name the server will not accept — the empty string included — is carried through
// so the handshake can answer CHARACTER_NAME_REFUSED, which is a refusal with a reply.
// An absent appearance is a request that failed to say what it is asking for, and
// there is no reply that could make sense of it.
func TestACreateCharacterRequestCarriesNamesTheHandshakeMustRefuse(t *testing.T) {
	t.Parallel()

	for name, value := range map[string]string{
		"the empty name":   "",
		"a name of spaces": "   ",
		"a very long name": string(bytes.Repeat([]byte("a"), 4096)),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			msg, err := Decode(EncodeCreateCharacterRequest(CreateCharacterRequest{
				Name: value, Appearance: anAppearance(), HasAppearance: true,
			}))
			if err != nil {
				t.Fatalf("Decode: %v", err)
			}
			if got := msg.CreateCharacter.Name; got != value {
				t.Errorf("Name = %q, want %q", got, value)
			}
		})
	}
}

func TestACreateCharacterRequestWithoutAnAppearanceIsMalformed(t *testing.T) {
	t.Parallel()

	frame := EncodeCreateCharacterRequest(CreateCharacterRequest{Name: "Eivor"})
	if _, err := Decode(frame); !errors.Is(err, ErrMalformed) {
		t.Errorf("Decode returned %v, want an error wrapping ErrMalformed", err)
	}
}

// An appearance carrying values the contract forbids is carried rather than refused —
// the division of labour every other enum-bearing request in this package follows. A
// reserved high byte and an Unknown hair model are decisions, and a decoder that
// refused them would close a connection whose framing is perfectly readable.
func TestForbiddenAppearanceValuesAreCarriedRatherThanRejected(t *testing.T) {
	t.Parallel()

	want := Appearance{
		SkinColor: 0xFF000000, ShirtColor: 0xDEADBEEF,
		HairModel: vnet.HairModel(200), HairColor: 0x80FFFFFF,
	}

	msg, err := Decode(EncodeCreateCharacterRequest(CreateCharacterRequest{
		Name: "Eivor", Appearance: want, HasAppearance: true,
	}))
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if got := msg.CreateCharacter.Appearance; got != want {
		t.Errorf("Appearance = %+v, want %+v", got, want)
	}
}

// The rule the test above leaves to somebody else, and this is that somebody: Validate
// is what a caller asks before an appearance is stored or sent, and it is deliberately
// not what Decode asks.
func TestValidateAnswersTheContractsInvariants(t *testing.T) {
	t.Parallel()

	t.Run("an appearance the contract allows", func(t *testing.T) {
		t.Parallel()

		// Every hair member but Unknown, against colours at the very top of the room a
		// colour has: 0x00FFFFFF is the largest legal value and 0x01000000 the smallest
		// illegal one, so this is the boundary rather than a value near it.
		for model, name := range vnet.EnumNamesHairModel {
			if model == vnet.HairModelUnknown {
				continue
			}
			worn := Appearance{
				SkinColor: ColorChannels, ShirtColor: ColorChannels,
				TrousersColor: ColorChannels, ShoesColor: ColorChannels,
				HairColor: ColorChannels, HairModel: model,
			}
			if err := worn.Validate(); err != nil {
				t.Errorf("a character in %s was refused: %v", name, err)
			}
		}
	})

	t.Run("a colour outside the channels", func(t *testing.T) {
		t.Parallel()

		// One subtest per field, because a check written over four of the five would
		// pass every test that only ever set the first.
		for what, set := range map[string]func(*Appearance){
			"skin":     func(a *Appearance) { a.SkinColor = ColorChannels + 1 },
			"shirt":    func(a *Appearance) { a.ShirtColor = 0xFF000000 },
			"trousers": func(a *Appearance) { a.TrousersColor = 0x01000000 },
			"shoes":    func(a *Appearance) { a.ShoesColor = 0x80FFFFFF },
			"hair":     func(a *Appearance) { a.HairColor = 0xFFFFFFFF },
		} {
			worn := anAppearance()
			set(&worn)
			if err := worn.Validate(); !errors.Is(err, ErrAppearance) {
				t.Errorf("a %s colour with the reserved high byte set was answered %v, want ErrAppearance", what, err)
			}
		}
	})

	t.Run("a hair model that is not a choice", func(t *testing.T) {
		t.Parallel()

		// Unknown is the absent-field value rather than a shaved head, and 200 is a
		// contract this build does not speak. Both fail closed; a colour has no such
		// spare value, which is why absence is not on this list.
		for _, model := range []vnet.HairModel{vnet.HairModelUnknown, vnet.HairModel(200)} {
			worn := anAppearance()
			worn.HairModel = model
			if err := worn.Validate(); !errors.Is(err, ErrAppearance) {
				t.Errorf("hair model %d was answered %v, want ErrAppearance", model, err)
			}
		}
	})

	t.Run("black is a colour somebody chose", func(t *testing.T) {
		t.Parallel()

		// The one case that must *not* be refused, and the reason schemas/common.fbs
		// writes the rule as a prohibition: a table scalar carries no presence bit, so
		// an absent colour and a chosen black are the same bytes on the wire. Refusing
		// absence would refuse a character wearing black shoes — and would make decode
		// correctness depend on the sender's builder settings.
		worn := Appearance{HairModel: vnet.HairModelShaved}
		if err := worn.Validate(); err != nil {
			t.Errorf("a character dressed entirely in black was refused: %v", err)
		}
	})
}

// A union tag naming a payload the envelope does not carry is a frame that lies about
// itself, and V7's two client payloads are no exception.
func TestTheCharacterRequestTagsWithoutPayloadsAreMalformed(t *testing.T) {
	t.Parallel()

	for _, kind := range []vnet.Payload{vnet.PayloadSelectCharacterRequest, vnet.PayloadCreateCharacterRequest} {
		t.Run(kind.String(), func(t *testing.T) {
			t.Parallel()
			b := flatbuffers.NewBuilder(64)
			vnet.EnvelopeStart(b)
			vnet.EnvelopeAddPayloadType(b, kind)
			env := vnet.EnvelopeEnd(b)
			vnet.FinishEnvelopeBuffer(b, env)
			if _, err := Decode(b.FinishedBytes()); !errors.Is(err, ErrMalformed) {
				t.Errorf("Decode returned %v, want an error wrapping ErrMalformed", err)
			}
		})
	}
}

// Decode is total over damage anywhere in the two character requests: every
// truncation and every flipped byte either decodes or errors, and none panics.
func TestDecodeIsTotalOverDamagedCharacterRequests(t *testing.T) {
	t.Parallel()

	frames := map[string][]byte{
		"SelectCharacterRequest": EncodeSelectCharacterRequest(SelectCharacterRequest{CharacterID: 900}),
		"CreateCharacterRequest": EncodeCreateCharacterRequest(CreateCharacterRequest{
			Name: "Eivor", Appearance: anAppearance(), HasAppearance: true,
		}),
	}

	for name, frame := range frames {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			for cut := range frame {
				_, _ = Decode(frame[:cut])
			}
			for i := range frame {
				damaged := bytes.Clone(frame)
				damaged[i] ^= 0xFF
				_, _ = Decode(damaged)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// Protocol V8 — putting something back on the ground
// ---------------------------------------------------------------------------

// A drop intent round trips through the encoder this package owns, and it carries one slot
// index and an ordering tick and nothing else.
//
// The absence is the design, exactly as it is for a repair. A count would let a client state
// what leaves its own pack; a position would let it put an item down anywhere in the world.
//
// The out-of-range rows are AttackRequest's rule and not InventoryMoveRequest's — the same
// asymmetry the repair test records, and for the same reason: nothing here indexes an array
// with the value.
func TestADropItemRequestRoundTripsAndNamesOnlyASlot(t *testing.T) {
	t.Parallel()

	for _, want := range []DropItemRequest{
		{Slot: 5, ClientTick: 4242},
		{Slot: InventorySlots, ClientTick: 1},
		{Slot: 255, ClientTick: 2},
		{Slot: 0, ClientTick: 0},
	} {
		msg, err := Decode(EncodeDropItemRequest(want))
		if err != nil {
			t.Fatalf("Decode of %+v: %v", want, err)
		}
		if msg.Kind != vnet.PayloadDropItemRequest {
			t.Fatalf("Kind = %s, want DropItemRequest", msg.Kind)
		}
		if msg.DropItem == nil {
			t.Fatalf("DropItemRequest payload is nil for %+v", want)
		}
		if *msg.DropItem != want {
			t.Errorf("decoded %+v, want %+v carried through verbatim", *msg.DropItem, want)
		}
	}

	// Two fields on the wire, measured the way the repair request's three are: a vtable is a
	// uint16 count plus a uint16 per field, so the field count is (size - 4) / 2. Measured on
	// a frame where neither field is zero, because FlatBuffers writes no bytes for a field
	// equal to its default and the all-zero row above has an empty vtable by construction.
	env := vnet.GetRootAsEnvelope(EncodeDropItemRequest(DropItemRequest{Slot: 5, ClientTick: 4242}), 0)
	tbl := payloadTable(t, env)
	vtableOffset := flatbuffers.UOffsetT(flatbuffers.SOffsetT(tbl.Pos) - flatbuffers.GetSOffsetT(tbl.Bytes[tbl.Pos:]))
	vtableSize := flatbuffers.GetVOffsetT(tbl.Bytes[vtableOffset:])
	if fields := (int(vtableSize) - 4) / 2; fields != 2 {
		t.Errorf("DropItemRequest has %d fields on the wire, want slot and client_tick", fields)
	}
}

// The truncation and corruption sweep for the payload tag 25 adds. Decode must stay total
// over bytes a client chose: an error or a message, never a panic.
func TestDecodeIsTotalOverADamagedDropItemRequest(t *testing.T) {
	t.Parallel()

	valid := EncodeDropItemRequest(DropItemRequest{Slot: 7, ClientTick: 31})

	for i := range len(valid) {
		if _, err := Decode(valid[:i]); err == nil {
			t.Errorf("a %d-byte prefix of a %d-byte drop frame decoded successfully", i, len(valid))
		}
	}
	for i := range len(valid) {
		damaged := bytes.Clone(valid)
		damaged[i] ^= 0xFF
		_, _ = Decode(damaged)
	}
}

// ---------------------------------------------------------------------------
// Protocol V26 — the Fimbulvetr's contract
// ---------------------------------------------------------------------------

// The V26 enums, pinned by value and by count. Every number here is a byte on the wire,
// so a renumbering compiles perfectly on both sides and relabels every value already
// sent — the trap RecipeID's own comment records.
//
// WardBound is not a wire enum at all: it is the bound on WardsNearby.columns, stated as
// a constant both generated APIs can read rather than as a paragraph each consumer copies.
func TestV26AppendsWithoutMovingWhatCameBefore(t *testing.T) {
	t.Parallel()

	for name, pair := range map[string][2]byte{
		"WeatherKind.Unknown":   {byte(vnet.WeatherKindUnknown), 0},
		"WeatherKind.Clear":     {byte(vnet.WeatherKindClear), 1},
		"WeatherKind.Rain":      {byte(vnet.WeatherKindRain), 2},
		"WeatherKind.Snow":      {byte(vnet.WeatherKindSnow), 3},
		"WeatherKind.Sandstorm": {byte(vnet.WeatherKindSandstorm), 4},
		"WeatherKind.Blizzard":  {byte(vnet.WeatherKindBlizzard), 5},

		"StormPhase.Unknown":     {byte(vnet.StormPhaseUnknown), 0},
		"StormPhase.Approaching": {byte(vnet.StormPhaseApproaching), 1},
		"StormPhase.Raging":      {byte(vnet.StormPhaseRaging), 2},
		"StormPhase.Passed":      {byte(vnet.StormPhasePassed), 3},

		"WardKind.Unknown":    {byte(vnet.WardKindUnknown), 0},
		"WardKind.Runestone":  {byte(vnet.WardKindRunestone), 1},
		"WardKind.Settlement": {byte(vnet.WardKindSettlement), 2},
	} {
		if pair[0] != pair[1] {
			t.Errorf("%s = %d, want %d", name, pair[0], pair[1])
		}
	}

	for name, pair := range map[string][2]int{
		"WeatherKind": {len(vnet.EnumNamesWeatherKind), 6},
		"StormPhase":  {len(vnet.EnumNamesStormPhase), 4},
		"WardKind":    {len(vnet.EnumNamesWardKind), 3},
	} {
		if pair[0] != pair[1] {
			t.Errorf("%s has %d members, want %d — a new one needs a decision, not a test edit", name, pair[0], pair[1])
		}
	}

	if got := uint16(vnet.WardBoundMaxWardedColumns); got != 2048 {
		t.Errorf("WardBound.MaxWardedColumns = %d, want 2048", got)
	}
}

func TestWardsNearbyCarriesACompleteOrderedReplacementIncludingEmpty(t *testing.T) {
	t.Parallel()

	for name, want := range map[string][]WardedColumn{
		"empty": {},
		"two kinds": {
			{CX: -4, CZ: 2, Kind: vnet.WardKindRunestone, Mine: true},
			{CX: 7, CZ: 9, Kind: vnet.WardKindSettlement, Mine: false},
		},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			frame, err := EncodeWardsNearby(WardsNearby{Columns: want})
			if err != nil {
				t.Fatalf("EncodeWardsNearby: %v", err)
			}
			envelope := vnet.GetRootAsEnvelope(frame, 0)
			if envelope.PayloadType() != vnet.PayloadWardsNearby {
				t.Fatalf("PayloadType = %s, want WardsNearby", envelope.PayloadType())
			}
			table := payloadTable(t, envelope)
			var nearby vnet.WardsNearby
			nearby.Init(table.Bytes, table.Pos)
			if nearby.ColumnsLength() != len(want) {
				t.Fatalf("ColumnsLength = %d, want %d", nearby.ColumnsLength(), len(want))
			}
			for i, expected := range want {
				var got vnet.WardedColumn
				if !nearby.Columns(&got, i) {
					t.Fatalf("column %d is absent", i)
				}
				if got.Cx() != expected.CX || got.Cz() != expected.CZ || got.Kind() != expected.Kind || got.Mine() != expected.Mine {
					t.Errorf("column %d = (%d,%d,%s,%v), want (%d,%d,%s,%v)", i,
						got.Cx(), got.Cz(), got.Kind(), got.Mine(),
						expected.CX, expected.CZ, expected.Kind, expected.Mine)
				}
			}
		})
	}
}

func TestWardsNearbyRefusesMoreThanTheGeneratedBound(t *testing.T) {
	t.Parallel()

	columns := make([]WardedColumn, MaxWardedColumns+1)
	if frame, err := EncodeWardsNearby(WardsNearby{Columns: columns}); err == nil || frame != nil {
		t.Fatalf("EncodeWardsNearby returned %d bytes and %v, want a refusal above %d columns", len(frame), err, MaxWardedColumns)
	}
}

// A storm warning round trips through the generated reader in all three phases, and the
// envelope names it as its own payload rather than as anything already on the wire.
//
// Both fields are read in every phase, because the pair is the message: `seconds_until`
// is a countdown under Approaching, a remaining duration under Raging and a zero under
// Passed, and a phase that decoded to the wrong member would leave the number reading as
// somebody else's storm.
func TestAStormWarningCarriesItsPhaseAndTheSecondsThatBelongToIt(t *testing.T) {
	t.Parallel()

	for name, want := range map[string]StormWarning{
		"approaching": {SecondsUntil: 900, Phase: vnet.StormPhaseApproaching},
		"raging":      {SecondsUntil: 120, Phase: vnet.StormPhaseRaging},
		"passed":      {SecondsUntil: 0, Phase: vnet.StormPhasePassed},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			frame := EncodeStormWarning(want)

			msg, err := Decode(frame)
			if err != nil {
				t.Fatalf("Decode: %v", err)
			}
			if msg.Kind != vnet.PayloadStormWarning {
				t.Fatalf("Kind = %s, want %s", msg.Kind, vnet.PayloadStormWarning)
			}

			env := vnet.GetRootAsEnvelope(frame, 0)
			tbl := payloadTable(t, env)
			warning := new(vnet.StormWarning)
			warning.Init(tbl.Bytes, tbl.Pos)

			got := StormWarning{SecondsUntil: warning.SecondsUntil(), Phase: warning.Phase()}
			if got != want {
				t.Errorf("decoded as %+v, want %+v", got, want)
			}
		})
	}
}

// The encoder builds the two frames the contract tells a client to refuse: a phase that
// is the enum's zero, and a `Passed` that still carries a countdown.
//
// **Refusing them is the client's job and not this package's**, for the reason
// EncodeCreateCharacterRequest builds names the handshake must reject: a decoder test
// needs the bytes, and an encoder that could not produce them would leave the refusal
// path with nothing to prove itself against. What this pins is only that the values
// survive the encoder verbatim.
func TestAStormWarningCarriesTheTwoShapesAClientMustRefuse(t *testing.T) {
	t.Parallel()

	for name, want := range map[string]StormWarning{
		"a phase with no name":     {SecondsUntil: 30, Phase: vnet.StormPhaseUnknown},
		"a passed storm still due": {SecondsUntil: 45, Phase: vnet.StormPhasePassed},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			env := vnet.GetRootAsEnvelope(EncodeStormWarning(want), 0)
			tbl := payloadTable(t, env)
			warning := new(vnet.StormWarning)
			warning.Init(tbl.Bytes, tbl.Pos)

			got := StormWarning{SecondsUntil: warning.SecondsUntil(), Phase: warning.Phase()}
			if got != want {
				t.Errorf("decoded as %+v, want %+v — the encoder repaired a value it was given", got, want)
			}
		})
	}
}

// The weather rides in the snapshot, at the recipient's own position, with nothing else
// displaced. The second half matters as much as the first: an appended field that moved
// an existing one would satisfy every assertion about itself while breaking every frame
// already on the wire.
func TestASnapshotCarriesTheWeatherWhereTheRecipientStands(t *testing.T) {
	t.Parallel()

	for name, want := range map[string]WeatherState{
		"clear carries nothing": {Kind: vnet.WeatherKindClear, Intensity: 0},
		"drizzle":               {Kind: vnet.WeatherKindRain, Intensity: 1},
		"snow at its hardest":   {Kind: vnet.WeatherKindSnow, Intensity: 255},
		"a sandstorm":           {Kind: vnet.WeatherKindSandstorm, Intensity: 200},
		"the storm's own kind":  {Kind: vnet.WeatherKindBlizzard, Intensity: 240},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			env := vnet.GetRootAsEnvelope(EncodeEntitySnapshot(EntitySnapshot{
				Tick:       9,
				TickOfDay:  4321,
				Weather:    want,
				HasWeather: true,
			}), 0)
			tbl := payloadTable(t, env)
			snapshot := new(vnet.EntitySnapshot)
			snapshot.Init(tbl.Bytes, tbl.Pos)

			held := snapshot.Weather(nil)
			if held == nil {
				t.Fatal("a snapshot that states its weather carries no weather struct")
			}
			got := WeatherState{Kind: held.Kind(), Intensity: held.Intensity()}
			if got != want {
				t.Errorf("weather decoded as %+v, want %+v", got, want)
			}

			if got := snapshot.ServerTick(); got != 9 {
				t.Errorf("ServerTick = %d, want 9 — weather was appended, not inserted", got)
			}
			if got := snapshot.TickOfDay(); got != 4321 {
				t.Errorf("TickOfDay = %d, want 4321 — weather was appended, not inserted", got)
			}
		})
	}
}

// A server that keeps no weather sends no struct, and it is HasWeather that decides —
// never the value beside it.
//
// **The two absences are not the same absence, which is the whole reason the flag
// exists.** An omitted struct field decodes as null, and the contract reads that as "this
// server keeps no weather"; the Go zero value written in its place would be a *present*
// struct carrying WeatherKindUnknown, which is a protocol error a client closes the
// session over. The second case here is the one that would catch an encoder that decided
// from the value: a fully populated WeatherState with the flag clear must still put
// nothing on the wire.
func TestASnapshotWithNoWeatherCarriesNoStructAtAll(t *testing.T) {
	t.Parallel()

	for name, snapshot := range map[string]EntitySnapshot{
		"a caller that said nothing about weather": {Tick: 1},
		"a value the flag does not authorise": {
			Tick:    1,
			Weather: WeatherState{Kind: vnet.WeatherKindBlizzard, Intensity: 255},
		},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			env := vnet.GetRootAsEnvelope(EncodeEntitySnapshot(snapshot), 0)
			tbl := payloadTable(t, env)
			decoded := new(vnet.EntitySnapshot)
			decoded.Init(tbl.Bytes, tbl.Pos)

			if held := decoded.Weather(nil); held != nil {
				t.Errorf("a weatherless snapshot carries %s at intensity %d, want no struct at all",
					vnet.EnumNamesWeatherKind[held.Kind()], held.Intensity())
			}
		})
	}
}

// A doused campfire is the only structure that says anything about fire, and every other
// structure in the same snapshot still reads as burning.
//
// The inversion in [StructureState.Doused] is what makes that true for a caller as well
// as for the wire: `lit` defaults to true, Go zeroes a bool to false, and naming the
// absence is what keeps a literal that mentions no fire from encoding a doused one.
func TestADousedCampfireIsTheOnlyStructureThatSaysAnythingAboutFire(t *testing.T) {
	t.Parallel()

	structures := []StructureState{
		{StructureID: 1, Kind: vnet.StructureKindCampfire, Anchor: [3]int32{2, 64, 3}, Facing: vnet.FacingNorth, OwnerEntityID: 7, Doused: true},
		{StructureID: 2, Kind: vnet.StructureKindCampfire, Anchor: [3]int32{9, 64, 3}, Facing: vnet.FacingEast, OwnerEntityID: 7},
		{StructureID: 3, Kind: vnet.StructureKindTent, Anchor: [3]int32{0, 64, 0}, Facing: vnet.FacingSouth, OwnerEntityID: 8},
	}

	env := vnet.GetRootAsEnvelope(EncodeEntitySnapshot(EntitySnapshot{Tick: 3, Structures: structures}), 0)
	tbl := payloadTable(t, env)
	snapshot := new(vnet.EntitySnapshot)
	snapshot.Init(tbl.Bytes, tbl.Pos)

	if got := snapshot.StructuresLength(); got != len(structures) {
		t.Fatalf("StructuresLength = %d, want %d", got, len(structures))
	}
	for i, want := range structures {
		var held vnet.StructureState
		if !snapshot.Structures(&held, i) {
			t.Fatalf("structure %d is missing from a snapshot that claims to hold it", i)
		}
		if got := held.Lit(); got != !want.Doused {
			t.Errorf("structure %d has Lit = %t, want %t", i, got, !want.Doused)
		}
		// Read beside it: `lit` was appended to StructureState, and a field that
		// displaced one of these would pass the assertion above on its own.
		anchor := held.Anchor(nil)
		if anchor == nil {
			t.Fatalf("structure %d carries no anchor", i)
		}
		got := StructureState{
			StructureID:   held.StructureId(),
			Kind:          held.Kind(),
			Anchor:        [3]int32{anchor.X(), anchor.Y(), anchor.Z()},
			Facing:        held.Facing(),
			OwnerEntityID: held.OwnerEntityId(),
			Doused:        !held.Lit(),
		}
		if got != want {
			t.Errorf("structure %d decoded as %+v, want %+v", i, got, want)
		}
	}
}

// The burning fire writes no byte and the doused one does, which is the claim the
// schema's `lit = true` default was chosen for and the only place anything measures it.
//
// **It reads the vtable rather than the frame's length, and that is the whole point.**
// The obvious version of this test — encode both and compare sizes — passes vacuously:
// both frames come to the same number of bytes, because the one byte a doused fire adds
// lands in padding the table was already carrying. What is actually being claimed is
// narrower and is visible only here: a burning fire occupies **no vtable slot** for `lit`,
// so its value is the schema's default showing through rather than anything this encoder
// wrote, and that is what makes a pre-V26 server's silence and a burning fire the same
// bytes.
//
// `lit` is StructureState's sixth field, so its vtable slot is 4 + 2*5 — the same number
// flatc's own accessor reads, and the same arithmetic the vendor vectors above use.
func TestABurningFireOccupiesNoSlotAndADousedOneDoes(t *testing.T) {
	t.Parallel()

	const litVTableSlot = flatbuffers.VOffsetT(14)

	env := vnet.GetRootAsEnvelope(EncodeEntitySnapshot(EntitySnapshot{
		Tick: 3,
		Structures: []StructureState{
			{StructureID: 1, Kind: vnet.StructureKindCampfire, Anchor: [3]int32{2, 64, 3}, Facing: vnet.FacingNorth, OwnerEntityID: 7},
			{StructureID: 2, Kind: vnet.StructureKindCampfire, Anchor: [3]int32{9, 64, 3}, Facing: vnet.FacingEast, OwnerEntityID: 7, Doused: true},
		},
	}), 0)
	tbl := payloadTable(t, env)
	snapshot := new(vnet.EntitySnapshot)
	snapshot.Init(tbl.Bytes, tbl.Pos)

	for i, wantWritten := range []bool{false, true} {
		var held vnet.StructureState
		if !snapshot.Structures(&held, i) {
			t.Fatalf("structure %d is missing from a snapshot that claims to hold it", i)
		}
		structure := held.Table()
		if written := structure.Offset(litVTableSlot) != 0; written != wantWritten {
			t.Errorf("structure %d wrote a lit byte = %t, want %t — a burning fire is supposed to "+
				"cost nothing and ride the schema's default, and a doused one is the case that pays",
				i, written, wantWritten)
		}
		if got := held.Lit(); got == wantWritten {
			t.Errorf("structure %d reads Lit = %t, which is not what the slot above says", i, got)
		}
	}
}
