// Package protocol turns frames into values and values into frames.
//
// It is the only package that touches FlatBuffers, and the only one that touches
// bytes a client chose. Both facts drive its shape: Decode copies everything it
// needs out of the buffer and hands back plain Go values, so no accessor over
// untrusted memory ever escapes into the rest of the server.
package protocol

import (
	"bytes"
	"errors"
	"fmt"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// Contract limits documented as decoder invariants in schemas/handshake.fbs.
// They live here because the schema cannot enforce them and both sides must
// agree on the numbers.
const (
	// MaxChunkSize is the largest chunk edge the RLE encoding can address: a
	// single run length is a uint16, and 40³ (64000) is the last cube that fits.
	MaxChunkSize = 40

	// MaxViewDistance bounds the streamed volume, which grows as (2r+1)³ chunks.
	MaxViewDistance = 16

	// InventorySlots is the fixed number of authoritative inventory slots this
	// server announces and emits. The handshake carries it so the client never
	// has to hardcode the value.
	InventorySlots uint8 = 36

	// HotbarSlots is the leading subset of InventorySlots the client may select
	// with its hotbar.
	HotbarSlots uint8 = 9
)

// ErrMalformed marks a frame that is not a decodable Envelope. Every malformed
// input funnels through it, so callers can log one class and close the
// connection rather than branching on the shape of the damage.
var ErrMalformed = errors.New("protocol: malformed envelope")

// Message is one decoded envelope.
//
// Kind is always set. The payload pointer for that Kind is set only for the
// payloads this server acts on; the rest are reported by Kind alone, which is
// enough to answer "you may not send me that".
type Message struct {
	Kind               vnet.Payload
	ClientHello        *ClientHello
	PlayerInput        *PlayerInput
	BlockEditRequest   *BlockEditRequest
	ChunkResendRequest *ChunkResendRequest
	MineRequest        *MineRequest
	InventoryMove      *InventoryMoveRequest
	Attack             *AttackRequest
	PlaceStructure     *PlaceStructureRequest
	RemoveStructure    *RemoveStructureRequest
	Craft              *CraftRequest
	Repair             *RepairRequest
}

// ClientHello is a decoded handshake request.
type ClientHello struct {
	ProtocolVersion vnet.ProtocolVersion
	PlayerName      string

	// PlayerToken is the identity token the client presents, copied verbatim —
	// length included.
	//
	// **The length rule is a handshake decision, not a framing one.** The contract
	// says absent, empty, or exactly 32 bytes, and anything else is
	// RejectReason.BAD_REQUEST; but that is a refusal with a *reply*, and a decoder
	// that shortened it to an error would close the connection with nothing said.
	// The house rule AttackRequest.slot documents: this package owns the envelope,
	// and what a value means is the caller's decision. session.Identities.Resolve is
	// where it is made.
	//
	// Absent and empty both arrive as a zero-length slice, which is what the contract
	// says they are: a first connection to this server.
	PlayerToken []byte
}

// PlayerInput is one tick's worth of decoded intent.
//
// Copied out verbatim, values the contract forbids included. This package owns the
// envelope; what a NaN axis *means* is a decision, and decisions belong to the
// simulation — game.Sim rejects a non-finite component before any physics runs.
// Judging it here would leave only two options, and both are wrong: clamping does
// nothing to a NaN, which compares false against every bound, and erroring would
// close a connection whose framing is still perfectly readable.
//
// There is deliberately no position field, because there is no position on the
// wire. See the header of schemas/player.fbs.
type PlayerInput struct {
	ClientTick uint32
	MoveX      float32
	MoveZ      float32
	Yaw        float32
	Pitch      float32
	Jump       bool
}

// BlockEditRequest is one decoded attempt to change a voxel. **Intent, never
// outcome.**
//
// Copied out verbatim, values the contract forbids included — the same division of
// labour as PlayerInput. This package owns the envelope; whether `Unknown` or the
// retired `Break` value is legal, whether an absent position may be read as the origin
// and whether the selected inventory slot holds something placeable are all decisions,
// and decisions belong to the simulation. game.Player.Edit refuses each of them before
// anything touches the world.
//
// There is deliberately no field for where the client thinks it is standing, because
// there is no such field on the wire. Reach is measured against the position the server
// computed, so there is no claim to disbelieve.
type BlockEditRequest struct {
	// Pos is the target voxel in world block coordinates, and HasPos says whether the
	// client sent one at all.
	//
	// The flag is not defensive noise: `pos` is a FlatBuffers struct field, so an absent
	// one decodes as null and the zero value it would otherwise become is the world
	// origin — a real location, which somebody would then have edited without naming it.
	// schemas/world.fbs states this as a decoder invariant in both directions.
	Pos    [3]int32
	HasPos bool

	// Action is copied straight through, `Unknown` included. FlatBuffers decodes an
	// absent scalar as zero and this enum's zero member exists to be refused, so
	// narrowing it here would only move the refusal somewhere with less context.
	Action vnet.EditAction

	// Slot names the authoritative inventory slot a Place spends.
	Slot uint8

	// ClientTick is ordering and staleness only, exactly as in PlayerInput, and never
	// read as a clock.
	ClientTick uint32
}

// BlockUpdate is one authoritative voxel change, server to client.
//
// A statement about the world rather than a reply to anyone: the same shape carries a
// break (block id 0) and a place, and it is equally the message for a change no client
// asked for. Nothing here is re-validated on the way out — it describes what the
// simulation already did.
type BlockUpdate struct {
	Pos     [3]int32
	BlockID uint16
}

// InventoryStack is one authoritative slot. The zero value is an empty slot.
//
// Durability lives here rather than in a parallel slice for the reason it does not on
// the wire: the contract's pair encoding is append-only and could not grow a third
// scalar per slot, so schemas/player.fbs carries durability in two more slot-indexed
// vectors. Off the wire there is no such constraint, and one value per slot is one
// length instead of three — the encoder projects the vectors, the decoder zips them,
// and nothing downstream can pair slot i's count with slot j's durability.
//
// (0, 0) is a slot that does not wear out: empty, or holding a resource. Otherwise
// MaxDurability is non-zero, Durability is at most MaxDurability, and Count is 1. A
// Durability of zero under a non-zero maximum is a worn-out item — unusable, still
// carried — and never an empty slot.
type InventoryStack struct {
	ItemID        uint16
	Count         uint16
	Durability    uint16
	MaxDurability uint16
}

// InventoryState is the complete inventory the server sends after every change.
// Stacks is slot-indexed and always has InventorySlots entries; an empty slot is
// the pair (0, 0).
type InventoryState struct {
	Stacks []InventoryStack
}

// MineRequest is a decoded start, continuation or cancellation of mining one
// voxel. It is intent only: the game owns hardness, progress and completion.
type MineRequest struct {
	Pos        [3]int32
	HasPos     bool
	Active     bool
	ClientTick uint32
}

// MineProgress is authoritative mining progress sent to a client. Progress is
// a fraction of 255 and never a tick count.
type MineProgress struct {
	Pos      [3]int32
	Progress uint8
}

// InventoryMoveRequest is decoded inventory intent. Decode enforces the wire
// invariants; the game revalidates it against the player's authoritative slots.
type InventoryMoveRequest struct {
	From  uint8
	To    uint8
	Count uint16
}

// AttackRequest is one decoded swing of the held weapon. **Intent, never outcome.**
//
// It names no victim: there is no target id, no position, no aim and no damage, so
// there is no claim here for the server to disbelieve. The simulation picks a target
// from the positions it already owns and the last aim it accepted on this session's
// PlayerInput.
//
// Copied out verbatim, values the contract forbids included — the same division of
// labour as PlayerInput and BlockEditRequest. Whether Slot names a slot that exists,
// holds a weapon, or holds one with durability left are all decisions, and decisions
// belong to the simulation, which refuses them silently rather than closing a
// connection whose framing is still perfectly readable.
type AttackRequest struct {
	// Slot is the authoritative inventory slot the swing spends. Deliberately not
	// range-checked here, unlike InventoryMoveRequest's: a move names slots the decoder
	// must bound before anything indexes with them, while a swing's slot is looked up
	// against the player's own inventory and a miss is an ordinary refusal.
	Slot uint8

	// ClientTick is ordering and staleness only, exactly as in PlayerInput, and never
	// read as a clock.
	ClientTick uint32
}

// CraftRequest is one decoded attempt to make something. **Intent, never outcome.**
//
// It names a recipe and nothing else: what that recipe costs, what it yields and whether
// the player is standing near the station it needs are all the server's, read from the
// authoritative inventory and the authoritative world. A message that let the client state
// its own ingredients would be a cheat vector however carefully the server re-checked them.
//
// Recipe is copied straight through, `Unknown` included. FlatBuffers decodes an absent
// scalar as zero and this enum's zero member exists to be refused, so narrowing it here
// would only move the refusal somewhere with less context.
type CraftRequest struct {
	Recipe vnet.RecipeID

	// ClientTick is ordering and staleness only, exactly as in PlayerInput, and never
	// read as a clock.
	ClientTick uint32
}

// RepairRequest is one decoded attempt to mend a carried item. **Intent, never outcome.**
//
// It names two slots and nothing else. There is deliberately no durability field in either
// direction: a client that could state how much wear to restore could repair by asking, and
// the restored value is the server's arithmetic over the slots it already owns.
//
// **Both indexes are copied verbatim, out-of-range values included**, unlike
// InventoryMoveRequest's — and the asymmetry is the same one AttackRequest records. A move
// names slots this package indexes with, so it has to bound them before anything reads an
// array; a repair names slots the *simulation* looks up against the player's own inventory,
// where an index past the end is an ordinary refusal rather than a frame that lies about
// itself. schemas/player.fbs states it that way too, and refusing here would close a
// connection whose framing is perfectly readable.
type RepairRequest struct {
	// KitSlot is the authoritative slot holding the kit this mend spends.
	KitSlot uint8

	// TargetSlot is the authoritative slot holding the item to mend. Whether it equals
	// KitSlot is a decision, and decisions belong to the simulation.
	TargetSlot uint8

	// ClientTick is ordering and staleness only, exactly as in PlayerInput, and never
	// read as a clock.
	ClientTick uint32
}

// PlaceStructureRequest is one decoded attempt to plant a structure. **Intent, never
// outcome.**
//
// It names a slot, an anchor voxel and a facing, and nothing else: what kind of thing
// the slot holds, what footprint that kind needs, whether the ground under it is solid
// and whether this player is allowed another one are all decisions, and decisions belong
// to the simulation. There is no structure id here either, because an id is minted by
// the server when a structure comes into existence — a client that could name one could
// name one it does not own.
//
// Copied out verbatim, values the contract forbids included — the same division of
// labour as BlockEditRequest. game.Player.PlaceStructure refuses each of them before
// anything reaches the registry.
type PlaceStructureRequest struct {
	// Slot names the authoritative inventory slot this placement spends.
	Slot uint8

	// Anchor is the target voxel in world block coordinates, and HasAnchor says whether
	// the client sent one at all.
	//
	// The flag is the requirement BlockEditRequest.HasPos records, for the same reason:
	// `anchor` is a FlatBuffers struct field, so an absent one decodes as null and the
	// zero value it would otherwise become is the world origin — a real location,
	// where somebody would then have planted a tent without naming it.
	Anchor    [3]int32
	HasAnchor bool

	// Facing is copied straight through, `Unknown` included. FlatBuffers decodes an
	// absent scalar as zero and this enum's zero member exists to be refused, so
	// narrowing it here would only move the refusal somewhere with less context.
	Facing vnet.Facing

	// ClientTick is ordering and staleness only, exactly as in PlayerInput, and never
	// read as a clock.
	ClientTick uint32
}

// RemoveStructureRequest is one decoded attempt to take a placed structure back.
// **Intent, never outcome.**
//
// It names an id the server minted and sent, which is the one kind of identifier a
// client may echo: ownership, reach and whether the structure still stands are all
// re-read from the authoritative registry, so naming somebody else's gains nothing.
//
// StructureID is deliberately not validated here, exactly as AttackRequest.Slot is not:
// whether an id names a standing structure this player may remove is a decision the
// simulation makes against state this package cannot see, and it refuses silently
// rather than closing a connection whose framing is still perfectly readable.
type RemoveStructureRequest struct {
	StructureID uint64

	// ClientTick is ordering and staleness only, exactly as in PlayerInput, and never
	// read as a clock.
	ClientTick uint32
}

// ActionRefused is the server's answer to an action it refused. **Server to client.**
//
// A reason code and the cell the refused request named, which is everything a client
// needs to tell the player why nothing happened. It carries no acceptance half and no
// correlation id: at most one of a player's actions is in flight per press, and the
// action code is enough to route the answer.
//
// The server never decodes one of these — receiving it is a client sending a payload
// only a server sends, which the session refuses as a protocol error — so there is no
// field for it in Message.
type ActionRefused struct {
	Action vnet.RefusedAction
	Reason vnet.RefusalReason

	// Anchor is the voxel the refused request named, and HasAnchor says whether it named
	// one. The flag is the requirement PlaceStructureRequest.HasAnchor records, read in
	// the other direction: a refusal whose request carried no anchor must not put the
	// world origin on the wire as though it had.
	Anchor    [3]int32
	HasAnchor bool
}

// StructureState is one placed structure's authoritative state, as a snapshot carries
// it.
//
// A table on the wire rather than a struct, so this shape can still change: see
// schemas/player.fbs. There is no position vector and no yaw — a structure sits on the
// voxel grid and faces one of four ways, so an anchor and a Facing say everything about
// where it is, and a float transform would invite an interpolator to move something that
// never moves.
type StructureState struct {
	StructureID   uint64
	Kind          vnet.StructureKind
	Anchor        [3]int32
	Facing        vnet.Facing
	OwnerEntityID uint64
}

// PlayerVitals is one recipient's authoritative health and life state.
//
// Server to client, and per recipient: a snapshot carries the vitals of the player it
// is addressed to, never anyone else's. The zero value is deliberately **not** a valid
// wire value — MaxHealth is the denominator of every health display, and zero is the
// absent-field case rather than a player with no maximum.
type PlayerVitals struct {
	Health       uint16
	MaxHealth    uint16
	LifeState    vnet.LifeState
	RespawnTicks uint32
	Invulnerable bool
}

// MobState is one mob's authoritative state, as a snapshot carries it.
//
// A table on the wire rather than a struct, so this shape can still change: see
// schemas/player.fbs. Every float in it must be finite for the reason EntityState's
// must, and the simulation is what guarantees that; this type only carries it.
type MobState struct {
	EntityID  uint64
	Kind      vnet.MobKind
	Pos       [3]float32
	Vel       [3]float32
	Yaw       float32
	Health    uint16
	MaxHealth uint16
	Action    vnet.MobAction
}

// EntitySnapshot is one tick of authoritative state, as one session sees it.
//
// A value rather than five positional arguments, matching InventoryState and
// BlockUpdate: what a session can see is the caller's decision and the encoder only
// lays it out. Vitals is a plain field and not a pointer, which is what makes the one
// required field on the wire impossible for a caller to omit — flatc's Go output
// carries no assertion of its own.
type EntitySnapshot struct {
	Tick     uint32
	Entities []EntityState
	Drops    []ItemDropState
	Mobs     []MobState

	// Structures visible to this session, under the same rule the three vectors above
	// obey. The newest snapshot is the complete existence set: a structure that stops
	// being sent has stopped existing for this session, and removed, collapsed and out
	// of view are the same fact on the wire.
	Structures []StructureState

	// Vitals belongs to the session this snapshot is being encoded for, which is why
	// this type is built per recipient rather than once per tick.
	Vitals PlayerVitals

	// TickOfDay is where this tick falls in the world's day, and zero for a server that
	// keeps no clock — the same zero Welcome.DayLengthTicks uses to say so, and read
	// only against that value.
	//
	// The one field here that is not about an entity. It rides in the snapshot because
	// it changes every tick and is read by the same frame the entities are drawn from;
	// a message of its own would arrive on its own schedule and put the sky a tick away
	// from the world underneath it.
	TickOfDay uint32
}

// ChunkResendRequest is one decoded ask for a chunk the client has lost. **A request
// for data, never for an outcome.**
//
// The smallest message a client can send and the least it can claim: a coordinate, and
// whether it sent one at all. Nothing in it says what the chunk holds, so there is
// nothing here to disbelieve — only a question to answer or refuse.
//
// Copied out verbatim, the coordinate the session has no business asking for included.
// Whether it may be resent is a decision, and decisions belong outside this package:
// session.Streamer.Resend refuses a coordinate outside the view volume, one this
// session was never sent, and one that arrives faster than the limit allows.
type ChunkResendRequest struct {
	// Coord is the chunk being asked for, and HasCoord says whether the client sent one
	// at all.
	//
	// The flag is the same requirement BlockEditRequest.HasPos records, for the same
	// reason: `coord` is a FlatBuffers struct field, so an absent one decodes as null and
	// the zero value it would otherwise become is chunk (0, 0, 0) — a real chunk, which
	// somebody would then have been sent without asking for it.
	Coord    ChunkCoord
	HasCoord bool
}

// EntityState is one entity's authoritative state, as a snapshot carries it.
//
// The server's answer, never a client's claim. Every float in it must be finite:
// schemas/player.fbs states the invariant in this direction too, because a
// non-finite position would poison the client's interpolation and, through the
// entity's transform, its renderer. game.Sim is what guarantees that; this type
// only carries it.
type EntityState struct {
	EntityID uint64
	Pos      [3]float32
	Vel      [3]float32
	Yaw      float32
}

// ItemDropState is one authoritative dropped item beside the player entities in
// a snapshot. Count and ItemID are non-zero for every value the server emits.
type ItemDropState struct {
	EntityID uint64
	Pos      [3]float32
	ItemID   uint16
	Count    uint16
}

// Welcome is the authoritative answer to an accepted handshake.
type Welcome struct {
	EntityID       uint64
	Spawn          [3]float32
	WorldSeed      int64
	TickRate       uint8
	ChunkSize      uint16
	ViewDistance   uint8
	InventorySlots uint8
	HotbarSlots    uint8

	// PlayerToken is the identity token this session plays under, which the client
	// stores and presents on its next connection. Present and exactly 32 bytes on
	// every accepted handshake — the identity was resolved before this struct was
	// built, so there is no case in which the server has nothing to put here.
	//
	// Not always the token the client sent: an unrecognised one is answered with a
	// newly minted identity, because the server never adopts a client-chosen value as
	// a key. See schemas/handshake.fbs.
	PlayerToken []byte

	// DayLengthTicks is how many ticks a full day lasts, and zero for a server that
	// keeps no clock.
	//
	// **The zero is the whole point of the field being here before anything sets it.**
	// A Welcome built by a caller that has never heard of a clock encodes three zeros
	// and announces exactly what it means: this world has no time of day, which is the
	// world as it was before V6. Nothing has to be updated for that to be true, and
	// nothing downstream may read the two boundaries below without checking this first.
	DayLengthTicks uint32

	// NightStartTicks and NightEndTicks are the boundaries of night within the day, in
	// the same ticks. Read only when DayLengthTicks is non-zero, and then
	// 0 < NightStartTicks < NightEndTicks <= DayLengthTicks.
	//
	// The encoder does not enforce that ordering, deliberately: this struct is built by
	// the simulation, which owns the constants, and a check here would be a second
	// opinion about a number that has one source. The *client* enforces it, because
	// there the values are untrusted input — see schemas/handshake.fbs.
	NightStartTicks uint32
	NightEndTicks   uint32
}

// Decode reads one frame into a Message.
//
// The deferred recover is not defensive clutter: the Go FlatBuffers runtime ships
// no buffer verifier, so a table offset pointing outside the buffer is discovered
// as an out-of-range slice index — a panic, on input a client chose. Guarding the
// one function that reads untrusted bytes converts that into an error. It only
// works because every field is copied out inside this function: handing a live
// accessor to a caller would move the same panic somewhere nothing guards it.
func Decode(frame []byte) (msg Message, err error) {
	defer func() {
		if r := recover(); r != nil {
			msg, err = Message{}, fmt.Errorf("%w: %v", ErrMalformed, r)
		}
	}()

	// GetRootAsEnvelope reads a root offset and the identifier sits behind it, so
	// anything shorter cannot be inspected safely at all.
	if minimum := int(flatbuffers.SizeUOffsetT) + len(vnet.EnvelopeIdentifier); len(frame) < minimum {
		return Message{}, fmt.Errorf("%w: %d bytes cannot hold a root offset and identifier (%d)", ErrMalformed, len(frame), minimum)
	}
	if !vnet.EnvelopeBufferHasIdentifier(frame) {
		return Message{}, fmt.Errorf("%w: not a %q buffer", ErrMalformed, vnet.EnvelopeIdentifier)
	}

	env := vnet.GetRootAsEnvelope(frame, 0)
	msg.Kind = env.PayloadType()

	switch msg.Kind {
	case vnet.PayloadClientHello:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var hello vnet.ClientHello
		hello.Init(table.Bytes, table.Pos)
		msg.ClientHello = &ClientHello{
			ProtocolVersion: hello.ProtocolVersion(),
			// PlayerName is untrusted display text: copied, never used as a key.
			PlayerName: string(hello.PlayerName()),
			// Cloned rather than aliased, for the reason every other field here is
			// copied: the accessor is a view over bytes a client chose, and handing one
			// out would move the recover above away from the code that needs it. Clone
			// answers nil for an absent vector, which is the same zero length an empty
			// one has — and the contract treats the two the same.
			PlayerToken: bytes.Clone(hello.PlayerTokenBytes()),
		}

	case vnet.PayloadPlayerInput:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var input vnet.PlayerInput
		input.Init(table.Bytes, table.Pos)
		msg.PlayerInput = &PlayerInput{
			ClientTick: input.ClientTick(),
			MoveX:      input.MoveX(),
			MoveZ:      input.MoveZ(),
			Yaw:        input.Yaw(),
			Pitch:      input.Pitch(),
			Jump:       input.Jump(),
		}

	case vnet.PayloadBlockEditRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.BlockEditRequest
		request.Init(table.Bytes, table.Pos)

		edit := &BlockEditRequest{
			Action:     request.Action(),
			Slot:       request.Slot(),
			ClientTick: request.ClientTick(),
		}
		// The accessor returns nil for an absent struct field, and it must not escape
		// this function either way: it is a view over bytes a client chose, and the
		// recover above is the only thing standing between a bad offset and a panic in a
		// goroutine holding a socket.
		if pos := request.Pos(nil); pos != nil {
			edit.Pos, edit.HasPos = [3]int32{pos.X(), pos.Y(), pos.Z()}, true
		}
		msg.BlockEditRequest = edit

	case vnet.PayloadChunkResendRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.ChunkResendRequest
		request.Init(table.Bytes, table.Pos)

		resend := &ChunkResendRequest{}
		// Same discipline as the position above: the accessor is a view over bytes a
		// client chose, it returns nil for an absent struct field, and it must not escape
		// this function either way.
		if coord := request.Coord(nil); coord != nil {
			resend.Coord, resend.HasCoord = ChunkCoord{X: coord.Cx(), Y: coord.Cy(), Z: coord.Cz()}, true
		}
		msg.ChunkResendRequest = resend

	case vnet.PayloadMineRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.MineRequest
		request.Init(table.Bytes, table.Pos)

		mine := &MineRequest{
			Active:     request.Active(),
			ClientTick: request.ClientTick(),
		}
		if pos := request.Pos(nil); pos != nil {
			mine.Pos, mine.HasPos = [3]int32{pos.X(), pos.Y(), pos.Z()}, true
		} else {
			return Message{}, fmt.Errorf("%w: MineRequest pos is absent", ErrMalformed)
		}
		msg.MineRequest = mine

	case vnet.PayloadInventoryMoveRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.InventoryMoveRequest
		request.Init(table.Bytes, table.Pos)

		move := &InventoryMoveRequest{
			From:  request.From(),
			To:    request.To(),
			Count: request.Count(),
		}
		switch {
		case move.From >= InventorySlots:
			return Message{}, fmt.Errorf("%w: InventoryMoveRequest from slot %d is outside %d slots", ErrMalformed, move.From, InventorySlots)
		case move.To >= InventorySlots:
			return Message{}, fmt.Errorf("%w: InventoryMoveRequest to slot %d is outside %d slots", ErrMalformed, move.To, InventorySlots)
		case move.Count == 0:
			return Message{}, fmt.Errorf("%w: InventoryMoveRequest count is zero", ErrMalformed)
		}
		msg.InventoryMove = move

	case vnet.PayloadAttackRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.AttackRequest
		request.Init(table.Bytes, table.Pos)

		// Both fields copied straight through. There is nothing here a decoder can
		// judge: a slot outside the inventory and a stale tick are both refusals the
		// simulation makes against state this package cannot see.
		msg.Attack = &AttackRequest{
			Slot:       request.Slot(),
			ClientTick: request.ClientTick(),
		}

	case vnet.PayloadPlaceStructureRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.PlaceStructureRequest
		request.Init(table.Bytes, table.Pos)

		place := &PlaceStructureRequest{
			Slot:       request.Slot(),
			Facing:     request.Facing(),
			ClientTick: request.ClientTick(),
		}
		// Same discipline as BlockEditRequest.Pos: the accessor is a view over bytes a
		// client chose, it returns nil for an absent struct field, and it must not escape
		// this function either way.
		if anchor := request.Anchor(nil); anchor != nil {
			place.Anchor, place.HasAnchor = [3]int32{anchor.X(), anchor.Y(), anchor.Z()}, true
		}
		msg.PlaceStructure = place

	case vnet.PayloadRemoveStructureRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.RemoveStructureRequest
		request.Init(table.Bytes, table.Pos)

		// Both fields copied straight through. An id nobody has and a stale tick are
		// both refusals the simulation makes against state this package cannot see.
		msg.RemoveStructure = &RemoveStructureRequest{
			StructureID: request.StructureId(),
			ClientTick:  request.ClientTick(),
		}

	case vnet.PayloadCraftRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.CraftRequest
		request.Init(table.Bytes, table.Pos)

		// Both fields copied straight through. `Unknown` and a value no member has are
		// refusals the simulation makes against a table this package cannot see, and a
		// stale tick is a refusal against state it cannot see either.
		msg.Craft = &CraftRequest{
			Recipe:     request.Recipe(),
			ClientTick: request.ClientTick(),
		}

	case vnet.PayloadRepairRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.RepairRequest
		request.Init(table.Bytes, table.Pos)

		// All three fields copied straight through, deliberately including slot indexes
		// past the end of the inventory and the two-slots-are-one case. Nothing here
		// indexes an array with them, and whether a slot holds a usable kit is a decision
		// the simulation makes against state this package cannot see.
		msg.Repair = &RepairRequest{
			KitSlot:    request.KitSlot(),
			TargetSlot: request.TargetSlot(),
			ClientTick: request.ClientTick(),
		}
	}

	return msg, nil
}

// unionPayload unwraps the union payload the envelope's tag promised.
//
// A union tag naming a payload the envelope does not carry is malformed rather
// than empty: the tag is what every consumer branches on, so a tag without its
// payload is a frame that lies about itself.
func unionPayload(env *vnet.Envelope, kind vnet.Payload) (flatbuffers.Table, error) {
	var table flatbuffers.Table
	if !env.Payload(&table) {
		return table, fmt.Errorf("%w: %s payload is absent", ErrMalformed, kind)
	}
	return table, nil
}

// EncodeServerWelcome builds the reply that admits a session.
func EncodeServerWelcome(w Welcome) []byte {
	b := flatbuffers.NewBuilder(128)

	// Vectors must be complete before the table that references them opens.
	tokenOffset := b.CreateByteVector(w.PlayerToken)

	vnet.ServerWelcomeStart(b)
	vnet.ServerWelcomeAddEntityId(b, w.EntityID)
	// A struct field is written inline, so it must be created while its parent
	// table is open — unlike a string or a vector, which must be created before.
	vnet.ServerWelcomeAddSpawn(b, vnet.CreateVec3(b, w.Spawn[0], w.Spawn[1], w.Spawn[2]))
	vnet.ServerWelcomeAddWorldSeed(b, w.WorldSeed)
	vnet.ServerWelcomeAddTickRate(b, w.TickRate)
	vnet.ServerWelcomeAddChunkSize(b, w.ChunkSize)
	vnet.ServerWelcomeAddViewDistance(b, w.ViewDistance)
	vnet.ServerWelcomeAddInventorySlots(b, w.InventorySlots)
	vnet.ServerWelcomeAddHotbarSlots(b, w.HotbarSlots)
	vnet.ServerWelcomeAddPlayerToken(b, tokenOffset)
	// Three zeros for a server with no clock, which is a legal announcement rather than
	// an omission: FlatBuffers writes no bytes for a scalar equal to its default, so the
	// pre-V6 shape of this table is exactly what a clock-less server still produces.
	vnet.ServerWelcomeAddDayLengthTicks(b, w.DayLengthTicks)
	vnet.ServerWelcomeAddNightStartTicks(b, w.NightStartTicks)
	vnet.ServerWelcomeAddNightEndTicks(b, w.NightEndTicks)
	welcome := vnet.ServerWelcomeEnd(b)

	return finishEnvelope(b, vnet.PayloadServerWelcome, welcome)
}

// EncodeServerReject builds the reply that refuses a session. detail is for logs
// and for the client's status text; peers branch on reason.
func EncodeServerReject(reason vnet.RejectReason, detail string) []byte {
	b := flatbuffers.NewBuilder(128)

	// Strings must exist before the table that references them opens.
	detailOffset := b.CreateString(detail)

	vnet.ServerRejectStart(b)
	vnet.ServerRejectAddReason(b, reason)
	vnet.ServerRejectAddDetail(b, detailOffset)
	reject := vnet.ServerRejectEnd(b)

	return finishEnvelope(b, vnet.PayloadServerReject, reject)
}

// EncodeClientHello builds a handshake request from a client that presents no
// identity token: its first connection to this server.
//
// The server never sends one; it exists so tests can produce the input the server
// parses, without hand-rolling a second encoder that could drift from this one —
// which is also why this is one line over [EncodeClientHelloWithToken] rather than a
// second builder. There is one encoder for this message and there are two ways to
// call it.
func EncodeClientHello(version vnet.ProtocolVersion, playerName string) []byte {
	return EncodeClientHelloWithToken(version, playerName, nil)
}

// EncodeClientHelloWithToken builds a handshake request that presents token.
//
// token is written verbatim, whatever its length: the wrong-length case is one the
// server has to refuse, so the encoder a test refuses it with has to be able to
// produce it.
func EncodeClientHelloWithToken(version vnet.ProtocolVersion, playerName string, token []byte) []byte {
	b := flatbuffers.NewBuilder(128)

	// Strings and vectors must exist before the table that references them opens.
	nameOffset := b.CreateString(playerName)
	tokenOffset := b.CreateByteVector(token)

	vnet.ClientHelloStart(b)
	vnet.ClientHelloAddProtocolVersion(b, version)
	vnet.ClientHelloAddPlayerName(b, nameOffset)
	vnet.ClientHelloAddPlayerToken(b, tokenOffset)
	hello := vnet.ClientHelloEnd(b)

	return finishEnvelope(b, vnet.PayloadClientHello, hello)
}

// ChunkCoord is a chunk address, in chunk units.
//
// Declared here rather than reused from the world package so that protocol stays
// the only package that knows what a FlatBuffer looks like, and world stays the
// only one that knows what a chunk looks like. The conversion is three fields at
// the one boundary that has to know both.
type ChunkCoord struct {
	X, Y, Z int32
}

// EncodeChunkData builds a chunk payload from run-length pairs.
//
// runs is the flat (block id, run length) encoding described in schemas/world.fbs;
// this function does not re-validate it, because it comes from our own encoder.
// The client is the side that has to distrust it.
func EncodeChunkData(coord ChunkCoord, runs []uint16) []byte {
	// A terrain chunk encodes to a few hundred pairs; sizing the builder up front
	// avoids the repeated growth of a buffer we know the shape of.
	b := flatbuffers.NewBuilder(len(runs)*2 + 128)

	// Vectors must be complete before the table that references them opens, and
	// FlatBuffers vectors are built back to front.
	vnet.ChunkDataStartRunsVector(b, len(runs))
	for i := len(runs) - 1; i >= 0; i-- {
		b.PrependUint16(runs[i])
	}
	runsOffset := b.EndVector(len(runs))

	vnet.ChunkDataStart(b)
	vnet.ChunkDataAddCoord(b, vnet.CreateChunkCoord(b, coord.X, coord.Y, coord.Z))
	vnet.ChunkDataAddRuns(b, runsOffset)
	chunk := vnet.ChunkDataEnd(b)

	return finishEnvelope(b, vnet.PayloadChunkData, chunk)
}

// EncodeChunkUnload builds the message that tells a client to drop a chunk.
func EncodeChunkUnload(coord ChunkCoord) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.ChunkUnloadStart(b)
	vnet.ChunkUnloadAddCoord(b, vnet.CreateChunkCoord(b, coord.X, coord.Y, coord.Z))
	unload := vnet.ChunkUnloadEnd(b)

	return finishEnvelope(b, vnet.PayloadChunkUnload, unload)
}

