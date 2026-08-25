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
	"time"

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
	// server announces and emits. The layout is the hotbar first, the pack in the
	// middle, and equipment last. The handshake carries the counts so the client
	// never has to hardcode the layout.
	InventorySlots uint8 = 39

	// HotbarSlots is the leading subset of InventorySlots the client may select
	// with its hotbar.
	HotbarSlots uint8 = 9

	// EquipmentSlots is the trailing subset of InventorySlots reserved for worn
	// equipment: head, chest and legs, in that order.
	EquipmentSlots uint8 = 3

	// SessionTicketLen is the exact length of a ClientHello.session_ticket, from V7:
	// a 32-byte body and a 64-byte detached signature over it. schemas/handshake.fbs
	// is authoritative for what the two halves are; nothing here reads either.
	//
	// It lives beside the four above because it is the same kind of number — a
	// contract limit the schema states in prose and cannot enforce, which both sides
	// have to agree on. Whether a ticket of some *other* length is a refusal is not
	// decided here; see ClientHello.SessionTicket.
	SessionTicketLen = 96

	// PlayerTokenLen is the exact length of a ServerWelcome.player_token, and it is
	// the one thing about that field V7 did not retire.
	//
	// The field's *meaning* is gone: a V7 server settles identity from a session
	// ticket, mints no tokens, and reads past ClientHello.player_token entirely. But
	// schemas/handshake.fbs still requires a welcome to carry the vector — present,
	// and exactly this many bytes — and requires a decoder to treat any other length
	// as a protocol error the way it treats a zero tick rate. So the number outlives
	// the meaning, and this is where a server building a welcome reads it.
	//
	// It is here rather than in internal/identity for exactly that reason: it is a
	// fact about the wire, and the package that names players no longer has a 32-byte
	// anything.
	PlayerTokenLen = 32
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
	Consume            *ConsumeRequest
	DropItem           *DropItemRequest
	LeaveRequest       *LeaveRequest
	SelectCharacter    *SelectCharacterRequest
	CreateCharacter    *CreateCharacterRequest
	Chat               *ChatRequest
	Party              *PartyRequest
	LootOpen           *LootOpenRequest
	LootTake           *LootTakeRequest
}

// LeaveRequest is an intentionally empty leave intent. The absence of a duration
// is the authority boundary: the server owns how long the character remains.
type LeaveRequest struct{}

// ChatRequest is display text copied verbatim from one client request. Whether the
// server accepts, rate-limits or delivers it is a simulation decision.
type ChatRequest struct {
	Text string
}

// PartyRequest is one intent to change party membership. TargetName is display text,
// never an identity; the authoritative game resolves it for Invite and Kick only.
type PartyRequest struct {
	Action     vnet.PartyAction
	TargetName string
}

// LootOpenRequest asks for the authoritative container attached to one corpse.
type LootOpenRequest struct {
	CorpseID   uint64
	ClientTick uint32
}

// LootTakeRequest asks to move one stable entry from one known container revision.
// It carries no stack contents or inventory outcome.
type LootTakeRequest struct {
	CorpseID   uint64
	EntryID    uint64
	Revision   uint32
	ClientTick uint32
}

// LootEntry is one authoritative stack in a per-recipient corpse container.
type LootEntry struct {
	EntryID       uint64
	ItemID        uint16
	Count         uint16
	Durability    uint16
	MaxDurability uint16
}

// LootState replaces the recipient's previous view of one corpse wholesale.
type LootState struct {
	CorpseID uint64
	Revision uint32
	Entries  []LootEntry
}

// LootClosed explicitly ends presentation for one corpse container.
type LootClosed struct {
	CorpseID uint64
}

// ChatMessage is one chat line the authoritative server accepted.
type ChatMessage struct {
	SenderEntityID uint64
	SenderName     string
	Text           string
}

// PartyInvite is one still-live invitation delivered by the authoritative server.
type PartyInvite struct {
	FromEntityID uint64
	FromName     string
	ExpiresMS    uint32
}

