package protocol

import (
	"bytes"
	"errors"
	"math"
	"math/rand/v2"
	"testing"

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
// **Tag 20 is why the version does not move.** Union members are append-only precisely
// so that appending one is not a break: a peer built against V6 as it shipped reads tag
// 20 as a payload it has no name for and drops it, which costs it the refusal feedback
// and nothing else. Bumping ProtocolVersion.Current for that would refuse every peer
// already on the wire in exchange for a message they were never going to read — so the
// number is asserted here rather than left to whoever edits the list below.
func TestProtocolV7AppendsFourTagsAndMovesToSeven(t *testing.T) {
	t.Parallel()

	if got := uint16(vnet.ProtocolVersionCurrent); got != 7 {
		t.Fatalf("ProtocolVersion.Current = %d, want 7", got)
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
	}
	for index, payload := range want {
		if got := byte(payload); got != byte(index+1) {
			t.Errorf("%s tag = %d, want %d", payload, got, index+1)
		}
	}

	// Membership, not just ordering. A swing is still answered by the next snapshot and
	// nothing else, and so is a craft and a repair; a *refused* placement is answered by
	// ActionRefused, and an accepted one is not — there is still no acknowledgement
	// payload anywhere in this contract, and the size of the union is the only place that
	// claim can be checked. V7's four are the handshake's new phase and the appearance
	// that rides beside it, and none of them acknowledges anything either: a character is
	// chosen and the answer is ServerWelcome. NONE is the implicit zero member every
	// FlatBuffers union carries.
	if got := len(vnet.EnumNamesPayload); got != len(want)+1 {
		t.Errorf("Payload has %d members, want %d plus NONE — a new member needs a decision, not a test edit", got, len(want))
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
		{EntityID: 51, Pos: [3]float32{-4, 12.25, 8}, ItemID: 7, Count: 65_535},
	}

	wantMobs := []MobState{
		{
			EntityID: 900, Kind: vnet.MobKindDraugr,
			Pos: [3]float32{8.5, 64, -12.25}, Vel: [3]float32{0.5, 0, -0.5},
			Yaw: 1.5, Health: 60, MaxHealth: 60, Action: vnet.MobActionChase,
		},
		{
			EntityID: 901, Kind: vnet.MobKindDraugr,
			Pos: [3]float32{-30, 44, 3}, Vel: [3]float32{},
			Yaw: -3, Health: 1, MaxHealth: 60, Action: vnet.MobActionWindup,
		},
	}
	wantVitals := PlayerVitals{
		Health: 35, MaxHealth: 100, LifeState: vnet.LifeStateAlive,
		RespawnTicks: 0, Invulnerable: true,
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
	for i, expected := range wantDrops {
		var drop vnet.ItemDropState
		if !snapshot.Drops(&drop, i) {
			t.Fatalf("drop %d is missing", i)
		}
		pos := drop.Pos(nil)
		if pos == nil {
			t.Fatalf("drop %d has no position", i)
		}
		got := ItemDropState{
			EntityID: drop.EntityId(),
			Pos:      [3]float32{pos.X(), pos.Y(), pos.Z()},
			ItemID:   drop.ItemId(),
			Count:    drop.Count(),
		}
		if got != expected {
			t.Errorf("drop %d decoded as %+v, want %+v", i, got, expected)
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
			EntityID:  mob.EntityId(),
			Kind:      mob.Kind(),
			Pos:       [3]float32{pos.X(), pos.Y(), pos.Z()},
			Vel:       [3]float32{vel.X(), vel.Y(), vel.Z()},
			Yaw:       mob.Yaw(),
			Health:    mob.Health(),
			MaxHealth: mob.MaxHealth(),
			Action:    mob.Action(),
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
		Health:       vitals.Health(),
		MaxHealth:    vitals.MaxHealth(),
		LifeState:    vitals.LifeState(),
		RespawnTicks: vitals.RespawnTicks(),
		Invulnerable: vitals.Invulnerable(),
	}
	if gotVitals != wantVitals {
		t.Errorf("self_vitals decoded as %+v, want %+v", gotVitals, wantVitals)
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
	// Reserved rather than sent: mining, block edits, crafting and repair refuse in the
	// same silence today and will reuse this message, and a member is an integer on the
	// wire — so the cheap moment to agree on the number is before anything depends on it.
	for name, pair := range map[string][2]byte{
		"RefusedAction.MineBlock": {byte(vnet.RefusedActionMineBlock), 2},
		"RefusedAction.EditBlock": {byte(vnet.RefusedActionEditBlock), 3},
		"RefusedAction.Craft":     {byte(vnet.RefusedActionCraft), 4},
		"RefusedAction.Repair":    {byte(vnet.RefusedActionRepair), 5},
	} {
		if pair[0] != pair[1] {
			t.Errorf("%s = %d, want %d", name, pair[0], pair[1])
		}
	}
	// **No member for a removal, and its absence is the decision.** A refused removal is
	// silence on purpose: a client that could tell "no such structure" from "not yours"
	// from "too far away" could map somebody else's camp by asking for ids it does not
	// have. Six members is what says nobody added one.
	if got := len(vnet.EnumNamesRefusedAction); got != 6 {
		t.Errorf("RefusedAction has %d members, want 6 — a removal is refused in silence by design", got)
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
		"MalformedNoAnchor":  {byte(vnet.RefusalReasonMalformedNoAnchor), 64},
		"MalformedFacing":    {byte(vnet.RefusalReasonMalformedFacing), 65},
		"MalformedSlot":      {byte(vnet.RefusalReasonMalformedSlot), 66},
		"MalformedKind":      {byte(vnet.RefusalReasonMalformedKind), 67},
	} {
		if pair[0] != pair[1] {
			t.Errorf("RefusalReason.%s = %d, want %d", name, pair[0], pair[1])
		}
	}
	if got := len(vnet.EnumNamesRefusalReason); got != 16 {
		t.Errorf("RefusalReason has %d members, want 16 — a new one needs a decision, not a test edit", got)
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

	want := PlayerVitals{Health: 0, MaxHealth: 100, LifeState: vnet.LifeStateDead, RespawnTicks: 60}

	env := vnet.GetRootAsEnvelope(EncodeEntitySnapshot(EntitySnapshot{Tick: 9, Vitals: want}), 0)
	tbl := payloadTable(t, env)
	decoded := new(vnet.EntitySnapshot)
	decoded.Init(tbl.Bytes, tbl.Pos)

	vitals := decoded.SelfVitals(nil)
	if vitals == nil {
		t.Fatal("the snapshot carries no self_vitals")
	}
	got := PlayerVitals{
		Health:       vitals.Health(),
		MaxHealth:    vitals.MaxHealth(),
		LifeState:    vitals.LifeState(),
		RespawnTicks: vitals.RespawnTicks(),
		Invulnerable: vitals.Invulnerable(),
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
		// Appended after Forge = 2.
		"StructureKind.Campfire": {byte(vnet.StructureKindCampfire), 3},
		// Appended after Tent = 4.
		"RecipeID.Campfire":     {byte(vnet.RecipeIDCampfire), 5},
		"RecipeID.LeatherPatch": {byte(vnet.RecipeIDLeatherPatch), 6},

		// V8's three, appended after LeatherPatch = 6. The value is a byte on the wire,
		// so a renumbering turns every craft a client asks for into a different one.
		"RecipeID.Shovel":  {byte(vnet.RecipeIDShovel), 7},
		"RecipeID.Pickaxe": {byte(vnet.RecipeIDPickaxe), 8},
		"RecipeID.Axe":     {byte(vnet.RecipeIDAxe), 9},
	} {
		if pair[0] != pair[1] {
			t.Errorf("%s = %d, want %d", name, pair[0], pair[1])
		}
	}

	// Membership, in the shape the Payload union is checked in: a member added without
	// a decision fails here rather than reaching the wire. Each count includes the
	// zero member every one of these enums carries to fail closed.
	for name, pair := range map[string][2]int{
		"MobKind":       {len(vnet.EnumNamesMobKind), 3},
		"StructureKind": {len(vnet.EnumNamesStructureKind), 4},
		"RecipeID":      {len(vnet.EnumNamesRecipeID), 10},
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

	env := vnet.GetRootAsEnvelope(EncodeInventoryState(InventoryState{Stacks: stacks}), 0)
	tbl := payloadTable(t, env)
	state := new(vnet.InventoryState)
	state.Init(tbl.Bytes, tbl.Pos)

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
// Protocol V6 — the world's clock
// ---------------------------------------------------------------------------

// The tick of day rides in the snapshot and survives the encoder unchanged, including
// the last tick of a day — the value most likely to be lost to an off-by-one, and the
// one a receiver must accept: the contract's bound is `tick_of_day < day_length_ticks`,
// so 23999 of 24000 is legal and 24000 is not.
func TestASnapshotCarriesTheTickOfDay(t *testing.T) {
	t.Parallel()

	for _, tickOfDay := range []uint32{0, 1, 14400, 23999} {
		env := vnet.GetRootAsEnvelope(EncodeEntitySnapshot(EntitySnapshot{Tick: 7, TickOfDay: tickOfDay}), 0)
		tbl := payloadTable(t, env)
		snapshot := new(vnet.EntitySnapshot)
		snapshot.Init(tbl.Bytes, tbl.Pos)

		if got := snapshot.TickOfDay(); got != tickOfDay {
			t.Errorf("TickOfDay = %d, want %d", got, tickOfDay)
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

func TestPlayerAppearanceCarriesTheEntityAndTheFace(t *testing.T) {
	t.Parallel()

	want := PlayerAppearance{EntityID: 4242, Appearance: anAppearance(), HasAppearance: true}

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