// EncodeEntitySnapshot builds one tick's authoritative state for one session.
//
// Takes what that session can see, which is why it takes a value rather than reading a
// world: visibility is the caller's decision, and the encoder's job is only to lay it
// out. The values are not re-validated — they come from the simulation, which is the
// only thing that produces them.
func EncodeEntitySnapshot(s EntitySnapshot) []byte {
	// An EntityState is 40 bytes inlined and an ItemDropState 24; a MobState is a table
	// and costs an offset besides its fields. Sizing the builder up front avoids the
	// repeated growth of a buffer whose shape is known, on the most frequently sent
	// payload in the game.
	b := flatbuffers.NewBuilder(len(s.Entities)*40 + len(s.Drops)*24 + len(s.Mobs)*64 + len(s.Structures)*48 + 128)

	// Every table a vector points at must be finished before that vector opens, so the
	// mob tables are built first and the vector below only carries their offsets. The
	// same rule is why the vectors here are all complete before the snapshot table
	// starts.
	mobOffsets := make([]flatbuffers.UOffsetT, len(s.Mobs))
	for i, mob := range s.Mobs {
		vnet.MobStateStart(b)
		vnet.MobStateAddEntityId(b, mob.EntityID)
		vnet.MobStateAddKind(b, mob.Kind)
		// A struct field is written inline, so it must be created while its parent table
		// is open — unlike a table, a string or a vector, which must be created before.
		vnet.MobStateAddPos(b, vnet.CreateVec3(b, mob.Pos[0], mob.Pos[1], mob.Pos[2]))
		vnet.MobStateAddVel(b, vnet.CreateVec3(b, mob.Vel[0], mob.Vel[1], mob.Vel[2]))
		vnet.MobStateAddYaw(b, mob.Yaw)
		vnet.MobStateAddHealth(b, mob.Health)
		vnet.MobStateAddMaxHealth(b, mob.MaxHealth)
		vnet.MobStateAddAction(b, mob.Action)
		mobOffsets[i] = vnet.MobStateEnd(b)
	}

	vnet.EntitySnapshotStartMobsVector(b, len(mobOffsets))
	for i := len(mobOffsets) - 1; i >= 0; i-- {
		b.PrependUOffsetT(mobOffsets[i])
	}
	mobsOffset := b.EndVector(len(mobOffsets))

	// Tables again, for the reason the mobs above are: a structure is a table on the
	// wire, so each one is finished before the vector that carries its offset opens.
	structureOffsets := make([]flatbuffers.UOffsetT, len(s.Structures))
	for i, structure := range s.Structures {
		vnet.StructureStateStart(b)
		vnet.StructureStateAddStructureId(b, structure.StructureID)
		vnet.StructureStateAddKind(b, structure.Kind)
		vnet.StructureStateAddAnchor(b, vnet.CreateBlockCoord(b, structure.Anchor[0], structure.Anchor[1], structure.Anchor[2]))
		vnet.StructureStateAddFacing(b, structure.Facing)
		vnet.StructureStateAddOwnerEntityId(b, structure.OwnerEntityID)
		structureOffsets[i] = vnet.StructureStateEnd(b)
	}

	vnet.EntitySnapshotStartStructuresVector(b, len(structureOffsets))
	for i := len(structureOffsets) - 1; i >= 0; i-- {
		b.PrependUOffsetT(structureOffsets[i])
	}
	structuresOffset := b.EndVector(len(structureOffsets))

	vnet.PlayerVitalsStart(b)
	vnet.PlayerVitalsAddHealth(b, s.Vitals.Health)
	vnet.PlayerVitalsAddMaxHealth(b, s.Vitals.MaxHealth)
	vnet.PlayerVitalsAddLifeState(b, s.Vitals.LifeState)
	vnet.PlayerVitalsAddRespawnTicks(b, s.Vitals.RespawnTicks)
	vnet.PlayerVitalsAddInvulnerable(b, s.Vitals.Invulnerable)
	vitalsOffset := vnet.PlayerVitalsEnd(b)

	// A vector of structs must be complete before the table that references it
	// opens, and FlatBuffers vectors are built back to front — so the entities are
	// prepended in reverse and come out in the order they were given.
	vnet.EntitySnapshotStartEntitiesVector(b, len(s.Entities))
	for i := len(s.Entities) - 1; i >= 0; i-- {
		e := s.Entities[i]
		vnet.CreateEntityState(b, e.EntityID,
			e.Pos[0], e.Pos[1], e.Pos[2],
			e.Vel[0], e.Vel[1], e.Vel[2],
			e.Yaw,
		)
	}
	entitiesOffset := b.EndVector(len(s.Entities))

	vnet.EntitySnapshotStartDropsVector(b, len(s.Drops))
	for i := len(s.Drops) - 1; i >= 0; i-- {
		drop := s.Drops[i]
		vnet.CreateItemDropState(b, drop.EntityID,
			drop.Pos[0], drop.Pos[1], drop.Pos[2],
			drop.ItemID, drop.Count,
		)
	}
	dropsOffset := b.EndVector(len(s.Drops))

	vnet.EntitySnapshotStart(b)
	vnet.EntitySnapshotAddServerTick(b, s.Tick)
	vnet.EntitySnapshotAddEntities(b, entitiesOffset)
	vnet.EntitySnapshotAddDrops(b, dropsOffset)
	vnet.EntitySnapshotAddMobs(b, mobsOffset)
	// Unconditional, and it has to be: self_vitals is the contract's one required
	// field, and flatc's Go output emits no assertion that would catch its absence.
	// Vitals being a plain field of EntitySnapshot rather than a pointer is what makes
	// there be nothing for a caller to forget.
	vnet.EntitySnapshotAddSelfVitals(b, vitalsOffset)
	vnet.EntitySnapshotAddStructures(b, structuresOffset)
	vnet.EntitySnapshotAddTickOfDay(b, s.TickOfDay)
	snapshot := vnet.EntitySnapshotEnd(b)

	return finishEnvelope(b, vnet.PayloadEntitySnapshot, snapshot)
}