// ClientHello is a decoded handshake request.
type ClientHello struct {
	ProtocolVersion vnet.ProtocolVersion
	PlayerName      string

	// PlayerToken is the retired identity token the client presents, copied verbatim —
	// length included.
	//
	// **A V7 server reads past it, and that includes its length.** The contract retires
	// the field rather than removing it, because tags never move and a peer built
	// against V5 or V6 still writes one; `session.Identities.Resolve` settles identity
	// from SessionTicket below and asks nothing at all about this vector. It is still
	// decoded, because a decoder that dropped a field would be deciding something —
	// which is not this package's job, and is the house rule AttackRequest.slot
	// documents: this package owns the envelope, and what a value means is the caller's.
	//
	// Nothing here refuses a wrong length any more, and nothing downstream does either:
	// that rule was the V6 handshake's, and the V6 handshake is what a session ticket
	// replaced.
	PlayerToken []byte

	// SessionTicket is the signed ticket the client presents, copied verbatim —
	// length included, and contents never inspected. **The identity half of a V7
	// handshake**, and what retires PlayerToken above.
	//
	// The length rule is a handshake decision for exactly the reason PlayerToken's
	// is, and the same house rule applies: the contract says absent, empty, or
	// exactly SessionTicketLen bytes, and anything else is RejectReason.BAD_REQUEST
	// — a refusal with a *reply*, which a decoder that shortened it to an error
	// would turn into a closed connection with nothing said.
	//
	// Absent and empty both arrive as a zero-length slice, which is what the
	// contract says they are: a client presenting no account.
	SessionTicket []byte
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
	// Slot is the inventory slot the player is mining with. Which slot, never what is in
	// it: the server reads its own inventory for the item. Absent on the wire decodes as
	// 0, a real hotbar slot rather than a sentinel — see the field's own note in
	// schemas/player.fbs.
	Slot uint8
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

// ConsumeRequest is one decoded attempt to eat a carried item. **Intent, never
// outcome.**
//
// It names one authoritative slot and nothing else: whether that slot exists, whether
// it holds food, how much hunger that food restores and whether the player is alive are
// all decisions the simulation makes against state this package cannot see. Slot is a
// uint16 on the wire and is copied verbatim, including values outside the inventory.
type ConsumeRequest struct {
	Slot uint16

	// ClientTick is ordering and staleness only, exactly as in PlayerInput, and never
	// read as a clock.
	ClientTick uint32
}

// DropItemRequest is one decoded attempt to put a whole stack back on the ground.
// **Intent, never outcome.**
//
// One slot index and nothing else. No count and no position in either direction: a count
// would let a client state what leaves its own pack, and a position would let it put an item
// down anywhere in the world.
//
// Slot is copied verbatim, out-of-range values included, exactly as AttackRequest.Slot is:
// whether a slot holds something a player may put down is a decision the simulation makes
// against a pack this package cannot see, and refusing it here would close a connection
// whose framing is perfectly readable.
type DropItemRequest struct {
	// Slot is the authoritative inventory slot to empty onto the ground.
	Slot uint8

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

// PartyMemberState is one other member of a snapshot recipient's party.
type PartyMemberState struct {
	EntityID  uint64
	Pos       [3]float32
	Health    uint16
	MaxHealth uint16
	Alive     bool
}

// PartyRosterMember is one stable character in authoritative party order. EntityID is
// zero exactly while that character is offline; CharacterID remains stable.
type PartyRosterMember struct {
	CharacterID uint64
	EntityID    uint64
	Name        string
	Online      bool
}

// PlayerVitals is one recipient's authoritative health and life state.
//
// Server to client, and per recipient: a snapshot carries the vitals of the player it
// is addressed to, never anyone else's. The zero value is deliberately **not** a valid
// wire value — MaxHealth and ExperienceToNext are display denominators, and zero is
// the absent-field case rather than a valid authoritative state.
type PlayerVitals struct {
	Health           uint16
	MaxHealth        uint16
	LifeState        vnet.LifeState
	RespawnTicks     uint32
	Invulnerable     bool
	Hunger           uint16
	MaxHunger        uint16
	Level            uint16
	Experience       uint32
	ExperienceToNext uint32
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

	// DeadPlayers is the entity ids in Entities the server currently holds dead — a fact
	// about the world rather than an event, so a session that arrives after a death is
	// told the same thing as one that watched it happen.
	//
	// Nil and empty are the same wire value and both are the ordinary case: the encoder
	// writes no field at all for either, so a tick on which nobody is dead costs exactly
	// what it cost before this vector existed.
	//
	// The invariant that ties it to Vitals is the caller's: the recipient's own entity id
	// belongs here exactly when Vitals.LifeState is Dead. This package lays bytes out and
	// does not re-check the simulation, but a client refuses a frame where the two
	// disagree — schemas/player.fbs states it as a decoder invariant — so it is a
	// disconnect rather than a cosmetic bug.
	DeadPlayers []uint64

	// TickOfDay is where this tick falls in the world's day, and zero for a server that
	// keeps no clock — the same zero Welcome.DayLengthTicks uses to say so, and read
	// only against that value.
	//
	// The one field here that is not about an entity. It rides in the snapshot because
	// it changes every tick and is read by the same frame the entities are drawn from;
	// a message of its own would arrive on its own schedule and put the sky a tick away
	// from the world underneath it.
	TickOfDay uint32

	// PartyLeaderEntityID is zero only when the recipient has no party. A non-zero
	// leader may be the recipient itself, so it need not occur in PartyMembers.
	PartyLeaderEntityID uint64

	// PartyMembers excludes the recipient. The caller that knows that recipient's id
	// owns that invariant; this encoder only lays out the authoritative projection.
	PartyMembers []PartyMemberState

	// PartyRoster is complete, includes the recipient, and begins with the leader.
	PartyRoster []PartyRosterMember

	// AccessibleLootCorpses is the complete set this recipient may currently open.
	AccessibleLootCorpses []uint64
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
//
// Durability and MaxDurability are projected onto the sparse
// EntitySnapshot.drop_durabilities vector. Both are zero for the wearless drops the
// world produces; a non-zero maximum carries one authoritative inventory slot's wear.
type ItemDropState struct {
	EntityID      uint64
	Pos           [3]float32
	ItemID        uint16
	Count         uint16
	Durability    uint16
	MaxDurability uint16
}

// Appearance is what one character looks like: four worn colours, a hair model and
// its colour.
//
// Copied out verbatim in both directions, colours whose reserved high byte is set
// included, and an Unknown hair model included. The house rule AttackRequest.Slot
// records: this package owns the envelope, and what a value *means* is the caller's
// decision. schemas/common.fbs states the invariants; the character store and the
// creation screen are where they are enforced, and both are separate issues.
//
// Each colour is 0x00RRGGBB — eight bits per channel, sRGB, the top eight bits
// reserved and zero. There is exactly one colour encoding on this wire and this is
// it; see schemas/common.fbs, which is authoritative.
type Appearance struct {
	SkinColor     uint32
	ShirtColor    uint32
	TrousersColor uint32
	ShoesColor    uint32
	HairModel     vnet.HairModel
	HairColor     uint32
}

// ErrAppearance marks an appearance that breaks an invariant schemas/common.fbs states.
//
// One sentinel rather than one per invariant, because every caller answers all of them
// the same way — a creation refused, a stored character refused entry — and what
// distinguishes them is the wrapped sentence an operator reads.
var ErrAppearance = errors.New("protocol: the appearance is not one this contract allows")

// ColorChannels is the mask a colour on this wire fits in: 0x00RRGGBB, with the
// most significant eight bits reserved and required to be zero.
//
// Exported because it is a contract limit like [InventorySlots] beside it — a number
// the schema states in prose and cannot enforce — and because a client-facing tool that
// offers a palette has to know where the room ends.
const ColorChannels uint32 = 0x00FFFFFF

// Validate enforces the invariants schemas/common.fbs documents for an Appearance.
//
// **Deliberately not called by [Decode], and that is the division of labour this whole
// package keeps.** Decode owns the envelope and copies values verbatim; a colour it
// disliked would arrive inside a frame that is perfectly readable, and closing the
// connection over it would answer a value question with a framing verdict. So this is a
// question a caller asks, at the two places the contract names it:
//
//   - whatever accepts a CreateCharacterRequest asks it **before the appearance is
//     stored**, because a character persisted with a hair model no member names is one
//     every client will afterwards refuse to load — and the person who cannot get in is
//     not the person who sent it;
//   - whatever puts a stored appearance back on the wire asks it again, for the reason
//     game.Sim.Join re-asks about a stored life: the only thing between a file on disk
//     and a frame a client must refuse is somebody having checked.
//
// The colours are checked and the hair model is checked; **absence is not**, and that
// asymmetry is the contract's rather than an omission here. A table scalar carries no
// presence bit, so an absent colour and a chosen black are the same bytes — refusing
// absence would refuse a character wearing black shoes, and would make decode
// correctness depend on the sender's builder settings. `HairModel.Unknown` has no such
// twin: it is not a choice a player can make, so the zero value fails closed.
func (a Appearance) Validate() error {
	for _, worn := range [...]struct {
		what  string
		color uint32
	}{
		{"skin", a.SkinColor},
		{"shirt", a.ShirtColor},
		{"trousers", a.TrousersColor},
		{"shoes", a.ShoesColor},
		{"hair", a.HairColor},
	} {
		if worn.color > ColorChannels {
			// Refused rather than masked: a set high byte means the peer is encoding
			// something this build does not know about, and masking it would show a
			// colour nobody chose while hiding the disagreement.
			return fmt.Errorf("%w: the %s colour %#08x sets the reserved high byte", ErrAppearance, worn.what, worn.color)
		}
	}

	// The generated names rather than a switch listing the five members, and the reason
	// is what this side does with the value: the server stores a hair model and never
	// draws one, so the vocabulary is the contract's and a member appended to it is
	// already acceptable here. A list kept in this file would be a second copy of that
	// vocabulary, and the first thing a second copy does is fall behind.
	if _, known := vnet.EnumNamesHairModel[a.HairModel]; !known {
		return fmt.Errorf("%w: hair model %d is not a member of this contract", ErrAppearance, a.HairModel)
	}
	if a.HairModel == vnet.HairModelUnknown {
		return fmt.Errorf("%w: the hair model is Unknown, which is the absent-field value rather than a choice", ErrAppearance)
	}
	return nil
}

// CharacterSummary is one character an account owns on this world, as
// ServerCharacterList lists it.
//
// Enough to draw a row in a character-select screen and nothing else. There is no
// position, no health and no inventory: those are read from the server's own store
// once a character has been chosen, and a list that carried them would hand out
// state before an identity was settled.
type CharacterSummary struct {
	// CharacterID is server-minted and outlives every session the character has.
	// **Not an entity id** — that names a body in a running simulation and is
	// forgotten when the session ends. Welcome.EntityID is what the chosen character
	// becomes once it is in the world.
	CharacterID uint64

	// Name is display text: shown, never parsed, never an identifier.
	Name string

	Appearance Appearance
}

// CharacterList is every character an account owns on this world, plus the limit.
//
// Server to client, and the second message of a V7 handshake. An empty Characters is
// a legal and expected answer — a new account, or one that has never played here —
// and it is not a refusal: it says the only way forward is a CreateCharacterRequest.
type CharacterList struct {
	Characters []CharacterSummary

	// MaxCharacters is how many characters this account may hold on this world,
	// including the ones above. Sent rather than hardcoded, for the reason every other
	// limit in Welcome is: the number belongs to the server.
	MaxCharacters uint8
}

// SelectCharacterRequest is one decoded choice of an existing character.
// **Client to server, and a claim rather than a statement.**
//
// It names an id the server minted and sent, which is the one kind of identifier a
// client may echo back — the rule RemoveStructureRequest already follows. Whether the
// id names a character *this account* owns is re-read from the server's own store, so
// naming somebody else's gains nothing.
//
// CharacterID is deliberately not validated here, exactly as RemoveStructureRequest's
// StructureID is not: whether an id names a character this account may play is a
// decision the handshake makes against a store this package cannot see.
type SelectCharacterRequest struct {
	CharacterID uint64
}

// CreateCharacterRequest is one decoded attempt to make a new character.
// **Client to server, and intent only.**
//
// A name and a face, which is everything a player chooses about a character and
// nothing a player decides about the world. There is no character id — an id is
// minted by the server when a character comes into existence — and no position, no
// health and no inventory.
//
// The two fields are treated differently on purpose, and schemas/handshake.fbs says
// why. Name is copied verbatim, the empty string included: what names a server
// accepts is a decision, answered with RejectReason.CHARACTER_NAME_REFUSED, which is
// a refusal with a reply where a decode error would close the connection with nothing
// said. An absent Appearance is not that — it is a request that failed to say what it
// is asking for — so Decode refuses it, exactly as it refuses a MineRequest with no
// position.
type CreateCharacterRequest struct {
	Name string

	Appearance Appearance

	// HasAppearance is false only for a frame that carried no appearance table at
	// all. Decode never returns one — it refuses that frame — and the field exists so
	// [EncodeCreateCharacterRequest] can *build* the input that has to be refused.
	HasAppearance bool
}

// PlayerAppearance is what one player entity looks like, is called and has reached.
// **Server to client.**
//
// Sent once, when a player enters a session's view, and cached by the client against
// the entity id. It is deliberately not a field of EntityState: that is a struct
// inlined into every snapshot, and five colours and a hair model never change for the
// life of a character, and level changes far less often than a snapshot, so carrying
// them there would pay for them at the tick rate for ever. The contract requires the
// current level to be resent whenever it changes; the later name-plate issue owns that
// delivery path. schemas/player.fbs holds the full argument beside the message.
//
// The server never decodes one — receiving it is a client sending a payload only a
// server sends, which the session refuses as a protocol error — so there is no field
// for it in Message.
type PlayerAppearance struct {
	EntityID uint64

	Appearance Appearance
	Name       string
	Level      uint16
	WornHead   uint16
	WornChest  uint16
	WornLegs   uint16

	// HasAppearance is honoured by the encoder so a test can build the frame a client
	// must refuse, exactly as ActionRefused.HasAnchor is. The server always sets it.
	HasAppearance bool

	// HasName distinguishes an omitted string from a present empty one. Empty is legal
	// display text; absence is the malformed V12-shaped description a V13 client must
	// refuse. The server always sets it for a joined character.
	HasName bool
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
	EquipmentSlots uint8

	// PlayerToken is the retired identity field, which a welcome must still carry:
	// present and exactly [PlayerTokenLen] bytes on every accepted handshake, because
	// a decoder treats any other length as a protocol error.
	//
	// **What a server puts here no longer names anybody.** Through V6 it was the
	// identity token the client stored and presented on its next connection; V7
	// settles identity from `ClientHello.session_ticket`, so this server mints
	// nothing and `session.Handshake` fills the field with zeroes — the right shape,
	// and not a credential. See schemas/handshake.fbs, which retires the field and
	// keeps its invariant.
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
			// Cloned for the same reason and read the same way: absent and empty are
			// one zero-length slice, and the contract treats them the same. The
			// contents are never inspected here — a ticket is opaque to this package,
			// and even its length is somebody else's decision.
			SessionTicket: bytes.Clone(hello.SessionTicketBytes()),
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
			Slot:       request.Slot(),
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

	case vnet.PayloadConsumeRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.ConsumeRequest
		request.Init(table.Bytes, table.Pos)

		// Both fields are copied straight through. Nothing here indexes an array with the
		// slot, and whether its stack is edible is a decision the simulation makes against
		// state this package cannot see.
		msg.Consume = &ConsumeRequest{
			Slot:       request.Slot(),
			ClientTick: request.ClientTick(),
		}

	case vnet.PayloadDropItemRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.DropItemRequest
		request.Init(table.Bytes, table.Pos)

		// Both fields copied straight through, a slot past the end of the pack included.
		// Nothing here indexes an array with it, and whether a slot holds something a player
		// may put down is a decision the simulation makes against state it cannot see.
		msg.DropItem = &DropItemRequest{
			Slot:       request.Slot(),
			ClientTick: request.ClientTick(),
		}

	case vnet.PayloadLeaveRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.LeaveRequest
		request.Init(table.Bytes, table.Pos)
		msg.LeaveRequest = &LeaveRequest{}

	case vnet.PayloadSelectCharacterRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.SelectCharacterRequest
		request.Init(table.Bytes, table.Pos)

		// Copied straight through, zero and unknown ids included. Whether an id names a
		// character this account may play is a decision the handshake makes against a
		// store this package cannot see.
		msg.SelectCharacter = &SelectCharacterRequest{CharacterID: request.CharacterId()}

	case vnet.PayloadCreateCharacterRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.CreateCharacterRequest
		request.Init(table.Bytes, table.Pos)

		create := &CreateCharacterRequest{
			// Untrusted display text: copied, never used as a key, and never judged
			// here. An empty or unacceptable name is CHARACTER_NAME_REFUSED, which is a
			// refusal with a reply.
			Name: string(request.Name()),
		}
		// The accessor returns nil for an absent table field, and it must not escape
		// this function either way: it is a view over bytes a client chose, and the
		// recover above is the only thing standing between a bad offset and a panic in a
		// goroutine holding a socket.
		if appearance := request.Appearance(nil); appearance != nil {
			create.Appearance, create.HasAppearance = decodeAppearance(appearance), true
		} else {
			return Message{}, fmt.Errorf("%w: CreateCharacterRequest appearance is absent", ErrMalformed)
		}
		msg.CreateCharacter = create

	case vnet.PayloadChatRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.ChatRequest
		request.Init(table.Bytes, table.Pos)
		// Display text, copied exactly as sent. Empty, absent and arbitrarily long
		// strings are framing-valid; the authoritative chat rule decides acceptance.
		msg.Chat = &ChatRequest{Text: string(request.Text())}

	case vnet.PayloadPartyRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.PartyRequest
		request.Init(table.Bytes, table.Pos)
		action := request.Action()
		switch action {
		case vnet.PartyActionInvite, vnet.PartyActionAccept, vnet.PartyActionDecline,
			vnet.PartyActionLeave, vnet.PartyActionKick:
		default:
			return Message{}, fmt.Errorf("%w: PartyRequest action %d is unknown", ErrMalformed, action)
		}
		msg.Party = &PartyRequest{
			Action:     action,
			TargetName: string(request.TargetName()),
		}

	case vnet.PayloadLootOpenRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.LootOpenRequest
		request.Init(table.Bytes, table.Pos)
		if request.CorpseId() == 0 {
			return Message{}, fmt.Errorf("%w: LootOpenRequest corpse id is absent", ErrMalformed)
		}
		msg.LootOpen = &LootOpenRequest{CorpseID: request.CorpseId(), ClientTick: request.ClientTick()}

	case vnet.PayloadLootTakeRequest:
		table, tErr := unionPayload(env, msg.Kind)
		if tErr != nil {
			return Message{}, tErr
		}
		var request vnet.LootTakeRequest
		request.Init(table.Bytes, table.Pos)
		switch {
		case request.CorpseId() == 0:
			return Message{}, fmt.Errorf("%w: LootTakeRequest corpse id is absent", ErrMalformed)
		case request.EntryId() == 0:
			return Message{}, fmt.Errorf("%w: LootTakeRequest entry id is absent", ErrMalformed)
		case request.Revision() == 0:
			return Message{}, fmt.Errorf("%w: LootTakeRequest revision is absent", ErrMalformed)
		}
		msg.LootTake = &LootTakeRequest{
			CorpseID: request.CorpseId(), EntryID: request.EntryId(),
			Revision: request.Revision(), ClientTick: request.ClientTick(),
		}
	}

	return msg, nil
}

// decodeAppearance copies one appearance out of the buffer.
//
// Verbatim, colours whose reserved high byte is set and an Unknown hair model
// included: schemas/common.fbs documents both as invariants, and neither is a framing
// question. This package owns the envelope; refusing a colour here would close a
// connection whose framing is perfectly readable.
//
// Takes the accessor rather than returning one, for the reason every other field in
// Decode is copied: an accessor is a view over bytes a client chose, and letting one
// escape would move the recover away from the code that needs it.
func decodeAppearance(a *vnet.Appearance) Appearance {
	return Appearance{
		SkinColor:     a.SkinColor(),
		ShirtColor:    a.ShirtColor(),
		TrousersColor: a.TrousersColor(),
		ShoesColor:    a.ShoesColor(),
		HairModel:     a.HairModel(),
		HairColor:     a.HairColor(),
	}
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
	vnet.ServerWelcomeAddEquipmentSlots(b, w.EquipmentSlots)
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
// identity at all: no V6 token and no V7 ticket.
//
// The server never sends one; it exists so tests can produce the input the server
// parses, without hand-rolling a second encoder that could drift from this one —
// which is also why this is one line over [EncodeClientHelloFull] rather than a
// second builder. There is one encoder for this message and there are three ways to
// call it.
func EncodeClientHello(version vnet.ProtocolVersion, playerName string) []byte {
	return EncodeClientHelloFull(version, playerName, nil, nil)
}

// EncodeClientHelloWithToken builds a handshake request that presents token — the V6
// shape, which a V7 server ignores. See schemas/handshake.fbs.
func EncodeClientHelloWithToken(version vnet.ProtocolVersion, playerName string, token []byte) []byte {
	return EncodeClientHelloFull(version, playerName, token, nil)
}

// EncodeClientHelloWithTicket builds a handshake request that presents ticket — the
// V7 shape.
func EncodeClientHelloWithTicket(version vnet.ProtocolVersion, playerName string, ticket []byte) []byte {
	return EncodeClientHelloFull(version, playerName, nil, ticket)
}

// EncodeClientHelloFull builds a handshake request presenting both fields, either of
// which may be nil.
//
// Both are written verbatim, whatever their length: the wrong-length case is one the
// server has to refuse, so the encoder a test refuses it with has to be able to
// produce it. A nil slice and an empty one both encode as a present, zero-length
// vector, which the contract reads as "nothing presented" — the same answer an absent
// field gives.
func EncodeClientHelloFull(version vnet.ProtocolVersion, playerName string, token, ticket []byte) []byte {
	b := flatbuffers.NewBuilder(256)

	// Strings and vectors must exist before the table that references them opens.
	nameOffset := b.CreateString(playerName)
	tokenOffset := b.CreateByteVector(token)
	ticketOffset := b.CreateByteVector(ticket)

	vnet.ClientHelloStart(b)
	vnet.ClientHelloAddProtocolVersion(b, version)
	vnet.ClientHelloAddPlayerName(b, nameOffset)
	vnet.ClientHelloAddPlayerToken(b, tokenOffset)
	vnet.ClientHelloAddSessionTicket(b, ticketOffset)
	hello := vnet.ClientHelloEnd(b)

	return finishEnvelope(b, vnet.PayloadClientHello, hello)
}

// EncodeServerCharacterList builds the answer to a hello the server is willing to
// continue with: every character this account owns on this world, and the limit.
//
// Nothing is re-validated on the way out, exactly as nothing is in
// [EncodeEntitySnapshot]: the summaries come from the server's own store, which is
// the only thing that produces them.
//
// An empty list encodes a present, zero-length vector rather than omitting the field.
// Both decode the same way and both are a legal answer — a new account, or one that
// has never played here — but emitting the shape the contract describes keeps a
// server's frames identical whether or not it happens to have characters to name.
func EncodeServerCharacterList(list CharacterList) []byte {
	// A summary is a table with a string and a nested table, so each costs a handful
	// of offsets besides its fields. Sizing up front avoids repeated growth of a
	// buffer whose shape is known.
	b := flatbuffers.NewBuilder(len(list.Characters)*96 + 128)

	// Every string and every table a vector points at must be finished before that
	// vector opens — so the names come first, then the appearances, then the summaries
	// that reference both, and only then the vector carrying their offsets.
	summaryOffsets := make([]flatbuffers.UOffsetT, len(list.Characters))
	for i, character := range list.Characters {
		nameOffset := b.CreateString(character.Name)
		appearanceOffset := encodeAppearance(b, character.Appearance)

		vnet.CharacterSummaryStart(b)
		vnet.CharacterSummaryAddCharacterId(b, character.CharacterID)
		vnet.CharacterSummaryAddName(b, nameOffset)
		vnet.CharacterSummaryAddAppearance(b, appearanceOffset)
		summaryOffsets[i] = vnet.CharacterSummaryEnd(b)
	}

	vnet.ServerCharacterListStartCharactersVector(b, len(summaryOffsets))
	for i := len(summaryOffsets) - 1; i >= 0; i-- {
		b.PrependUOffsetT(summaryOffsets[i])
	}
	charactersOffset := b.EndVector(len(summaryOffsets))

	vnet.ServerCharacterListStart(b)
	vnet.ServerCharacterListAddCharacters(b, charactersOffset)
	vnet.ServerCharacterListAddMaxCharacters(b, list.MaxCharacters)
	built := vnet.ServerCharacterListEnd(b)

	return finishEnvelope(b, vnet.PayloadServerCharacterList, built)
}

// EncodePlayerAppearance builds the message that tells a session what one player
// looks like.
//
// Sent once, when that player enters view. HasAppearance is honoured rather than
// assumed, because "a message with no appearance" is one of the inputs a client has to
// refuse and there would otherwise be no way to build it.
func EncodePlayerAppearance(p PlayerAppearance) []byte {
	b := flatbuffers.NewBuilder(128)

	// A nested table must be finished before the table that references it opens —
	// unlike a struct, which is written inline while its parent is open.
	var appearanceOffset flatbuffers.UOffsetT
	if p.HasAppearance {
		appearanceOffset = encodeAppearance(b, p.Appearance)
	}
	var nameOffset flatbuffers.UOffsetT
	if p.HasName {
		nameOffset = b.CreateString(p.Name)
	}

	vnet.PlayerAppearanceStart(b)
	vnet.PlayerAppearanceAddEntityId(b, p.EntityID)
	if p.HasAppearance {
		vnet.PlayerAppearanceAddAppearance(b, appearanceOffset)
	}
	if p.HasName {
		vnet.PlayerAppearanceAddName(b, nameOffset)
	}
	vnet.PlayerAppearanceAddLevel(b, p.Level)
	vnet.PlayerAppearanceAddWornHead(b, p.WornHead)
	vnet.PlayerAppearanceAddWornChest(b, p.WornChest)
	vnet.PlayerAppearanceAddWornLegs(b, p.WornLegs)
	built := vnet.PlayerAppearanceEnd(b)

	return finishEnvelope(b, vnet.PayloadPlayerAppearance, built)
}

// EncodeSelectCharacterRequest builds one choice of an existing character. The server
// never sends one, so this exists for the reason [EncodeAttackRequest] does: the tests
// need the bytes a client produces, and a second encoder written by hand could drift
// from this one.
//
// The id is written verbatim, zero and unknown ids included: both are refusals the
// handshake has to make and a test has to be able to send.
func EncodeSelectCharacterRequest(r SelectCharacterRequest) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.SelectCharacterRequestStart(b)
	vnet.SelectCharacterRequestAddCharacterId(b, r.CharacterID)
	request := vnet.SelectCharacterRequestEnd(b)

	return finishEnvelope(b, vnet.PayloadSelectCharacterRequest, request)
}

// EncodeCreateCharacterRequest builds one attempt to make a character, for the same
// reason.
//
// Nothing is validated on the way out. An empty name and a name no server would accept
// are both inputs the handshake must refuse politely; an absent appearance is the one
// the *decoder* must refuse, and HasAppearance is honoured so a test can build it.
func EncodeCreateCharacterRequest(r CreateCharacterRequest) []byte {
	b := flatbuffers.NewBuilder(128)

	// A string and a nested table must both exist before the table that references
	// them opens.
	nameOffset := b.CreateString(r.Name)
	var appearanceOffset flatbuffers.UOffsetT
	if r.HasAppearance {
		appearanceOffset = encodeAppearance(b, r.Appearance)
	}

	vnet.CreateCharacterRequestStart(b)
	vnet.CreateCharacterRequestAddName(b, nameOffset)
	if r.HasAppearance {
		vnet.CreateCharacterRequestAddAppearance(b, appearanceOffset)
	}
	request := vnet.CreateCharacterRequestEnd(b)

	return finishEnvelope(b, vnet.PayloadCreateCharacterRequest, request)
}

// encodeAppearance writes one appearance table and returns its offset.
//
// It must be called while no other table is open, because a nested table is reached
// through an offset and has to be finished before its parent starts — the rule every
// vector in this file already obeys, and the one difference between an Appearance and
// the Vec3 beside it in a snapshot.
//
// Values are written verbatim, a set reserved high byte and an Unknown hair model
// included: those are refusals somebody else makes, and a test has to be able to build
// the frame they refuse.
func encodeAppearance(b *flatbuffers.Builder, a Appearance) flatbuffers.UOffsetT {
	vnet.AppearanceStart(b)
	vnet.AppearanceAddSkinColor(b, a.SkinColor)
	vnet.AppearanceAddShirtColor(b, a.ShirtColor)
	vnet.AppearanceAddTrousersColor(b, a.TrousersColor)
	vnet.AppearanceAddShoesColor(b, a.ShoesColor)
	vnet.AppearanceAddHairModel(b, a.HairModel)
	vnet.AppearanceAddHairColor(b, a.HairColor)
	return vnet.AppearanceEnd(b)
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
	// An EntityState is 40 bytes inlined and an ItemDropState 24; a sparse durable
	// drop adds one 16-byte ItemDropDurability. A MobState is a table
	// and costs an offset besides its fields; a dead player is one 8-byte id. Sizing the
	// builder up front avoids the repeated growth of a buffer whose shape is known, on
	// the most frequently sent payload in the game.
	durableDrops := 0
	for _, drop := range s.Drops {
		if drop.MaxDurability != 0 {
			durableDrops++
		}
	}
	b := flatbuffers.NewBuilder(len(s.Entities)*40 + len(s.Drops)*24 + durableDrops*16 + len(s.Mobs)*64 + len(s.Structures)*48 + len(s.DeadPlayers)*8 + len(s.PartyMembers)*32 + len(s.PartyRoster)*64 + len(s.AccessibleLootCorpses)*8 + 128)

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
	vnet.PlayerVitalsAddHunger(b, s.Vitals.Hunger)
	vnet.PlayerVitalsAddMaxHunger(b, s.Vitals.MaxHunger)
	vnet.PlayerVitalsAddLevel(b, s.Vitals.Level)
	vnet.PlayerVitalsAddExperience(b, s.Vitals.Experience)
	vnet.PlayerVitalsAddExperienceToNext(b, s.Vitals.ExperienceToNext)
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

	// Wear is sparse: the common block, loot and structure drops pay no vector, no
	// vtable slot and no per-element padding. Entries remain in drop order, making the
	// bytes deterministic while entity_id is what binds each one to the fixed drop
	// vector beside it.
	var dropDurabilitiesOffset flatbuffers.UOffsetT
	if durableDrops > 0 {
		vnet.EntitySnapshotStartDropDurabilitiesVector(b, durableDrops)
		for i := len(s.Drops) - 1; i >= 0; i-- {
			drop := s.Drops[i]
			if drop.MaxDurability == 0 {
				continue
			}
			vnet.CreateItemDropDurability(b, drop.EntityID, drop.Durability, drop.MaxDurability)
		}
		dropDurabilitiesOffset = b.EndVector(durableDrops)
	}

	// **Built only when somebody is dead, which is what makes the field free the rest of
	// the time.** An empty FlatBuffers vector is still a vector — four bytes of length
	// and an offset pointing at them, plus the vtable slot that reaches it — and this
	// field is empty on almost every tick of almost every session. Skipping it leaves the
	// field itself free; the later sparse drop wear field follows the same rule independently.
	var deadOffset flatbuffers.UOffsetT
	if len(s.DeadPlayers) > 0 {
		vnet.EntitySnapshotStartDeadPlayersVector(b, len(s.DeadPlayers))
		for i := len(s.DeadPlayers) - 1; i >= 0; i-- {
			b.PrependUint64(s.DeadPlayers[i])
		}
		deadOffset = b.EndVector(len(s.DeadPlayers))
	}

	// Like dead players and drop wear, the no-party case costs no vector and no
	// vtable slot. Members are structs, built back to front so the authoritative
	// party order survives on the wire.
	var partyMembersOffset flatbuffers.UOffsetT
	if len(s.PartyMembers) > 0 {
		vnet.EntitySnapshotStartPartyMembersVector(b, len(s.PartyMembers))
		for i := len(s.PartyMembers) - 1; i >= 0; i-- {
			member := s.PartyMembers[i]
			vnet.CreatePartyMemberState(b, member.EntityID,
				member.Pos[0], member.Pos[1], member.Pos[2],
				member.Health, member.MaxHealth, member.Alive,
			)
		}
		partyMembersOffset = b.EndVector(len(s.PartyMembers))
	}

	// Roster members are tables because each owns a string. Build every string, then
	// every table, before opening the vector that carries their offsets.
	partyRosterOffsets := make([]flatbuffers.UOffsetT, len(s.PartyRoster))
	for i, member := range s.PartyRoster {
		nameOffset := b.CreateString(member.Name)
		vnet.PartyRosterMemberStart(b)
		vnet.PartyRosterMemberAddCharacterId(b, member.CharacterID)
		vnet.PartyRosterMemberAddEntityId(b, member.EntityID)
		vnet.PartyRosterMemberAddName(b, nameOffset)
		vnet.PartyRosterMemberAddOnline(b, member.Online)
		partyRosterOffsets[i] = vnet.PartyRosterMemberEnd(b)
	}
	var partyRosterOffset flatbuffers.UOffsetT
	if len(partyRosterOffsets) > 0 {
		vnet.EntitySnapshotStartPartyRosterVector(b, len(partyRosterOffsets))
		for i := len(partyRosterOffsets) - 1; i >= 0; i-- {
			b.PrependUOffsetT(partyRosterOffsets[i])
		}
		partyRosterOffset = b.EndVector(len(partyRosterOffsets))
	}

	var accessibleLootOffset flatbuffers.UOffsetT
	if len(s.AccessibleLootCorpses) > 0 {
		vnet.EntitySnapshotStartAccessibleLootCorpsesVector(b, len(s.AccessibleLootCorpses))
		for i := len(s.AccessibleLootCorpses) - 1; i >= 0; i-- {
			b.PrependUint64(s.AccessibleLootCorpses[i])
		}
		accessibleLootOffset = b.EndVector(len(s.AccessibleLootCorpses))
	}

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
	if deadOffset != 0 {
		vnet.EntitySnapshotAddDeadPlayers(b, deadOffset)
	}
	if dropDurabilitiesOffset != 0 {
		vnet.EntitySnapshotAddDropDurabilities(b, dropDurabilitiesOffset)
	}
	vnet.EntitySnapshotAddPartyLeaderEntityId(b, s.PartyLeaderEntityID)
	if partyMembersOffset != 0 {
		vnet.EntitySnapshotAddPartyMembers(b, partyMembersOffset)
	}
	if partyRosterOffset != 0 {
		vnet.EntitySnapshotAddPartyRoster(b, partyRosterOffset)
	}
	if accessibleLootOffset != 0 {
		vnet.EntitySnapshotAddAccessibleLootCorpses(b, accessibleLootOffset)
	}
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
	vnet.MineRequestAddSlot(b, r.Slot)
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

// EncodeChatRequest builds client chat intent for protocol round-trip tests. Text is
// copied verbatim; acceptance belongs to the authoritative chat rule.
func EncodeChatRequest(r ChatRequest) []byte {
	b := flatbuffers.NewBuilder(128)
	text := b.CreateString(r.Text)

	vnet.ChatRequestStart(b)
	vnet.ChatRequestAddText(b, text)
	request := vnet.ChatRequestEnd(b)

	return finishEnvelope(b, vnet.PayloadChatRequest, request)
}

// EncodePartyRequest builds client party intent for protocol round-trip tests.
func EncodePartyRequest(r PartyRequest) []byte {
	b := flatbuffers.NewBuilder(128)
	target := b.CreateString(r.TargetName)

	vnet.PartyRequestStart(b)
	vnet.PartyRequestAddAction(b, r.Action)
	vnet.PartyRequestAddTargetName(b, target)
	request := vnet.PartyRequestEnd(b)

	return finishEnvelope(b, vnet.PayloadPartyRequest, request)
}

// EncodeLootOpenRequest builds one corpse-open intent for protocol round-trip tests.
func EncodeLootOpenRequest(r LootOpenRequest) []byte {
	b := flatbuffers.NewBuilder(128)
	vnet.LootOpenRequestStart(b)
	vnet.LootOpenRequestAddCorpseId(b, r.CorpseID)
	vnet.LootOpenRequestAddClientTick(b, r.ClientTick)
	request := vnet.LootOpenRequestEnd(b)
	return finishEnvelope(b, vnet.PayloadLootOpenRequest, request)
}

// EncodeLootTakeRequest builds one stable-entry intent and carries no stack contents.
func EncodeLootTakeRequest(r LootTakeRequest) []byte {
	b := flatbuffers.NewBuilder(128)
	vnet.LootTakeRequestStart(b)
	vnet.LootTakeRequestAddCorpseId(b, r.CorpseID)
	vnet.LootTakeRequestAddEntryId(b, r.EntryID)
	vnet.LootTakeRequestAddRevision(b, r.Revision)
	vnet.LootTakeRequestAddClientTick(b, r.ClientTick)
	request := vnet.LootTakeRequestEnd(b)
	return finishEnvelope(b, vnet.PayloadLootTakeRequest, request)
}

// EncodeLootState builds one complete, per-recipient authoritative container state.
func EncodeLootState(state LootState) []byte {
	b := flatbuffers.NewBuilder(len(state.Entries)*16 + 128)
	vnet.LootStateStartEntriesVector(b, len(state.Entries))
	for i := len(state.Entries) - 1; i >= 0; i-- {
		entry := state.Entries[i]
		vnet.CreateLootEntry(b, entry.EntryID, entry.ItemID, entry.Count, entry.Durability, entry.MaxDurability)
	}
	entries := b.EndVector(len(state.Entries))
	vnet.LootStateStart(b)
	vnet.LootStateAddCorpseId(b, state.CorpseID)
	vnet.LootStateAddRevision(b, state.Revision)
	vnet.LootStateAddEntries(b, entries)
	loot := vnet.LootStateEnd(b)
	return finishEnvelope(b, vnet.PayloadLootState, loot)
}

// EncodeLootClosed explicitly ends one open corpse container.
func EncodeLootClosed(closed LootClosed) []byte {
	b := flatbuffers.NewBuilder(128)
	vnet.LootClosedStart(b)
	vnet.LootClosedAddCorpseId(b, closed.CorpseID)
	payload := vnet.LootClosedEnd(b)
	return finishEnvelope(b, vnet.PayloadLootClosed, payload)
}

// EncodeChatMessage builds one authoritative, accepted chat line. Strings are created
// before the table opens, as FlatBuffers requires for referenced values.
func EncodeChatMessage(message ChatMessage) []byte {
	b := flatbuffers.NewBuilder(128)
	senderName := b.CreateString(message.SenderName)
	line := b.CreateString(message.Text)

	vnet.ChatMessageStart(b)
	vnet.ChatMessageAddSenderEntityId(b, message.SenderEntityID)
	vnet.ChatMessageAddSenderName(b, senderName)
	vnet.ChatMessageAddText(b, line)
	chat := vnet.ChatMessageEnd(b)

	return finishEnvelope(b, vnet.PayloadChatMessage, chat)
}

// EncodePartyInvite builds one authoritative invitation with its remaining lifetime.
func EncodePartyInvite(invite PartyInvite) []byte {
	b := flatbuffers.NewBuilder(128)
	fromName := b.CreateString(invite.FromName)

	vnet.PartyInviteStart(b)
	vnet.PartyInviteAddFromEntityId(b, invite.FromEntityID)
	vnet.PartyInviteAddFromName(b, fromName)
	vnet.PartyInviteAddExpiresMs(b, invite.ExpiresMS)
	encoded := vnet.PartyInviteEnd(b)

	return finishEnvelope(b, vnet.PayloadPartyInvite, encoded)
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

// EncodeConsumeRequest builds one consume intent, for the reason
// EncodeRepairRequest exists: the server never sends one, and the tests need the bytes
// a client produces.
//
// Nothing is validated on the way out. A slot past the end of the inventory is an
// input the simulation must refuse, and there would otherwise be no way to build one.
func EncodeConsumeRequest(r ConsumeRequest) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.ConsumeRequestStart(b)
	vnet.ConsumeRequestAddSlot(b, r.Slot)
	vnet.ConsumeRequestAddClientTick(b, r.ClientTick)
	request := vnet.ConsumeRequestEnd(b)

	return finishEnvelope(b, vnet.PayloadConsumeRequest, request)
}

// EncodeDropItemRequest builds one drop intent, for the reason EncodeRepairRequest exists:
// the server never sends one, and the tests need the bytes a client produces.
//
// Nothing is validated on the way out. A slot past the end of the inventory is an input the
// simulation must refuse, and there would otherwise be no way to build one.
func EncodeDropItemRequest(r DropItemRequest) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.DropItemRequestStart(b)
	vnet.DropItemRequestAddSlot(b, r.Slot)
	vnet.DropItemRequestAddClientTick(b, r.ClientTick)
	request := vnet.DropItemRequestEnd(b)

	return finishEnvelope(b, vnet.PayloadDropItemRequest, request)
}

// EncodeLeaveRequest builds the empty leave intent. It exists for tests and parity
// with every other client request; production clients build the same contract frame.
func EncodeLeaveRequest() []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.LeaveRequestStart(b)
	request := vnet.LeaveRequestEnd(b)

	return finishEnvelope(b, vnet.PayloadLeaveRequest, request)
}

// EncodeLeaveStarted acknowledges the server-owned linger duration.
func EncodeLeaveStarted(remaining time.Duration) []byte {
	b := flatbuffers.NewBuilder(128)

	vnet.LeaveStartedStart(b)
	vnet.LeaveStartedAddRemainingMs(b, uint32(remaining/time.Millisecond))
	started := vnet.LeaveStartedEnd(b)

	return finishEnvelope(b, vnet.PayloadLeaveStarted, started)
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