// EncodePlayerInput builds one tick of intent. The server never sends one — input
// only ever travels client to server — so this exists for the same reason
// EncodeClientHello does: the tests need the bytes a client produces, and a second
// encoder written by hand could drift from this one.
func EncodePlayerInput(in PlayerInput) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.PlayerInputStart(b)
	vnet.PlayerInputAddClientTick(b, in.ClientTick)
	vnet.PlayerInputAddMoveX(b, in.MoveX)
	vnet.PlayerInputAddMoveZ(b, in.MoveZ)
	vnet.PlayerInputAddYaw(b, in.Yaw)
	vnet.PlayerInputAddPitch(b, in.Pitch)
	vnet.PlayerInputAddJump(b, in.Jump)
	input := vnet.PlayerInputEnd(b)

	return finishEnvelope(b, vnet.PayloadPlayerInput, input)
}

// EncodeBlockUpdate builds the message that tells every session which can see a voxel
// what stands there now.
func EncodeBlockUpdate(u BlockUpdate) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.BlockUpdateStart(b)
	// A struct field is written inline, so it must be created while its parent table is
	// open — unlike a string or a vector, which must be created before.
	vnet.BlockUpdateAddPos(b, vnet.CreateBlockCoord(b, u.Pos[0], u.Pos[1], u.Pos[2]))
	vnet.BlockUpdateAddBlockId(b, u.BlockID)
	update := vnet.BlockUpdateEnd(b)

	return finishEnvelope(b, vnet.PayloadBlockUpdate, update)
}

// EncodeInventoryState builds the complete inventory the server sends on join and
// whenever one of its counts changes.
//
// Three slot-indexed vectors, all of length InventorySlots, projected from one slice of
// slots: the pair encoding could not grow a third scalar without moving every index
// already on the wire, so durability rides beside it instead. Projecting all three from
// the same slice is what makes a frame unable to pair one slot's count with another's
// durability.
func EncodeInventoryState(state InventoryState) []byte {
	// Always emit the shape ServerWelcome announced. The game supplies exactly this
	// many entries; padding here also keeps an empty zero-value state a valid empty
	// inventory in protocol tests and prevents an internal caller from putting a
	// short vector on the wire.
	slots := int(InventorySlots)
	pairs := make([]uint16, 0, slots*2)
	durability := make([]uint16, 0, slots)
	maxDurability := make([]uint16, 0, slots)
	for slot := range slots {
		var stack InventoryStack
		if slot < len(state.Stacks) {
			stack = state.Stacks[slot]
		}
		pairs = append(pairs, stack.ItemID, stack.Count)
		durability = append(durability, stack.Durability)
		maxDurability = append(maxDurability, stack.MaxDurability)
	}

	b := flatbuffers.NewBuilder(len(pairs)*2 + slots*4 + 128)

	vnet.InventoryStateStartStacksVector(b, len(pairs))
	for i := len(pairs) - 1; i >= 0; i-- {
		b.PrependUint16(pairs[i])
	}
	stacks := b.EndVector(len(pairs))

	vnet.InventoryStateStartDurabilityVector(b, len(durability))
	for i := len(durability) - 1; i >= 0; i-- {
		b.PrependUint16(durability[i])
	}
	durabilityOffset := b.EndVector(len(durability))

	vnet.InventoryStateStartMaxDurabilityVector(b, len(maxDurability))
	for i := len(maxDurability) - 1; i >= 0; i-- {
		b.PrependUint16(maxDurability[i])
	}
	maxDurabilityOffset := b.EndVector(len(maxDurability))

	vnet.InventoryStateStart(b)
	vnet.InventoryStateAddStacks(b, stacks)
	vnet.InventoryStateAddDurability(b, durabilityOffset)
	vnet.InventoryStateAddMaxDurability(b, maxDurabilityOffset)
	inventory := vnet.InventoryStateEnd(b)

	return finishEnvelope(b, vnet.PayloadInventoryState, inventory)
}

// EncodeBlockEditRequest builds an edit request. The server never sends one — an edit
// request only ever travels client to server — so this exists for the same reason
// EncodeClientHello and EncodePlayerInput do: the tests need the bytes a client
// produces, and a second encoder written by hand could drift from this one.
//
// HasPos is honoured rather than assumed, because "a request with no position" is one of
// the inputs the server has to refuse and there would otherwise be no way to build it.
func EncodeBlockEditRequest(r BlockEditRequest) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.BlockEditRequestStart(b)
	if r.HasPos {
		vnet.BlockEditRequestAddPos(b, vnet.CreateBlockCoord(b, r.Pos[0], r.Pos[1], r.Pos[2]))
	}
	vnet.BlockEditRequestAddAction(b, r.Action)
	vnet.BlockEditRequestAddSlot(b, r.Slot)
	vnet.BlockEditRequestAddClientTick(b, r.ClientTick)
	request := vnet.BlockEditRequestEnd(b)

	return finishEnvelope(b, vnet.PayloadBlockEditRequest, request)
}

// EncodeMineRequest builds mining intent for protocol tests. The server never
// sends one; HasPos lets tests construct the absent-struct protocol error.
func EncodeMineRequest(r MineRequest) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.MineRequestStart(b)
	if r.HasPos {
		vnet.MineRequestAddPos(b, vnet.CreateBlockCoord(b, r.Pos[0], r.Pos[1], r.Pos[2]))
	}
	vnet.MineRequestAddActive(b, r.Active)
	vnet.MineRequestAddClientTick(b, r.ClientTick)
	request := vnet.MineRequestEnd(b)

	return finishEnvelope(b, vnet.PayloadMineRequest, request)
}

// EncodeMineProgress builds authoritative progress for one voxel. Progress is a
// fraction of 255, exactly as the schema documents.
func EncodeMineProgress(p MineProgress) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.MineProgressStart(b)
	vnet.MineProgressAddPos(b, vnet.CreateBlockCoord(b, p.Pos[0], p.Pos[1], p.Pos[2]))
	vnet.MineProgressAddProgress(b, p.Progress)
	progress := vnet.MineProgressEnd(b)

	return finishEnvelope(b, vnet.PayloadMineProgress, progress)
}

// EncodeInventoryMoveRequest builds inventory intent for protocol tests. The
// decoder, not this helper, enforces slot bounds and a non-zero count so tests can
// exercise malformed values without hand-rolling FlatBuffers.
func EncodeInventoryMoveRequest(r InventoryMoveRequest) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.InventoryMoveRequestStart(b)
	vnet.InventoryMoveRequestAddFrom(b, r.From)
	vnet.InventoryMoveRequestAddTo(b, r.To)
	vnet.InventoryMoveRequestAddCount(b, r.Count)
	request := vnet.InventoryMoveRequestEnd(b)

	return finishEnvelope(b, vnet.PayloadInventoryMoveRequest, request)
}

// EncodeAttackRequest builds one swing. The server never sends one — an attack request
// only ever travels client to server — so this exists for the same reason
// EncodeClientHello, EncodePlayerInput and EncodeMineRequest do: the tests need the
// bytes a client produces, and a second encoder written by hand could drift from this
// one.
//
// Nothing is validated on the way out, deliberately: a slot outside the inventory is one
// of the inputs the simulation has to refuse, and there would otherwise be no way to
// build it.
func EncodeAttackRequest(r AttackRequest) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.AttackRequestStart(b)
	vnet.AttackRequestAddSlot(b, r.Slot)
	vnet.AttackRequestAddClientTick(b, r.ClientTick)
	request := vnet.AttackRequestEnd(b)

	return finishEnvelope(b, vnet.PayloadAttackRequest, request)
}

// EncodePlaceStructureRequest builds one placement intent. The server never sends one,
// so this exists for the reason EncodeBlockEditRequest does: the tests need the bytes a
// client produces, and a second encoder written by hand could drift from this one.
//
// HasAnchor is honoured rather than assumed, because "a request with no anchor" is one of
// the inputs the simulation has to refuse and there would otherwise be no way to build it.
// Nothing else is validated on the way out either: an Unknown facing and a slot outside
// the inventory are both refusals worth being able to express.
func EncodePlaceStructureRequest(r PlaceStructureRequest) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.PlaceStructureRequestStart(b)
	vnet.PlaceStructureRequestAddSlot(b, r.Slot)
	if r.HasAnchor {
		vnet.PlaceStructureRequestAddAnchor(b, vnet.CreateBlockCoord(b, r.Anchor[0], r.Anchor[1], r.Anchor[2]))
	}
	vnet.PlaceStructureRequestAddFacing(b, r.Facing)
	vnet.PlaceStructureRequestAddClientTick(b, r.ClientTick)
	request := vnet.PlaceStructureRequestEnd(b)

	return finishEnvelope(b, vnet.PayloadPlaceStructureRequest, request)
}

// EncodeRemoveStructureRequest builds one removal intent, for the same reason as above.
// The id is written verbatim, zero and unknown ids included: both are refusals the
// simulation has to make and a test has to be able to send.
func EncodeRemoveStructureRequest(r RemoveStructureRequest) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.RemoveStructureRequestStart(b)
	vnet.RemoveStructureRequestAddStructureId(b, r.StructureID)
	vnet.RemoveStructureRequestAddClientTick(b, r.ClientTick)
	request := vnet.RemoveStructureRequestEnd(b)

	return finishEnvelope(b, vnet.PayloadRemoveStructureRequest, request)
}

// EncodeActionRefused builds the answer to an action the server would not perform.
//
// The one message in this contract that says no to anything but a connection. Every
// refused action before it was silence plus a line in a server log — the reason was
// computed, written down where no player could reach it, and dropped.
//
// **Not an acknowledgement, and there is still no acceptance payload.** A structure
// exists when a snapshot says it does, and the absence of one of these is not a yes.
//
// The anchor is written only when the refused request named a voxel, because absent is
// what the contract says an action with no cell carries — the origin is a real place
// and would read as a claim about it.
func EncodeActionRefused(r ActionRefused) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.ActionRefusedStart(b)
	vnet.ActionRefusedAddAction(b, r.Action)
	vnet.ActionRefusedAddReason(b, r.Reason)
	if r.HasAnchor {
		vnet.ActionRefusedAddAnchor(b, vnet.CreateBlockCoord(b, r.Anchor[0], r.Anchor[1], r.Anchor[2]))
	}
	refused := vnet.ActionRefusedEnd(b)

	return finishEnvelope(b, vnet.PayloadActionRefused, refused)
}

// EncodeCraftRequest builds one craft intent. The server never sends one, so this exists
// for the reason EncodeAttackRequest does: the tests need the bytes a client produces, and
// a second encoder written by hand could drift from this one.
//
// Nothing is validated on the way out. `RecipeID.Unknown` and a value no member has are
// both inputs the simulation must refuse, and there would otherwise be no way to build one.
func EncodeCraftRequest(r CraftRequest) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.CraftRequestStart(b)
	vnet.CraftRequestAddRecipe(b, r.Recipe)
	vnet.CraftRequestAddClientTick(b, r.ClientTick)
	request := vnet.CraftRequestEnd(b)

	return finishEnvelope(b, vnet.PayloadCraftRequest, request)
}

// EncodeRepairRequest builds one repair intent, for the reason EncodeCraftRequest exists:
// the server never sends one, and the tests need the bytes a client produces rather than a
// second encoder written by hand that could drift from this one.
//
// Nothing is validated on the way out. A slot past the end of the inventory and a request
// naming the same slot twice are both inputs the simulation must refuse, and there would
// otherwise be no way to build one.
func EncodeRepairRequest(r RepairRequest) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.RepairRequestStart(b)
	vnet.RepairRequestAddKitSlot(b, r.KitSlot)
	vnet.RepairRequestAddTargetSlot(b, r.TargetSlot)
	vnet.RepairRequestAddClientTick(b, r.ClientTick)
	request := vnet.RepairRequestEnd(b)

	return finishEnvelope(b, vnet.PayloadRepairRequest, request)
}

// EncodeChunkResendRequest builds one ask for a chunk the client has lost. The server
// never sends one — a resend request only ever travels client to server — so this exists
// for the same reason EncodeClientHello, EncodePlayerInput and EncodeBlockEditRequest do:
// the tests need the bytes a client produces, and a second encoder written by hand could
// drift from this one.
//
// HasCoord is honoured rather than assumed, because "a request with no coordinate" is one
// of the inputs the server has to refuse and there would otherwise be no way to build it.
func EncodeChunkResendRequest(r ChunkResendRequest) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.ChunkResendRequestStart(b)
	if r.HasCoord {
		vnet.ChunkResendRequestAddCoord(b, vnet.CreateChunkCoord(b, r.Coord.X, r.Coord.Y, r.Coord.Z))
	}
	request := vnet.ChunkResendRequestEnd(b)

	return finishEnvelope(b, vnet.PayloadChunkResendRequest, request)
}

// finishEnvelope wraps a built payload in the one root type on the wire and
// stamps the file identifier, so every encoder here produces frames the peer's
// identifier check accepts.
func finishEnvelope(b *flatbuffers.Builder, kind vnet.Payload, payload flatbuffers.UOffsetT) []byte {
	vnet.EnvelopeStart(b)
	vnet.EnvelopeAddPayloadType(b, kind)
	vnet.EnvelopeAddPayload(b, payload)
	envelope := vnet.EnvelopeEnd(b)

	vnet.FinishEnvelopeBuffer(b, envelope)
	return b.FinishedBytes()
}
